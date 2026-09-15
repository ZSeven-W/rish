//! FPREM (Intel SDM Vol. 2A): exact reduction of the values in our f64 stack.
//!
//! This is not an 80-bit x87 implementation. Precision/range were already lost
//! when operands entered the existing f64 stack; stack tags, unsupported 80-bit
//! encodings, and extended-format denormal/underflow exceptions are not modeled.
//! In particular, a subnormal f64 is still a normal extended-format number.

use iced_x86::Instruction;

use crate::arch::registers::Cr0;
use crate::cpu::VECTOR_INVALID_OPCODE;
use crate::{Cpu, CpuError};

const CONDITIONS: u16 = 0x4700;
const C2: u16 = 1 << 10;
const INVALID: u16 = 1;
const EXCEPTIONS: u16 = 0x3f;
const SUMMARY_BUSY: u16 = 0x8080;
const FRACTION: u64 = (1 << 52) - 1;
const QUIET: u64 = 1 << 51;
const SIGN: u64 = 1 << 63;

pub fn execute(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    if instruction.has_lock_prefix() {
        return cpu.raise(VECTOR_INVALID_OPCODE, 0, false);
    }
    if cpu.regs.cr0.intersects(Cr0::EM | Cr0::TS) {
        return cpu.raise(7, 0, false); // #NM
    }
    if cpu.fpu_status_word & !cpu.fpu_control_word & EXCEPTIONS != 0 {
        if cpu.regs.cr0.contains(Cr0::NE) {
            return cpu.raise(16, 0, false); // pending #MF
        }
        // The portable core has no legacy FERR/IRQ13 route. Do not silently
        // execute past an exception in a guest that selected that route.
        return Err(CpuError::GuestFault(
            "x87 legacy FERR exception delivery is unsupported".into(),
        ));
    }

    let top = usize::from(cpu.fpu_top);
    let dividend = cpu.fpu_stack[top];
    let divisor = cpu.fpu_stack[(top + 1) & 7];
    let nan = dividend.is_nan() || divisor.is_nan();
    // A quiet NaN takes precedence over invalid finite/infinite combinations.
    let invalid = signaling_nan(dividend)
        || signaling_nan(divisor)
        || (!nan && (dividend.is_infinite() || divisor == 0.0));
    if invalid {
        cpu.fpu_status_word |= INVALID;
        if cpu.fpu_control_word & INVALID == 0 {
            cpu.fpu_status_word |= SUMMARY_BUSY;
            // x87 records this exception now; the next waiting instruction
            // delivers it. An unmasked invalid operation does not alter ST0.
            return Ok(());
        }
    }
    let (remainder, quotient, partial) = if nan {
        (propagate_nan(dividend, divisor), 0, false)
    } else if invalid {
        (f64::from_bits(0xfff8_0000_0000_0000), 0, false)
    } else if divisor.is_infinite() || dividend == 0.0 {
        (dividend, 0, false)
    } else {
        reduce(dividend, divisor)
    };
    cpu.fpu_stack[top] = remainder;
    cpu.fpu_status_word &= !CONDITIONS;
    if partial {
        // C0/C1/C3 are undefined during a partial reduction; choose zero.
        cpu.fpu_status_word |= C2;
    } else {
        // The flags describe the magnitude of Q, including for negative Q.
        cpu.fpu_status_word |=
            ((quotient & 4) << 6) | ((quotient & 2) << 13) | ((quotient & 1) << 9);
    }
    Ok(())
}

fn signaling_nan(value: f64) -> bool {
    value.is_nan() && value.to_bits() & QUIET == 0
}

fn propagate_nan(a: f64, b: f64) -> f64 {
    // x87 prefers QNaN over SNaN, then the larger significand, then positive
    // sign on a tie. Compare bits without inadvertently quieting an SNaN.
    let rank = |value: f64| {
        let bits = value.to_bits();
        (
            value.is_nan(),
            bits & QUIET != 0,
            bits & FRACTION,
            bits & SIGN == 0,
        )
    };
    let selected = if rank(a) >= rank(b) { a } else { b };
    f64::from_bits(selected.to_bits() | QUIET)
}

/// Return a normalized 53-bit significand and its unbiased leading exponent.
fn normalized(value: f64) -> (u64, i32) {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & FRACTION;
    if exponent == 0 {
        let shift = fraction.leading_zeros() - 11;
        (fraction << shift, -1022 - shift as i32)
    } else {
        (fraction | (1 << 52), exponent - 1023)
    }
}

/// Encode significand * 2^exponent exactly, including subnormal results.
/// Reduction leaves at most 53 significant bits, with no nonzero bits below
/// the f64 subnormal quantum, so no floating-point rounding is necessary.
fn encode(significand: u64, exponent: i32, sign: u64) -> f64 {
    if significand == 0 {
        return f64::from_bits(sign);
    }
    let shift = significand.leading_zeros() - 11;
    let normalized = significand << shift;
    let leading_exponent = exponent + 52 - shift as i32;
    let magnitude = if leading_exponent >= -1022 {
        (((leading_exponent + 1023) as u64) << 52) | (normalized & FRACTION)
    } else {
        normalized >> (-1022 - leading_exponent)
    };
    f64::from_bits(sign | magnitude)
}

fn reduce(dividend: f64, divisor: f64) -> (f64, u16, bool) {
    let (a, exp_a) = normalized(dividend);
    let (b, exp_b) = normalized(divisor);
    let difference = exp_a - exp_b;
    if difference < 0 {
        return (dividend, 0, false);
    }
    let partial = difference >= 64;
    // For partial reduction choose Intel's permitted N=32. Integer arithmetic
    // computes (a << N) mod b, then restores the scale of b * 2^(D-N).
    // The largest complete numerator is only 53+63=116 bits; even when Q
    // exceeds f64's integer precision, its low bits and remainder stay exact.
    let shift = if partial { 32 } else { difference };
    let numerator = u128::from(a) << shift;
    let quotient = (numerator / u128::from(b)) as u16 & 7;
    let remainder = (numerator % u128::from(b)) as u64;
    (
        encode(remainder, exp_a - shift - 52, dividend.to_bits() & SIGN),
        quotient,
        partial,
    )
}

#[cfg(test)]
mod tests;
