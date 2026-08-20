//! Stack operations: push/pop, pushf/popf, enter/leave.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::arch::registers::RFlags;
use crate::ops::{operand_size, read_register, write_register};
use crate::{CpuError, cpu::Cpu};

pub fn push_pop(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Pushf | Mnemonic::Pushfq | Mnemonic::Pushfd => {
            let value = cpu.regs.rflags.bits();
            match cpu.regs.mode() {
                crate::arch::registers::CpuMode::Long => cpu.push64(value)?,
                crate::arch::registers::CpuMode::Protected32 => cpu.push32(value as u32)?,
                _ => cpu.push16(value as u16)?,
            }
            return Ok(());
        }
        Mnemonic::Popf | Mnemonic::Popfq | Mnemonic::Popfd => {
            let value = cpu.pop_native()?;
            let bits = match cpu.regs.mode() {
                crate::arch::registers::CpuMode::Long => value,
                crate::arch::registers::CpuMode::Protected32 => value & 0xFFFF_FFFF,
                _ => value & 0xFFFF,
            };
            let keep = cpu.regs.rflags & RFlags::TF;
            cpu.regs.rflags = RFlags::from_bits_truncate(bits) | keep;
            return Ok(());
        }
        Mnemonic::Enter => {
            return enter(cpu, instruction);
        }
        Mnemonic::Leave => {
            let frame = cpu.regs.rbp();
            cpu.regs.gpr[crate::arch::registers::index::RSP] = frame;
            let value = cpu.pop_native()?;
            cpu.regs.gpr[crate::arch::registers::index::RBP] = value;
            return Ok(());
        }
        Mnemonic::Pop
            if matches!(
                instruction.op0_register(),
                Register::ES
                    | Register::CS
                    | Register::SS
                    | Register::DS
                    | Register::FS
                    | Register::GS
            ) =>
        {
            let selector = cpu.pop_native()? as u16;
            let segment = crate::arch::segments::SegmentSelector(selector);
            let loaded = match cpu.regs.mode() {
                crate::arch::registers::CpuMode::Real
                | crate::arch::registers::CpuMode::Protected16 => {
                    crate::arch::segments::SegmentRegister::real_mode(segment)
                }
                _ => cpu.load_segment_from_table(segment)?,
            };
            match instruction.op0_register() {
                Register::ES => cpu.regs.es = loaded,
                Register::CS => cpu.regs.cs = loaded,
                Register::SS => cpu.regs.ss = loaded,
                Register::DS => cpu.regs.ds = loaded,
                Register::FS => cpu.regs.fs = loaded,
                Register::GS => cpu.regs.gs = loaded,
                _ => unreachable!("matched pop segment codes"),
            }
            return Ok(());
        }
        Mnemonic::Push
            if matches!(
                instruction.op0_register(),
                Register::ES
                    | Register::CS
                    | Register::SS
                    | Register::DS
                    | Register::FS
                    | Register::GS
            ) =>
        {
            let selector = match instruction.op0_register() {
                Register::ES => cpu.regs.es.selector,
                Register::CS => cpu.regs.cs.selector,
                Register::SS => cpu.regs.ss.selector,
                Register::DS => cpu.regs.ds.selector,
                Register::FS => cpu.regs.fs.selector,
                Register::GS => cpu.regs.gs.selector,
                _ => unreachable!("matched push segment codes"),
            };
            cpu.push_native(u64::from(selector.0))?;
            return Ok(());
        }
        Mnemonic::Push => {
            let value = match instruction.op0_kind() {
                OpKind::Immediate8 | OpKind::Immediate8to64 => {
                    (instruction.immediate8() as i8 as i64) as u64
                }
                OpKind::Immediate8to16 => (instruction.immediate8to16() as i64) as u64,
                OpKind::Immediate8to32 => (instruction.immediate8to32() as i64) as u64,
                OpKind::Memory => {
                    cpu.read_operand(instruction, 0, crate::ops::memory_size(instruction))?
                }
                _ => read_register(
                    &cpu.regs,
                    instruction.op0_register(),
                    operand_size(instruction, 0),
                ),
            };
            cpu.push_native(value)?;
            return Ok(());
        }
        Mnemonic::Pop => {
            let value = cpu.pop_native()?;
            if instruction.op0_kind() == OpKind::Memory {
                cpu.write_operand(instruction, 0, crate::ops::memory_size(instruction), value)?;
            } else {
                write_register(
                    &mut cpu.regs,
                    instruction.op0_register(),
                    operand_size(instruction, 0),
                    value,
                );
            }
            return Ok(());
        }
        _ => {}
    }
    Ok(())
}

fn enter(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let frame_size = instruction.immediate(0) as u16;
    let nesting = instruction.immediate(1) as u8 & 0x1F;
    let frame = cpu.regs.rbp();
    cpu.push_native(frame)?;
    let frame_pointer = cpu.regs.rsp();
    if nesting != 0 {
        for level in 1..nesting {
            let linear = match cpu.regs.mode() {
                crate::arch::registers::CpuMode::Long => frame.wrapping_sub(u64::from(level) * 8),
                crate::arch::registers::CpuMode::Protected32 => {
                    frame.wrapping_sub(u64::from(level) * 4)
                }
                _ => frame.wrapping_sub(u64::from(level) * 2),
            };
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            let value = cpu.memory.read_u64(physical)?;
            cpu.push_native(value)?;
        }
        cpu.push_native(frame_pointer)?;
    }
    cpu.regs.gpr[crate::arch::registers::index::RBP] = frame_pointer;
    cpu.regs.gpr[crate::arch::registers::index::RSP] = cpu
        .regs
        .gpr(crate::arch::registers::index::RSP)
        .wrapping_sub(u64::from(frame_size));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu() -> Cpu {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.set_rsp(0x8000);
        cpu
    }

    fn decode(bitness: u32, bytes: &[u8], ip: u64) -> Instruction {
        let mut decoder =
            iced_x86::Decoder::with_ip(bitness, bytes, ip, iced_x86::DecoderOptions::NONE);
        decoder.decode()
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
    fn push_pop_round_trip() {
        let mut cpu = cpu();
        cpu.regs
            .set_gpr(crate::arch::registers::index::RAX, 0x1234_5678_9ABC_DEF0);
        run(&mut cpu, 64, &[0x50]).unwrap(); // push rax
        assert_eq!(cpu.regs.rsp(), 0x7FF8);
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 0);
        run(&mut cpu, 64, &[0x58]).unwrap(); // pop rax
        assert_eq!(cpu.regs.rsp(), 0x8000);
        assert_eq!(
            cpu.regs.gpr(crate::arch::registers::index::RAX),
            0x1234_5678_9ABC_DEF0
        );
    }

    #[test]
    fn push_imm8_sign_extends_to_64() {
        let mut cpu = cpu();
        run(&mut cpu, 64, &[0x6A, 0x81]).unwrap(); // push -127
        let value = cpu.pop_native().unwrap();
        assert_eq!(value, 0xFFFF_FFFF_FFFF_FF81);
    }

    #[test]
    fn pushfq_pop_fq_round_trip() {
        let mut cpu = cpu();
        cpu.regs.rflags = RFlags::CF | RFlags::ZF | RFlags::IF;
        run(&mut cpu, 64, &[0x9C]).unwrap(); // pushfq
        cpu.regs.rflags = RFlags::empty();
        run(&mut cpu, 64, &[0x9D]).unwrap(); // popfq
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        assert!(cpu.regs.rflags.contains(RFlags::IF));
    }

    #[test]
    fn leave_restores_frame_and_stack() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RBP, 0x7000);
        run(&mut cpu, 64, &[0xC9]).unwrap(); // leave
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RSP), 0x7008);
    }
}
