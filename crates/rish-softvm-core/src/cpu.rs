//! The interpreter core: fetch, dispatch, exceptions, and interrupts.

use std::collections::VecDeque;

use iced_x86::{Decoder, DecoderOptions, Instruction, Register};

use crate::arch::paging::{AccessKind, translate};
use crate::arch::registers::{CpuMode, Registers, index};
use crate::arch::segments::{Descriptor, SegmentRegister, SegmentSelector};
use crate::devices::{Cmos, InterruptLines, Pic8259, Pit8254, PortBus, Uart16550};
use crate::ops::{arithmetic, branch, data, logic, sse, stack, string, system};
use crate::{CpuError, Memory};

const MAX_INSTRUCTION_BYTES: usize = 15;

const VECTOR_DIVIDE: u8 = 0;
const VECTOR_INVALID_OPCODE: u8 = 6;
const VECTOR_DOUBLE_FAULT: u8 = 8;
const VECTOR_PAGE_FAULT: u8 = 14;

pub struct Cpu {
    pub regs: Registers,
    pub memory: Memory,
    pub ports: PortBus,
    pub pic: Pic8259,
    pub pit: Pit8254,
    pub cmos: Cmos,
    pub uart_console: Uart16550,
    pub uart_control: Uart16550,
    pub lines: InterruptLines,
    pub tsc: u64,
    pub halted: bool,
    pub boot_ok_seen: bool,
    pub kernel_gs_base: u64,
    pub xcr0: u64,
    pub fpu_control_word: u16,
    pub fpu_status_word: u16,
    pub mxcsr: u32,
    pub trace: VecDeque<(u64, u64, u64, u64, [u8; 4])>,
    pending_interrupts: VecDeque<Deliverable>,
    in_exception: bool,
}

#[derive(Clone, Copy, Debug)]
struct Deliverable {
    vector: u8,
    error_code: u16,
    has_error_code: bool,
}

impl Cpu {
    pub fn new(memory_mib: usize, boot_epoch_seconds: u64) -> Result<Self, CpuError> {
        let memory = Memory::new(memory_mib)?;
        let ports = PortBus::default();
        Ok(Self {
            regs: Registers::default(),
            memory,
            ports,
            pic: Pic8259::new(),
            pit: Pit8254::new(),
            cmos: Cmos::new(boot_epoch_seconds),
            uart_console: Uart16550::new(0x3F8, 4096, 65536),
            uart_control: Uart16550::new(0x2F8, 65536, 65536),
            lines: InterruptLines::default(),
            tsc: 0,
            halted: false,
            boot_ok_seen: false,
            kernel_gs_base: 0,
            xcr0: 0x3,
            fpu_control_word: 0x037F,
            fpu_status_word: 0,
            mxcsr: 0x1F80,
            trace: VecDeque::new(),
            pending_interrupts: VecDeque::new(),
            in_exception: false,
        })
    }

    /// Executes exactly one instruction, then services one pending interrupt.
    pub fn step(&mut self) -> Result<(), CpuError> {
        if self.halted {
            return Err(CpuError::Halted);
        }
        self.deliver_pending()?;
        self.execute_one()?;
        self.regs.instructions_retired = self.regs.instructions_retired.saturating_add(1);
        self.tsc = self.tsc.saturating_add(1);
        Ok(())
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
        let (instruction, bytes) = self.decode_at_rip()?;
        let mut head = [0_u8; 4];
        let count = bytes.len().min(4);
        head[..count].copy_from_slice(&bytes[..count]);
        if self.trace.len() >= 4096 {
            self.trace.pop_front();
        }
        self.trace.push_back((
            self.regs.rip,
            self.regs.gpr(index::RAX),
            self.regs.gpr(index::RBP),
            self.regs.gpr(index::RSP),
            head,
        ));
        self.regs.rip = self.regs.rip.wrapping_add(instruction.len() as u64);
        let result = self.dispatch(&instruction);
        if let Err(CpuError::UnimplementedInstruction {
            code,
            address,
            bytes: _,
        }) = &result
        {
            return Err(CpuError::UnimplementedInstruction {
                code: code.clone(),
                address: *address,
                bytes,
            });
        }
        result
    }

    fn decode_at_rip(&self) -> Result<(Instruction, Vec<u8>), CpuError> {
        let (bitness, ip) = self.decode_environment();
        let mut buffer = [0_u8; MAX_INSTRUCTION_BYTES];
        let linear = self.regs.code_base().wrapping_add(ip);
        let physical = self.translate(linear, AccessKind::Execute)?;
        let remaining_in_page = 4096 - (physical & 0xFFF);
        let count = (MAX_INSTRUCTION_BYTES as u64).min(remaining_in_page) as usize;
        self.memory.read(physical, &mut buffer[..count])?;
        let mut decoder = Decoder::with_ip(bitness, &buffer[..count], ip, DecoderOptions::NONE);
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return Err(CpuError::UnimplementedInstruction {
                code: "invalid".to_owned(),
                address: self.regs.rip,
                bytes: buffer[..count].to_vec(),
            });
        }
        Ok((instruction, buffer[..instruction.len()].to_vec()))
    }

    fn decode_environment(&self) -> (u32, u64) {
        match self.regs.mode() {
            CpuMode::Real | CpuMode::Protected16 => (16, self.regs.rip & 0xFFFF),
            CpuMode::Protected32 => (32, self.regs.rip & 0xFFFF_FFFF),
            CpuMode::Long => (64, self.regs.rip),
        }
    }

    pub(crate) fn dispatch(&mut self, instruction: &Instruction) -> Result<(), CpuError> {
        use iced_x86::Mnemonic;
        let mnemonic = instruction.mnemonic();
        if matches!(mnemonic, Mnemonic::Mov)
            && (is_control_register(instruction.op0_register())
                || is_control_register(instruction.op1_register())
                || is_debug_register(instruction.op0_register())
                || is_debug_register(instruction.op1_register()))
        {
            return system::system_op(self, instruction);
        }
        match mnemonic {
            Mnemonic::Nop | Mnemonic::Pause => Ok(()),
            Mnemonic::Mov => data::mov(self, instruction),
            Mnemonic::Lea => data::lea(self, instruction),
            Mnemonic::Movzx | Mnemonic::Movsx | Mnemonic::Movsxd => data::movx(self, instruction),
            Mnemonic::Xchg => data::xchg(self, instruction),
            Mnemonic::Xadd => data::xadd(self, instruction),
            Mnemonic::Cmpxchg => data::cmpxchg(self, instruction),
            Mnemonic::Xlatb => data::xlatb(self, instruction),
            Mnemonic::Add => arithmetic::add(self, instruction),
            Mnemonic::Sub => arithmetic::sub(self, instruction),
            Mnemonic::Cmp => arithmetic::cmp(self, instruction),
            Mnemonic::Adc | Mnemonic::Sbb => arithmetic::adc_sbb(self, instruction),
            Mnemonic::Inc | Mnemonic::Dec => arithmetic::inc_dec(self, instruction),
            Mnemonic::Neg => arithmetic::neg(self, instruction),
            Mnemonic::Mul | Mnemonic::Imul | Mnemonic::Div | Mnemonic::Idiv => {
                arithmetic::mul_div(self, instruction)
            }
            Mnemonic::Cbw
            | Mnemonic::Cwde
            | Mnemonic::Cdqe
            | Mnemonic::Cwd
            | Mnemonic::Cdq
            | Mnemonic::Cqo => arithmetic::convert(self, instruction),
            Mnemonic::And | Mnemonic::Or | Mnemonic::Xor => logic::and_or_xor(self, instruction),
            Mnemonic::Test => logic::test(self, instruction),
            Mnemonic::Not
            | Mnemonic::Shl
            | Mnemonic::Shr
            | Mnemonic::Sar
            | Mnemonic::Rol
            | Mnemonic::Ror
            | Mnemonic::Rcl
            | Mnemonic::Rcr => logic::shift_rotate(self, instruction),
            Mnemonic::Bsf
            | Mnemonic::Bsr
            | Mnemonic::Bt
            | Mnemonic::Bts
            | Mnemonic::Btr
            | Mnemonic::Btc => logic::bit_scan_test(self, instruction),
            Mnemonic::Push
            | Mnemonic::Pop
            | Mnemonic::Pushf
            | Mnemonic::Pushfq
            | Mnemonic::Pushfd
            | Mnemonic::Popf
            | Mnemonic::Popfq
            | Mnemonic::Popfd
            | Mnemonic::Enter
            | Mnemonic::Leave => stack::push_pop(self, instruction),
            Mnemonic::Jmp => branch::jump(self, instruction),
            Mnemonic::Call => branch::call(self, instruction),
            Mnemonic::Ret | Mnemonic::Retf => branch::ret(self, instruction),
            Mnemonic::Loop
            | Mnemonic::Loope
            | Mnemonic::Loopne
            | Mnemonic::Jcxz
            | Mnemonic::Jecxz
            | Mnemonic::Jrcxz => branch::jump(self, instruction),
            Mnemonic::Jo
            | Mnemonic::Jno
            | Mnemonic::Jb
            | Mnemonic::Jae
            | Mnemonic::Je
            | Mnemonic::Jne
            | Mnemonic::Jbe
            | Mnemonic::Ja
            | Mnemonic::Js
            | Mnemonic::Jns
            | Mnemonic::Jp
            | Mnemonic::Jnp
            | Mnemonic::Jl
            | Mnemonic::Jge
            | Mnemonic::Jle
            | Mnemonic::Jg => branch::jcc(self, instruction),
            Mnemonic::Seto
            | Mnemonic::Setno
            | Mnemonic::Setb
            | Mnemonic::Setae
            | Mnemonic::Sete
            | Mnemonic::Setne
            | Mnemonic::Setbe
            | Mnemonic::Seta
            | Mnemonic::Sets
            | Mnemonic::Setns
            | Mnemonic::Setp
            | Mnemonic::Setnp
            | Mnemonic::Setl
            | Mnemonic::Setge
            | Mnemonic::Setle
            | Mnemonic::Setg => branch::setcc(self, instruction),
            Mnemonic::Stc
            | Mnemonic::Clc
            | Mnemonic::Cmc
            | Mnemonic::Cld
            | Mnemonic::Std
            | Mnemonic::Cli
            | Mnemonic::Sti
            | Mnemonic::Sahf
            | Mnemonic::Lahf => branch::flag_ops(self, instruction),
            Mnemonic::Movaps
            | Mnemonic::Movups
            | Mnemonic::Movapd
            | Mnemonic::Movupd
            | Mnemonic::Movdqa
            | Mnemonic::Movdqu
            | Mnemonic::Movq
            | Mnemonic::Movd
            | Mnemonic::Movss
            | Mnemonic::Movlps
            | Mnemonic::Movlpd
            | Mnemonic::Movhps
            | Mnemonic::Movhpd
            | Mnemonic::Movddup
            | Mnemonic::Movsldup
            | Mnemonic::Movshdup
            | Mnemonic::Xorps
            | Mnemonic::Xorpd
            | Mnemonic::Pxor
            | Mnemonic::Andps
            | Mnemonic::Andpd
            | Mnemonic::Pand
            | Mnemonic::Andnps
            | Mnemonic::Andnpd
            | Mnemonic::Pandn
            | Mnemonic::Orps
            | Mnemonic::Orpd
            | Mnemonic::Por
            | Mnemonic::Pshufd
            | Mnemonic::Pshuflw
            | Mnemonic::Pshufhw
            | Mnemonic::Shufps
            | Mnemonic::Shufpd
            | Mnemonic::Punpcklbw
            | Mnemonic::Punpcklwd
            | Mnemonic::Punpckldq
            | Mnemonic::Punpcklqdq
            | Mnemonic::Punpckhbw
            | Mnemonic::Punpckhwd
            | Mnemonic::Punpckhdq
            | Mnemonic::Punpckhqdq
            | Mnemonic::Movntdq
            | Mnemonic::Movntps
            | Mnemonic::Movntq
            | Mnemonic::Movnti
            | Mnemonic::Pcmpeqb
            | Mnemonic::Pcmpeqw
            | Mnemonic::Pcmpeqd
            | Mnemonic::Pcmpeqq
            | Mnemonic::Psllw
            | Mnemonic::Pslld
            | Mnemonic::Psllq
            | Mnemonic::Psrlw
            | Mnemonic::Psrld
            | Mnemonic::Psrlq
            | Mnemonic::Pslldq
            | Mnemonic::Psrldq
            | Mnemonic::Cvtsi2sd
            | Mnemonic::Cvtsi2ss
            | Mnemonic::Cvttsd2si
            | Mnemonic::Cvttss2si
            | Mnemonic::Emms
            | Mnemonic::Femms => sse::sse_op(self, instruction),
            Mnemonic::Movsd
                if is_xmm_register(instruction.op0_register())
                    || is_xmm_register(instruction.op1_register()) =>
            {
                sse::sse_op(self, instruction)
            }
            Mnemonic::Cmovo
            | Mnemonic::Cmovno
            | Mnemonic::Cmovb
            | Mnemonic::Cmovae
            | Mnemonic::Cmove
            | Mnemonic::Cmovne
            | Mnemonic::Cmovbe
            | Mnemonic::Cmova
            | Mnemonic::Cmovs
            | Mnemonic::Cmovns
            | Mnemonic::Cmovp
            | Mnemonic::Cmovnp
            | Mnemonic::Cmovl
            | Mnemonic::Cmovge
            | Mnemonic::Cmovle
            | Mnemonic::Cmovg => branch::cmovcc(self, instruction),
            Mnemonic::Movsb
            | Mnemonic::Movsw
            | Mnemonic::Movsd
            | Mnemonic::Movsq
            | Mnemonic::Stosb
            | Mnemonic::Stosw
            | Mnemonic::Stosd
            | Mnemonic::Stosq
            | Mnemonic::Lodsb
            | Mnemonic::Lodsw
            | Mnemonic::Lodsd
            | Mnemonic::Lodsq
            | Mnemonic::Scasb
            | Mnemonic::Scasw
            | Mnemonic::Scasd
            | Mnemonic::Scasq
            | Mnemonic::Cmpsb
            | Mnemonic::Cmpsw
            | Mnemonic::Cmpsd
            | Mnemonic::Cmpsq => string::string_op(self, instruction),
            Mnemonic::In | Mnemonic::Out => system::in_out(self, instruction),
            Mnemonic::Hlt => system::hlt(self, instruction),
            Mnemonic::Cpuid => system::cpuid(self, instruction),
            Mnemonic::Rdtsc | Mnemonic::Rdtscp => system::rdtsc(self, instruction),
            Mnemonic::Rdmsr
            | Mnemonic::Wrmsr
            | Mnemonic::Rdpmc
            | Mnemonic::Lgdt
            | Mnemonic::Lidt
            | Mnemonic::Sgdt
            | Mnemonic::Sidt
            | Mnemonic::Lldt
            | Mnemonic::Sldt
            | Mnemonic::Ltr
            | Mnemonic::Str
            | Mnemonic::Clts
            | Mnemonic::Lmsw
            | Mnemonic::Smsw
            | Mnemonic::Invlpg
            | Mnemonic::Wbinvd
            | Mnemonic::Invd
            | Mnemonic::Lfence
            | Mnemonic::Sfence
            | Mnemonic::Mfence
            | Mnemonic::Iretd
            | Mnemonic::Iretq
            | Mnemonic::Int
            | Mnemonic::Int3
            | Mnemonic::Into
            | Mnemonic::Bound => system::system_op(self, instruction),
            Mnemonic::Bswap
            | Mnemonic::Popcnt
            | Mnemonic::Ud2
            | Mnemonic::Rdrand
            | Mnemonic::Rdseed
            | Mnemonic::Prefetcht0
            | Mnemonic::Prefetcht1
            | Mnemonic::Prefetcht2
            | Mnemonic::Prefetchnta
            | Mnemonic::Clflush
            | Mnemonic::Clflushopt
            | Mnemonic::Fxsave
            | Mnemonic::Fxrstor
            | Mnemonic::Fninit
            | Mnemonic::Fnstcw
            | Mnemonic::Fldcw
            | Mnemonic::Fnstsw
            | Mnemonic::Fnclex
            | Mnemonic::Wait
            | Mnemonic::Shld
            | Mnemonic::Shrd
            | Mnemonic::Swapgs
            | Mnemonic::Xgetbv
            | Mnemonic::Xsetbv
            | Mnemonic::Endbr64
            | Mnemonic::Endbr32 => system::extra_op(self, instruction),
            _ => Err(CpuError::UnimplementedInstruction {
                code: format!("{mnemonic:?}"),
                address: self.regs.rip,
                bytes: Vec::new(),
            }),
        }
    }
    /// Translates a linear address using the current mode and CR3.
    pub fn translate(&self, linear: u64, kind: AccessKind) -> Result<u64, CpuError> {
        translate(
            &self.memory,
            self.regs.cr3,
            self.regs.cr0,
            self.regs.cr4,
            self.regs.efer,
            linear,
            kind,
        )
        .map_err(|fault| CpuError::GuestFault(format!("page fault at {:#x}", fault.linear)))
    }

    /// Computes the effective linear address for a memory operand.
    pub fn effective_address(&self, instruction: &Instruction, _operand: u32) -> u64 {
        if instruction.is_ip_rel_memory_operand() {
            return instruction.ip_rel_memory_address();
        }
        let base = instruction.memory_base();
        let index = instruction.memory_index();
        let scale = instruction.memory_index_scale();
        let displacement = instruction.memory_displacement64();
        let address_size = address_size_of(instruction);
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
        let segment_base = self.regs.data_base(instruction.segment_prefix());
        match self.regs.mode() {
            CpuMode::Real | CpuMode::Protected16 | CpuMode::Protected32 => {
                segment_base.wrapping_add(address)
            }
            CpuMode::Long => address,
        }
    }

    /// Reads a memory operand of the given width as a zero-extended u64.
    pub fn read_operand(
        &mut self,
        instruction: &Instruction,
        operand: u32,
        size: u8,
    ) -> Result<u64, CpuError> {
        let linear = self.effective_address(instruction, operand);
        let physical = self.translate(linear, AccessKind::Read)?;
        match size {
            1 => Ok(u64::from(self.memory.read_u8(physical)?)),
            2 => Ok(u64::from(self.memory.read_u16(physical)?)),
            4 => Ok(u64::from(self.memory.read_u32(physical)?)),
            8 => self.memory.read_u64(physical),
            _ => Err(CpuError::GuestFault(format!(
                "unsupported operand size {size}"
            ))),
        }
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
        let physical = self.translate(linear, AccessKind::Write)?;
        match size {
            1 => self.memory.write_u8(physical, value as u8),
            2 => self.memory.write_u16(physical, value as u16),
            4 => self.memory.write_u32(physical, value as u32),
            8 => self.memory.write_u64(physical, value),
            _ => Err(CpuError::GuestFault(format!(
                "unsupported operand size {size}"
            ))),
        }
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

    // ---- exceptions and interrupts ----

    /// Raises an exception through the IDT.
    pub fn raise(
        &mut self,
        vector: u8,
        error_code: u16,
        has_error_code: bool,
    ) -> Result<(), CpuError> {
        if self.in_exception {
            // Double fault while delivering: hardware would triple fault.
            return Err(CpuError::TripleFault);
        }
        self.in_exception = true;
        let result = self.deliver_exception(vector, error_code, has_error_code);
        self.in_exception = false;
        result
    }

    fn deliver_exception(
        &mut self,
        vector: u8,
        error_code: u16,
        has_error_code: bool,
    ) -> Result<(), CpuError> {
        let handler = self.idt_gate(vector)?;
        self.transfer_to_gate(handler, error_code, has_error_code, true)
    }

    fn deliver_pending(&mut self) -> Result<(), CpuError> {
        if self
            .regs
            .rflags
            .intersects(crate::arch::registers::RFlags::IF)
        {
            if let Some(irq) = self.pic.pending_irq() {
                let vector = self.pic.acknowledge(irq);
                let handler = self.idt_gate(vector)?;
                self.transfer_to_gate(handler, 0, false, false)?;
                return Ok(());
            }
        }
        if let Some(item) = self.pending_interrupts.pop_front() {
            let handler = self.idt_gate(item.vector)?;
            self.transfer_to_gate(handler, item.error_code, item.has_error_code, false)?;
        }
        Ok(())
    }

    fn idt_gate(&self, vector: u8) -> Result<Gate, CpuError> {
        let entry_size = match self.regs.mode() {
            CpuMode::Long => 16,
            _ => 8,
        };
        let address = self
            .regs
            .idt_base
            .wrapping_add(u64::from(vector) * entry_size);
        if self.regs.idt_limit < u32::from(vector) * 8 + 7 {
            return Err(CpuError::GuestFault(format!(
                "IDT limit {} cannot reach vector {vector}",
                self.regs.idt_limit
            )));
        }
        let raw = self.memory.read_u64(address)?;
        let selector = SegmentSelector((raw >> 16) as u16);
        let offset = match self.regs.mode() {
            CpuMode::Long => {
                let high = self.memory.read_u64(address + 8)?;
                (raw & 0xFFFF) | ((raw >> 32) & 0xFFFF_0000) | ((high & 0xFFFF_FFFF) << 32)
            }
            _ => (raw & 0xFFFF) | (raw >> 16 & 0xFFFF_0000),
        };
        Ok(Gate { selector, offset })
    }

    fn transfer_to_gate(
        &mut self,
        gate: Gate,
        error_code: u16,
        has_error_code: bool,
        is_exception: bool,
    ) -> Result<(), CpuError> {
        let old_flags = self.regs.rflags;
        let old_cs = self.regs.cs.selector;
        let new_cs = self.load_code_segment(gate.selector)?;
        let new_flags = if is_exception {
            old_flags
        } else {
            old_flags - crate::arch::registers::RFlags::IF - crate::arch::registers::RFlags::TF
        };
        self.regs.rflags = new_flags;
        match self.regs.mode() {
            CpuMode::Long => {
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
        self.regs.cs = new_cs;
        self.regs.rip = match self.regs.mode() {
            CpuMode::Long => gate.offset,
            CpuMode::Protected32 => gate.offset & 0xFFFF_FFFF,
            _ => gate.offset & 0xFFFF,
        };
        Ok(())
    }

    fn load_code_segment(
        &mut self,
        selector: SegmentSelector,
    ) -> Result<SegmentRegister, CpuError> {
        match self.regs.mode() {
            CpuMode::Real | CpuMode::Protected16 => Ok(SegmentRegister::real_mode(selector)),
            _ => self.load_segment_from_table(selector),
        }
    }

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
        let table_base = if selector.table() == 0 {
            self.regs.gdt_base
        } else {
            self.regs.ldtr.base
        };
        let address = table_base.wrapping_add(u64::from(selector.index()) * 8);
        let entry = self.memory.read_u64(address)?;
        let descriptor = Descriptor::decode(entry);
        Ok(descriptor.load(selector))
    }

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

    /// Serializes a port read through the attached device set.
    pub fn io_read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        match port {
            0x3F8..=0x3FF => Ok(u32::from(self.uart_console_read(port)?)),
            0x2F8..=0x2FF => Ok(u32::from(self.uart_control_read(port)?)),
            0x20 | 0x21 | 0xA0 | 0xA1 => {
                crate::devices::PortDevice::read(&mut self.pic, port, size)
            }
            0x40..=0x43 => crate::devices::PortDevice::read(&mut self.pit, port, size),
            0x70 | 0x71 => crate::devices::PortDevice::read(&mut self.cmos, port, size),
            _ => self.ports.read(port, size),
        }
    }

    /// Serializes a port write through the attached device set.
    pub fn io_write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        match port {
            0x3F8..=0x3FF => self.uart_console_write(port, value as u8),
            0x2F8..=0x2FF => self.uart_control_write(port, value as u8),
            0x20 | 0x21 | 0xA0 | 0xA1 => {
                crate::devices::PortDevice::write(&mut self.pic, port, size, value)
            }
            0x40..=0x43 => crate::devices::PortDevice::write(&mut self.pit, port, size, value),
            0x70 | 0x71 => crate::devices::PortDevice::write(&mut self.cmos, port, size, value),
            _ => self.ports.write(port, size, value),
        }
    }

    fn uart_console_read(&mut self, port: u16) -> Result<u8, CpuError> {
        crate::devices::PortDevice::read(&mut self.uart_console, port, 1).map(|value| value as u8)
    }

    fn uart_console_write(&mut self, port: u16, value: u8) -> Result<(), CpuError> {
        crate::devices::PortDevice::write(&mut self.uart_console, port, 1, u32::from(value))
    }

    fn uart_control_read(&mut self, port: u16) -> Result<u8, CpuError> {
        crate::devices::PortDevice::read(&mut self.uart_control, port, 1).map(|value| value as u8)
    }

    fn uart_control_write(&mut self, port: u16, value: u8) -> Result<(), CpuError> {
        crate::devices::PortDevice::write(&mut self.uart_control, port, 1, u32::from(value))
    }
}

#[derive(Clone, Copy, Debug)]
struct Gate {
    selector: SegmentSelector,
    offset: u64,
}

fn is_xmm_register(register: Register) -> bool {
    matches!(
        register,
        Register::XMM0
            | Register::XMM1
            | Register::XMM2
            | Register::XMM3
            | Register::XMM4
            | Register::XMM5
            | Register::XMM6
            | Register::XMM7
            | Register::XMM8
            | Register::XMM9
            | Register::XMM10
            | Register::XMM11
            | Register::XMM12
            | Register::XMM13
            | Register::XMM14
            | Register::XMM15
    )
}

fn is_control_register(register: Register) -> bool {
    matches!(
        register,
        Register::CR0 | Register::CR2 | Register::CR3 | Register::CR4 | Register::CR8
    )
}

fn is_debug_register(register: Register) -> bool {
    matches!(
        register,
        Register::DR0
            | Register::DR1
            | Register::DR2
            | Register::DR3
            | Register::DR6
            | Register::DR7
    )
}

fn address_size_of(instruction: &Instruction) -> u32 {
    let register = if instruction.memory_base() != Register::None {
        instruction.memory_base()
    } else {
        instruction.memory_index()
    };
    match register {
        Register::None => 8,
        Register::RAX
        | Register::RCX
        | Register::RDX
        | Register::RBX
        | Register::RSP
        | Register::RBP
        | Register::RSI
        | Register::RDI
        | Register::R8
        | Register::R9
        | Register::R10
        | Register::R11
        | Register::R12
        | Register::R13
        | Register::R14
        | Register::R15 => 8,
        Register::EAX
        | Register::ECX
        | Register::EDX
        | Register::EBX
        | Register::ESP
        | Register::EBP
        | Register::ESI
        | Register::EDI
        | Register::R8D
        | Register::R9D
        | Register::R10D
        | Register::R11D
        | Register::R12D
        | Register::R13D
        | Register::R14D
        | Register::R15D => 4,
        _ => 2,
    }
}

pub fn register_index(register: Register) -> usize {
    match register {
        Register::RAX | Register::EAX | Register::AX | Register::AL | Register::AH => index::RAX,
        Register::RCX | Register::ECX | Register::CX | Register::CL | Register::CH => index::RCX,
        Register::RDX | Register::EDX | Register::DX | Register::DL | Register::DH => index::RDX,
        Register::RBX | Register::EBX | Register::BX | Register::BL | Register::BH => index::RBX,
        Register::RSP | Register::ESP | Register::SP => index::RSP,
        Register::RBP | Register::EBP | Register::BP => index::RBP,
        Register::RSI | Register::ESI | Register::SI => index::RSI,
        Register::RDI | Register::EDI | Register::DI => index::RDI,
        Register::R8 | Register::R8D | Register::R8W | Register::R8L => index::R8,
        Register::R9 | Register::R9D | Register::R9W | Register::R9L => index::R9,
        Register::R10 | Register::R10D | Register::R10W | Register::R10L => index::R10,
        Register::R11 | Register::R11D | Register::R11W | Register::R11L => index::R11,
        Register::R12 | Register::R12D | Register::R12W | Register::R12L => index::R12,
        Register::R13 | Register::R13D | Register::R13W | Register::R13L => index::R13,
        Register::R14 | Register::R14D | Register::R14W | Register::R14L => index::R14,
        Register::R15 | Register::R15D | Register::R15W | Register::R15L => index::R15,
        _ => index::RAX,
    }
}

#[allow(dead_code)]
fn _dispatch_table_vectors() -> (u8, u8, u8) {
    (VECTOR_DIVIDE, VECTOR_INVALID_OPCODE, VECTOR_DOUBLE_FAULT)
}

#[allow(dead_code)]
fn _page_fault_vector() -> u8 {
    VECTOR_PAGE_FAULT
}
