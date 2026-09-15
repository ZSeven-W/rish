//! Legacy SSE conversions. Integer widths, floating precision, and the
//! number of converted lanes follow each instruction's operand contract.

use super::{read_scalar_src, read_xmm, write_xmm};
use crate::arch::registers::Cr4;
use crate::ops::{operand_size, read_register, write_register};
use crate::{Cpu, CpuError};
use iced_x86::{Instruction, Mnemonic, OpKind};

const INVALID: u32 = 1;
const DENORMAL: u32 = 1 << 1;
const PRECISION: u32 = 1 << 5;

/// Legacy CVTDQ2PD converts only the low two signed i32 lanes (m64 for
/// memory). Every i32 is exactly representable as f64, without exceptions.
pub(super) fn cvtdq2pd(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let raw = read_scalar_src(cpu, instruction, true)?;
    let (low, _) = integer_to_float(i64::from(raw as i32), true, 0);
    let (high, _) = integer_to_float(i64::from((raw >> 32) as i32), true, 0);
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        u128::from(low) | (u128::from(high) << 64),
    );
    Ok(())
}

/// Legacy CVTPS2PD reads only the low two f32 lanes (m64 for memory) and
/// replaces the whole XMM destination. Widening finite values is exact.
pub(super) fn cvtps2pd(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let raw = read_scalar_src(cpu, instruction, true)?;
    let daz = cpu.mxcsr & (1 << 6) != 0;
    let (low, low_exception) = widen_single(raw as u32, daz);
    let (high, high_exception) = widen_single((raw >> 32) as u32, daz);
    if record_exception(cpu, low_exception | high_exception)? {
        write_xmm(
            &mut cpu.regs,
            instruction.op0_register(),
            u128::from(low) | (u128::from(high) << 64),
        );
    }
    Ok(())
}

/// Build IEEE-754 bits without depending on the host floating-point state.
fn widen_single(raw: u32, daz: bool) -> (u64, u32) {
    let sign = u64::from(raw >> 31) << 63;
    let exponent = (raw >> 23) & 0xff;
    let fraction = raw & 0x7f_ffff;
    if exponent == 0 {
        if fraction == 0 || daz {
            return (sign, 0);
        }
        // A subnormal f32 is fraction * 2^-149, a normal finite f64.
        let highest = 31 - fraction.leading_zeros();
        let bits = sign
            | (u64::from(highest + 874) << 52)
            | ((u64::from(fraction) << (52 - highest)) & 0x000f_ffff_ffff_ffff);
        return (bits, DENORMAL);
    }
    if exponent == 0xff {
        let bits = sign | 0x7ff0_0000_0000_0000 | (u64::from(fraction) << 29);
        return if fraction == 0 {
            (bits, 0)
        } else {
            (
                bits | (1 << 51),
                if fraction & (1 << 22) == 0 {
                    INVALID
                } else {
                    0
                },
            )
        };
    }
    (
        sign | (u64::from(exponent + 896) << 52) | (u64::from(fraction) << 29),
        0,
    )
}

pub(super) fn cvtsi2s(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let source_size = operand_size(instruction, 1);
    let raw = match instruction.op1_kind() {
        OpKind::Memory => cpu.read_operand(instruction, 1, source_size)?,
        _ => read_register(&cpu.regs, instruction.op1_register(), source_size),
    };
    let integer = if source_size == 8 {
        raw as i64
    } else {
        i64::from(raw as i32)
    };
    let double = instruction.mnemonic() == Mnemonic::Cvtsi2sd;
    let (bits, inexact) = integer_to_float(integer, double, (cpu.mxcsr >> 13) & 3);
    if !record_exception(cpu, if inexact { PRECISION } else { 0 })? {
        return Ok(());
    }
    let mask = if double {
        u128::from(u64::MAX)
    } else {
        u128::from(u32::MAX)
    };
    let destination = read_xmm(&cpu.regs, instruction.op0_register());
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        (destination & !mask) | u128::from(bits),
    );
    Ok(())
}

/// Assemble IEEE-754 bits directly, rounding the exact signed integer once.
/// In particular an i64-to-f32 conversion must not first round through f64.
fn integer_to_float(value: i64, double: bool, rounding: u32) -> (u64, bool) {
    if value == 0 {
        return (0, false);
    }
    let negative = value < 0;
    let magnitude = value.unsigned_abs();
    let fraction_bits = if double { 52 } else { 23 };
    let bias = if double { 1023 } else { 127 };
    let mut exponent = 63 - magnitude.leading_zeros();
    let (mut significand, remainder, shift) = if exponent > fraction_bits {
        let shift = exponent - fraction_bits;
        (
            magnitude >> shift,
            magnitude & ((1_u64 << shift) - 1),
            shift,
        )
    } else {
        (magnitude << (fraction_bits - exponent), 0, 0)
    };
    if remainder != 0 {
        let increment = match rounding {
            0 => {
                let halfway = 1_u64 << (shift - 1);
                remainder > halfway || (remainder == halfway && significand & 1 != 0)
            }
            1 => negative,
            2 => !negative,
            _ => false,
        };
        significand += u64::from(increment);
        if significand == 1_u64 << (fraction_bits + 1) {
            significand >>= 1;
            exponent += 1;
        }
    }
    let sign = u64::from(negative) << if double { 63 } else { 31 };
    let bits = sign
        | (u64::from(exponent + bias) << fraction_bits)
        | (significand & ((1_u64 << fraction_bits) - 1));
    (bits, remainder != 0)
}

pub(super) fn cvt2si_round(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    float_to_integer(cpu, instruction, (cpu.mxcsr >> 13) & 3)
}

pub(super) fn cvtts2si(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    float_to_integer(cpu, instruction, 3) // Truncation ignores MXCSR.RC.
}

fn float_to_integer(
    cpu: &mut Cpu,
    instruction: &Instruction,
    rounding: u32,
) -> Result<(), CpuError> {
    let double = matches!(
        instruction.mnemonic(),
        Mnemonic::Cvtsd2si | Mnemonic::Cvttsd2si
    );
    let raw = read_scalar_src(cpu, instruction, double)?;
    let (mut value, denormal) = if double {
        let value = f64::from_bits(raw);
        (value, value.is_subnormal())
    } else {
        let value = f32::from_bits(raw as u32);
        (f64::from(value), value.is_subnormal())
    };
    if denormal && cpu.mxcsr & (1 << 6) != 0 {
        value = 0.0_f64.copysign(value); // DAZ treats the source as signed zero.
    }
    let rounded = match rounding {
        0 => value.round_ties_even(),
        1 => value.floor(),
        2 => value.ceil(),
        _ => value.trunc(),
    };
    let destination_size = operand_size(instruction, 0);
    let limit = if destination_size == 8 {
        9_223_372_036_854_775_808.0
    } else {
        2_147_483_648.0
    };
    let (result, exception) = if !rounded.is_finite() || rounded < -limit || rounded >= limit {
        // SSE returns integer indefinite on masked invalid, not Rust's
        // saturating cast (nor zero for NaN). The minimum signed value is valid.
        (
            if destination_size == 8 {
                1_u64 << 63
            } else {
                1_u64 << 31
            },
            INVALID,
        )
    } else {
        (
            rounded as i64 as u64,
            if rounded != value { PRECISION } else { 0 },
        )
    };
    if record_exception(cpu, exception)? {
        write_register(
            &mut cpu.regs,
            instruction.op0_register(),
            destination_size,
            result,
        );
    }
    Ok(())
}

/// Preserve sticky status and leave the destination untouched on an unmasked
/// conversion exception. CR4.OSXMMEXCPT selects #XM versus #UD.
fn record_exception(cpu: &mut Cpu, exception: u32) -> Result<bool, CpuError> {
    cpu.mxcsr |= exception;
    if exception & !(cpu.mxcsr >> 7) != 0 {
        let vector = if cpu.regs.cr4.contains(Cr4::OSXMMEXCPT) {
            19
        } else {
            6
        };
        cpu.raise(vector, 0, false)?;
        return Ok(false);
    }
    Ok(true)
}
