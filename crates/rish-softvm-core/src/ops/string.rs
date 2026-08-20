//! String operations with REP/REPE/REPNE prefixes.

use iced_x86::{Instruction, Mnemonic, Register};

use crate::arch::registers::{RFlags, index};
use crate::ops::{read_register, write_register};
use crate::{CpuError, cpu::Cpu};

pub fn string_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    let size = crate::ops::memory_size(instruction);
    let direction = if cpu.regs.rflags.contains(RFlags::DF) {
        -(i64::from(size))
    } else {
        i64::from(size)
    };
    let mut count: u64 = 1;
    let rep = instruction.has_rep_prefix();
    let repe = instruction.has_repe_prefix();
    let repne = instruction.has_repne_prefix();
    if rep || repe || repne {
        // The count register is CX in 16-bit mode, ECX in 32-bit, RCX in 64-bit.
        count = match cpu.regs.mode() {
            crate::arch::registers::CpuMode::Real
            | crate::arch::registers::CpuMode::Protected16 => {
                read_register(&cpu.regs, Register::RCX, 8) & 0xFFFF
            }
            crate::arch::registers::CpuMode::Protected32 => {
                read_register(&cpu.regs, Register::RCX, 8) & 0xFFFF_FFFF
            }
            crate::arch::registers::CpuMode::Long => read_register(&cpu.regs, Register::RCX, 8),
        };
    }
    match mnemonic {
        Mnemonic::Movsb | Mnemonic::Movsw | Mnemonic::Movsd | Mnemonic::Movsq => {
            for _ in 0..count {
                let source = cpu.regs.gpr(index::RSI);
                let destination = cpu.regs.gpr(index::RDI);
                let physical_source =
                    cpu.translate(source, crate::arch::paging::AccessKind::Read)?;
                let physical_destination =
                    cpu.translate(destination, crate::arch::paging::AccessKind::Write)?;
                let mut buffer = [0_u8; 8];
                cpu.memory
                    .read(physical_source, &mut buffer[..size as usize])?;
                cpu.memory
                    .write(physical_destination, &buffer[..size as usize])?;
                advance_both(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, size);
            }
        }
        Mnemonic::Stosb | Mnemonic::Stosw | Mnemonic::Stosd | Mnemonic::Stosq => {
            let value = read_accumulator(&cpu.regs, size);
            for _ in 0..count {
                let destination = cpu.regs.gpr(index::RDI);
                let physical =
                    cpu.translate(destination, crate::arch::paging::AccessKind::Write)?;
                match size {
                    1 => cpu.memory.write_u8(physical, value as u8)?,
                    2 => cpu.memory.write_u16(physical, value as u16)?,
                    4 => cpu.memory.write_u32(physical, value as u32)?,
                    _ => cpu.memory.write_u64(physical, value)?,
                }
                advance_rdi(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, size);
            }
        }
        Mnemonic::Lodsb | Mnemonic::Lodsw | Mnemonic::Lodsd | Mnemonic::Lodsq => {
            for _ in 0..count {
                let source = cpu.regs.gpr(index::RSI);
                let physical = cpu.translate(source, crate::arch::paging::AccessKind::Read)?;
                let value = match size {
                    1 => u64::from(cpu.memory.read_u8(physical)?),
                    2 => u64::from(cpu.memory.read_u16(physical)?),
                    4 => u64::from(cpu.memory.read_u32(physical)?),
                    _ => cpu.memory.read_u64(physical)?,
                };
                write_accumulator(&mut cpu.regs, size, value);
                advance_rsi(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, size);
            }
        }
        Mnemonic::Scasb | Mnemonic::Scasw | Mnemonic::Scasd | Mnemonic::Scasq => {
            let accumulator = read_accumulator(&cpu.regs, size);
            for _ in 0..count {
                let address = cpu.regs.gpr(index::RDI);
                let physical = cpu.translate(address, crate::arch::paging::AccessKind::Read)?;
                let value = match size {
                    1 => u64::from(cpu.memory.read_u8(physical)?),
                    2 => u64::from(cpu.memory.read_u16(physical)?),
                    4 => u64::from(cpu.memory.read_u32(physical)?),
                    _ => cpu.memory.read_u64(physical)?,
                };
                let flags =
                    crate::ops::sub_with_flags(accumulator, value, false, u32::from(size) * 8);
                crate::ops::set_szp(&mut cpu.regs, flags.result, u32::from(size) * 8);
                crate::ops::set_carry(&mut cpu.regs, flags.carry);
                crate::ops::set_overflow(&mut cpu.regs, flags.overflow);
                crate::ops::set_adjust(&mut cpu.regs, flags.adjust);
                advance_rdi(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, size);
                if repe && !cpu.regs.rflags.contains(RFlags::ZF) {
                    break;
                }
                if repne && cpu.regs.rflags.contains(RFlags::ZF) {
                    break;
                }
            }
        }
        Mnemonic::Cmpsb | Mnemonic::Cmpsw | Mnemonic::Cmpsd | Mnemonic::Cmpsq => {
            for _ in 0..count {
                let source = cpu.regs.gpr(index::RSI);
                let destination = cpu.regs.gpr(index::RDI);
                let physical_source =
                    cpu.translate(source, crate::arch::paging::AccessKind::Read)?;
                let physical_destination =
                    cpu.translate(destination, crate::arch::paging::AccessKind::Read)?;
                let left = match size {
                    1 => u64::from(cpu.memory.read_u8(physical_source)?),
                    2 => u64::from(cpu.memory.read_u16(physical_source)?),
                    4 => u64::from(cpu.memory.read_u32(physical_source)?),
                    _ => cpu.memory.read_u64(physical_source)?,
                };
                let right = match size {
                    1 => u64::from(cpu.memory.read_u8(physical_destination)?),
                    2 => u64::from(cpu.memory.read_u16(physical_destination)?),
                    4 => u64::from(cpu.memory.read_u32(physical_destination)?),
                    _ => cpu.memory.read_u64(physical_destination)?,
                };
                let flags = crate::ops::sub_with_flags(left, right, false, u32::from(size) * 8);
                crate::ops::set_szp(&mut cpu.regs, flags.result, u32::from(size) * 8);
                crate::ops::set_carry(&mut cpu.regs, flags.carry);
                crate::ops::set_overflow(&mut cpu.regs, flags.overflow);
                crate::ops::set_adjust(&mut cpu.regs, flags.adjust);
                advance_both(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, size);
                if repe && !cpu.regs.rflags.contains(RFlags::ZF) {
                    break;
                }
                if repne && cpu.regs.rflags.contains(RFlags::ZF) {
                    break;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn advance_both(regs: &mut crate::arch::registers::Registers, direction: i64) {
    regs.gpr[index::RSI] = regs.gpr[index::RSI].wrapping_add(direction as u64);
    regs.gpr[index::RDI] = regs.gpr[index::RDI].wrapping_add(direction as u64);
}

fn advance_rsi(regs: &mut crate::arch::registers::Registers, direction: i64) {
    regs.gpr[index::RSI] = regs.gpr[index::RSI].wrapping_add(direction as u64);
}

fn advance_rdi(regs: &mut crate::arch::registers::Registers, direction: i64) {
    regs.gpr[index::RDI] = regs.gpr[index::RDI].wrapping_add(direction as u64);
}

fn decrement_counter(regs: &mut crate::arch::registers::Registers, _size: u8) {
    regs.gpr[index::RCX] = regs.gpr[index::RCX].wrapping_sub(1);
}

fn read_accumulator(regs: &crate::arch::registers::Registers, size: u8) -> u64 {
    match size {
        1 => read_register(regs, Register::AL, 1),
        2 => read_register(regs, Register::AX, 2),
        4 => read_register(regs, Register::EAX, 4),
        _ => read_register(regs, Register::RAX, 8),
    }
}

fn write_accumulator(regs: &mut crate::arch::registers::Registers, size: u8, value: u64) {
    match size {
        1 => write_register(regs, Register::AL, 1, value),
        2 => write_register(regs, Register::AX, 2, value),
        4 => write_register(regs, Register::EAX, 4, value),
        _ => write_register(regs, Register::RAX, 8, value),
    }
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
    fn movsb_copies_and_advances() {
        let mut cpu = cpu();
        cpu.memory.write(0x5000, b"hello").unwrap();
        cpu.regs.set_gpr(index::RSI, 0x5000);
        cpu.regs.set_gpr(index::RDI, 0x6000);
        run(&mut cpu, 64, &[0xA4]).unwrap(); // movsb
        assert_eq!(cpu.memory.read_u8(0x6000).unwrap(), b'h');
        assert_eq!(cpu.regs.gpr(index::RSI), 0x5001);
        assert_eq!(cpu.regs.gpr(index::RDI), 0x6001);
    }

    #[test]
    fn rep_stosq_fills_memory() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xABAB_ABAB_ABAB_ABAB);
        cpu.regs.set_gpr(index::RDI, 0x6000);
        cpu.regs.set_gpr(index::RCX, 4);
        run(&mut cpu, 64, &[0xF3, 0x48, 0xAB]).unwrap(); // rep stosq
        assert_eq!(cpu.memory.read_u64(0x6000).unwrap(), 0xABAB_ABAB_ABAB_ABAB);
        assert_eq!(cpu.memory.read_u64(0x6018).unwrap(), 0xABAB_ABAB_ABAB_ABAB);
        assert_eq!(cpu.regs.gpr(index::RDI), 0x6020);
        assert_eq!(cpu.regs.gpr(index::RCX), 0);
    }

    #[test]
    fn rep_movsb_copies_a_block() {
        let mut cpu = cpu();
        cpu.memory.write(0x5000, b"abcd").unwrap();
        cpu.regs.set_gpr(index::RSI, 0x5000);
        cpu.regs.set_gpr(index::RDI, 0x6000);
        cpu.regs.set_gpr(index::RCX, 4);
        run(&mut cpu, 64, &[0xF3, 0xA4]).unwrap(); // rep movsb
        let mut buffer = [0_u8; 4];
        cpu.memory.read(0x6000, &mut buffer).unwrap();
        assert_eq!(&buffer, b"abcd");
    }

    #[test]
    fn rep_stosd_preserves_rsi() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x1234_5678);
        cpu.regs.set_gpr(index::RDI, 0x6000);
        cpu.regs.set_gpr(index::RSI, 0xABCD);
        cpu.regs.set_gpr(index::RCX, 4);
        run(&mut cpu, 64, &[0xF3, 0xAB]).unwrap(); // rep stosd
        assert_eq!(cpu.regs.gpr(index::RDI), 0x6010);
        assert_eq!(cpu.regs.gpr(index::RSI), 0xABCD);
    }

    #[test]
    fn rep_lodsd_preserves_rdi() {
        let mut cpu = cpu();
        cpu.memory.write_u32(0x5000, 0x42).unwrap();
        cpu.regs.set_gpr(index::RSI, 0x5000);
        cpu.regs.set_gpr(index::RDI, 0xABCD);
        cpu.regs.set_gpr(index::RCX, 2);
        run(&mut cpu, 64, &[0xF3, 0xAD]).unwrap(); // rep lodsd
        assert_eq!(cpu.regs.gpr(index::RSI), 0x5008);
        assert_eq!(cpu.regs.gpr(index::RDI), 0xABCD);
    }

    #[test]
    fn mov_byte_from_sil_stores_sil_not_al() {
        // Regression: SPL/BPL/SIL/DIL were missing from register_index and
        // fell through to RAX, so mov [mem], sil stored AL instead of SIL.
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x6000);
        cpu.regs.set_gpr(index::RSI, 0x98);
        cpu.regs.set_gpr(index::RDX, 0x6006);
        // loop: mov [rax], sil; add rax, 2; mov [rax-1], sil; cmp rax, rdx; jne
        cpu.memory
            .write(
                0x1000,
                &[
                    0x40, 0x88, 0x30, 0x48, 0x83, 0xC0, 0x02, 0x40, 0x88, 0x70, 0xFF, 0x48, 0x39,
                    0xD0, 0x75, 0xF0,
                ],
            )
            .unwrap();
        cpu.regs.rip = 0x1000;
        cpu.regs.efer |= crate::arch::registers::Efer::LMA;
        cpu.regs.cs = crate::arch::segments::SegmentRegister {
            base: 0,
            long_mode: true,
            default_32: false,
            code: true,
            limit: u32::MAX,
            granularity: true,
            writable_or_readable: true,
            ..Default::default()
        };
        // Run 3 loop iterations (15 instructions).
        for _ in 0..15 {
            cpu.step().unwrap();
        }
        let mut out = [0_u8; 6];
        cpu.memory.read(0x6000, &mut out).unwrap();
        assert_eq!(&out, &[0x98, 0x98, 0x98, 0x98, 0x98, 0x98]);
    }
}
