//! Integer arithmetic: add/sub/cmp/adc/sbb/inc/dec/neg/mul/imul/div/idiv.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::ops::{
    AddResult, carry, operand_size, read_operand0, read_operand1, read_register, set_adjust,
    set_carry, set_overflow, set_szp, write_operand0, write_register,
};
use crate::{CpuError, cpu::Cpu};

fn binary(
    cpu: &mut Cpu,
    instruction: &Instruction,
    sub: bool,
    with_carry: bool,
) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    let carry_in = with_carry && carry(&cpu.regs);
    let bits = u32::from(size) * 8;
    let flags: AddResult = if sub {
        crate::ops::sub_with_flags(left, right, carry_in, bits)
    } else {
        crate::ops::add_with_flags(left, right, carry_in, bits)
    };
    set_szp(&mut cpu.regs, flags.result, bits);
    set_carry(&mut cpu.regs, flags.carry);
    set_overflow(&mut cpu.regs, flags.overflow);
    set_adjust(&mut cpu.regs, flags.adjust);
    write_operand0(cpu, instruction, flags.result)
}

pub fn add(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    binary(cpu, instruction, false, false)
}

pub fn sub(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    binary(cpu, instruction, true, false)
}

pub fn cmp(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    // CMP computes flags only; it must not modify the destination operand.
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    let bits = u32::from(size) * 8;
    let flags = crate::ops::sub_with_flags(left, right, false, bits);
    set_szp(&mut cpu.regs, flags.result, bits);
    set_carry(&mut cpu.regs, flags.carry);
    set_overflow(&mut cpu.regs, flags.overflow);
    set_adjust(&mut cpu.regs, flags.adjust);
    Ok(())
}

pub fn adc_sbb(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    binary(
        cpu,
        instruction,
        instruction.mnemonic() == Mnemonic::Sbb,
        true,
    )
}
pub fn inc_dec(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let increment = instruction.mnemonic() == Mnemonic::Inc;
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let bits = u32::from(size) * 8;
    let flags = if increment {
        crate::ops::add_with_flags(left, 1, false, bits)
    } else {
        crate::ops::sub_with_flags(left, 1, false, bits)
    };
    let old_overflow = cpu.regs.rflags.contains(crate::arch::registers::RFlags::OF);
    let _ = old_overflow;
    set_szp(&mut cpu.regs, flags.result, bits);
    set_overflow(&mut cpu.regs, flags.overflow);
    set_adjust(&mut cpu.regs, flags.adjust);
    // INC/DEC do not modify CF.
    write_operand0(cpu, instruction, flags.result)
}

pub fn neg(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let value = read_operand0(cpu, instruction)?;
    let bits = u32::from(size) * 8;
    let result = 0_u64.wrapping_sub(value);
    set_szp(&mut cpu.regs, result, bits);
    set_carry(&mut cpu.regs, value != 0);
    set_overflow(
        &mut cpu.regs,
        result & crate::ops::bits_mask(bits) == crate::ops::sign_bit(bits),
    );
    write_operand0(cpu, instruction, result)
}

pub fn mul_div(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Mul => mul(cpu, instruction, false),
        Mnemonic::Imul if instruction.op_count() == 1 => mul(cpu, instruction, true),
        Mnemonic::Imul => imul_two_three(cpu, instruction),
        Mnemonic::Div => div(cpu, instruction, false),
        Mnemonic::Idiv => div(cpu, instruction, true),
        _ => Ok(()),
    }
}

fn mul(cpu: &mut Cpu, instruction: &Instruction, signed: bool) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let multiplicand = read_operand0(cpu, instruction)?;
    let accumulator = match size {
        1 => read_register(&cpu.regs, Register::AL, 1),
        2 => read_register(&cpu.regs, Register::AX, 2),
        4 => read_register(&cpu.regs, Register::EAX, 4),
        _ => read_register(&cpu.regs, Register::RAX, 8),
    };
    let product: u128 = if signed {
        (sign_extend(accumulator, size) as i128 * sign_extend(multiplicand, size) as i128) as u128
    } else {
        (accumulator as u128) * (multiplicand as u128)
    };
    let bits = u32::from(size) * 8;
    let full_mask = (1_u128 << (bits * 2)) - 1;
    let product = product & full_mask;
    let high = (product >> bits) as u64;
    let low = product as u64;
    cpu.regs
        .rflags
        .set(crate::arch::registers::RFlags::CF, high != 0);
    cpu.regs
        .rflags
        .set(crate::arch::registers::RFlags::OF, high != 0);
    match size {
        1 => {
            write_register(&mut cpu.regs, Register::AX, 2, product as u64);
        }
        2 => {
            write_register(&mut cpu.regs, Register::AX, 2, low);
            write_register(&mut cpu.regs, Register::DX, 2, high);
        }
        4 => {
            write_register(&mut cpu.regs, Register::EAX, 4, low);
            write_register(&mut cpu.regs, Register::EDX, 4, high);
        }
        _ => {
            write_register(&mut cpu.regs, Register::RAX, 8, low);
            write_register(&mut cpu.regs, Register::RDX, 8, high);
        }
    }
    Ok(())
}

fn imul_two_three(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = if instruction.op1_kind() == OpKind::Immediate8
        || instruction.op1_kind() == OpKind::Immediate16
        || instruction.op1_kind() == OpKind::Immediate32
    {
        instruction.immediate(1)
    } else {
        read_operand1(cpu, instruction)?
    };
    let bits = u32::from(size) * 8;
    let mask = crate::ops::bits_mask(bits);
    let product = (sign_extend(left, size) as i128) * (sign_extend(right, size) as i128);
    let truncated = (product as u64) & mask;
    let signed_truncated = sign_extend(truncated, size) as i128;
    set_carry(&mut cpu.regs, signed_truncated != product);
    set_overflow(&mut cpu.regs, signed_truncated != product);
    write_operand0(cpu, instruction, truncated)
}

fn div(cpu: &mut Cpu, instruction: &Instruction, signed: bool) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let divisor = read_operand0(cpu, instruction)?;
    let (dividend, dividend_size) = match size {
        1 => (read_register(&cpu.regs, Register::AX, 2), 1),
        2 => (
            read_register(&cpu.regs, Register::DX, 2) << 16
                | read_register(&cpu.regs, Register::AX, 2),
            2,
        ),
        4 => (
            read_register(&cpu.regs, Register::EDX, 4) << 32
                | read_register(&cpu.regs, Register::EAX, 4),
            4,
        ),
        _ => (
            read_register(&cpu.regs, Register::RDX, 8) << 32
                | (read_register(&cpu.regs, Register::RAX, 8) & 0xFFFF_FFFF),
            8,
        ),
    };
    let dividend_full = if dividend_size == 8 {
        let high = read_register(&cpu.regs, Register::RDX, 8);
        (u128::from(high) << 64) | u128::from(read_register(&cpu.regs, Register::RAX, 8))
    } else {
        u128::from(dividend)
    };
    if divisor == 0 {
        return cpu.raise(0, 0, false);
    }
    let (quotient, remainder) = if signed {
        let dividend = sign_extend128(dividend_full, dividend_size * 16);
        let divisor = sign_extend(divisor, size) as i128;
        let quotient = dividend / divisor;
        let remainder = dividend % divisor;
        let max = (1_i128 << (u32::from(size) * 8 - 1)) - 1;
        let min = -(1_i128 << (u32::from(size) * 8 - 1));
        if quotient > max || quotient < min {
            return cpu.raise(0, 0, false);
        }
        (quotient as u64, remainder as u64)
    } else {
        let quotient = dividend_full / u128::from(divisor);
        let remainder = dividend_full % u128::from(divisor);
        let mask = crate::ops::bits_mask(u32::from(size) * 8);
        if quotient > u128::from(mask) {
            return cpu.raise(0, 0, false);
        }
        (quotient as u64, remainder as u64)
    };
    match size {
        1 => {
            write_register(&mut cpu.regs, Register::AL, 1, quotient);
            write_register(&mut cpu.regs, Register::AH, 1, remainder);
        }
        2 => {
            write_register(&mut cpu.regs, Register::AX, 2, quotient);
            write_register(&mut cpu.regs, Register::DX, 2, remainder);
        }
        4 => {
            write_register(&mut cpu.regs, Register::EAX, 4, quotient);
            write_register(&mut cpu.regs, Register::EDX, 4, remainder);
        }
        _ => {
            write_register(&mut cpu.regs, Register::RAX, 8, quotient);
            write_register(&mut cpu.regs, Register::RDX, 8, remainder);
        }
    }
    Ok(())
}

pub fn convert(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Cbw => {
            let al = read_register(&cpu.regs, Register::AL, 1) as i8;
            write_register(&mut cpu.regs, Register::AX, 2, al as i16 as u64);
        }
        Mnemonic::Cwde => {
            let ax = read_register(&cpu.regs, Register::AX, 2) as i16;
            write_register(&mut cpu.regs, Register::EAX, 4, ax as i32 as u64);
        }
        Mnemonic::Cdqe => {
            let eax = read_register(&cpu.regs, Register::EAX, 4) as i32;
            write_register(&mut cpu.regs, Register::RAX, 8, eax as i64 as u64);
        }
        Mnemonic::Cwd => {
            let ax = read_register(&cpu.regs, Register::AX, 2) as i16;
            write_register(&mut cpu.regs, Register::DX, 2, (ax >> 15) as u16 as u64);
        }
        Mnemonic::Cdq => {
            let eax = read_register(&cpu.regs, Register::EAX, 4) as i32;
            write_register(&mut cpu.regs, Register::EDX, 4, (eax >> 31) as u32 as u64);
        }
        Mnemonic::Cqo => {
            let rax = read_register(&cpu.regs, Register::RAX, 8) as i64;
            write_register(&mut cpu.regs, Register::RDX, 8, (rax >> 63) as u64);
        }
        _ => {}
    }
    Ok(())
}

fn sign_extend(value: u64, size: u8) -> i64 {
    let bits = u32::from(size) * 8;
    ((value << (64 - bits)) as i64) >> (64 - bits)
}

fn sign_extend128(value: u128, bits: u32) -> i128 {
    ((value << (128 - bits)) as i128) >> (128 - bits)
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
    fn add_64_bit_registers() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 10);
        run(&mut cpu, 64, &[0x48, 0x83, 0xC0, 0x05]).unwrap(); // add rax, 5
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), 15);
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn add_carries_and_sets_flags() {
        let mut cpu = cpu();
        cpu.regs
            .set_gpr(crate::arch::registers::index::RAX, u64::MAX);
        run(&mut cpu, 64, &[0x48, 0x83, 0xC0, 0x01]).unwrap(); // add rax, 1
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), 0);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn sub_borrow_sets_carry() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 0);
        run(&mut cpu, 64, &[0x48, 0x83, 0xE8, 0x01]).unwrap(); // sub rax, 1
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), u64::MAX);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn mul_8_bit_writes_ax() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 0x20);
        cpu.regs.set_gpr(crate::arch::registers::index::RBX, 0x10);
        run(&mut cpu, 64, &[0xF6, 0xE3]).unwrap(); // mul bl
        assert_eq!(
            cpu.regs.gpr(crate::arch::registers::index::RAX) & 0xFFFF,
            0x200
        );
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn div_16_bit_unsigned() {
        let mut cpu = cpu();
        cpu.regs
            .set_gpr(crate::arch::registers::index::RAX, 0x0000_0007);
        cpu.regs.set_gpr(crate::arch::registers::index::RDX, 0);
        cpu.regs
            .set_gpr(crate::arch::registers::index::RBX, 0x0000_0002);
        run(&mut cpu, 64, &[0x66, 0xF7, 0xF3]).unwrap(); // div bx
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX) & 0xFFFF, 3);
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RDX) & 0xFFFF, 1);
    }

    #[test]
    fn div_by_zero_raises_divide_error() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 1);
        cpu.regs.set_gpr(crate::arch::registers::index::RBX, 0);
        // div rbx (48 F7 F3) with no IDT installed faults through the IDT
        // limit; the guest fault is the observable signal here.
        let result = run(&mut cpu, 64, &[0x48, 0xF7, 0xF3]);
        assert!(result.is_err());
    }

    #[test]
    fn cmp_does_not_modify_the_destination() {
        let mut cpu = cpu();
        cpu.regs
            .set_gpr(crate::arch::registers::index::RBP, 0x1000000);
        // 48 81 fd 00 00 00 01: cmp rbp, 0x1000000
        run(&mut cpu, 64, &[0x48, 0x81, 0xFD, 0x00, 0x00, 0x00, 0x01]).unwrap();
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RBP), 0x1000000);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
    }
    #[test]
    fn inc_does_not_touch_carry() {
        let mut cpu = cpu();
        cpu.regs.rflags |= RFlags::CF;
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 5);
        run(&mut cpu, 64, &[0x48, 0xFF, 0xC0]).unwrap(); // inc rax
        assert_eq!(cpu.regs.gpr(crate::arch::registers::index::RAX), 6);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn cwd_cdq_cqo_sign_extend() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(crate::arch::registers::index::RAX, 0x8000);
        run(&mut cpu, 64, &[0x66, 0x99]).unwrap(); // cwd
        assert_eq!(
            cpu.regs.gpr(crate::arch::registers::index::RDX) & 0xFFFF,
            0xFFFF
        );
        cpu.regs
            .set_gpr(crate::arch::registers::index::RAX, 0x7FFF_FFFF);
        run(&mut cpu, 64, &[0x99]).unwrap(); // cdq
        assert_eq!(
            cpu.regs.gpr(crate::arch::registers::index::RDX) & 0xFFFF_FFFF,
            0
        );
    }
}
