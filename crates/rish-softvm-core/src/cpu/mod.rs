//! The interpreter core: fetch, dispatch, exceptions, and interrupts.

use std::collections::VecDeque;

use iced_x86::{Instruction, Register};

use crate::arch::paging::{self, AccessKind, Translation};
use crate::arch::registers::{CpuMode, Registers, index};
use crate::arch::segments::{Descriptor, SegmentRegister, SegmentSelector};
use crate::devices::{Cmos, InterruptLines, Pic8259, Pit8254, PortBus, Uart16550};
use crate::{CpuError, Memory};

mod decode;
mod dispatch;
mod interrupts;
mod tlb;

pub use decode::MAX_INSTRUCTION_BYTES;
pub use dispatch::register_index;
pub use interrupts::{
    Deliverable, Gate, VECTOR_DIVIDE, VECTOR_DOUBLE_FAULT, VECTOR_GENERAL_PROTECTION,
    VECTOR_INVALID_OPCODE, VECTOR_PAGE_FAULT,
};

use decode::{DecodeCache, DecodeMapping};
use tlb::Tlb;

/// Guest nanoseconds charged per retired instruction, for the PIT and CMOS.
/// Ten nanoseconds models a ~100 MHz guest. Lowering this speeds the guest
/// clock and stretches guest-time deadlines (a lever for slow daemons such as
/// dockerd waiting on containerd), at the cost of coarser timekeeping.
const NANOSECONDS_PER_INSTRUCTION: u64 = 10;

/// Instructions between chipset updates. Devices are polled in batches so the
/// per-instruction cost stays low; the batch is charged in full to the timer
/// so guest time still advances at the same rate.
const DEVICE_TICK_INTERVAL: u32 = 64;

/// One port-I/O access, for boot diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoEvent {
    pub retired: u64,
    pub write: bool,
    pub port: u16,
    pub size: u8,
    pub value: u32,
}

/// One delivered exception or interrupt, for boot diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultEvent {
    pub retired: u64,
    pub vector: u8,
    pub error_code: u16,
    /// Instruction pointer the fault was taken at.
    pub rip: u64,
    /// Faulting linear address (meaningful for vector 14).
    pub cr2: u64,
    pub is_exception: bool,
}

/// One retired-instruction trace entry: RIP, the sixteen captured GPRs,
/// and up to sixteen instruction bytes.
pub type TraceEntry = (
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    [u8; 16],
);

pub struct Cpu {
    pub regs: Registers,
    pub memory: Memory,
    pub ports: PortBus,
    pub pic: Pic8259,
    pub pit: Pit8254,
    pub cmos: Cmos,
    pub acpi_pm: crate::devices::acpi_pm::AcpiPm,
    pub uart_console: Uart16550,
    pub uart_control: Uart16550,
    pub lines: InterruptLines,
    pub tsc: u64,
    pub halted: bool,
    pub boot_ok_seen: bool,
    pub kernel_gs_base: u64,
    pub xcr0: u64,
    pub msr_star: u64,
    pub msr_lstar: u64,
    pub msr_cstar: u64,
    pub msr_fmask: u64,
    pub msr_tsc_aux: u64,
    pub fpu_control_word: u16,
    pub fpu_status_word: u16,
    /// x87 register stack: eight 64-bit slots. ST(i) is the slot at
    /// `(fpu_top + i) & 7`; a push predecrements the top, a pop increments it.
    /// The extended 80-bit format is approximated with f64, which carries every
    /// value real programs load through the FPU exactly enough for their use.
    pub fpu_stack: [f64; 8],
    /// Top-of-stack index into `fpu_stack`.
    pub fpu_top: u8,
    pub mxcsr: u32,
    /// Retired-instruction ring for gap diagnostics. Empty unless a capacity
    /// is set, because recording every instruction is the single largest
    /// interpreter cost.
    pub trace: VecDeque<TraceEntry>,
    trace_capacity: usize,
    /// Recent port-I/O accesses. Empty unless a capacity is set.
    pub io_log: VecDeque<IoEvent>,
    io_log_capacity: usize,
    /// How many interrupts and exceptions have been delivered through the IDT.
    pub interrupts_delivered: u64,
    pub exceptions_raised: u64,
    /// Recent exceptions and interrupts. Bounded; always on, because the
    /// record is one small struct per fault and faults are rare.
    pub fault_log: VecDeque<FaultEvent>,
    pub(crate) pending_interrupts: VecDeque<Deliverable>,
    pub(crate) in_exception: bool,
    pub(crate) in_double_fault: bool,
    tlb: Tlb,
    decoded: DecodeCache,
    device_countdown: u32,
    /// Set by hlt with IF=1 (the idle loop): the CPU retires no instructions
    /// until an interrupt is deliverable.
    pub waiting_for_interrupt: bool,
    /// One-instruction STI shadow: delivery is held off until the
    /// instruction after sti has retired.
    pub interrupt_shadow: bool,
    /// Linear address of the instruction currently executing, used to give
    /// fault-class exceptions a return address that points back at the
    /// faulting instruction.
    pub(crate) instruction_start: u64,
}

impl Cpu {
    pub fn new(memory_mib: usize, boot_epoch_seconds: u64) -> Result<Self, CpuError> {
        let mut memory = Memory::new(memory_mib)?;
        memory.attach_lapic();
        let trace_capacity = std::env::var("RISH_TRACE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        Ok(Self {
            regs: Registers::default(),
            memory,
            ports: PortBus::default(),
            pic: Pic8259::new(),
            pit: Pit8254::new(),
            cmos: Cmos::new(boot_epoch_seconds),
            acpi_pm: crate::devices::acpi_pm::AcpiPm::new(),
            uart_console: Uart16550::new(0x3F8, 4096, 65536),
            uart_control: Uart16550::new(0x2F8, 65536, 65536),
            lines: InterruptLines::default(),
            tsc: 0,
            halted: false,
            boot_ok_seen: false,
            kernel_gs_base: 0,
            xcr0: 0x3,
            msr_star: 0,
            msr_lstar: 0,
            msr_cstar: 0,
            msr_fmask: 0,
            msr_tsc_aux: 0,
            fpu_control_word: 0x037F,
            fpu_status_word: 0,
            fpu_stack: [0.0; 8],
            fpu_top: 0,
            mxcsr: 0x1F80,
            trace: VecDeque::new(),
            trace_capacity,
            io_log: VecDeque::new(),
            io_log_capacity: 0,
            interrupts_delivered: 0,
            exceptions_raised: 0,
            fault_log: VecDeque::new(),
            pending_interrupts: VecDeque::new(),
            in_exception: false,
            in_double_fault: false,
            tlb: Tlb::new(),
            decoded: DecodeCache::new(),
            device_countdown: DEVICE_TICK_INTERVAL,
            waiting_for_interrupt: false,
            interrupt_shadow: false,
            instruction_start: 0,
        })
    }

    /// Sets how many retired instructions the diagnostic trace keeps. Zero
    /// disables tracing, which is the default.
    pub fn set_trace_capacity(&mut self, capacity: usize) {
        self.trace_capacity = capacity;
        if capacity == 0 {
            self.trace.clear();
        }
    }

    /// Sets how many port-I/O accesses the diagnostic log keeps. Zero
    /// disables logging, which is the default.
    pub fn set_io_log_capacity(&mut self, capacity: usize) {
        self.io_log_capacity = capacity;
        if capacity == 0 {
            self.io_log.clear();
        }
    }

    fn record_io(&mut self, write: bool, port: u16, size: u8, value: u32) {
        if self.io_log_capacity == 0 {
            return;
        }
        if self.io_log.len() >= self.io_log_capacity {
            self.io_log.pop_front();
        }
        self.io_log.push_back(IoEvent {
            retired: self.regs.instructions_retired,
            write,
            port,
            size,
            value,
        });
    }

    /// TLB and decode-cache hit/miss counters, for boot diagnostics.
    #[must_use]
    pub fn cache_counters(&self) -> ((u64, u64), (u64, u64)) {
        (self.tlb.counters(), self.decoded.counters())
    }

    /// Executes exactly one instruction, then services one pending interrupt.
    pub fn step(&mut self) -> Result<(), CpuError> {
        if self.halted {
            return Err(CpuError::Halted);
        }
        if self.interrupt_shadow {
            // The instruction after sti executes before any delivery.
            self.interrupt_shadow = false;
        } else {
            self.deliver_pending()?;
        }
        if self.waiting_for_interrupt {
            // hlt with interrupts enabled: the CPU is asleep. Advance the
            // clock sources so the timer can eventually fire, and keep the
            // serial lines fresh for the wake-up interrupt.
            self.service_devices(DEVICE_TICK_INTERVAL);
            return Ok(());
        }
        self.execute_one()?;
        self.regs.instructions_retired = self.regs.instructions_retired.saturating_add(1);
        self.tsc = self.tsc.saturating_add(1);
        self.device_countdown -= 1;
        if self.device_countdown == 0 {
            self.device_countdown = DEVICE_TICK_INTERVAL;
            self.service_devices(DEVICE_TICK_INTERVAL);
        }
        Ok(())
    }

    /// Advances the chipset by a batch of instructions' worth of guest time.
    fn service_devices(&mut self, instructions: u32) {
        self.memory.lapic_tick(instructions);
        let timer_fired = self
            .pit
            .advance(NANOSECONDS_PER_INSTRUCTION * u64::from(instructions));
        // The 8250 serial IRQs are edge triggered: deliver a fresh edge for
        // each new receive-data or transmit-empty event rather than holding a
        // level, which an edge-triggered controller would only ever latch once.
        let console_edge = self.uart_console.poll_interrupt_edge();
        let control_edge = self.uart_control.poll_interrupt_edge();
        let levels = u32::from(self.lines.asserted);
        if timer_fired {
            // The PIT output in rate-generator mode is a pulse per period:
            // an edge for both interrupt controllers, not a held level.
            self.pic.pulse(0);
            self.memory.ioapic_pulse(0, levels);
        }
        if console_edge {
            let line = self.uart_console.irq_line();
            self.pic.pulse(line);
            self.memory.ioapic_pulse(line, levels);
        }
        if control_edge {
            let line = self.uart_control.irq_line();
            self.pic.pulse(line);
            self.memory.ioapic_pulse(line, levels);
        }
        self.pic.set_input(self.lines);
        self.memory.ioapic_set_lines(levels);
    }

    /// Pulses any pending serial interrupt immediately, outside the batched
    /// device tick. Called right after a serial register write so a receive- or
    /// transmit-interrupt enable is honoured on the next instruction instead of
    /// up to a full tick later, which is the difference between the guest
    /// draining the control channel and stalling on it.
    fn service_serial_edges(&mut self) {
        let levels = u32::from(self.lines.asserted);
        if self.uart_console.poll_interrupt_edge() {
            let line = self.uart_console.irq_line();
            self.pic.pulse(line);
            self.memory.ioapic_pulse(line, levels);
        }
        if self.uart_control.poll_interrupt_edge() {
            let line = self.uart_control.irq_line();
            self.pic.pulse(line);
            self.memory.ioapic_pulse(line, levels);
        }
    }

    /// Runs up to the given number of instructions, returning how many ran.
    pub fn run_instructions(&mut self, limit: u64) -> Result<u64, CpuError> {
        for executed in 0..limit {
            if self.halted {
                return Ok(executed);
            }
            self.step()?;
        }
        Ok(limit)
    }

    fn execute_one(&mut self) -> Result<(), CpuError> {
        loop {
            self.instruction_start = self.regs.rip;
            let instruction = match self.fetch() {
                Ok(instruction) => instruction,
                Err(CpuError::PageFault { linear, error_code }) => {
                    // Instruction fetch fault: deliver through the guest IDT;
                    // the handler maps the page and the fetch is retried.
                    self.regs.cr2 = linear;
                    self.raise(VECTOR_PAGE_FAULT, error_code, true)?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if self.trace_capacity != 0 {
                let bytes = self.read_fetch_window(self.instruction_start)?;
                self.record_trace(&bytes);
            }
            // An instruction that faults part-way through must leave no
            // architectural trace: x86 faults are restartable, so the handler
            // maps the page and the very same instruction runs again from a
            // clean state. A `push`/`call` that decrements RSP and then faults
            // on the store is the load-bearing case — without the rollback the
            // retry pushes a second time and every stack slot shifts by eight.
            //
            // Only instructions that can touch memory can page-fault, and only
            // the general-purpose file and flags are ever modified before such
            // a fault (no instruction writes an XMM register and then faults on
            // a separate memory operand), so the snapshot is skipped entirely
            // for register-only instructions and never copies the 256-byte XMM
            // file. This keeps the fault-safety net off the hot path.
            let length = instruction.len() as u64;
            let snapshot = if may_fault(&instruction) {
                Some((self.regs.gpr, self.regs.rflags))
            } else {
                None
            };
            self.regs.rip = self.regs.rip.wrapping_add(length);
            match self.dispatch(&instruction) {
                Err(CpuError::PageFault { linear, error_code }) => {
                    // Roll the register file back to before the instruction and
                    // rewind rip so the fault frame names the faulting insn.
                    if let Some((gpr, rflags)) = snapshot {
                        self.regs.gpr = gpr;
                        self.regs.rflags = rflags;
                    }
                    self.regs.rip = self.regs.rip.wrapping_sub(length);
                    self.regs.cr2 = linear;
                    self.raise(VECTOR_PAGE_FAULT, error_code, true)?;
                    continue;
                }
                Err(CpuError::UnimplementedInstruction { code, address, .. }) => {
                    let bytes = self
                        .read_fetch_window(self.instruction_start)
                        .unwrap_or([0; MAX_INSTRUCTION_BYTES]);
                    return Err(CpuError::UnimplementedInstruction {
                        code,
                        address,
                        bytes: bytes.to_vec(),
                    });
                }
                other => return other,
            }
        }
    }

    fn record_trace(&mut self, bytes: &[u8; MAX_INSTRUCTION_BYTES]) {
        let mut head = [0_u8; 16];
        head[..MAX_INSTRUCTION_BYTES].copy_from_slice(bytes);
        if self.trace.len() >= self.trace_capacity {
            self.trace.pop_front();
        }
        let gpr = &self.regs.gpr;
        self.trace.push_back((
            self.regs.rip,
            gpr[index::RAX],
            gpr[index::RCX],
            gpr[index::RDX],
            gpr[index::RBX],
            gpr[index::RSI],
            gpr[index::RDI],
            gpr[index::RBP],
            gpr[index::RSP],
            gpr[index::R8],
            gpr[index::R9],
            gpr[index::R10],
            gpr[index::R11],
            gpr[index::R12],
            gpr[index::R13],
            gpr[index::R14],
            gpr[index::R15],
            head,
        ));
    }

    /// Reads and decodes the instruction at RIP, reusing a cached decode when
    /// the same bytes still live at the same physical address.
    fn fetch(&mut self) -> Result<Instruction, CpuError> {
        let (bitness, ip) = self.decode_environment();
        let linear = self.regs.code_base().wrapping_add(ip);
        let translation_epoch = self.tlb.epoch();
        let execution_context = self.decode_execution_context(bitness);
        if !tlb_disabled()
            && let Some(cached) = self.decoded.lookup_mapped(
                &self.memory,
                ip,
                linear,
                translation_epoch,
                execution_context,
            )
        {
            return Ok(cached);
        }
        let mut window = [0_u8; MAX_INSTRUCTION_BYTES];
        let first = self.translate(linear, AccessKind::Execute)?;
        let in_page = (4096 - (first & 0xFFF)) as usize;
        let single_page = in_page >= MAX_INSTRUCTION_BYTES;
        let cacheable =
            single_page && first <= self.memory.len().saturating_sub(MAX_INSTRUCTION_BYTES) as u64;
        // A single-page instruction lies entirely within `first`'s page, so its
        // page write counter validates the whole cached decode. Check the cache
        // before touching guest memory: a hit needs no read and no byte compare.
        if single_page {
            let generation = self.memory.page_generation(first);
            if cacheable {
                let mapping = DecodeMapping {
                    ip,
                    linear,
                    physical: first,
                    page_generation: generation,
                    translation_epoch,
                    execution_context,
                };
                if let Some(instruction) = self.decoded.lookup(mapping) {
                    return Ok(instruction);
                }
            }
            self.memory.read(first, &mut window)?;
        } else {
            // Instruction fetch crosses page boundaries like hardware: keep
            // translating each successive linear page until 15 bytes are read.
            self.memory.read(first, &mut window[..in_page])?;
            let mut fetched = in_page;
            while fetched < MAX_INSTRUCTION_BYTES {
                let physical =
                    self.translate(linear.wrapping_add(fetched as u64), AccessKind::Execute)?;
                let count =
                    (MAX_INSTRUCTION_BYTES - fetched).min(4096 - (physical & 0xFFF) as usize);
                self.memory
                    .read(physical, &mut window[fetched..fetched + count])?;
                fetched += count;
            }
        }
        let instruction = decode::decode_window(bitness, &window, ip);
        if instruction.is_invalid() {
            return Err(CpuError::UnimplementedInstruction {
                code: "invalid".to_owned(),
                address: self.regs.rip,
                bytes: window.to_vec(),
            });
        }
        if cacheable {
            let generation = self.memory.page_generation(first);
            self.decoded.insert(
                DecodeMapping {
                    ip,
                    linear,
                    physical: first,
                    page_generation: generation,
                    translation_epoch,
                    execution_context,
                },
                instruction,
            );
        }
        Ok(instruction)
    }

    /// Re-reads the current instruction bytes only for tracing or fatal error
    /// diagnostics. Normal execution receives just the decoded instruction, so
    /// a cache hit does not copy a 15-byte window through the hot return path.
    fn read_fetch_window(
        &mut self,
        instruction_pointer: u64,
    ) -> Result<[u8; MAX_INSTRUCTION_BYTES], CpuError> {
        let bitness = self.decode_environment().0;
        let ip = match bitness {
            16 => instruction_pointer & 0xFFFF,
            32 => instruction_pointer & 0xFFFF_FFFF,
            _ => instruction_pointer,
        };
        let linear = self.regs.code_base().wrapping_add(ip);
        let first = self.translate(linear, AccessKind::Execute)?;
        let mut window = [0_u8; MAX_INSTRUCTION_BYTES];
        let in_page = (4096 - (first & 0xFFF)) as usize;
        if in_page >= MAX_INSTRUCTION_BYTES {
            self.memory.read(first, &mut window)?;
            return Ok(window);
        }
        self.memory.read(first, &mut window[..in_page])?;
        let mut fetched = in_page;
        while fetched < MAX_INSTRUCTION_BYTES {
            let physical =
                self.translate(linear.wrapping_add(fetched as u64), AccessKind::Execute)?;
            let count = (MAX_INSTRUCTION_BYTES - fetched).min(4096 - (physical & 0xFFF) as usize);
            self.memory
                .read(physical, &mut window[fetched..fetched + count])?;
            fetched += count;
        }
        Ok(window)
    }

    fn decode_environment(&self) -> (u32, u64) {
        match self.regs.mode() {
            CpuMode::Real | CpuMode::Protected16 => (16, self.regs.rip & 0xFFFF),
            CpuMode::Protected32 => (32, self.regs.rip & 0xFFFF_FFFF),
            CpuMode::Long => (64, self.regs.rip),
        }
    }

    #[inline]
    fn decode_execution_context(&self, bitness: u32) -> u32 {
        let mut context = bitness | (u32::from(self.regs.cpl()) << 8);
        if self.regs.cr0.contains(crate::arch::registers::Cr0::PG) {
            context |= 1 << 10;
        }
        if self.regs.cr4.contains(crate::arch::registers::Cr4::SMEP) {
            context |= 1 << 11;
        }
        if self.regs.efer.contains(crate::arch::registers::Efer::NXE) {
            context |= 1 << 12;
        }
        context
    }

    // ---- address translation ----

    /// Translates a linear address for an access made at the current
    /// privilege level.
    #[inline]
    pub fn translate(&self, linear: u64, kind: AccessKind) -> Result<u64, CpuError> {
        self.translate_as(linear, kind, self.regs.cpl())
    }

    /// Translates a linear address for an access the CPU itself performs on
    /// descriptor tables, which hardware always does with supervisor rights.
    #[inline]
    pub fn translate_privileged(&self, linear: u64, kind: AccessKind) -> Result<u64, CpuError> {
        self.translate_as(linear, kind, 0)
    }

    fn translate_as(&self, linear: u64, kind: AccessKind, cpl: u8) -> Result<u64, CpuError> {
        if !self.regs.cr0.contains(crate::arch::registers::Cr0::PG) {
            return Ok(linear);
        }
        // The TLB is behind a cell so translation stays available on shared
        // borrows, matching how hardware reads it during any access.
        // Diagnostic switch RISH_NO_TLB=1 bypasses the cache to isolate TLB
        // coherence bugs from walk bugs.
        let cached = if tlb_disabled() {
            None
        } else {
            self.tlb_lookup(linear)
        };
        let walked = match cached {
            Some(translation) => translation,
            None => {
                let walked = paging::walk(
                    &self.memory,
                    self.regs.cr3,
                    self.regs.cr4,
                    self.regs.efer,
                    linear,
                )
                .map_err(|fault| page_fault(paging::with_access_bits(fault, kind, cpl)))?;
                self.tlb_insert(linear, walked);
                walked
            }
        };
        if let Err(fault) = paging::check_access(
            &walked,
            linear,
            kind,
            cpl,
            self.regs.cr0,
            self.regs.cr4,
            self.regs.efer,
            self.regs
                .rflags
                .contains(crate::arch::registers::RFlags::AC),
        ) {
            // A permission fault means the guest kernel is about to fix the
            // PTE (demand paging, copy-on-write, dirty/accessed bits). Drop the
            // cached entry so the retried access re-walks and sees the update.
            // Linux does not always issue an invlpg on these in-place PTE
            // upgrades — it relies on the faulting access re-walking — so a
            // surviving stale entry turns into an endless fault storm.
            self.tlb_invalidate(linear);
            return Err(page_fault(fault));
        }
        Ok(walked.physical(linear))
    }

    #[inline]
    fn tlb_lookup(&self, linear: u64) -> Option<Translation> {
        // SAFETY-free interior mutability: the TLB is a pure cache, so a
        // shared borrow may update its counters and entries.
        let tlb = &self.tlb as *const Tlb as *mut Tlb;
        unsafe { (*tlb).lookup(linear) }
    }

    #[inline]
    fn tlb_invalidate(&self, linear: u64) {
        let tlb = &self.tlb as *const Tlb as *mut Tlb;
        unsafe { (*tlb).invalidate(linear) }
    }

    #[inline]
    fn tlb_insert(&self, linear: u64, translation: Translation) {
        let tlb = &self.tlb as *const Tlb as *mut Tlb;
        unsafe { (*tlb).insert(linear, translation) }
    }

    /// Drops every cached translation, as a CR3 reload does.
    pub fn flush_tlb(&mut self) {
        self.tlb.flush();
    }

    /// Drops the cached translation for one page, as `invlpg` does.
    pub fn invalidate_page(&mut self, linear: u64) {
        self.tlb.invalidate(linear);
    }

    // ---- operands ----

    /// Computes the effective linear address for a memory operand.
    pub fn effective_address(&self, instruction: &Instruction, _operand: u32) -> u64 {
        let segment_base = self.regs.data_base(instruction.segment_prefix());
        if instruction.is_ip_rel_memory_operand() {
            // A segment override still applies to a RIP-relative operand.
            // Linux addresses per-CPU data as %gs:offset(%rip), so dropping
            // the base here silently reads the static per-CPU template
            // instead of this CPU's copy.
            return segment_base.wrapping_add(instruction.ip_rel_memory_address());
        }
        let base = instruction.memory_base();
        let index = instruction.memory_index();
        let scale = instruction.memory_index_scale();
        let displacement = instruction.memory_displacement64();
        let address_size = dispatch::address_size_of(instruction);
        let mut address = displacement;
        if base != Register::None {
            address = address.wrapping_add(self.regs.gpr(register_index(base)));
        }
        if index != Register::None {
            address = address.wrapping_add(
                self.regs
                    .gpr(register_index(index))
                    .wrapping_mul(u64::from(scale)),
            );
        }
        let address = match address_size {
            2 => address & 0xFFFF,
            4 => address & 0xFFFF_FFFF,
            _ => address,
        };
        // In long mode `data_base` already reports zero for every segment
        // except FS and GS, so the base always applies.
        segment_base.wrapping_add(address)
    }

    /// Reads a memory operand of the given width as a zero-extended u64.
    pub fn read_operand(
        &mut self,
        instruction: &Instruction,
        operand: u32,
        size: u8,
    ) -> Result<u64, CpuError> {
        let linear = self.effective_address(instruction, operand);
        // Fast path: the whole access lies inside one page, so a single
        // physical translation is exact.
        if (linear & 0xFFF) + u64::from(size) <= 0x1000 {
            let physical = self.translate(linear, AccessKind::Read)?;
            return match size {
                1 => Ok(u64::from(self.memory.read_u8(physical)?)),
                2 => Ok(u64::from(self.memory.read_u16(physical)?)),
                4 => Ok(u64::from(self.memory.read_u32(physical)?)),
                8 => self.memory.read_u64(physical),
                _ => Err(CpuError::GuestFault(format!(
                    "unsupported operand size {size}"
                ))),
            };
        }
        // Slow path: a misaligned access that spans a page boundary. The two
        // pages need not be physically contiguous, so translate each one.
        if !matches!(size, 1 | 2 | 4 | 8) {
            return Err(CpuError::GuestFault(format!(
                "unsupported operand size {size}"
            )));
        }
        let mut bytes = [0_u8; 8];
        self.read_linear_bytes(linear, &mut bytes[..size as usize])?;
        Ok(u64::from_le_bytes(bytes))
    }

    /// Writes a memory operand of the given width from a value.
    pub fn write_operand(
        &mut self,
        instruction: &Instruction,
        operand: u32,
        size: u8,
        value: u64,
    ) -> Result<(), CpuError> {
        let linear = self.effective_address(instruction, operand);
        // Fast path: the whole access lies inside one page.
        if (linear & 0xFFF) + u64::from(size) <= 0x1000 {
            let physical = self.translate(linear, AccessKind::Write)?;
            return match size {
                1 => self.memory.write_u8(physical, value as u8),
                2 => self.memory.write_u16(physical, value as u16),
                4 => self.memory.write_u32(physical, value as u32),
                8 => self.memory.write_u64(physical, value),
                _ => Err(CpuError::GuestFault(format!(
                    "unsupported operand size {size}"
                ))),
            };
        }
        // Slow path: the store spans a page boundary; translate each page.
        if !matches!(size, 1 | 2 | 4 | 8) {
            return Err(CpuError::GuestFault(format!(
                "unsupported operand size {size}"
            )));
        }
        self.write_linear_bytes(linear, &value.to_le_bytes()[..size as usize])
    }

    /// Operand size in bytes for a decoded instruction.
    pub fn operand_size(&self, instruction: &Instruction) -> u8 {
        match instruction.code_size() {
            iced_x86::CodeSize::Code16 => 2,
            iced_x86::CodeSize::Code32 => 4,
            iced_x86::CodeSize::Code64 => 8,
            iced_x86::CodeSize::Unknown => 8,
        }
    }

    // ---- descriptor tables ----

    /// Loads a data/stack segment selector from the GDT or LDT.
    pub fn load_segment_from_table(
        &self,
        selector: SegmentSelector,
    ) -> Result<SegmentRegister, CpuError> {
        if selector.index() == 0 && selector.table() == 0 {
            return Ok(SegmentRegister {
                selector,
                base: 0,
                limit: 0,
                ..SegmentRegister::default()
            });
        }
        let entry = self.descriptor_entry(selector)?;
        Ok(Descriptor::decode(entry).load(selector))
    }

    /// Loads a 16-byte system descriptor (LDT or TSS) from the GDT. The upper
    /// half carries bits 63:32 of the base, which an 8-byte read would drop.
    pub fn load_system_descriptor(
        &self,
        selector: SegmentSelector,
    ) -> Result<SegmentRegister, CpuError> {
        let low = self.descriptor_entry(selector)?;
        let mut loaded = Descriptor::decode(low).load(selector);
        if self.regs.mode() == CpuMode::Long {
            let address = self.descriptor_address(selector).wrapping_add(8);
            let high = self
                .memory
                .read_u64(self.translate_privileged(address, AccessKind::Read)?)?;
            loaded.base |= (high & 0xFFFF_FFFF) << 32;
        }
        Ok(loaded)
    }

    /// Nulls the data segments whose descriptor privilege level is stronger
    /// than the ring being returned to, as a privilege-lowering return does.
    pub fn drop_inaccessible_data_segments(&mut self, cpl: u8) {
        for segment in [
            &mut self.regs.ds,
            &mut self.regs.es,
            &mut self.regs.fs,
            &mut self.regs.gs,
        ] {
            if segment.selector.0 != 0 && !segment.conforming && segment.attributes.dpl < cpl {
                *segment = SegmentRegister {
                    selector: SegmentSelector(0),
                    base: segment.base,
                    limit: 0,
                    ..SegmentRegister::default()
                };
            }
        }
    }

    fn descriptor_address(&self, selector: SegmentSelector) -> u64 {
        let table_base = if selector.table() == 0 {
            self.regs.gdt_base
        } else {
            self.regs.ldtr.base
        };
        table_base.wrapping_add(u64::from(selector.index()) * 8)
    }

    fn descriptor_entry(&self, selector: SegmentSelector) -> Result<u64, CpuError> {
        let address = self.descriptor_address(selector);
        self.memory
            .read_u64(self.translate_privileged(address, AccessKind::Read)?)
    }

    // ---- stack ----

    pub fn push16(&mut self, value: u16) -> Result<(), CpuError> {
        self.regs.gpr[index::RSP] = self.regs.gpr[index::RSP].wrapping_sub(2);
        let linear = self
            .regs
            .stack_base()
            .wrapping_add(self.regs.gpr[index::RSP]);
        let physical = self.translate(linear, AccessKind::Write)?;
        self.memory.write_u16(physical, value)
    }

    pub fn push32(&mut self, value: u32) -> Result<(), CpuError> {
        self.regs.gpr[index::RSP] = self.regs.gpr[index::RSP].wrapping_sub(4);
        let linear = self
            .regs
            .stack_base()
            .wrapping_add(self.regs.gpr[index::RSP]);
        let physical = self.translate(linear, AccessKind::Write)?;
        self.memory.write_u32(physical, value)
    }

    pub fn push64(&mut self, value: u64) -> Result<(), CpuError> {
        self.regs.gpr[index::RSP] = self.regs.gpr[index::RSP].wrapping_sub(8);
        let physical = self.translate(self.regs.gpr[index::RSP], AccessKind::Write)?;
        self.memory.write_u64(physical, value)
    }

    fn pop16(&mut self) -> Result<u16, CpuError> {
        let linear = self
            .regs
            .stack_base()
            .wrapping_add(self.regs.gpr[index::RSP]);
        let physical = self.translate(linear, AccessKind::Read)?;
        let value = self.memory.read_u16(physical)?;
        self.regs.gpr[index::RSP] = self.regs.gpr[index::RSP].wrapping_add(2);
        Ok(value)
    }

    fn pop32(&mut self) -> Result<u32, CpuError> {
        let linear = self
            .regs
            .stack_base()
            .wrapping_add(self.regs.gpr[index::RSP]);
        let physical = self.translate(linear, AccessKind::Read)?;
        let value = self.memory.read_u32(physical)?;
        self.regs.gpr[index::RSP] = self.regs.gpr[index::RSP].wrapping_add(4);
        Ok(value)
    }

    pub fn pop64(&mut self) -> Result<u64, CpuError> {
        let physical = self.translate(self.regs.gpr[index::RSP], AccessKind::Read)?;
        let value = self.memory.read_u64(physical)?;
        self.regs.gpr[index::RSP] = self.regs.gpr[index::RSP].wrapping_add(8);
        Ok(value)
    }

    /// Pops the natural stack width for the current mode.
    pub fn pop_native(&mut self) -> Result<u64, CpuError> {
        match self.regs.mode() {
            CpuMode::Long => self.pop64(),
            CpuMode::Protected32 => Ok(u64::from(self.pop32()?)),
            _ => Ok(u64::from(self.pop16()?)),
        }
    }

    /// Pushes the natural stack width for the current mode.
    pub fn push_native(&mut self, value: u64) -> Result<(), CpuError> {
        match self.regs.mode() {
            CpuMode::Long => self.push64(value),
            CpuMode::Protected32 => self.push32(value as u32),
            _ => self.push16(value as u16),
        }
    }

    /// Reads `bytes.len()` bytes from a linear address, translating each page
    /// separately so an access that straddles a page boundary reads the right
    /// physical bytes on both sides. Unaligned SSE moves (`movdqu`) routinely
    /// cross pages, so reading a single contiguous physical run would return
    /// the wrong bytes past the boundary.
    pub(crate) fn read_linear_bytes(
        &mut self,
        linear: u64,
        bytes: &mut [u8],
    ) -> Result<(), CpuError> {
        let mut read = 0_usize;
        while read < bytes.len() {
            let physical = self.translate(linear.wrapping_add(read as u64), AccessKind::Read)?;
            let in_page = (4096 - (physical & 0xFFF) as usize).min(bytes.len() - read);
            self.memory
                .read(physical, &mut bytes[read..read + in_page])?;
            read += in_page;
        }
        Ok(())
    }

    /// Writes a buffer at a linear address, translating each page separately.
    /// The write-side counterpart to [`Cpu::read_linear_bytes`].
    pub(crate) fn write_linear_bytes(&mut self, linear: u64, bytes: &[u8]) -> Result<(), CpuError> {
        let mut written = 0_usize;
        while written < bytes.len() {
            let physical =
                self.translate(linear.wrapping_add(written as u64), AccessKind::Write)?;
            let in_page = (4096 - (physical & 0xFFF) as usize).min(bytes.len() - written);
            self.memory
                .write(physical, &bytes[written..written + in_page])?;
            written += in_page;
        }
        Ok(())
    }

    // ---- port I/O ----

    /// Serializes a port read through the attached device set.
    pub fn io_read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        let value = self.dispatch_io_read(port, size)?;
        self.record_io(false, port, size, value);
        Ok(value)
    }

    fn dispatch_io_read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        match port {
            0x3F8..=0x3FF => {
                let value =
                    crate::devices::PortDevice::read(&mut self.uart_console, port, 1)? as u8;
                // Draining the receive register can leave more buffered input
                // and the interrupt still enabled; re-arm the next receive edge
                // promptly so the guest ISR drains the whole batch in one run
                // instead of one byte per batched device tick.
                self.service_serial_edges();
                Ok(u32::from(value))
            }
            0x2F8..=0x2FF => {
                let value =
                    crate::devices::PortDevice::read(&mut self.uart_control, port, 1)? as u8;
                self.service_serial_edges();
                Ok(u32::from(value))
            }
            0x20 | 0x21 | 0xA0 | 0xA1 => {
                crate::devices::PortDevice::read(&mut self.pic, port, size)
            }
            0x40..=0x43 | crate::devices::pit8254::PORT_SYSTEM_CONTROL_B => {
                crate::devices::PortDevice::read(&mut self.pit, port, size)
            }
            0x70 | 0x71 => crate::devices::PortDevice::read(&mut self.cmos, port, size),
            _ if crate::devices::acpi_pm::AcpiPm::handles(port) => {
                Ok(self.acpi_pm.read(port, size, self.tsc))
            }
            _ => self.ports.read(port, size),
        }
    }

    /// Serializes a port write through the attached device set.
    pub fn io_write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        self.record_io(true, port, size, value);
        self.dispatch_io_write(port, size, value)
    }

    fn dispatch_io_write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        match port {
            0x3F8..=0x3FF => {
                crate::devices::PortDevice::write(&mut self.uart_console, port, 1, value & 0xFF)?;
                // Deliver any serial interrupt the write just armed (typically
                // an IER enable) on the next instruction, while the enable is
                // still in effect. The guest driver enables the receive or
                // transmit interrupt in a very short window and expects the
                // edge promptly; waiting for the next batched device tick can
                // miss it, stranding the byte and stalling the control channel.
                self.service_serial_edges();
                Ok(())
            }
            0x2F8..=0x2FF => {
                crate::devices::PortDevice::write(&mut self.uart_control, port, 1, value & 0xFF)?;
                self.service_serial_edges();
                Ok(())
            }
            0x20 | 0x21 | 0xA0 | 0xA1 => {
                crate::devices::PortDevice::write(&mut self.pic, port, size, value)
            }
            0x40..=0x43 | crate::devices::pit8254::PORT_SYSTEM_CONTROL_B => {
                crate::devices::PortDevice::write(&mut self.pit, port, size, value)
            }
            0x70 | 0x71 => crate::devices::PortDevice::write(&mut self.cmos, port, size, value),
            _ if crate::devices::acpi_pm::AcpiPm::handles(port) => {
                self.acpi_pm.write(port, size, value);
                Ok(())
            }
            _ => self.ports.write(port, size, value),
        }
    }
}

/// Conservatively reports whether an instruction can take a page fault, so the
/// register-rollback snapshot is only paid for when it might be restored.
///
/// True for any explicit or string memory operand, and for the stack- and
/// interrupt-frame instructions whose implicit memory access is not exposed as
/// a memory operand. Register-, immediate-, and near-branch-only instructions
/// cannot fault and never need a snapshot. Over-approximating is always safe;
/// under-approximating would drop a needed rollback, so unknown implicit-memory
/// mnemonics are listed explicitly below.
fn may_fault(instruction: &Instruction) -> bool {
    use iced_x86::{Mnemonic, OpKind};
    for index in 0..instruction.op_count() {
        match instruction.op_kind(index) {
            OpKind::Memory
            | OpKind::MemorySegSI
            | OpKind::MemorySegESI
            | OpKind::MemorySegRSI
            | OpKind::MemoryESDI
            | OpKind::MemoryESEDI
            | OpKind::MemoryESRDI => return true,
            _ => {}
        }
    }
    matches!(
        instruction.mnemonic(),
        Mnemonic::Push
            | Mnemonic::Pop
            | Mnemonic::Call
            | Mnemonic::Ret
            | Mnemonic::Retf
            | Mnemonic::Leave
            | Mnemonic::Enter
            | Mnemonic::Pushf
            | Mnemonic::Pushfd
            | Mnemonic::Pushfq
            | Mnemonic::Popf
            | Mnemonic::Popfd
            | Mnemonic::Popfq
            | Mnemonic::Pusha
            | Mnemonic::Pushad
            | Mnemonic::Popa
            | Mnemonic::Popad
            | Mnemonic::Iret
            | Mnemonic::Iretd
            | Mnemonic::Iretq
            | Mnemonic::Int
            | Mnemonic::Int3
            | Mnemonic::Int1
            | Mnemonic::Into
            | Mnemonic::Bound
            | Mnemonic::Xsave
            | Mnemonic::Xsave64
            | Mnemonic::Xsavec
            | Mnemonic::Xsavec64
            | Mnemonic::Xsaves
            | Mnemonic::Xsaves64
            | Mnemonic::Xrstor
            | Mnemonic::Xrstor64
            | Mnemonic::Xrstors
            | Mnemonic::Xrstors64
            | Mnemonic::Fxsave
            | Mnemonic::Fxsave64
            | Mnemonic::Fxrstor
            | Mnemonic::Fxrstor64
    )
}

fn tlb_disabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let disabled = std::env::var_os("RISH_NO_TLB").is_some();
            STATE.store(if disabled { 2 } else { 1 }, Ordering::Relaxed);
            disabled
        }
        2 => true,
        _ => false,
    }
}

fn page_fault(fault: paging::PageFault) -> CpuError {
    CpuError::PageFault {
        linear: fault.linear,
        error_code: fault.error_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::registers::Efer;
    use crate::arch::segments::SegmentRegister;

    fn long_mode_cpu() -> Cpu {
        let mut cpu = Cpu::new(4, 0).unwrap();
        cpu.regs.efer |= Efer::LMA;
        cpu.regs.cs = SegmentRegister {
            base: 0,
            long_mode: true,
            code: true,
            limit: u32::MAX,
            granularity: true,
            writable_or_readable: true,
            ..Default::default()
        };
        cpu
    }

    fn run_one(cpu: &mut Cpu, bytes: &[u8]) {
        cpu.memory.write(0x1000, bytes).unwrap();
        cpu.regs.rip = 0x1000;
        let instruction = decode::decode_window(64, bytes, 0x1000);
        cpu.regs.rip = 0x1000 + instruction.len() as u64;
        cpu.dispatch(&instruction).unwrap();
    }

    #[test]
    fn tracing_reloads_exact_bytes_on_decode_cache_hits() {
        let mut cpu = long_mode_cpu();
        cpu.memory.write(0x1000, &[0x31, 0xC0]).unwrap(); // xor eax, eax
        cpu.set_trace_capacity(2);

        for _ in 0..2 {
            cpu.regs.rip = 0x1000;
            cpu.step().unwrap();
        }

        assert_eq!(cpu.trace.len(), 2);
        for entry in &cpu.trace {
            assert_eq!(entry.0, 0x1000);
            assert_eq!(&entry.17[..2], &[0x31, 0xC0]);
        }
    }

    #[test]
    fn an_unimplemented_instruction_still_reports_its_bytes() {
        let mut cpu = long_mode_cpu();
        let encoded = [0xC5, 0xF8, 0x77]; // vzeroupper
        cpu.memory.write(0x1000, &encoded).unwrap();
        cpu.regs.rip = 0x1000;

        let error = cpu.step().unwrap_err();
        let CpuError::UnimplementedInstruction { bytes, .. } = error else {
            panic!("expected unimplemented instruction, got {error}");
        };
        assert_eq!(&bytes[..encoded.len()], &encoded);
    }

    #[test]
    fn a_push_that_faults_on_the_store_leaves_rsp_unchanged() {
        // x86 faults are restartable: a push whose store page-faults must roll
        // RSP back so the retried push writes the same slot, not one eight
        // bytes lower. The page-fault gate uses IST 1 so the fault frame lands
        // on a mapped stack while the push target stays unmapped.
        let mut cpu = long_mode_cpu();
        cpu.regs.cr0 |= crate::arch::registers::Cr0::PG;
        cpu.regs.cr4 |= crate::arch::registers::Cr4::PAE;
        cpu.regs.cr3 = 0x10000;
        cpu.memory.write_u64(0x10000, 0x11000 | 0x3).unwrap();
        cpu.memory.write_u64(0x11000, 0x12000 | 0x3).unwrap();
        cpu.memory.write_u64(0x12000, 0x13000 | 0x3).unwrap();
        // Identity-map pages 0..0x20 (code, IDT, handler, IST stack) but not
        // the push stack at 0x30000.
        for page in 0..0x20u64 {
            cpu.memory
                .write_u64(0x13000 + page * 8, (page << 12) | 0x3)
                .unwrap();
        }
        // A TSS whose IST1 entry points at a mapped stack at 0x6000.
        cpu.regs.tr_base = 0x7000;
        cpu.memory.write_u64(0x7000 + 36, 0x6000).unwrap(); // IST1
        // IDT page-fault gate (vector 14) with IST index 1.
        cpu.regs.idt_base = 0x1F000;
        cpu.regs.idt_limit = 0xFFF;
        let gate = 0x1F000 + 14 * 16;
        cpu.memory
            .write_u64(gate, (0x08 << 16) | 0x8E01_0000_0000 | 0x4000)
            .unwrap();
        cpu.memory.write_u64(gate + 8, 0).unwrap();
        cpu.memory.write(0x1000, &[0x50]).unwrap(); // push rax
        cpu.memory.write(0x4000, &[0xF4]).unwrap(); // handler: hlt
        cpu.regs.rip = 0x1000;
        cpu.regs.set_rsp(0x30000); // unmapped -> the push store faults
        cpu.regs.set_gpr(index::RAX, 0xCAFE);
        cpu.step().unwrap();
        // Delivery switched to the IST stack; the fault frame must record the
        // rolled-back user RSP (the value before the push, not eight lower).
        // Frame from the 0x6000 IST top: SS, RSP, RFLAGS, CS, RIP, error.
        assert_eq!(
            cpu.memory.read_u64(0x6000 - 16).unwrap(),
            0x30000,
            "saved RSP rolled back"
        );
        assert_eq!(cpu.regs.cr2, 0x30000 - 8, "cr2 names the faulting store");
        // The handler (a hlt) ran, so rip advanced past it and the CPU halted.
        assert_eq!(cpu.regs.rip, 0x4001, "ran the page-fault handler");
        assert!(cpu.halted);
    }

    #[test]
    fn an_operand_access_that_crosses_a_page_boundary_hits_both_frames() {
        // A misaligned 8-byte operand can straddle two virtual pages whose
        // physical frames are not contiguous. Translating a single physical
        // address and reading eight bytes from it would grab the wrong frame
        // for the tail — the interpreter must translate each page. This was the
        // masked heap-corruption bug that crashed the guest agent.
        let mut cpu = long_mode_cpu();
        cpu.regs.cr0 |= crate::arch::registers::Cr0::PG;
        cpu.regs.cr4 |= crate::arch::registers::Cr4::PAE;
        cpu.regs.cr3 = 0x10000;
        cpu.memory.write_u64(0x10000, 0x11000 | 0x3).unwrap(); // PML4[0]
        cpu.memory.write_u64(0x11000, 0x12000 | 0x3).unwrap(); // PDPT[0]
        cpu.memory.write_u64(0x12000, 0x13000 | 0x3).unwrap(); // PD[0]
        // PT at 0x13000: code page plus two non-contiguous data pages.
        cpu.memory.write_u64(0x13000 + 8, 0x1000 | 0x3).unwrap(); // virt 0x1000 -> 0x1000
        cpu.memory
            .write_u64(0x13000 + 0x40 * 8, 0x50000 | 0x3)
            .unwrap(); // virt 0x40000 -> 0x50000
        cpu.memory
            .write_u64(0x13000 + 0x41 * 8, 0x60000 | 0x3)
            .unwrap(); // virt 0x41000 -> 0x60000 (not adjacent to 0x50000)
        // Read side: bytes straddle the boundary across the two frames.
        cpu.memory
            .write(0x50FFC, &[0x11, 0x22, 0x33, 0x44])
            .unwrap();
        cpu.memory
            .write(0x60000, &[0x55, 0x66, 0x77, 0x88])
            .unwrap();
        cpu.memory.write(0x1000, &[0x48, 0x8B, 0x03]).unwrap(); // mov rax, [rbx]
        cpu.regs.rip = 0x1000;
        cpu.regs.set_gpr(index::RBX, 0x40FFC);
        cpu.step().unwrap();
        assert_eq!(
            cpu.regs.gpr(index::RAX),
            0x8877_6655_4433_2211,
            "the load stitches bytes from both physical frames"
        );
        // Write side: store across the same boundary and read it back per frame.
        cpu.memory.write(0x1004, &[0x48, 0x89, 0x0B]).unwrap(); // mov [rbx], rcx
        cpu.regs.rip = 0x1004;
        cpu.regs.set_gpr(index::RCX, 0x0FEE_DDCC_BBAA_9988);
        cpu.step().unwrap();
        let mut low = [0_u8; 4];
        cpu.memory.read(0x50FFC, &mut low).unwrap();
        let mut high = [0_u8; 4];
        cpu.memory.read(0x60000, &mut high).unwrap();
        assert_eq!(
            low,
            [0x88, 0x99, 0xAA, 0xBB],
            "low half landed in frame one"
        );
        assert_eq!(
            high,
            [0xCC, 0xDD, 0xEE, 0x0F],
            "high half landed in frame two"
        );
    }

    #[test]
    fn a_gs_prefixed_rip_relative_operand_adds_the_segment_base() {
        // Linux addresses per-CPU data as %gs:offset(%rip); the base must
        // apply or the access lands on the static per-CPU template.
        let mut cpu = long_mode_cpu();
        cpu.regs.gs.base = 0x2000;
        // mov rax, gs:[rip+0x10]  ->  65 48 8b 05 10 00 00 00
        let bytes = [0x65, 0x48, 0x8B, 0x05, 0x10, 0x00, 0x00, 0x00];
        // rip after the instruction is 0x1008, so the operand names 0x1018,
        // which the GS base moves to 0x3018.
        cpu.memory.write_u64(0x3018, 0xDEAD_BEEF_1234_5678).unwrap();
        cpu.memory.write_u64(0x1018, 0x1111_1111_1111_1111).unwrap();
        run_one(&mut cpu, &bytes);
        assert_eq!(cpu.regs.gpr(index::RAX), 0xDEAD_BEEF_1234_5678);
    }

    #[test]
    fn an_unprefixed_rip_relative_operand_keeps_its_address() {
        let mut cpu = long_mode_cpu();
        cpu.regs.gs.base = 0x2000;
        // mov rax, [rip+0x10]  ->  48 8b 05 10 00 00 00
        let bytes = [0x48, 0x8B, 0x05, 0x10, 0x00, 0x00, 0x00];
        cpu.memory.write_u64(0x1017, 0x1111_1111_1111_1111).unwrap();
        run_one(&mut cpu, &bytes);
        assert_eq!(cpu.regs.gpr(index::RAX), 0x1111_1111_1111_1111);
    }

    #[test]
    fn a_gs_prefixed_base_register_operand_adds_the_segment_base() {
        // The other per-CPU addressing form: %gs:(%rsi). Dropping the base
        // here makes this_cpu_cmpxchg compare the wrong memory forever.
        let mut cpu = long_mode_cpu();
        cpu.regs.gs.base = 0x2000;
        cpu.regs.set_gpr(index::RSI, 0x40);
        cpu.memory.write_u64(0x2040, 0x0123_4567_89AB_CDEF).unwrap();
        cpu.memory.write_u64(0x0040, 0x2222_2222_2222_2222).unwrap();
        // mov rax, gs:[rsi]  ->  65 48 8b 06
        run_one(&mut cpu, &[0x65, 0x48, 0x8B, 0x06]);
        assert_eq!(cpu.regs.gpr(index::RAX), 0x0123_4567_89AB_CDEF);
    }

    #[test]
    fn long_mode_ignores_the_data_segment_bases() {
        // DS, ES and SS bases are forced to zero in 64-bit mode.
        let mut cpu = long_mode_cpu();
        cpu.regs.ds.base = 0x5000;
        cpu.regs.es.base = 0x5000;
        cpu.regs.set_gpr(index::RSI, 0x40);
        cpu.memory.write_u64(0x0040, 0x3333_3333_3333_3333).unwrap();
        run_one(&mut cpu, &[0x48, 0x8B, 0x06]);
        assert_eq!(cpu.regs.gpr(index::RAX), 0x3333_3333_3333_3333);
    }

    #[test]
    fn sti_holds_delivery_until_the_next_instruction_retires() {
        // A pending interrupt at `sti; hlt` must wake the hlt, not fire
        // between the two instructions.
        let mut cpu = long_mode_cpu();
        // Minimal IDT with a present gate for vector 0x30 at 0x4000.
        cpu.regs.idt_base = 0x3000;
        cpu.regs.idt_limit = 0xFFF;
        let vector_offset = 0x3000 + 0x30_u64 * 16;
        cpu.memory
            .write_u64(vector_offset, (0x10 << 16) | 0x8E00_0000_0000 | 0x4000)
            .unwrap();
        cpu.memory.write_u64(vector_offset + 8, 0).unwrap();
        // Install code: sti; hlt at 0x1000, and a hlt in the handler. The
        // interrupt gate clears IF, so the handler's hlt halts outright.
        cpu.memory.write(0x1000, &[0xFB, 0xF4]).unwrap();
        cpu.memory.write(0x4000, &[0xF4]).unwrap();
        cpu.regs.rip = 0x1000;
        cpu.regs.set_rsp(0x8000);
        cpu.regs.rflags -= crate::arch::registers::RFlags::IF;
        // The interrupt is already pending before sti executes.
        cpu.memory.lapic_enqueue_interrupt(0x30);
        cpu.step().unwrap(); // sti retires; delivery is shadowed
        assert_eq!(cpu.regs.rip, 0x1001);
        cpu.step().unwrap(); // hlt retires and the CPU goes to sleep
        assert!(cpu.waiting_for_interrupt);
        cpu.step().unwrap(); // the pending vector wakes the hlt into the handler
        assert!(!cpu.waiting_for_interrupt);
        assert!(cpu.halted, "handler ran with IF masked and halted");
        assert_eq!(cpu.regs.rip, 0x4001);
    }

    #[test]
    fn a_fault_frame_points_at_the_faulting_instruction() {
        // rdmsr_safe relies on the #GP frame naming the rdmsr instruction
        // itself, so the exception-table fixup can find it.
        let mut cpu = long_mode_cpu();
        cpu.regs.idt_base = 0x3000;
        cpu.regs.idt_limit = 0xFFF;
        let gate = 0x3000 + 13_u64 * 16;
        cpu.memory
            .write_u64(gate, (0x10 << 16) | 0x8E00_0000_0000 | 0x4000)
            .unwrap();
        cpu.memory.write_u64(gate + 8, 0).unwrap();
        // rdmsr of an unimplemented MSR at 0x1000.
        cpu.memory.write(0x1000, &[0x0F, 0x32]).unwrap();
        cpu.regs.rip = 0x1000;
        cpu.regs.set_rsp(0x8000);
        cpu.regs.set_gpr(index::RCX, 0x3A); // IA32_FEATURE_CONTROL
        cpu.step().unwrap();
        assert_eq!(cpu.regs.rip, 0x4000, "entered the #GP handler");
        // Frame from the top of the stack down: ss, rsp, rflags, cs, rip,
        // error code. The pushed rip must be the rdmsr itself, not the
        // following instruction.
        let pushed_rip = cpu.memory.read_u64(0x8000 - 0x28).unwrap();
        assert_eq!(pushed_rip, 0x1000);
    }

    #[test]
    fn an_fs_prefixed_rip_relative_operand_adds_the_segment_base() {
        let mut cpu = long_mode_cpu();
        cpu.regs.fs.base = 0x2000;
        // mov rax, fs:[rip+0x10]  ->  64 48 8b 05 10 00 00 00
        let bytes = [0x64, 0x48, 0x8B, 0x05, 0x10, 0x00, 0x00, 0x00];
        cpu.memory.write_u64(0x3018, 0xAABB_CCDD_EEFF_0011).unwrap();
        run_one(&mut cpu, &bytes);
        assert_eq!(cpu.regs.gpr(index::RAX), 0xAABB_CCDD_EEFF_0011);
    }
}
