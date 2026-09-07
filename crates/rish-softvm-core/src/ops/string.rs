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
                let mut buffer = [0_u8; 8];
                cpu.read_linear_bytes(source, &mut buffer[..size as usize])?;
                cpu.write_linear_bytes(destination, &buffer[..size as usize])?;
                advance_both(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, rep || repe || repne);
            }
        }
        Mnemonic::Stosb | Mnemonic::Stosw | Mnemonic::Stosd | Mnemonic::Stosq => {
            let value = read_accumulator(&cpu.regs, size);
            for _ in 0..count {
                let destination = cpu.regs.gpr(index::RDI);
                cpu.write_linear_bytes(destination, &value.to_le_bytes()[..size as usize])?;
                advance_rdi(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, rep || repe || repne);
            }
        }
        Mnemonic::Lodsb | Mnemonic::Lodsw | Mnemonic::Lodsd | Mnemonic::Lodsq => {
            for _ in 0..count {
                let source = cpu.regs.gpr(index::RSI);
                let value = read_value(cpu, source, size)?;
                write_accumulator(&mut cpu.regs, size, value);
                advance_rsi(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, rep || repe || repne);
            }
        }
        Mnemonic::Scasb | Mnemonic::Scasw | Mnemonic::Scasd | Mnemonic::Scasq => {
            let accumulator = read_accumulator(&cpu.regs, size);
            for _ in 0..count {
                let address = cpu.regs.gpr(index::RDI);
                let value = read_value(cpu, address, size)?;
                let flags =
                    crate::ops::sub_with_flags(accumulator, value, false, u32::from(size) * 8);
                crate::ops::set_szp(&mut cpu.regs, flags.result, u32::from(size) * 8);
                crate::ops::set_carry(&mut cpu.regs, flags.carry);
                crate::ops::set_overflow(&mut cpu.regs, flags.overflow);
                crate::ops::set_adjust(&mut cpu.regs, flags.adjust);
                advance_rdi(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, rep || repe || repne);
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
                let left = read_value(cpu, source, size)?;
                let right = read_value(cpu, destination, size)?;
                let flags = crate::ops::sub_with_flags(left, right, false, u32::from(size) * 8);
                crate::ops::set_szp(&mut cpu.regs, flags.result, u32::from(size) * 8);
                crate::ops::set_carry(&mut cpu.regs, flags.carry);
                crate::ops::set_overflow(&mut cpu.regs, flags.overflow);
                crate::ops::set_adjust(&mut cpu.regs, flags.adjust);
                advance_both(&mut cpu.regs, direction);
                decrement_counter(&mut cpu.regs, rep || repe || repne);
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

fn read_value(cpu: &mut Cpu, address: u64, size: u8) -> Result<u64, CpuError> {
    let mut bytes = [0; 8];
    cpu.read_linear_bytes(address, &mut bytes[..size as usize])?;
    Ok(u64::from_le_bytes(bytes))
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

fn decrement_counter(regs: &mut crate::arch::registers::Registers, repeated: bool) {
    if repeated {
        regs.gpr[index::RCX] = regs.gpr[index::RCX].wrapping_sub(1);
    }
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
    fn movsq_stitches_nonadjacent_physical_pages() {
        let mut cpu = cpu();
        cpu.regs.cr0 |= crate::arch::registers::Cr0::PG | crate::arch::registers::Cr0::PE;
        cpu.regs.cr4 |= crate::arch::registers::Cr4::PAE;
        cpu.regs.cr3 = 0x10000;
        for (address, value) in [
            (0x10000, 0x11003),
            (0x11000, 0x12003),
            (0x12000, 0x13003),
            (0x13008, 0x1003),
            (0x13200, 0x50003),
            (0x13208, 0x60003),
            (0x13210, 0x70003),
            (0x13218, 0x80003),
        ] {
            cpu.memory.write_u64(address, value).unwrap();
        }
        cpu.memory.write(0x50ffc, &[1, 2, 3, 4]).unwrap();
        cpu.memory.write(0x60000, &[5, 6, 7, 8]).unwrap();
        cpu.regs.gpr[index::RSI] = 0x40ffc;
        cpu.regs.gpr[index::RDI] = 0x42ffc;
        cpu.regs.gpr[index::RCX] = 1;
        run(&mut cpu, 64, &[0xf3, 0x48, 0xa5]).unwrap();
        let mut low = [0; 4];
        let mut high = [0; 4];
        cpu.memory.read(0x70ffc, &mut low).unwrap();
        cpu.memory.read(0x80000, &mut high).unwrap();
        assert_eq!(low, [1, 2, 3, 4]);
        assert_eq!(high, [5, 6, 7, 8]);
        cpu.regs.gpr[index::RSI] = 0x40ffc;
        run(&mut cpu, 64, &[0x48, 0xad]).unwrap(); // lodsq
        assert_eq!(cpu.regs.gpr[index::RAX], 0x0807_0605_0403_0201);
        cpu.regs.gpr[index::RDI] = 0x42ffc;
        run(&mut cpu, 64, &[0x48, 0xaf]).unwrap(); // scasq
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        cpu.regs.gpr[index::RSI] = 0x40ffc;
        cpu.regs.gpr[index::RDI] = 0x42ffc;
        run(&mut cpu, 64, &[0x48, 0xa7]).unwrap(); // cmpsq
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        cpu.regs.gpr[index::RAX] = 0x8877_6655_4433_2211;
        cpu.regs.gpr[index::RDI] = 0x42ffc;
        run(&mut cpu, 64, &[0x48, 0xab]).unwrap(); // stosq
        cpu.memory.read(0x70ffc, &mut low).unwrap();
        cpu.memory.read(0x80000, &mut high).unwrap();
        assert_eq!(low, [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(high, [0x55, 0x66, 0x77, 0x88]);
    }

    #[test]
    fn unprefixed_string_operations_preserve_count_register() {
        for opcode in [0xa4, 0xaa, 0xac, 0xae, 0xa6] {
            let mut cpu = cpu();
            cpu.regs.gpr[index::RSI] = 0x5000;
            cpu.regs.gpr[index::RDI] = 0x6000;
            cpu.regs.gpr[index::RCX] = 0x1234_5678_9abc_def0;
            run(&mut cpu, 64, &[opcode]).unwrap();
            assert_eq!(cpu.regs.gpr[index::RCX], 0x1234_5678_9abc_def0);
        }
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
