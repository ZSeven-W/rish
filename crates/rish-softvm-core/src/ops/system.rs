//! System instructions: port I/O, MSRs, tables, control registers, halt.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::arch::registers::{Cr0, Cr4, Efer, index};
use crate::arch::segments::SegmentSelector;
use crate::ops::{
    operand_size, read_operand0, read_operand1, read_register, write_operand0, write_register,
};
use crate::{CpuError, cpu::Cpu};

pub fn in_out(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let is_in = instruction.mnemonic() == Mnemonic::In;
    // The accumulator register and the port can appear in either operand
    // slot: IN/OUT imm8, AL has the port as op0, while IN/OUT DX, AL has
    // the port register as op1.
    let accumulator = [instruction.op0_register(), instruction.op1_register()]
        .into_iter()
        .find(|register| matches!(register, Register::AL | Register::AX | Register::EAX))
        .unwrap_or(Register::AL);
    let size = match accumulator {
        Register::AL => 1,
        Register::AX => 2,
        _ => 4,
    };
    let port: u16 = if instruction.op0_kind() == OpKind::Immediate8
        || instruction.op1_kind() == OpKind::Immediate8
    {
        instruction.immediate8to16() as u16
    } else {
        read_register(&cpu.regs, Register::DX, 2) as u16
    };
    if is_in {
        let value = cpu.io_read(port, size)?;
        write_register(&mut cpu.regs, accumulator, size, u64::from(value));
    } else {
        let value = read_register(&cpu.regs, accumulator, size);
        cpu.io_write(port, size, value as u32)?;
    }
    Ok(())
}
pub fn hlt(cpu: &mut Cpu, _instruction: &Instruction) -> Result<(), CpuError> {
    if cpu.regs.rflags.contains(crate::arch::registers::RFlags::IF) {
        // Interrupts enabled: this is the idle loop. Stop retiring
        // instructions until an interrupt is deliverable.
        cpu.waiting_for_interrupt = true;
    } else {
        // Interrupts disabled: a deliberate halt (panic/machine_halt).
        cpu.halted = true;
    }
    Ok(())
}

pub fn cpuid(cpu: &mut Cpu, _instruction: &Instruction) -> Result<(), CpuError> {
    let leaf = read_register(&cpu.regs, Register::EAX, 4) as u32;
    let subleaf = read_register(&cpu.regs, Register::ECX, 4) as u32;
    let (eax, ebx, ecx, edx) = cpuid_leaf(leaf, subleaf);
    write_register(&mut cpu.regs, Register::EAX, 4, u64::from(eax));
    write_register(&mut cpu.regs, Register::EBX, 4, u64::from(ebx));
    write_register(&mut cpu.regs, Register::ECX, 4, u64::from(ecx));
    write_register(&mut cpu.regs, Register::EDX, 4, u64::from(edx));
    Ok(())
}

fn cpuid_leaf(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    match leaf {
        // The vendor string is returned in EBX, EDX, ECX order.
        0x0000_0000 => (
            0x0000_0016,
            u32::from_le_bytes(*b"Genu"),
            u32::from_le_bytes(*b"ntel"),
            u32::from_le_bytes(*b"ineI"),
        ),
        0x0000_0001 => {
            // Family 6 model 158, no hyperthreads exposed; conservative but
            // complete feature set for a stock x86_64 kernel.
            let eax = 0x0009_0600;
            let ecx = (1 << 0)  // SSE3
                | (1 << 9)      // SSSE3
                | (1 << 13)     // CX16
                | (1 << 19)     // SSE4.1
                | (1 << 20)     // SSE4.2
                | (1 << 22)     // MOVBE
                | (1 << 23)     // POPCNT
                | (1 << 30); // RDRAND
            let edx = (1 << 0)  // FPU
                | (1 << 4)      // TSC
                | (1 << 5)      // MSR
                | (1 << 6)      // PAE
                | (1 << 8)      // CX8
                | (1 << 9)      // APIC
                | (1 << 11)     // SEP
                | (1 << 13)     // PGE
                | (1 << 15)     // CMOV
                | (1 << 19)     // CLFSH
                | (1 << 23)     // MMX
                | (1 << 24)     // FXSR
                | (1 << 25)     // SSE
                | (1 << 26)     // SSE2
                | (1 << 28); // HTT
            (eax, 0, ecx, edx)
        }
        0x0000_0007 if subleaf == 0 => {
            let ebx = (1 << 3) | (1 << 8) | (1 << 18);
            (0, ebx, 0, 0)
        }
        0x0000_0007 => (0, 0, 0, 0),
        0x0000_000B | 0x0000_001F => (0, 0, 0, 0),
        0x0000_000D if subleaf == 0 => (0x3, 0, 0, 0),
        0x0000_000D => (0, 0, 0, 0),
        0x8000_0000 => (0x8000_0008, 0, 0, 0),
        0x8000_0001 => {
            let ecx = 1 << 0; // LAHF/SAHF
            let edx = (1 << 11) // SYSCALL/SYSRET
                | (1 << 20)     // NX
                | (1 << 26)     // 1GB pages
                | (1 << 27)     // RDTSCP
                | (1 << 29); // LM
            (0, 0, ecx, edx)
        }
        0x8000_0007 => (0, 0, 0, 1 << 8), // Invariant TSC
        0x8000_0008 => (0x0000_3028, 0, 0, 0),
        _ => (0, 0, 0, 0),
    }
}

pub fn rdtsc(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let tsc = cpu.tsc;
    write_register(&mut cpu.regs, Register::EAX, 4, tsc & 0xFFFF_FFFF);
    write_register(&mut cpu.regs, Register::EDX, 4, tsc >> 32);
    if instruction.mnemonic() == Mnemonic::Rdtscp {
        // TSC_AUX = 0.
        write_register(&mut cpu.regs, Register::ECX, 4, 0);
    }
    Ok(())
}

pub fn system_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Rdmsr => {
            let address = read_register(&cpu.regs, Register::ECX, 4) as u32;
            let value = msr_read(cpu, address)?;
            write_register(&mut cpu.regs, Register::EAX, 4, value & 0xFFFF_FFFF);
            write_register(&mut cpu.regs, Register::EDX, 4, value >> 32);
        }
        Mnemonic::Wrmsr => {
            let address = read_register(&cpu.regs, Register::ECX, 4) as u32;
            let value = read_register(&cpu.regs, Register::EAX, 4)
                | read_register(&cpu.regs, Register::EDX, 4) << 32;
            msr_write(cpu, address, value)?;
        }
        Mnemonic::Rdpmc => {
            let value = cpu.tsc;
            write_register(&mut cpu.regs, Register::EAX, 4, value & 0xFFFF_FFFF);
            write_register(&mut cpu.regs, Register::EDX, 4, value >> 32);
        }
        Mnemonic::Lgdt => {
            let wide = matches!(instruction.memory_size(), iced_x86::MemorySize::Fword10);
            let limit = read_operand0(cpu, instruction)? as u16;
            let base = cpu.effective_address(instruction, 0) + 2;
            let physical = cpu.translate(base, crate::arch::paging::AccessKind::Read)?;
            cpu.regs.gdt_base = if wide {
                cpu.memory.read_u64(physical)?
            } else {
                u64::from(cpu.memory.read_u32(physical)?)
            };
            cpu.regs.gdt_limit = u32::from(limit);
        }
        Mnemonic::Lidt => {
            let wide = matches!(instruction.memory_size(), iced_x86::MemorySize::Fword10);
            let limit = read_operand0(cpu, instruction)? as u16;
            let base = cpu.effective_address(instruction, 0) + 2;
            let physical = cpu.translate(base, crate::arch::paging::AccessKind::Read)?;
            cpu.regs.idt_base = if wide {
                cpu.memory.read_u64(physical)?
            } else {
                u64::from(cpu.memory.read_u32(physical)?)
            };
            cpu.regs.idt_limit = u32::from(limit);
        }
        Mnemonic::Sgdt => {
            let physical = cpu.translate(
                cpu.effective_address(instruction, 0),
                crate::arch::paging::AccessKind::Write,
            )?;
            cpu.memory.write_u16(physical, cpu.regs.gdt_limit as u16)?;
            cpu.memory.write_u64(physical + 2, cpu.regs.gdt_base)?;
        }
        Mnemonic::Sidt => {
            let physical = cpu.translate(
                cpu.effective_address(instruction, 0),
                crate::arch::paging::AccessKind::Write,
            )?;
            cpu.memory.write_u16(physical, cpu.regs.idt_limit as u16)?;
            cpu.memory.write_u64(physical + 2, cpu.regs.idt_base)?;
        }
        Mnemonic::Lldt => {
            let selector = SegmentSelector(read_operand0(cpu, instruction)? as u16);
            cpu.regs.ldtr = if selector.0 == 0 {
                crate::arch::segments::SegmentRegister::default()
            } else {
                cpu.load_segment_from_table(selector)?
            };
        }
        Mnemonic::Sldt => {
            let value = u64::from(cpu.regs.ldtr.selector.0);
            if instruction.op0_kind() == OpKind::Memory {
                cpu.write_operand(instruction, 0, 2, value)?;
            } else {
                write_register(&mut cpu.regs, instruction.op0_register(), 2, value);
            }
        }
        Mnemonic::Ltr => {
            let selector = SegmentSelector(read_operand0(cpu, instruction)? as u16);
            let loaded = cpu.load_segment_from_table(selector)?;
            cpu.regs.tr = loaded;
            cpu.regs.tr_base = loaded.base;
        }
        Mnemonic::Str => {
            let value = u64::from(cpu.regs.tr.selector.0);
            if instruction.op0_kind() == OpKind::Memory {
                cpu.write_operand(instruction, 0, 2, value)?;
            } else {
                write_register(&mut cpu.regs, instruction.op0_register(), 2, value);
            }
        }
        Mnemonic::Mov if is_control_register(instruction.op1_register()) => {
            let cr = control_register(instruction.op1_register());
            let value = match cr {
                0 => cpu.regs.cr0.bits(),
                2 => cpu.regs.cr2,
                3 => cpu.regs.cr3,
                4 => cpu.regs.cr4.bits(),
                8 => cpu.regs.cr8,
                _ => return cpu.raise(13, 0, true),
            };
            write_register(
                &mut cpu.regs,
                instruction.op0_register(),
                operand_size(instruction, 0),
                value,
            );
        }
        Mnemonic::Mov if is_control_register(instruction.op0_register()) => {
            let value = read_operand1(cpu, instruction)?;
            let cr = control_register(instruction.op0_register());
            match cr {
                0 => cpu.regs.cr0 = Cr0::from_bits_truncate(value),
                2 => cpu.regs.cr2 = value,
                3 => cpu.regs.cr3 = value,
                4 => cpu.regs.cr4 = Cr4::from_bits_truncate(value),
                8 => cpu.regs.cr8 = value,
                _ => return cpu.raise(13, 0, true),
            }
            // MOV to CR0 updates the paging/cache consistency model.
            if cr == 0 && cpu.regs.cr0.contains(Cr0::PG) {
                cpu.regs.cr0 |= Cr0::WP;
            }
        }
        Mnemonic::Mov if is_debug_register(instruction.op1_register()) => {
            write_register(
                &mut cpu.regs,
                instruction.op0_register(),
                operand_size(instruction, 0),
                0,
            );
        }
        Mnemonic::Mov if is_debug_register(instruction.op0_register()) => {}
        Mnemonic::Clts => cpu.regs.cr0 -= Cr0::TS,
        Mnemonic::Lmsw => {
            let value = read_operand0(cpu, instruction)?;
            let mut cr0 = cpu.regs.cr0.bits();
            cr0 = (cr0 & 0xFFFF_FFF0) | (value & 0xF);
            cpu.regs.cr0 = Cr0::from_bits_truncate(cr0);
        }
        Mnemonic::Smsw => {
            let value = cpu.regs.cr0.bits() & 0xFFFF;
            if instruction.op0_kind() == OpKind::Memory {
                cpu.write_operand(instruction, 0, 2, value)?;
            } else {
                write_register(&mut cpu.regs, instruction.op0_register(), 2, value);
            }
        }
        Mnemonic::Invlpg => {}
        Mnemonic::Wbinvd | Mnemonic::Invd => {}
        Mnemonic::Lfence | Mnemonic::Sfence | Mnemonic::Mfence => {}
        Mnemonic::Iret | Mnemonic::Iretq | Mnemonic::Iretd => {
            let target = cpu.pop_native()?;
            let selector = SegmentSelector(cpu.pop_native()? as u16);
            let flags = cpu.pop_native()?;
            cpu.regs.rflags = crate::arch::registers::RFlags::from_bits_truncate(flags)
                | (cpu.regs.rflags & crate::arch::registers::RFlags::VM);
            cpu.regs.cs = match cpu.regs.mode() {
                crate::arch::registers::CpuMode::Real
                | crate::arch::registers::CpuMode::Protected16 => {
                    crate::arch::segments::SegmentRegister::real_mode(selector)
                }
                _ => cpu.load_segment_from_table(selector)?,
            };
            if cpu.regs.mode() == crate::arch::registers::CpuMode::Long {
                // Same-privilege return: RSP and SS follow on the frame.
                let rsp = cpu.pop_native()?;
                let ss = SegmentSelector(cpu.pop_native()? as u16);
                cpu.regs.set_rsp(rsp);
                cpu.regs.ss = cpu.load_segment_from_table(ss)?;
            }
            cpu.regs.rip = target;
        }
        Mnemonic::Int3 | Mnemonic::Int => {
            let vector = if instruction.mnemonic() == Mnemonic::Int3 {
                3
            } else {
                instruction.immediate8()
            };
            cpu.inject_interrupt(vector, 0, false)?;
        }
        Mnemonic::Into => {
            if cpu.regs.rflags.contains(crate::arch::registers::RFlags::OF) {
                cpu.inject_interrupt(4, 0, false)?;
            }
        }
        Mnemonic::Bound => {
            return cpu.raise(5, 0, false);
        }
        _ => {
            return Err(CpuError::UnimplementedInstruction {
                code: format!("{:?}", instruction.code()),
                address: cpu.regs.rip,
                bytes: Vec::new(),
            });
        }
    }
    Ok(())
}

pub fn extra_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Bswap => {
            let size = operand_size(instruction, 0);
            let value = read_operand0(cpu, instruction)?;
            let swapped = match size {
                4 => u64::from((value as u32).swap_bytes()),
                8 => value.swap_bytes(),
                _ => return Err(bad_op(cpu, instruction)),
            };
            write_operand0(cpu, instruction, swapped)?;
        }
        Mnemonic::Popcnt => {
            let size = operand_size(instruction, 1);
            let value = if instruction.op1_kind() == OpKind::Memory {
                cpu.read_operand(instruction, 1, crate::ops::memory_size(instruction))?
            } else {
                read_register(&cpu.regs, instruction.op1_register(), size)
            };
            let count = value.count_ones();
            cpu.regs
                .rflags
                .set(crate::arch::registers::RFlags::ZF, value == 0);
            cpu.regs.rflags -= crate::arch::registers::RFlags::CF
                - crate::arch::registers::RFlags::OF
                - crate::arch::registers::RFlags::SF
                - crate::arch::registers::RFlags::AF
                - crate::arch::registers::RFlags::PF;
            write_register(
                &mut cpu.regs,
                instruction.op0_register(),
                size,
                u64::from(count),
            );
        }
        Mnemonic::Ud2 => cpu.raise(6, 0, false)?,
        Mnemonic::Rdrand | Mnemonic::Rdseed => {
            let size = operand_size(instruction, 0);
            let value = cpu.tsc.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            cpu.regs.rflags |= crate::arch::registers::RFlags::CF;
            if instruction.op0_kind() == OpKind::Memory {
                cpu.write_operand(instruction, 0, size, value)?;
            } else {
                write_register(&mut cpu.regs, instruction.op0_register(), size, value);
            }
        }
        Mnemonic::Prefetcht0
        | Mnemonic::Prefetcht1
        | Mnemonic::Prefetcht2
        | Mnemonic::Prefetchnta
        | Mnemonic::Prefetchw
        | Mnemonic::Clflush
        | Mnemonic::Clflushopt
        | Mnemonic::Clac
        | Mnemonic::Stac
        | Mnemonic::Endbr64
        | Mnemonic::Endbr32 => {}
        Mnemonic::Fxsave => {
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
            let mut region = [0_u8; 512];
            region[0..2].copy_from_slice(&cpu.fpu_control_word.to_le_bytes());
            region[2..4].copy_from_slice(&cpu.fpu_status_word.to_le_bytes());
            region[4..6].copy_from_slice(&0xFFFF_u16.to_le_bytes());
            region[24..28].copy_from_slice(&cpu.mxcsr.to_le_bytes());
            cpu.memory.write(physical, &region)?;
        }
        Mnemonic::Fxrstor => {
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            let mut region = [0_u8; 512];
            cpu.memory.read(physical, &mut region)?;
            cpu.fpu_control_word = u16::from_le_bytes([region[0], region[1]]);
            cpu.fpu_status_word = u16::from_le_bytes([region[2], region[3]]);
            cpu.mxcsr = u32::from_le_bytes([region[24], region[25], region[26], region[27]]);
        }
        Mnemonic::Fninit => {
            cpu.fpu_control_word = 0x037F;
            cpu.fpu_status_word = 0;
        }
        Mnemonic::Fnstcw => {
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
            cpu.memory.write_u16(physical, cpu.fpu_control_word)?;
        }
        Mnemonic::Fldcw => {
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            cpu.fpu_control_word = cpu.memory.read_u16(physical)?;
        }
        Mnemonic::Fnstsw => {
            if instruction.op0_kind() == OpKind::Memory {
                let linear = cpu.effective_address(instruction, 0);
                let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
                cpu.memory.write_u16(physical, cpu.fpu_status_word)?;
            } else {
                write_register(
                    &mut cpu.regs,
                    Register::AX,
                    2,
                    u64::from(cpu.fpu_status_word),
                );
            }
        }
        Mnemonic::Fnclex => cpu.fpu_status_word &= !0xFF,
        Mnemonic::Wait => {}
        Mnemonic::Shld | Mnemonic::Shrd => {
            shld_shrd(cpu, instruction)?;
        }
        Mnemonic::Swapgs => {
            std::mem::swap(&mut cpu.regs.gs.base, &mut cpu.kernel_gs_base);
        }
        Mnemonic::Xgetbv => {
            let index = read_register(&cpu.regs, Register::ECX, 4) as u32;
            let value = if index == 0 { cpu.xcr0 } else { 0 };
            write_register(&mut cpu.regs, Register::EAX, 4, value & 0xFFFF_FFFF);
            write_register(&mut cpu.regs, Register::EDX, 4, value >> 32);
        }
        Mnemonic::Xsetbv => {
            let index = read_register(&cpu.regs, Register::ECX, 4) as u32;
            if index == 0 {
                let value = read_register(&cpu.regs, Register::EAX, 4)
                    | read_register(&cpu.regs, Register::EDX, 4) << 32;
                cpu.xcr0 = value & 0x7;
            }
        }
        Mnemonic::Syscall => syscall(cpu)?,
        Mnemonic::Sysret | Mnemonic::Sysretq => sysret(cpu)?,
        _ => return Err(bad_op(cpu, instruction)),
    }
    Ok(())
}

fn shld_shrd(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let destination = read_operand0(cpu, instruction)?;
    let source = read_register(&cpu.regs, instruction.op1_register(), size);
    let count = match instruction.op2_kind() {
        OpKind::Immediate8 => instruction.immediate(2) & 0x3F,
        _ => read_register(&cpu.regs, Register::CL, 1) & 0x3F,
    };
    let bits = u32::from(size) * 8;
    let result = if instruction.mnemonic() == Mnemonic::Shld {
        if count >= u64::from(bits) {
            u64::MAX
        } else {
            (destination << count) | (source >> (bits - count as u32))
        }
    } else if count >= u64::from(bits) {
        0
    } else {
        (destination >> count) | (source << (bits - count as u32))
    };
    let mask = crate::ops::bits_mask(bits);
    let result = result & mask;
    crate::ops::set_szp(&mut cpu.regs, result, bits);
    if count == 1 {
        let msb = destination & (1_u64 << (bits - 1)) != 0;
        cpu.regs.rflags.set(crate::arch::registers::RFlags::CF, msb);
        let new_msb = (result >> (bits - 1)) & 1 != 0;
        cpu.regs
            .rflags
            .set(crate::arch::registers::RFlags::OF, msb != new_msb);
    }
    write_operand0(cpu, instruction, result)?;
    Ok(())
}

fn syscall(cpu: &mut Cpu) -> Result<(), CpuError> {
    use crate::arch::registers::index;
    // RCX := next RIP, R11 := RFLAGS, switch to the kernel segments from
    // IA32_STAR, mask RFLAGS with IA32_FMASK, and jump to IA32_LSTAR.
    cpu.regs.set_gpr(index::RCX, cpu.regs.rip);
    cpu.regs.set_gpr(index::R11, cpu.regs.rflags.bits());
    let kernel_cs = ((cpu.msr_star >> 32) & 0xFFFC) as u16;
    cpu.regs.cs = cpu.load_segment_from_table(SegmentSelector(kernel_cs))?;
    cpu.regs.ss = cpu.load_segment_from_table(SegmentSelector(kernel_cs + 8))?;
    cpu.regs.rflags =
        crate::arch::registers::RFlags::from_bits_retain(cpu.regs.rflags.bits() & !cpu.msr_fmask);
    cpu.regs.rip = cpu.msr_lstar;
    Ok(())
}

fn sysret(cpu: &mut Cpu) -> Result<(), CpuError> {
    use crate::arch::registers::index;
    // RIP := RCX, RFLAGS := R11 (reserved bits cleared, bit 1 set), switch
    // back to the user segments derived from IA32_STAR[63:48].
    cpu.regs.rip = cpu.regs.gpr(index::RCX);
    let flags = cpu.regs.gpr(index::R11);
    cpu.regs.rflags = crate::arch::registers::RFlags::from_bits_retain((flags & 0x3C7FD7) | 2);
    let user_base = (cpu.msr_star >> 48) as u16;
    cpu.regs.cs = cpu.load_segment_from_table(SegmentSelector(user_base.wrapping_add(16) | 3))?;
    cpu.regs.ss = cpu.load_segment_from_table(SegmentSelector(user_base.wrapping_add(8) | 3))?;
    Ok(())
}

fn bad_op(cpu: &Cpu, instruction: &Instruction) -> CpuError {
    CpuError::UnimplementedInstruction {
        code: format!("{:?}", instruction.mnemonic()),
        address: cpu.regs.rip,
        bytes: Vec::new(),
    }
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

fn control_register(register: Register) -> u8 {
    match register {
        Register::CR0 => 0,
        Register::CR2 => 2,
        Register::CR3 => 3,
        Register::CR4 => 4,
        Register::CR8 => 8,
        _ => 0,
    }
}

const MSR_IA32_APIC_BASE: u32 = 0x1B;
const MSR_IA32_TSC: u32 = 0x10;
const MSR_IA32_MTRR_DEF_TYPE: u32 = 0x2FF;
const MSR_IA32_MISC_ENABLE: u32 = 0x1A0;
const MSR_IA32_SYSENTER_CS: u32 = 0x174;
const MSR_IA32_SYSENTER_ESP: u32 = 0x175;
const MSR_IA32_SYSENTER_EIP: u32 = 0x176;
const MSR_IA32_PAT: u32 = 0x277;
const MSR_FS_BASE: u32 = 0xC000_0100;
const MSR_GS_BASE: u32 = 0xC000_0101;
const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;
const MSR_EFER: u32 = 0xC000_0080;
const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_CSTAR: u32 = 0xC000_0083;
const MSR_FMASK: u32 = 0xC000_0084;
const MSR_IA32_TSC_ADJUST: u32 = 0x3B;
const MSR_IA32_BIOS_SIGN_ID: u32 = 0x8B;
const MSR_IA32_PLATFORM_ID: u32 = 0x17;
const MSR_IA32_MTRRCAP: u32 = 0xFE;
const MSR_IA32_MCG_CAP: u32 = 0x179;
const MSR_IA32_MCG_STATUS: u32 = 0x17A;
const MSR_IA32_MCG_CTL: u32 = 0x17B;
const MSR_IA32_THERM_INTERRUPT: u32 = 0x19B;
const MSR_IA32_THERM_STATUS: u32 = 0x19C;
const MSR_MTRR_PHYS_BASE_FIRST: u32 = 0x200;
const MSR_MTRR_PHYS_BASE_LAST: u32 = 0x20F;
const MSR_MTRR_FIX_FIRST: u32 = 0x250;
const MSR_MTRR_FIX_LAST: u32 = 0x25F;
const MSR_IA32_MC_FIRST: u32 = 0x400;
const MSR_IA32_MC_LAST: u32 = 0x403;

fn msr_read(cpu: &Cpu, address: u32) -> Result<u64, CpuError> {
    match address {
        MSR_EFER => Ok(cpu.regs.efer.bits()),
        MSR_IA32_APIC_BASE => Ok(0xFEE0_0000 | (1 << 11) | (1 << 8)),
        MSR_IA32_TSC => Ok(cpu.tsc),
        MSR_IA32_MTRR_DEF_TYPE => Ok(0x6),
        MSR_IA32_MTRRCAP => Ok(0x508),
        MSR_IA32_MCG_CAP => Ok(0x100),
        MSR_IA32_MISC_ENABLE => Ok(0),
        MSR_IA32_PAT => Ok(0x0007_0406_0007_0406),
        MSR_FS_BASE => Ok(cpu.regs.fs.base),
        MSR_GS_BASE => Ok(cpu.regs.gs.base),
        MSR_KERNEL_GS_BASE => Ok(cpu.kernel_gs_base),
        MSR_STAR => Ok(cpu.msr_star),
        MSR_LSTAR => Ok(cpu.msr_lstar),
        MSR_CSTAR => Ok(cpu.msr_cstar),
        MSR_FMASK => Ok(cpu.msr_fmask),
        MSR_IA32_TSC_ADJUST
        | MSR_IA32_BIOS_SIGN_ID
        | MSR_IA32_PLATFORM_ID
        | MSR_IA32_MCG_STATUS
        | MSR_IA32_MCG_CTL
        | MSR_IA32_THERM_INTERRUPT
        | MSR_IA32_THERM_STATUS
        | MSR_IA32_SYSENTER_CS
        | MSR_IA32_SYSENTER_ESP
        | MSR_IA32_SYSENTER_EIP
        | MSR_MTRR_PHYS_BASE_FIRST..=MSR_MTRR_PHYS_BASE_LAST
        | MSR_MTRR_FIX_FIRST..=MSR_MTRR_FIX_LAST
        | MSR_IA32_MC_FIRST..=MSR_IA32_MC_LAST => Ok(0),
        _ => Err(CpuError::GuestFault(format!(
            "unimplemented MSR read {address:#x}"
        ))),
    }
}

fn msr_write(cpu: &mut Cpu, address: u32, value: u64) -> Result<(), CpuError> {
    match address {
        MSR_EFER => {
            cpu.regs.efer = Efer::from_bits_truncate(value);
        }
        MSR_FS_BASE => cpu.regs.fs.base = value,
        MSR_GS_BASE => cpu.regs.gs.base = value,
        MSR_KERNEL_GS_BASE => cpu.kernel_gs_base = value,
        MSR_STAR => cpu.msr_star = value,
        MSR_LSTAR => cpu.msr_lstar = value,
        MSR_CSTAR => cpu.msr_cstar = value,
        MSR_FMASK => cpu.msr_fmask = value,
        MSR_IA32_APIC_BASE => {
            // BSP bit and enable bit only; relocation ignored for now.
        }
        MSR_IA32_TSC_ADJUST
        | MSR_IA32_BIOS_SIGN_ID
        | MSR_IA32_PLATFORM_ID
        | MSR_IA32_MTRR_DEF_TYPE
        | MSR_IA32_MTRRCAP
        | MSR_IA32_MCG_CAP
        | MSR_IA32_MCG_STATUS
        | MSR_IA32_MCG_CTL
        | MSR_IA32_THERM_INTERRUPT
        | MSR_IA32_THERM_STATUS
        | MSR_IA32_PAT
        | MSR_IA32_MISC_ENABLE
        | MSR_IA32_SYSENTER_CS
        | MSR_IA32_SYSENTER_ESP
        | MSR_IA32_SYSENTER_EIP
        | MSR_MTRR_PHYS_BASE_FIRST..=MSR_MTRR_PHYS_BASE_LAST
        | MSR_MTRR_FIX_FIRST..=MSR_MTRR_FIX_LAST
        | MSR_IA32_MC_FIRST..=MSR_IA32_MC_LAST => {}
        _ => {
            return Err(CpuError::GuestFault(format!(
                "unimplemented MSR write {address:#x}"
            )));
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn _cr_index_hint() -> usize {
    index::RAX
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::registers::Cr0;

    fn cpu() -> Cpu {
        Cpu::new(1, 0).unwrap()
    }

    fn decode(bitness: u32, bytes: &[u8], ip: u64) -> Instruction {
        let mut decoder =
            iced_x86::Decoder::with_ip(bitness, bytes, ip, iced_x86::DecoderOptions::NONE);
        decoder.decode()
    }

    fn encode_descriptor(descriptor: &crate::arch::segments::Descriptor) -> u64 {
        let mut entry = 0_u64;
        entry |= (descriptor.base & 0xFF00_0000) << 32;
        entry |= (descriptor.base & 0x00FF_0000) << 16;
        entry |= (descriptor.base & 0xFFFF) << 16;
        let mut limit = descriptor.limit;
        if descriptor.granularity {
            limit >>= 12;
        }
        entry |= u64::from(limit & 0xF) << 48;
        entry |= u64::from(limit & 0xFFFF);
        if descriptor.granularity {
            entry |= 1 << 55;
        }
        if descriptor.default_32 {
            entry |= 1 << 54;
        }
        if descriptor.long_mode {
            entry |= 1 << 53;
        }
        if descriptor.present {
            entry |= 1 << 47;
        }
        entry |= u64::from(descriptor.dpl & 0b11) << 45;
        entry |= u64::from(descriptor.descriptor_type & 0xF) << 40;
        if descriptor.code {
            entry |= 1 << 43;
        }
        if descriptor.conforming {
            entry |= 1 << 42;
        }
        if descriptor.expand_down {
            entry |= 1 << 42;
        }
        if descriptor.writable_or_readable {
            entry |= 1 << 41;
        }
        if descriptor.accessed {
            entry |= 1 << 40;
        }
        entry
    }

    fn run(cpu: &mut Cpu, bitness: u32, bytes: &[u8]) -> Result<(), CpuError> {
        cpu.memory.write(0x1000, bytes).unwrap();
        cpu.regs.rip = 0x1000;
        if bitness == 64 {
            cpu.regs.efer |= crate::arch::registers::Efer::LMA;
        }
        cpu.regs.cs = crate::arch::segments::SegmentRegister {
            base: 0,
            long_mode: bitness == 64,
            default_32: bitness == 32,
            code: true,
            limit: u32::MAX,
            granularity: true,
            writable_or_readable: true,
            ..Default::default()
        };
        let instruction = decode(bitness, bytes, 0x1000);
        let size = instruction.len();
        cpu.regs.rip = 0x1000 + size as u64;
        cpu.dispatch(&instruction)
    }

    #[test]
    fn out_to_uart_sends_bytes() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x41);
        // mov dx, 0x3f8 ; out dx, al
        run(&mut cpu, 64, &[0x66, 0xBA, 0xF8, 0x03]).unwrap();
        run(&mut cpu, 64, &[0xEE]).unwrap();
        assert_eq!(cpu.uart_console.drain_output(), b"A");
    }

    #[test]
    fn in_from_cmos_index_data() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x0A);
        // out 0x70, al ; in al, 0x71
        run(&mut cpu, 64, &[0xE6, 0x70]).unwrap();
        run(&mut cpu, 64, &[0xE4, 0x71]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX) & 0xFF, 0x20);
    }

    #[test]
    fn cpuid_leaves_vendor_and_features() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0);
        run(&mut cpu, 64, &[0x0F, 0xA2]).unwrap();
        assert_eq!(
            cpu.regs.gpr(index::RBX),
            u32::from_le_bytes(*b"Genu") as u64
        );
        cpu.regs.set_gpr(index::RAX, 1);
        run(&mut cpu, 64, &[0x0F, 0xA2]).unwrap();
        let edx = cpu.regs.gpr(index::RDX) as u32;
        assert_ne!(edx & (1 << 26), 0); // SSE2
        assert_ne!(edx & (1 << 15), 0); // CMOV
        assert_ne!(edx & (1 << 9), 0); // APIC
    }

    #[test]
    fn syscall_switches_to_lstar_and_records_rip_rflags() {
        let mut cpu = cpu();
        // GDT entries for kernel CS/SS: install flat code at 0x10 and data
        // at 0x18 before the syscall segment loads.
        let cs = crate::arch::segments::Descriptor {
            base: 0,
            limit: 0xFFFFF,
            granularity: true,
            default_32: false,
            long_mode: true,
            present: true,
            dpl: 0,
            system: false,
            descriptor_type: 0b1010,
            code: true,
            conforming: false,
            expand_down: false,
            writable_or_readable: true,
            accessed: false,
        };
        let ds = crate::arch::segments::Descriptor {
            base: 0,
            limit: 0xFFFFF,
            granularity: true,
            default_32: false,
            long_mode: false,
            present: true,
            dpl: 0,
            system: false,
            descriptor_type: 0b0010,
            code: false,
            conforming: false,
            expand_down: false,
            writable_or_readable: true,
            accessed: false,
        };
        cpu.memory
            .write_u64(0x10000, encode_descriptor(&cs))
            .unwrap();
        cpu.memory
            .write_u64(0x10008, encode_descriptor(&ds))
            .unwrap();
        cpu.regs.gdt_base = 0x10000;
        cpu.regs.gdt_limit = 0x27;
        cpu.regs.rip = 0x2000;
        cpu.regs.rflags = crate::arch::registers::RFlags::IF | crate::arch::registers::RFlags::CF;
        cpu.msr_star = (0x23_u64 << 48) | (0x10_u64 << 32);
        cpu.msr_lstar = 0xFFFF_FFFF_8100_0000;
        cpu.msr_fmask = 0x200; // clears IF
        run(&mut cpu, 64, &[0x0F, 0x05]).unwrap(); // syscall
        assert_eq!(cpu.regs.gpr(index::RCX), 0x1002);
        assert_eq!(cpu.regs.gpr(index::R11) & 0x201, 0x201);
        assert_eq!(cpu.regs.rip, 0xFFFF_FFFF_8100_0000);
        assert_eq!(cpu.regs.cs.selector.0, 0x10);
        assert_eq!(cpu.regs.ss.selector.0, 0x18);
        assert!(!cpu.regs.rflags.contains(crate::arch::registers::RFlags::IF));
    }

    #[test]
    fn sysret_restores_user_segments_and_flags() {
        let mut cpu = cpu();
        let cs = crate::arch::segments::Descriptor {
            base: 0,
            limit: 0xFFFFF,
            granularity: true,
            default_32: false,
            long_mode: true,
            present: true,
            dpl: 3,
            system: false,
            descriptor_type: 0b1010,
            code: true,
            conforming: false,
            expand_down: false,
            writable_or_readable: true,
            accessed: false,
        };
        let ds = crate::arch::segments::Descriptor {
            base: 0,
            limit: 0xFFFFF,
            granularity: true,
            default_32: false,
            long_mode: false,
            present: true,
            dpl: 3,
            system: false,
            descriptor_type: 0b0010,
            code: false,
            conforming: false,
            expand_down: false,
            writable_or_readable: true,
            accessed: false,
        };
        cpu.memory
            .write_u64(0x10028, encode_descriptor(&cs))
            .unwrap();
        cpu.memory
            .write_u64(0x10020, encode_descriptor(&ds))
            .unwrap();
        cpu.regs.gdt_base = 0x10000;
        cpu.regs.gdt_limit = 0x2F;
        cpu.regs.set_gpr(index::RCX, 0x0000_0000_4000_1000);
        cpu.regs.set_gpr(index::R11, 0x2 | (1 << 9)); // IF set
        cpu.msr_star = 0x23_u64 << 48;
        run(&mut cpu, 64, &[0x48, 0x0F, 0x07]).unwrap(); // sysretq
        assert_eq!(cpu.regs.rip, 0x0000_0000_4000_1000);
        assert_eq!(cpu.regs.cs.selector.0, 0x33);
        assert_eq!(cpu.regs.ss.selector.0, 0x2B);
        assert!(cpu.regs.rflags.contains(crate::arch::registers::RFlags::IF));
    }

    #[test]
    fn rdmsr_efer_round_trip() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RCX, u64::from(MSR_EFER));
        run(&mut cpu, 64, &[0x0F, 0x32]).unwrap();
        assert_eq!(
            cpu.regs.gpr(index::RAX) & 0xFFFF_FFFF,
            cpu.regs.efer.bits() & 0xFFFF_FFFF
        );
    }

    #[test]
    fn mov_to_cr0_enables_protected_mode() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, Cr0::PE.bits());
        // mov cr0, rax (0F 22 C0)
        run(&mut cpu, 64, &[0x0F, 0x22, 0xC0]).unwrap();
        assert!(cpu.regs.cr0.contains(Cr0::PE));
    }

    #[test]
    fn cli_sti_toggle_interrupt_flag() {
        let mut cpu = cpu();
        run(&mut cpu, 64, &[0xFA]).unwrap();
        assert!(!cpu.regs.rflags.contains(crate::arch::registers::RFlags::IF));
        run(&mut cpu, 64, &[0xFB]).unwrap();
        assert!(cpu.regs.rflags.contains(crate::arch::registers::RFlags::IF));
    }

    #[test]
    fn hlt_halts_the_cpu() {
        let mut cpu = cpu();
        run(&mut cpu, 64, &[0xF4]).unwrap();
        assert!(cpu.halted);
        assert!(matches!(cpu.step(), Err(CpuError::Halted)));
    }
}
