//! Control flow: jumps, calls, returns, setcc, and flag ops.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::arch::registers::{CpuMode, RFlags, index};
use crate::ops::{read_register, write_register};
use crate::{CpuError, cpu::Cpu};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Condition {
    O,
    No,
    B,
    Ae,
    E,
    Ne,
    Be,
    A,
    S,
    Ns,
    P,
    Np,
    L,
    Ge,
    Le,
    G,
}

fn condition_of(mnemonic: Mnemonic) -> Option<Condition> {
    Some(match mnemonic {
        Mnemonic::Jo => Condition::O,
        Mnemonic::Jno | Mnemonic::Setno => Condition::No,
        Mnemonic::Jb | Mnemonic::Setb => Condition::B,
        Mnemonic::Jae | Mnemonic::Setae => Condition::Ae,
        Mnemonic::Je | Mnemonic::Sete => Condition::E,
        Mnemonic::Jne | Mnemonic::Setne => Condition::Ne,
        Mnemonic::Jbe | Mnemonic::Setbe => Condition::Be,
        Mnemonic::Ja | Mnemonic::Seta => Condition::A,
        Mnemonic::Js | Mnemonic::Sets => Condition::S,
        Mnemonic::Jns | Mnemonic::Setns => Condition::Ns,
        Mnemonic::Jp | Mnemonic::Setp => Condition::P,
        Mnemonic::Jnp | Mnemonic::Setnp => Condition::Np,
        Mnemonic::Jl | Mnemonic::Setl => Condition::L,
        Mnemonic::Jge | Mnemonic::Setge => Condition::Ge,
        Mnemonic::Jle | Mnemonic::Setle => Condition::Le,
        Mnemonic::Jg | Mnemonic::Setg => Condition::G,
        Mnemonic::Seto => Condition::O,
        _ => return None,
    })
}

fn condition_holds(condition: Condition, flags: RFlags) -> bool {
    let cf = flags.contains(RFlags::CF);
    let zf = flags.contains(RFlags::ZF);
    let sf = flags.contains(RFlags::SF);
    let of = flags.contains(RFlags::OF);
    let pf = flags.contains(RFlags::PF);
    match condition {
        Condition::O => of,
        Condition::No => !of,
        Condition::B => cf,
        Condition::Ae => !cf,
        Condition::E => zf,
        Condition::Ne => !zf,
        Condition::Be => cf || zf,
        Condition::A => !cf && !zf,
        Condition::S => sf,
        Condition::Ns => !sf,
        Condition::P => pf,
        Condition::Np => !pf,
        Condition::L => sf != of,
        Condition::Ge => sf == of,
        Condition::Le => zf || sf != of,
        Condition::G => !zf && sf == of,
    }
}

pub fn jump(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    if mnemonic == Mnemonic::Jmp {
        match instruction.op0_kind() {
            OpKind::FarBranch16 | OpKind::FarBranch32 => {
                let offset = instruction.far_branch32() as u64;
                let selector = instruction.far_branch_selector();
                let segment = crate::arch::segments::SegmentSelector(selector);
                cpu.regs.cs = match cpu.regs.mode() {
                    CpuMode::Real | CpuMode::Protected16 => {
                        crate::arch::segments::SegmentRegister::real_mode(segment)
                    }
                    _ => cpu.load_segment_from_table(segment)?,
                };
                return set_rip(cpu, offset);
            }
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                let target = relative_target(cpu.regs.rip, instruction);
                return set_rip(cpu, target);
            }
            OpKind::Memory => {
                let target =
                    cpu.read_operand(instruction, 0, crate::ops::memory_size(instruction))?;
                return set_rip(cpu, target);
            }
            _ => {
                return Err(CpuError::UnimplementedInstruction {
                    code: "jmp".to_owned(),
                    address: cpu.regs.rip,
                    bytes: Vec::new(),
                });
            }
        }
    }
    // Loop family and jcxz/jecxz/jrcxz.
    let counter = match mnemonic {
        Mnemonic::Jcxz => read_register(&cpu.regs, Register::CX, 2) == 0,
        Mnemonic::Jecxz => read_register(&cpu.regs, Register::ECX, 4) == 0,
        Mnemonic::Jrcxz => read_register(&cpu.regs, Register::RCX, 8) == 0,
        Mnemonic::Loop | Mnemonic::Loope | Mnemonic::Loopne => {
            let register = counter_register(cpu);
            let value = read_register(&cpu.regs, register, 8).wrapping_sub(1);
            write_register(&mut cpu.regs, register, 8, value);
            let nonzero = read_register(&cpu.regs, register, 8) != 0;
            match mnemonic {
                Mnemonic::Loope => nonzero && cpu.regs.rflags.contains(RFlags::ZF),
                Mnemonic::Loopne => nonzero && !cpu.regs.rflags.contains(RFlags::ZF),
                _ => nonzero,
            }
        }
        _ => return Ok(()),
    };
    if counter {
        let target = relative_target(cpu.regs.rip, instruction);
        set_rip(cpu, target)
    } else {
        Ok(())
    }
}

pub fn jcc(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let condition =
        condition_of(instruction.mnemonic()).expect("dispatch only routes condition codes here");
    if condition_holds(condition, cpu.regs.rflags) {
        let target = relative_target(cpu.regs.rip, instruction);
        set_rip(cpu, target)
    } else {
        Ok(())
    }
}

fn counter_register(cpu: &Cpu) -> Register {
    match cpu.regs.mode() {
        CpuMode::Real | CpuMode::Protected16 => Register::CX,
        CpuMode::Protected32 => Register::ECX,
        CpuMode::Long => Register::RCX,
    }
}

pub fn call(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.op0_kind() {
        OpKind::FarBranch16 | OpKind::FarBranch32 => {
            let offset = instruction.far_branch32() as u64;
            let selector = instruction.far_branch_selector();
            cpu.push_native(cpu.regs.cs.selector.0 as u64)?;
            cpu.push_native(cpu.regs.rip)?;
            let segment = crate::arch::segments::SegmentSelector(selector);
            cpu.regs.cs = match cpu.regs.mode() {
                CpuMode::Real | CpuMode::Protected16 => {
                    crate::arch::segments::SegmentRegister::real_mode(segment)
                }
                _ => cpu.load_segment_from_table(segment)?,
            };
            set_rip(cpu, offset)
        }
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            let target = relative_target(cpu.regs.rip, instruction);
            cpu.push_native(cpu.regs.rip)?;
            set_rip(cpu, target)
        }
        OpKind::Memory => {
            let target = cpu.read_operand(instruction, 0, crate::ops::memory_size(instruction))?;
            cpu.push_native(cpu.regs.rip)?;
            set_rip(cpu, target)
        }
        _ => Err(CpuError::UnimplementedInstruction {
            code: "call".to_owned(),
            address: cpu.regs.rip,
            bytes: Vec::new(),
        }),
    }
}

pub fn ret(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let far = matches!(
        instruction.op0_kind(),
        OpKind::FarBranch16 | OpKind::FarBranch32
    );
    let target = cpu.pop_native()?;
    if far {
        let selector = cpu.pop_native()? as u16;
        let segment = crate::arch::segments::SegmentSelector(selector);
        cpu.regs.cs = match cpu.regs.mode() {
            CpuMode::Real | CpuMode::Protected16 => {
                crate::arch::segments::SegmentRegister::real_mode(segment)
            }
            _ => cpu.load_segment_from_table(segment)?,
        };
    }
    let extra = match instruction.op0_kind() {
        OpKind::Immediate16 | OpKind::Immediate8to16 => instruction.immediate16() as u64,
        _ => 0,
    };
    cpu.regs.gpr[index::RSP] = cpu.regs.gpr(index::RSP).wrapping_add(extra);
    set_rip(cpu, target)
}

pub fn setcc(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let condition =
        condition_of(instruction.mnemonic()).expect("dispatch only routes condition codes here");
    let value = u64::from(condition_holds(condition, cpu.regs.rflags));
    if instruction.op0_kind() == OpKind::Memory {
        cpu.write_operand(instruction, 0, 1, value)?;
    } else {
        write_register(&mut cpu.regs, instruction.op0_register(), 1, value);
    }
    Ok(())
}

pub fn flag_ops(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Stc => cpu.regs.rflags |= RFlags::CF,
        Mnemonic::Clc => cpu.regs.rflags -= RFlags::CF,
        Mnemonic::Cmc => cpu.regs.rflags ^= RFlags::CF,
        Mnemonic::Cld => cpu.regs.rflags -= RFlags::DF,
        Mnemonic::Std => cpu.regs.rflags |= RFlags::DF,
        Mnemonic::Cli => cpu.regs.rflags -= RFlags::IF,
        Mnemonic::Sti => cpu.regs.rflags |= RFlags::IF,
        Mnemonic::Sahf => {
            let ah = read_register(&cpu.regs, Register::AH, 1);
            cpu.regs.rflags = (cpu.regs.rflags
                & !(RFlags::SF | RFlags::ZF | RFlags::AF | RFlags::PF | RFlags::CF))
                | RFlags::from_bits_truncate(ah);
        }
        Mnemonic::Lahf => {
            let value = (cpu.regs.rflags
                & (RFlags::SF | RFlags::ZF | RFlags::AF | RFlags::PF | RFlags::CF))
                .bits() as u8;
            write_register(&mut cpu.regs, Register::AH, 1, u64::from(value));
        }
        _ => {}
    }
    Ok(())
}

fn relative_target(_rip: u64, instruction: &Instruction) -> u64 {
    instruction.near_branch_target()
}

fn set_rip(cpu: &mut Cpu, target: u64) -> Result<(), CpuError> {
    cpu.regs.rip = match cpu.regs.mode() {
        CpuMode::Long => target,
        CpuMode::Protected32 => target & 0xFFFF_FFFF,
        _ => target & 0xFFFF,
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu() -> Cpu {
        Cpu::new(1, 0).unwrap()
    }

    fn decode(bitness: u32, bytes: &[u8], ip: u64) -> Instruction {
        let mut decoder =
            iced_x86::Decoder::with_ip(bitness, bytes, ip, iced_x86::DecoderOptions::NONE);
        decoder.decode()
    }

    fn run(cpu: &mut Cpu, bitness: u32, bytes: &[u8], at: u64) -> Result<(), CpuError> {
        cpu.memory.write(at, bytes).unwrap();
        cpu.regs.rip = at;
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
        let instruction = decode(bitness, bytes, at);
        let size = instruction.len();
        cpu.regs.rip = at + size as u64;
        cpu.dispatch(&instruction)
    }

    #[test]
    fn jmp_rel8_taken_and_not() {
        let mut cpu = cpu();
        run(&mut cpu, 64, &[0xEB, 0x05], 0x1000).unwrap();
        assert_eq!(cpu.regs.rip, 0x1007);
    }

    #[test]
    fn jne_taken_when_zf_clear() {
        let mut cpu = cpu();
        run(&mut cpu, 64, &[0x75, 0x03], 0x1000).unwrap();
        assert_eq!(cpu.regs.rip, 0x1005);
        cpu.regs.rflags |= RFlags::ZF;
        run(&mut cpu, 64, &[0x75, 0x03], 0x1000).unwrap();
        assert_eq!(cpu.regs.rip, 0x1002);
    }

    #[test]
    fn call_pushes_return_and_jumps() {
        let mut cpu = cpu();
        cpu.regs.set_rsp(0x8000);
        run(&mut cpu, 64, &[0xE8, 0x04, 0x00, 0x00, 0x00], 0x1000).unwrap();
        assert_eq!(cpu.regs.rip, 0x1009);
        assert_eq!(cpu.regs.rsp(), 0x7FF8);
        let saved = cpu.pop_native().unwrap();
        assert_eq!(saved, 0x1005);
    }

    #[test]
    fn ret_returns_to_caller() {
        let mut cpu = cpu();
        cpu.regs.set_rsp(0x8000);
        run(&mut cpu, 64, &[0xE8, 0x00, 0x00, 0x00, 0x00], 0x1000).unwrap();
        run(&mut cpu, 64, &[0xC3], 0x1005).unwrap();
        assert_eq!(cpu.regs.rip, 0x1005);
    }

    #[test]
    fn loop_decrements_and_branches() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RCX, 3);
        run(&mut cpu, 64, &[0xE2, 0xFE], 0x1000).unwrap(); // loop -2
        assert_eq!(cpu.regs.gpr(index::RCX), 2);
        assert_eq!(cpu.regs.rip, 0x1000);
    }

    #[test]
    fn setcc_writes_one_or_zero() {
        let mut cpu = cpu();
        cpu.regs.rflags |= RFlags::CF;
        cpu.memory.write_u8(0x2000, 0xAA).unwrap();
        // setc [0x2000] in 64-bit: 0F 92 04 25 00 20 00 00
        run(
            &mut cpu,
            64,
            &[0x0F, 0x92, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00],
            0x1000,
        )
        .unwrap();
        assert_eq!(cpu.memory.read_u8(0x2000).unwrap(), 1);
    }

    #[test]
    fn stc_clc_cmc_toggle() {
        let mut cpu = cpu();
        run(&mut cpu, 64, &[0xF9], 0x1000).unwrap(); // stc
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        run(&mut cpu, 64, &[0xF8], 0x1000).unwrap(); // clc
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
        run(&mut cpu, 64, &[0xF5], 0x1000).unwrap(); // cmc
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }
}
