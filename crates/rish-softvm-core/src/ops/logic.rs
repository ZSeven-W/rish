//! Bitwise logic, shifts, rotates, and bit scan/test.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::ops::{
    carry, operand_size, read_operand0, read_operand1, read_register, set_adjust, set_carry,
    set_overflow, set_szp, write_operand0, write_register,
};
use crate::{CpuError, cpu::Cpu};

pub fn and_or_xor(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    let result = match instruction.mnemonic() {
        Mnemonic::Or => left | right,
        Mnemonic::Xor => left ^ right,
        _ => left & right,
    };
    set_szp(&mut cpu.regs, result, u32::from(size) * 8);
    set_carry(&mut cpu.regs, false);
    set_overflow(&mut cpu.regs, false);
    set_adjust(&mut cpu.regs, false);
    write_operand0(cpu, instruction, result)
}

pub fn test(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    let result = left & right;
    set_szp(&mut cpu.regs, result, u32::from(size) * 8);
    set_carry(&mut cpu.regs, false);
    set_overflow(&mut cpu.regs, false);
    set_adjust(&mut cpu.regs, false);
    Ok(())
}

pub fn shift_rotate(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    if mnemonic == Mnemonic::Not {
        let value = read_operand0(cpu, instruction)?;
        return write_operand0(cpu, instruction, !value);
    }
    let size = operand_size(instruction, 0);
    let value = read_operand0(cpu, instruction)?;
    let count = match instruction.op1_kind() {
        OpKind::Immediate8 => instruction.immediate(1) & 0x3F,
        OpKind::Register => {
            let cx = read_register(&cpu.regs, Register::CL, 1);
            if size == 1 { cx & 0x1F } else { cx & 0x3F }
        }
        _ => 1,
    };
    let bits = u32::from(size) * 8;
    let mask = crate::ops::bits_mask(bits);
    let truncated = value & mask;
    let is_left = matches!(mnemonic, Mnemonic::Shl | Mnemonic::Rol | Mnemonic::Rcl);
    let is_rotate = matches!(
        mnemonic,
        Mnemonic::Rol | Mnemonic::Ror | Mnemonic::Rcl | Mnemonic::Rcr
    );
    let is_arithmetic = mnemonic == Mnemonic::Sar;
    let result;
    let carry_out;
    let overflow_out;
    if count == 0 {
        result = truncated;
        carry_out = carry(&cpu.regs);
        overflow_out = false;
    } else if is_rotate {
        let count = count % u64::from(bits);
        if count == 0 {
            result = truncated;
            carry_out = carry(&cpu.regs);
            overflow_out = false;
        } else if is_left {
            result = ((truncated << count) | (truncated >> (bits - count as u32))) & mask;
            carry_out = result & 1 != 0;
            overflow_out = count == 1 && ((carry_out as u64 ^ (result >> (bits - 1))) & 1) != 0;
        } else {
            result = ((truncated >> count) | (truncated << (bits - count as u32))) & mask;
            carry_out = result & (1_u64 << (bits - 1)) != 0;
            overflow_out = count == 1 && ((carry_out as u64 ^ (result >> (bits - 1))) & 1) != 0;
        }
    } else if is_left {
        if count >= u64::from(bits) {
            result = 0;
            carry_out = count == u64::from(bits) && truncated & 1 != 0;
        } else {
            result = truncated.wrapping_shl(count as u32) & mask;
            carry_out = truncated & (1_u64 << (bits - count as u32)) != 0;
        }
        overflow_out = if count == 1 {
            ((result >> (bits - 1)) & 1) != (carry_out as u64)
        } else {
            false
        };
    } else if is_arithmetic {
        let sign = truncated & (1_u64 << (bits - 1)) != 0;
        let sign_bits = if count >= u64::from(bits) {
            if sign { mask } else { 0 }
        } else if sign {
            (u64::MAX << (bits - count as u32)) & mask
        } else {
            0
        };
        result = ((truncated >> count) | sign_bits) & mask;
        carry_out = count <= u64::from(bits) && truncated & (1_u64 << (count - 1)) != 0;
        overflow_out = false;
    } else {
        if count >= u64::from(bits) {
            result = 0;
            carry_out = false;
        } else {
            result = (truncated >> count) & mask;
            carry_out = truncated & (1_u64 << (count - 1)) != 0;
        }
        overflow_out = if count == 1 {
            truncated & (1_u64 << (bits - 1)) != 0
        } else {
            false
        };
    }
    set_carry(&mut cpu.regs, carry_out);
    set_overflow(&mut cpu.regs, overflow_out);
    set_szp(&mut cpu.regs, result, bits);
    write_operand0(cpu, instruction, result)
}

pub fn bit_scan_test(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    let size = operand_size(instruction, 0);
    // BSF/BSR take the searched value in operand 1; BTS/BTR/BTC operate
    // on the destination (operand 0).
    let source = if matches!(mnemonic, Mnemonic::Bsf | Mnemonic::Bsr) {
        read_operand1(cpu, instruction)?
    } else {
        read_operand0(cpu, instruction)?
    };
    match mnemonic {
        Mnemonic::Bsf => {
            let index = source.trailing_zeros();
            cpu.regs
                .rflags
                .set(crate::arch::registers::RFlags::ZF, source == 0);
            write_register(
                &mut cpu.regs,
                instruction.op0_register(),
                size,
                u64::from(index),
            );
        }
        Mnemonic::Bsr => {
            let index = if source == 0 {
                0
            } else {
                63 - source.leading_zeros()
            };
            cpu.regs
                .rflags
                .set(crate::arch::registers::RFlags::ZF, source == 0);
            write_register(
                &mut cpu.regs,
                instruction.op0_register(),
                size,
                u64::from(index),
            );
        }
        _ => {
            let bit = match instruction.op1_kind() {
                OpKind::Immediate8 => instruction.immediate(1),
                OpKind::Register => read_operand1(cpu, instruction)?,
                _ => 0,
            };
            let bit_index = if instruction.op0_kind() == OpKind::Memory {
                bit % 64
            } else {
                bit % (u64::from(size) * 8)
            };
            let present = source & (1_u64 << bit_index) != 0;
            set_carry(&mut cpu.regs, present);
            match mnemonic {
                Mnemonic::Bts => {
                    write_operand0(cpu, instruction, source | (1_u64 << bit_index))?;
                }
                Mnemonic::Btr => {
                    write_operand0(cpu, instruction, source & !(1_u64 << bit_index))?;
                }
                Mnemonic::Btc => {
                    write_operand0(cpu, instruction, source ^ (1_u64 << bit_index))?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::registers::RFlags;

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
    fn and_sets_flags_and_clears_carry() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 0xFF);
        cpu.regs.rflags |= RFlags::CF;
        run(&mut cpu, 64, &[0x48, 0x83, 0xE0, 0x0F]).unwrap(); // and rax, 0xF
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), 0x0F);
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn xor_register_with_self_zeroes() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 42);
        run(&mut cpu, 64, &[0x48, 0x31, 0xC0]).unwrap(); // xor rax, rax
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), 0);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn shl_shifts_in_zero_and_sets_carry() {
        let mut cpu = cpu();
        cpu.regs
            .set_gpr(crate::arch::registers::index::RAX, 0x8000_0000_0000_0000);
        run(&mut cpu, 64, &[0x48, 0xD1, 0xE0]).unwrap(); // shl rax, 1
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), 0);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        assert!(cpu.regs.rflags.contains(RFlags::OF));
    }

    #[test]
    fn sar_preserves_sign() {
        let mut cpu = cpu();
        cpu.regs
            .set_gpr(crate::arch::registers::index::RAX, 0x8000_0000_0000_0000);
        run(&mut cpu, 64, &[0x48, 0xD1, 0xF8]).unwrap(); // sar rax, 1
        assert_eq!(
            cpu.regs.gpr(crate::arch::registers::index::RAX),
            0xC000_0000_0000_0000
        );
    }

    #[test]
    fn test_instruction_only_sets_flags() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 0xF0);
        run(&mut cpu, 64, &[0x48, 0xA9, 0x0F, 0x00, 0x00, 0x00]).unwrap(); // test rax, 0xF
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), 0xF0);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn bsf_finds_lowest_set_bit() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 0x40);
        run(&mut cpu, 64, &[0x48, 0x0F, 0xBC, 0xD8]).unwrap(); // bsf rbx, rax
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RBX), 6);
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn bts_imm8_sets_bit_58() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RSI, 0x35E_4063);
        // bts rsi, 0x3a (48 0f ba ee 3a)
        run(&mut cpu, 64, &[0x48, 0x0F, 0xBA, 0xEE, 0x3A]).unwrap();
        assert_eq!(
            cpu.regs.gpr(crate::arch::registers::index::RSI),
            0x35E_4063 | (1_u64 << 58)
        );
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
    }
}