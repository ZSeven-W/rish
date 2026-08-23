//! Exception and interrupt delivery through the guest IDT.
//!
//! Delivery follows the hardware rules the Linux entry code depends on: the
//! gate type decides whether IF survives, a gate that raises the privilege
//! level switches to the stack in the TSS, and an IST index selects one of the
//! seven dedicated stacks.

use crate::arch::paging::AccessKind;
use crate::arch::registers::{CpuMode, RFlags};
use crate::arch::segments::{SegmentRegister, SegmentSelector};
use crate::{CpuError, cpu::Cpu};

pub const VECTOR_DIVIDE: u8 = 0;
pub const VECTOR_INVALID_OPCODE: u8 = 6;
pub const VECTOR_DOUBLE_FAULT: u8 = 8;
pub const VECTOR_GENERAL_PROTECTION: u8 = 13;
pub const VECTOR_PAGE_FAULT: u8 = 14;

/// 64-bit TSS field offsets.
const TSS_RSP0: u64 = 4;
const TSS_IST1: u64 = 36;

/// One decoded IDT gate.
#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub selector: SegmentSelector,
    pub offset: u64,
    /// Interrupt stack table index, 0 when unused (long mode only).
    pub ist: u8,
    /// Descriptor type nibble: 0xE interrupt gate, 0xF trap gate.
    pub kind: u8,
    pub dpl: u8,
    pub present: bool,
}

impl Gate {
    /// An interrupt gate clears IF on entry; a trap gate leaves it alone.
    #[must_use]
    pub fn clears_interrupt_flag(&self) -> bool {
        self.kind & 0xF != 0xF
    }
}

/// An interrupt or exception waiting to be delivered.
#[derive(Clone, Copy, Debug)]
pub struct Deliverable {
    pub vector: u8,
    pub error_code: u16,
    pub has_error_code: bool,
}

impl Cpu {
    /// Raises an exception through the IDT.
    ///
    /// Every vector routed here is a fault, so the frame's return address is
    /// rewound to the faulting instruction itself; that is what the kernel's
    /// exception-table fixups (rdmsr_safe and friends) search for. Traps
    /// (INT n, INT3, INTO) go through [`Cpu::inject_interrupt`] instead and
    /// keep the address of the following instruction.
    pub fn raise(
        &mut self,
        vector: u8,
        error_code: u16,
        has_error_code: bool,
    ) -> Result<(), CpuError> {
        self.regs.rip = self.instruction_start;
        if self.in_exception {
            // A fault while delivering a fault is a double fault; a fault
            // while delivering that is a triple fault, which resets the CPU.
            if self.in_double_fault {
                return Err(CpuError::TripleFault);
            }
            self.in_double_fault = true;
            let result = self.deliver_gate(VECTOR_DOUBLE_FAULT, 0, true, true);
            self.in_double_fault = false;
            return result;
        }
        self.in_exception = true;
        let result = self.deliver_gate(vector, error_code, has_error_code, true);
        self.in_exception = false;
        result
    }

    /// Delivers the highest-priority pending interrupt, if any is unmasked.
    pub(crate) fn deliver_pending(&mut self) -> Result<(), CpuError> {
        // Software interrupts (INT n) and injected traps are not maskable by
        // IF: they are already part of the instruction that requested them.
        if let Some(item) = self.pending_interrupts.pop_front() {
            self.deliver_gate(item.vector, item.error_code, item.has_error_code, true)?;
            self.waiting_for_interrupt = false;
            return Ok(());
        }
        if !self.regs.rflags.contains(RFlags::IF) {
            return Ok(());
        }
        if let Some(vector) = self.next_lapic_vector() {
            self.deliver_gate(vector, 0, false, false)?;
            self.waiting_for_interrupt = false;
            return Ok(());
        }
        if let Some(irq) = self.pic.pending_irq() {
            let vector = self.pic.acknowledge(irq);
            self.deliver_gate(vector, 0, false, false)?;
            self.waiting_for_interrupt = false;
        }
        Ok(())
    }

    fn next_lapic_vector(&mut self) -> Option<u8> {
        self.memory.lapic_pop_interrupt()
    }

    fn deliver_gate(
        &mut self,
        vector: u8,
        error_code: u16,
        has_error_code: bool,
        is_exception: bool,
    ) -> Result<(), CpuError> {
        let gate = self.idt_gate(vector)?;
        self.transfer_to_gate(gate, vector, error_code, has_error_code, is_exception)
    }

    /// Keeps a bounded record of delivered faults for boot diagnostics.
    fn record_fault(&mut self, vector: u8, error_code: u16, is_exception: bool) {
        const FAULT_LOG_CAPACITY: usize = 64;
        if self.fault_log.len() >= FAULT_LOG_CAPACITY {
            self.fault_log.pop_front();
        }
        self.fault_log.push_back(crate::cpu::FaultEvent {
            retired: self.regs.instructions_retired,
            vector,
            error_code,
            rip: self.regs.rip,
            cr2: self.regs.cr2,
            is_exception,
        });
    }

    pub(crate) fn idt_gate(&self, vector: u8) -> Result<Gate, CpuError> {
        let long_mode = self.regs.mode() == CpuMode::Long;
        let entry_size: u64 = if long_mode { 16 } else { 8 };
        let required = u32::from(vector) * entry_size as u32 + entry_size as u32 - 1;
        if self.regs.idt_limit < required {
            return Err(CpuError::GuestFault(format!(
                "IDT limit {} cannot reach vector {vector}",
                self.regs.idt_limit
            )));
        }
        let address = self
            .regs
            .idt_base
            .wrapping_add(u64::from(vector) * entry_size);
        // The IDT lives at a linear address, and a gate may cross a page.
        let low = self
            .memory
            .read_u64(self.translate_privileged(address, AccessKind::Read)?)?;
        let selector = SegmentSelector((low >> 16) as u16);
        let attributes = (low >> 40) & 0xFF;
        let offset = if long_mode {
            let high = self
                .memory
                .read_u64(self.translate_privileged(address + 8, AccessKind::Read)?)?;
            (low & 0xFFFF) | ((low >> 32) & 0xFFFF_0000) | ((high & 0xFFFF_FFFF) << 32)
        } else {
            (low & 0xFFFF) | ((low >> 32) & 0xFFFF_0000)
        };
        Ok(Gate {
            selector,
            offset,
            ist: if long_mode {
                ((low >> 32) & 0b111) as u8
            } else {
                0
            },
            kind: (attributes & 0xF) as u8,
            dpl: ((attributes >> 5) & 0b11) as u8,
            present: attributes & 0x80 != 0,
        })
    }

    pub(crate) fn transfer_to_gate(
        &mut self,
        gate: Gate,
        vector: u8,
        error_code: u16,
        has_error_code: bool,
        is_exception: bool,
    ) -> Result<(), CpuError> {
        if !gate.present {
            return Err(CpuError::GuestFault(format!(
                "IDT gate for vector {vector:#x} (selector {:#x}) is not present; \
idt={:#x}/{:#x}",
                gate.selector.0, self.regs.idt_base, self.regs.idt_limit
            )));
        }
        let old_flags = self.regs.rflags;
        let old_cs = self.regs.cs.selector;
        let old_rsp = self.regs.rsp();
        let old_ss = self.regs.ss.selector;
        let old_cpl = self.regs.cpl();
        let old_mode = self.regs.mode();
        let new_cs = self.load_code_segment(gate.selector)?;
        let new_cpl = if old_mode == CpuMode::Real {
            0
        } else {
            new_cs.selector.rpl()
        };
        if old_mode == CpuMode::Long {
            self.switch_to_gate_stack(&gate, old_cpl, new_cpl)?;
        }
        // The handler's code segment takes effect before the frame is pushed,
        // so the pushes run at the handler's privilege level against the
        // handler's stack. Entering through an interrupt gate masks further
        // interrupts; a trap gate keeps them enabled. Both clear TF and NT.
        let mut new_flags = old_flags - RFlags::TF - RFlags::NT - RFlags::RF;
        if gate.clears_interrupt_flag() {
            new_flags -= RFlags::IF;
        }
        self.regs.cs = new_cs;
        match old_mode {
            CpuMode::Long => {
                self.push64(u64::from(old_ss.0))?;
                self.push64(old_rsp)?;
                self.push64(old_flags.bits())?;
                self.push64(u64::from(old_cs.0))?;
                self.push64(self.regs.rip)?;
                if has_error_code {
                    self.push64(u64::from(error_code))?;
                }
            }
            CpuMode::Protected32 => {
                self.push32(old_flags.bits() as u32)?;
                self.push32(u32::from(old_cs.0))?;
                self.push32(self.regs.rip as u32)?;
                if has_error_code {
                    self.push32(u32::from(error_code))?;
                }
            }
            CpuMode::Real | CpuMode::Protected16 => {
                self.push16(old_flags.bits() as u16)?;
                self.push16(old_cs.0)?;
                self.push16(self.regs.rip as u16)?;
                if has_error_code {
                    self.push16(error_code)?;
                }
            }
        }
        if is_exception {
            self.exceptions_raised = self.exceptions_raised.saturating_add(1);
        } else {
            self.interrupts_delivered = self.interrupts_delivered.saturating_add(1);
        }
        self.record_fault(vector, error_code, is_exception);
        self.regs.rflags = new_flags;
        self.regs.rip = match old_mode {
            CpuMode::Long => gate.offset,
            CpuMode::Protected32 => gate.offset & 0xFFFF_FFFF,
            _ => gate.offset & 0xFFFF,
        };
        Ok(())
    }

    /// Selects the stack the handler runs on: an IST entry when the gate names
    /// one, the TSS ring-0 stack when the privilege level rises, and otherwise
    /// the interrupted stack.
    fn switch_to_gate_stack(
        &mut self,
        gate: &Gate,
        old_cpl: u8,
        new_cpl: u8,
    ) -> Result<(), CpuError> {
        let stack = if gate.ist != 0 {
            Some(self.tss_field(TSS_IST1 + u64::from(gate.ist - 1) * 8)?)
        } else if new_cpl < old_cpl {
            Some(self.tss_field(TSS_RSP0 + u64::from(new_cpl) * 8)?)
        } else {
            None
        };
        if let Some(rsp) = stack {
            // Hardware aligns the handler stack to 16 bytes.
            self.regs.set_rsp(rsp & !0xF);
            self.regs.ss = SegmentRegister {
                selector: SegmentSelector(0),
                ..SegmentRegister::default()
            };
        }
        Ok(())
    }

    /// Reads one 64-bit field from the task state segment.
    fn tss_field(&self, offset: u64) -> Result<u64, CpuError> {
        if self.regs.tr_base == 0 {
            return Err(CpuError::GuestFault(
                "interrupt needs a stack from the TSS, but no task register is loaded".to_owned(),
            ));
        }
        let address = self.regs.tr_base.wrapping_add(offset);
        self.memory
            .read_u64(self.translate_privileged(address, AccessKind::Read)?)
    }

    pub(crate) fn load_code_segment(
        &mut self,
        selector: SegmentSelector,
    ) -> Result<SegmentRegister, CpuError> {
        match self.regs.mode() {
            CpuMode::Real | CpuMode::Protected16 => Ok(SegmentRegister::real_mode(selector)),
            _ => self.load_segment_from_table(selector),
        }
    }

    /// Injects a raw interrupt through the IDT (used by INT n and traps).
    pub fn inject_interrupt(
        &mut self,
        vector: u8,
        error_code: u16,
        has_error_code: bool,
    ) -> Result<(), CpuError> {
        self.pending_interrupts.push_back(Deliverable {
            vector,
            error_code,
            has_error_code,
        });
        Ok(())
    }
}
