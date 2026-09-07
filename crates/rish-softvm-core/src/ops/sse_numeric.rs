//! Packed numeric operations shared by the SSE dispatcher.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum PackedOp {
    AddWrap,
    SubWrap,
    CmpGt,
    MinU,
    MaxU,
    MinS,
    MaxS,
    AddSatU,
    SubSatU,
    AddSatS,
    SubSatS,
}

/// Per-lane packed integer arithmetic over a 128-bit register with a source
/// that may be a register or a memory operand.
pub(super) fn packed_arith(
    cpu: &mut Cpu,
    instruction: &Instruction,
    lane_bits: u32,
    op: PackedOp,
) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let mask = if lane_bits == 128 {
        u128::MAX
    } else {
        (1_u128 << lane_bits) - 1
    };
    let sign_bit = 1_u128 << (lane_bits - 1);
    // Sign-extend a lane value to a full i128 for signed comparisons/saturation.
    let sext = |value: u128| -> i128 {
        if value & sign_bit != 0 {
            (value | !mask) as i128
        } else {
            value as i128
        }
    };
    let smin = -(1_i128 << (lane_bits - 1));
    let smax = (1_i128 << (lane_bits - 1)) - 1;
    let umax = mask;
    let lanes = 128 / lane_bits;
    let mut result = 0_u128;
    for lane in 0..lanes {
        let shift = lane * lane_bits;
        let a = (left >> shift) & mask;
        let b = (right >> shift) & mask;
        let lane_result: u128 = match op {
            PackedOp::AddWrap => a.wrapping_add(b) & mask,
            PackedOp::SubWrap => a.wrapping_sub(b) & mask,
            PackedOp::CmpGt => {
                if sext(a) > sext(b) {
                    mask
                } else {
                    0
                }
            }
            PackedOp::MinU => a.min(b),
            PackedOp::MaxU => a.max(b),
            PackedOp::MinS => (sext(a).min(sext(b)) as u128) & mask,
            PackedOp::MaxS => (sext(a).max(sext(b)) as u128) & mask,
            PackedOp::AddSatU => (a + b).min(umax),
            PackedOp::SubSatU => a.saturating_sub(b),
            PackedOp::AddSatS => (sext(a) + sext(b)).clamp(smin, smax) as u128 & mask,
            PackedOp::SubSatS => (sext(a) - sext(b)).clamp(smin, smax) as u128 & mask,
        };
        result |= (lane_result & mask) << shift;
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

/// Reads a scalar float memory operand of `bytes` width into the low bits.
pub(super) fn read_scalar_mem(
    cpu: &mut Cpu,
    instruction: &Instruction,
    operand: u32,
    bytes: usize,
) -> Result<u64, CpuError> {
    let linear = cpu.effective_address(instruction, operand);
    let mut buf = [0_u8; 8];
    cpu.read_linear_bytes(linear, &mut buf[..bytes])?;
    Ok(u64::from_le_bytes(buf))
}

/// UCOMIS{S,D} / COMIS{S,D}: ordered scalar compare that sets ZF/PF/CF and
/// clears OF/SF/AF. Unordered (NaN) operands set ZF=PF=CF. The signalling
/// distinction between COMIS and UCOMIS raises no exception here.
pub(super) fn comis(
    cpu: &mut Cpu,
    instruction: &Instruction,
    double: bool,
) -> Result<(), CpuError> {
    use crate::arch::registers::RFlags;
    let left_raw = read_xmm(&cpu.regs, instruction.op0_register()) as u64;
    let width = if double { 8 } else { 4 };
    let right_raw = match instruction.op1_kind() {
        OpKind::Memory => read_scalar_mem(cpu, instruction, 1, width)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()) as u64,
    };
    let (left, right) = if double {
        (f64::from_bits(left_raw), f64::from_bits(right_raw))
    } else {
        (
            f64::from(f32::from_bits(left_raw as u32)),
            f64::from(f32::from_bits(right_raw as u32)),
        )
    };
    let mut flags = cpu.regs.rflags;
    flags.remove(RFlags::OF | RFlags::SF | RFlags::AF | RFlags::ZF | RFlags::PF | RFlags::CF);
    match left.partial_cmp(&right) {
        None => flags.insert(RFlags::ZF | RFlags::PF | RFlags::CF), // unordered
        Some(std::cmp::Ordering::Greater) => {}                     // ZF=PF=CF=0
        Some(std::cmp::Ordering::Less) => flags.insert(RFlags::CF),
        Some(std::cmp::Ordering::Equal) => flags.insert(RFlags::ZF),
    }
    cpu.regs.rflags = flags;
    Ok(())
}

/// PINSRW: insert a 16-bit word from a general register or memory into one of
/// the eight word lanes of the destination register, selected by imm8[2:0].
pub(super) fn pinsrw(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let lane = u32::from(instruction.immediate(2) as u8 & 0b111);
    let word = match instruction.op1_kind() {
        OpKind::Memory => read_scalar_mem(cpu, instruction, 1, 2)? as u16,
        _ => read_register(&cpu.regs, instruction.op1_register(), 4) as u16,
    };
    let shift = lane * 16;
    let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
    destination = (destination & !(0xFFFF_u128 << shift)) | (u128::from(word) << shift);
    write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
    Ok(())
}

/// PMOVMSKB: gather the top bit of each of the 16 source bytes into the low
/// 16 bits of a general-purpose register, zero-extended.
pub(super) fn pmovmskb(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let source = read_xmm(&cpu.regs, instruction.op1_register());
    let mut mask: u64 = 0;
    for byte in 0..16 {
        let bit = (source >> (byte * 8 + 7)) & 1;
        mask |= (bit as u64) << byte;
    }
    write_register(
        &mut cpu.regs,
        instruction.op0_register(),
        operand_size(instruction, 0),
        mask,
    );
    Ok(())
}

/// MOVMSKPS/MOVMSKPD: gather the sign bit of each single- or double-precision
/// lane into the low bits of a general-purpose register, zero-extended.
pub(super) fn movmsk(
    cpu: &mut Cpu,
    instruction: &Instruction,
    lane_bits: u32,
) -> Result<(), CpuError> {
    let source = read_xmm(&cpu.regs, instruction.op1_register());
    let lanes = 128 / lane_bits;
    let mut mask: u64 = 0;
    for lane in 0..lanes {
        let bit =
            (source >> (u128::from(lane) * u128::from(lane_bits) + u128::from(lane_bits - 1))) & 1;
        mask |= (bit as u64) << lane;
    }
    write_register(
        &mut cpu.regs,
        instruction.op0_register(),
        operand_size(instruction, 0),
        mask,
    );
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

pub(super) fn apply_float(op: FloatOp, a: f64, b: f64) -> f64 {
    match op {
        FloatOp::Add => a + b,
        FloatOp::Sub => a - b,
        FloatOp::Mul => a * b,
        FloatOp::Div => a / b,
        // x86 MIN/MAX return the second operand when the inputs are unordered
        // or equal, which differs from Rust's NaN-ignoring min/max.
        FloatOp::Min => {
            if a < b {
                a
            } else {
                b
            }
        }
        FloatOp::Max => {
            if a > b {
                a
            } else {
                b
            }
        }
    }
}

/// Reads the low scalar of the second operand as raw bits (register or memory).
pub(super) fn read_scalar_src(
    cpu: &mut Cpu,
    instruction: &Instruction,
    double: bool,
) -> Result<u64, CpuError> {
    let width = if double { 8 } else { 4 };
    match instruction.op1_kind() {
        OpKind::Memory => read_scalar_mem(cpu, instruction, 1, width),
        _ => Ok(read_xmm(&cpu.regs, instruction.op1_register()) as u64),
    }
}

/// Writes a scalar float result into the low lane of the destination xmm,
/// preserving the upper bits.
pub(super) fn write_scalar_result(cpu: &mut Cpu, register: Register, double: bool, value: f64) {
    let mut destination = read_xmm(&cpu.regs, register);
    if double {
        destination = (destination & !u128::from(u64::MAX)) | u128::from(value.to_bits());
    } else {
        destination = (destination & !u128::from(u32::MAX)) | u128::from((value as f32).to_bits());
    }
    write_xmm(&mut cpu.regs, register, destination);
}

pub(super) fn scalar_float(
    cpu: &mut Cpu,
    instruction: &Instruction,
    double: bool,
    op: FloatOp,
) -> Result<(), CpuError> {
    let left_raw = read_xmm(&cpu.regs, instruction.op0_register()) as u64;
    let right_raw = read_scalar_src(cpu, instruction, double)?;
    let (a, b) = if double {
        (f64::from_bits(left_raw), f64::from_bits(right_raw))
    } else {
        (
            f64::from(f32::from_bits(left_raw as u32)),
            f64::from(f32::from_bits(right_raw as u32)),
        )
    };
    write_scalar_result(
        cpu,
        instruction.op0_register(),
        double,
        apply_float(op, a, b),
    );
    Ok(())
}

/// SSE compare predicate (imm8[2:0]). Rust's float relops already return false
/// when either operand is NaN, which matches the ordered/unordered semantics of
/// these predicates exactly — the negated forms below are the NLT/NLE
/// predicates, which are defined to be true when the operands are unordered.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub(super) fn float_compare(a: f64, b: f64, predicate: u8) -> bool {
    match predicate & 0x7 {
        0 => a == b,                      // EQ (ordered)
        1 => a < b,                       // LT (ordered)
        2 => a <= b,                      // LE (ordered)
        3 => a.is_nan() || b.is_nan(),    // UNORD
        4 => a != b,                      // NEQ (true when unordered)
        5 => !(a < b),                    // NLT (true when unordered)
        6 => !(a <= b),                   // NLE (true when unordered)
        _ => !(a.is_nan() || b.is_nan()), // ORD
    }
}

/// CMPPS/CMPPD/CMPSS/CMPSD: compare and write an all-ones or all-zeros mask per
/// element. Go's `math` package and any FP-heavy code lean on these.
pub(super) fn cmp_float(
    cpu: &mut Cpu,
    instruction: &Instruction,
    scalar: bool,
    double: bool,
) -> Result<(), CpuError> {
    let predicate = instruction.immediate(2) as u8;
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let elem_bits = if double { 64_u32 } else { 32 };
    let mask: u128 = if double {
        u128::from(u64::MAX)
    } else {
        u128::from(u32::MAX)
    };
    let decode = |bits: u128| -> f64 {
        if double {
            f64::from_bits(bits as u64)
        } else {
            f64::from(f32::from_bits(bits as u32))
        }
    };
    if scalar {
        let right_raw = read_scalar_src(cpu, instruction, double)?;
        let a = decode(left & mask);
        let b = decode(u128::from(right_raw) & mask);
        let element = if float_compare(a, b, predicate) {
            mask
        } else {
            0
        };
        write_xmm(
            &mut cpu.regs,
            instruction.op0_register(),
            (left & !mask) | element,
        );
    } else {
        let right = match instruction.op1_kind() {
            OpKind::Memory => read_mem128(cpu, instruction, 1)?,
            _ => read_xmm(&cpu.regs, instruction.op1_register()),
        };
        let mut result = 0_u128;
        for lane in 0..(128 / elem_bits) {
            let shift = lane * elem_bits;
            let a = decode((left >> shift) & mask);
            let b = decode((right >> shift) & mask);
            if float_compare(a, b, predicate) {
                result |= mask << shift;
            }
        }
        write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    }
    Ok(())
}

pub(super) fn sqrt_scalar(
    cpu: &mut Cpu,
    instruction: &Instruction,
    double: bool,
) -> Result<(), CpuError> {
    let raw = read_scalar_src(cpu, instruction, double)?;
    let value = if double {
        f64::from_bits(raw).sqrt()
    } else {
        f64::from(f32::from_bits(raw as u32).sqrt())
    };
    write_scalar_result(cpu, instruction.op0_register(), double, value);
    Ok(())
}

pub(super) fn packed_float(
    cpu: &mut Cpu,
    instruction: &Instruction,
    double: bool,
    op: FloatOp,
) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let mut result = 0_u128;
    if double {
        for lane in 0..2 {
            let a = f64::from_bits((left >> (lane * 64)) as u64);
            let b = f64::from_bits((right >> (lane * 64)) as u64);
            result |= u128::from(apply_float(op, a, b).to_bits()) << (lane * 64);
        }
    } else {
        for lane in 0..4 {
            let a = f64::from(f32::from_bits((left >> (lane * 32)) as u32));
            let b = f64::from(f32::from_bits((right >> (lane * 32)) as u32));
            let value = (apply_float(op, a, b) as f32).to_bits();
            result |= u128::from(value) << (lane * 32);
        }
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

/// PMULUDQ: multiply the two even 32-bit unsigned lanes into 64-bit results.
pub(super) fn pmuludq(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let a0 = (left & 0xFFFF_FFFF) as u64;
    let b0 = (right & 0xFFFF_FFFF) as u64;
    let a1 = ((left >> 64) & 0xFFFF_FFFF) as u64;
    let b1 = ((right >> 64) & 0xFFFF_FFFF) as u64;
    let result = u128::from(a0 * b0) | (u128::from(a1 * b1) << 64);
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

/// MOVHLPS (high=true): dest low = src high. MOVLHPS (high=false): dest high =
/// src low. Both are register-only three-quarter moves.
pub(super) fn movhl(cpu: &mut Cpu, instruction: &Instruction, high: bool) -> Result<(), CpuError> {
    let src = read_xmm(&cpu.regs, instruction.op1_register());
    let mut dest = read_xmm(&cpu.regs, instruction.op0_register());
    if high {
        dest = (dest & !u128::from(u64::MAX)) | (src >> 64);
    } else {
        dest = (dest & u128::from(u64::MAX)) | ((src & u128::from(u64::MAX)) << 64);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), dest);
    Ok(())
}

/// UNPCKL/UNPCKH for float lanes: interleave the low or high halves of the two
/// operands at the given lane width (32 for ps, 64 for pd).
pub(super) fn unpck_float(
    cpu: &mut Cpu,
    instruction: &Instruction,
    lane_bits: u32,
    high: bool,
) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let mask = (1_u128 << lane_bits) - 1;
    let lanes = 128 / lane_bits;
    let base = if high { lanes / 2 } else { 0 };
    let mut result = 0_u128;
    for pair in 0..(lanes / 2) {
        let src_lane = base + pair;
        let lo = (left >> (src_lane * lane_bits)) & mask;
        let hi = (right >> (src_lane * lane_bits)) & mask;
        result |= lo << (pair * 2 * lane_bits);
        result |= hi << ((pair * 2 + 1) * lane_bits);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

/// CVTSS2SD (to_double=true) / CVTSD2SS: change scalar precision, writing the
/// low lane and preserving the upper bits of the destination.
pub(super) fn cvt_precision(
    cpu: &mut Cpu,
    instruction: &Instruction,
    to_double: bool,
) -> Result<(), CpuError> {
    let source_is_double = !to_double;
    let raw = read_scalar_src(cpu, instruction, source_is_double)?;
    let value = if source_is_double {
        f64::from_bits(raw)
    } else {
        f64::from(f32::from_bits(raw as u32))
    };
    write_scalar_result(cpu, instruction.op0_register(), to_double, value);
    Ok(())
}

/// CVTSD2SI / CVTSS2SI: convert a scalar float to a signed integer using
/// round-to-nearest-even, writing a general-purpose register.
pub(super) fn cvt2si_round(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let double = instruction.mnemonic() == Mnemonic::Cvtsd2si;
    let raw = read_scalar_src(cpu, instruction, double)?;
    let value = if double {
        f64::from_bits(raw)
    } else {
        f64::from(f32::from_bits(raw as u32))
    };
    let rounded = value.round_ties_even();
    let size = operand_size(instruction, 0);
    let result = if size == 8 {
        rounded as i64 as u64
    } else {
        rounded as i32 as u64
    };
    write_register(&mut cpu.regs, instruction.op0_register(), size, result);
    Ok(())
}

pub(super) fn cvtts2si(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let source = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
    let value = if instruction.mnemonic() == Mnemonic::Cvttsd2si {
        f64::from_bits(source) as i64 as u64
    } else {
        f32::from_bits(source as u32) as i32 as u64
    };
    write_register(
        &mut cpu.regs,
        instruction.op0_register(),
        operand_size(instruction, 0),
        value,
    );
    Ok(())
}

pub(super) fn is_xmm(register: Register) -> bool {
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

pub(super) fn bad(label: &str, cpu: &Cpu, instruction: &Instruction) -> CpuError {
    CpuError::UnimplementedInstruction {
        code: format!(
            "{label} ({:?}, {:?})",
            instruction.op0_kind(),
            instruction.op1_kind()
        ),
        address: cpu.regs.rip,
        bytes: Vec::new(),
    }
}
