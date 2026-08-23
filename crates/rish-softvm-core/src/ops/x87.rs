//! x87 FPU: a functional subset over an f64-approximated register stack.
//!
//! Go's runtime uses SSE2 for floating point, but statically linked cgo pieces
//! inside dockerd/containerd still reach for the legacy x87 stack (fild, fld,
//! fmulp, fucomi, …). This module models the eight-deep register stack, the
//! control/status words, and the load/store/arith/compare instructions those
//! programs actually execute. Values are held as f64; the 80-bit extended
//! format is converted on the memory boundary, which is exact for every finite
//! double a program pushes through the FPU.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::arch::registers::RFlags;
use crate::{Cpu, CpuError};

impl Cpu {
    fn fpu_push(&mut self, value: f64) {
        self.fpu_top = self.fpu_top.wrapping_sub(1) & 7;
        self.fpu_stack[self.fpu_top as usize] = value;
    }

    fn fpu_pop(&mut self) -> f64 {
        let value = self.fpu_stack[self.fpu_top as usize];
        self.fpu_top = (self.fpu_top + 1) & 7;
        value
    }

    fn fpu_st(&self, index: usize) -> f64 {
        self.fpu_stack[(self.fpu_top as usize + index) & 7]
    }

    fn fpu_set_st(&mut self, index: usize, value: f64) {
        let slot = (self.fpu_top as usize + index) & 7;
        self.fpu_stack[slot] = value;
    }
}

fn st_index(register: Register) -> Option<usize> {
    match register {
        Register::ST0 => Some(0),
        Register::ST1 => Some(1),
        Register::ST2 => Some(2),
        Register::ST3 => Some(3),
        Register::ST4 => Some(4),
        Register::ST5 => Some(5),
        Register::ST6 => Some(6),
        Register::ST7 => Some(7),
        _ => None,
    }
}

/// Reads the memory source of an x87 instruction as a double, honouring the
/// format iced reports (32/64/80-bit float, 16/32/64-bit integer).
fn read_fp_memory(cpu: &mut Cpu, instruction: &Instruction) -> Result<f64, CpuError> {
    use iced_x86::MemorySize;
    let linear = cpu.effective_address(instruction, 0);
    Ok(match instruction.memory_size() {
        MemorySize::Float32 => {
            let mut bytes = [0_u8; 4];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            f64::from(f32::from_le_bytes(bytes))
        }
        MemorySize::Float64 => {
            let mut bytes = [0_u8; 8];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            f64::from_le_bytes(bytes)
        }
        MemorySize::Float80 => {
            let mut bytes = [0_u8; 10];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            extended80_to_f64(&bytes)
        }
        MemorySize::Int16 => {
            let mut bytes = [0_u8; 2];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            f64::from(i16::from_le_bytes(bytes))
        }
        MemorySize::Int32 => {
            let mut bytes = [0_u8; 4];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            f64::from(i32::from_le_bytes(bytes))
        }
        MemorySize::Int64 => {
            let mut bytes = [0_u8; 8];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            i64::from_le_bytes(bytes) as f64
        }
        other => {
            return Err(CpuError::GuestFault(format!(
                "x87 memory operand size {other:?} is unsupported"
            )));
        }
    })
}

/// Stores `value` to the instruction's memory destination in the reported
/// floating-point format.
fn store_fp_memory(cpu: &mut Cpu, instruction: &Instruction, value: f64) -> Result<(), CpuError> {
    use iced_x86::MemorySize;
    let linear = cpu.effective_address(instruction, 0);
    match instruction.memory_size() {
        MemorySize::Float32 => cpu.write_linear_bytes(linear, &(value as f32).to_le_bytes()),
        MemorySize::Float64 => cpu.write_linear_bytes(linear, &value.to_le_bytes()),
        MemorySize::Float80 => cpu.write_linear_bytes(linear, &f64_to_extended80(value)),
        other => Err(CpuError::GuestFault(format!(
            "x87 float store size {other:?} is unsupported"
        ))),
    }
}

/// Stores `value` rounded to an integer of the reported width. `truncate`
/// selects round-toward-zero (fisttp) instead of the control-word mode, which
/// this model always treats as round-to-nearest.
fn store_int_memory(
    cpu: &mut Cpu,
    instruction: &Instruction,
    value: f64,
    truncate: bool,
) -> Result<(), CpuError> {
    use iced_x86::MemorySize;
    let rounded = if truncate {
        value.trunc()
    } else {
        value.round_ties_even()
    };
    let linear = cpu.effective_address(instruction, 0);
    match instruction.memory_size() {
        MemorySize::Int16 => cpu.write_linear_bytes(linear, &(rounded as i16).to_le_bytes()),
        MemorySize::Int32 => cpu.write_linear_bytes(linear, &(rounded as i32).to_le_bytes()),
        MemorySize::Int64 => cpu.write_linear_bytes(linear, &(rounded as i64).to_le_bytes()),
        other => Err(CpuError::GuestFault(format!(
            "x87 integer store size {other:?} is unsupported"
        ))),
    }
}

pub(crate) fn extended80_to_f64(bytes: &[u8; 10]) -> f64 {
    let mantissa = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let sign_exp = u16::from_le_bytes([bytes[8], bytes[9]]);
    let sign = (sign_exp >> 15) & 1;
    let exponent = i32::from(sign_exp & 0x7fff);
    if exponent == 0 && mantissa == 0 {
        return if sign == 1 { -0.0 } else { 0.0 };
    }
    if exponent == 0x7fff {
        if mantissa << 1 == 0 {
            return if sign == 1 {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
        }
        return f64::NAN;
    }
    let value = (mantissa as f64) * 2.0_f64.powi(exponent - 16383 - 63);
    if sign == 1 { -value } else { value }
}

pub(crate) fn f64_to_extended80(value: f64) -> [u8; 10] {
    let bits = value.to_bits();
    let sign = ((bits >> 63) & 1) as u16;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    let mut out = [0_u8; 10];
    if exponent == 0x7ff {
        let sign_exp = (sign << 15) | 0x7fff;
        let mantissa = if fraction == 0 {
            0x8000_0000_0000_0000_u64
        } else {
            0xC000_0000_0000_0000_u64
        };
        out[0..8].copy_from_slice(&mantissa.to_le_bytes());
        out[8..10].copy_from_slice(&sign_exp.to_le_bytes());
        return out;
    }
    if exponent == 0 && fraction == 0 {
        out[8..10].copy_from_slice(&(sign << 15).to_le_bytes());
        return out;
    }
    let exponent80 = ((exponent - 1023 + 16383) as u16) & 0x7fff;
    let mantissa = 0x8000_0000_0000_0000_u64 | (fraction << 11);
    let sign_exp = (sign << 15) | exponent80;
    out[0..8].copy_from_slice(&mantissa.to_le_bytes());
    out[8..10].copy_from_slice(&sign_exp.to_le_bytes());
    out
}

/// Sets the x87 condition codes C3/C2/C0 (bits 14/10/8) from a comparison.
fn set_compare_status(cpu: &mut Cpu, a: f64, b: f64) {
    let (c3, c2, c0) = if a.is_nan() || b.is_nan() {
        (1, 1, 1)
    } else if a > b {
        (0, 0, 0)
    } else if a < b {
        (0, 0, 1)
    } else {
        (1, 0, 0)
    };
    let mut status = cpu.fpu_status_word & !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
    status |= (c3 << 14) | (c2 << 10) | (c0 << 8);
    cpu.fpu_status_word = status;
}

/// Sets EFLAGS ZF/PF/CF from a comparison, as fcomi/fucomi do.
fn set_compare_flags(cpu: &mut Cpu, a: f64, b: f64) {
    let (zf, pf, cf) = if a.is_nan() || b.is_nan() {
        (true, true, true)
    } else if a > b {
        (false, false, false)
    } else if a < b {
        (false, false, true)
    } else {
        (true, false, false)
    };
    cpu.regs.rflags.set(RFlags::ZF, zf);
    cpu.regs.rflags.set(RFlags::PF, pf);
    cpu.regs.rflags.set(RFlags::CF, cf);
    cpu.regs.rflags -= RFlags::OF;
    cpu.regs.rflags -= RFlags::SF;
}

fn source_index(instruction: &Instruction) -> usize {
    st_index(instruction.op0_register())
        .or_else(|| st_index(instruction.op1_register()))
        .unwrap_or(0)
}

/// Reads the arithmetic operand: an ST register or a memory float.
fn read_arith_operand(cpu: &mut Cpu, instruction: &Instruction) -> Result<f64, CpuError> {
    if instruction.op0_kind() == OpKind::Memory {
        read_fp_memory(cpu, instruction)
    } else {
        Ok(cpu.fpu_st(source_index(instruction)))
    }
}

pub fn x87_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        // ---- loads ----
        Mnemonic::Fld => {
            let value = if instruction.op0_kind() == OpKind::Memory {
                read_fp_memory(cpu, instruction)?
            } else {
                cpu.fpu_st(st_index(instruction.op0_register()).unwrap_or(0))
            };
            cpu.fpu_push(value);
        }
        Mnemonic::Fild => {
            let value = read_fp_memory(cpu, instruction)?;
            cpu.fpu_push(value);
        }
        Mnemonic::Fld1 => cpu.fpu_push(1.0),
        Mnemonic::Fldz => cpu.fpu_push(0.0),
        Mnemonic::Fldpi => cpu.fpu_push(std::f64::consts::PI),
        Mnemonic::Fldl2e => cpu.fpu_push(std::f64::consts::LOG2_E),
        Mnemonic::Fldl2t => cpu.fpu_push(std::f64::consts::LN_10 / std::f64::consts::LN_2),
        Mnemonic::Fldlg2 => cpu.fpu_push(std::f64::consts::LN_2 / std::f64::consts::LN_10),
        Mnemonic::Fldln2 => cpu.fpu_push(std::f64::consts::LN_2),

        // ---- stores ----
        Mnemonic::Fst => {
            let value = cpu.fpu_st(0);
            if instruction.op0_kind() == OpKind::Memory {
                store_fp_memory(cpu, instruction, value)?;
            } else {
                cpu.fpu_set_st(st_index(instruction.op0_register()).unwrap_or(0), value);
            }
        }
        Mnemonic::Fstp => {
            let value = cpu.fpu_st(0);
            if instruction.op0_kind() == OpKind::Memory {
                store_fp_memory(cpu, instruction, value)?;
            } else {
                cpu.fpu_set_st(st_index(instruction.op0_register()).unwrap_or(0), value);
            }
            cpu.fpu_pop();
        }
        Mnemonic::Fist => {
            let value = cpu.fpu_st(0);
            store_int_memory(cpu, instruction, value, false)?;
        }
        Mnemonic::Fistp => {
            let value = cpu.fpu_pop();
            store_int_memory(cpu, instruction, value, false)?;
        }
        Mnemonic::Fisttp => {
            let value = cpu.fpu_pop();
            store_int_memory(cpu, instruction, value, true)?;
        }

        // ---- arithmetic on ST(0) ----
        Mnemonic::Fadd => {
            let rhs = read_arith_operand(cpu, instruction)?;
            let dst = destination_index(instruction);
            cpu.fpu_set_st(dst, cpu.fpu_st(dst) + rhs);
        }
        Mnemonic::Fsub => {
            let rhs = read_arith_operand(cpu, instruction)?;
            let dst = destination_index(instruction);
            cpu.fpu_set_st(dst, cpu.fpu_st(dst) - rhs);
        }
        Mnemonic::Fsubr => {
            let rhs = read_arith_operand(cpu, instruction)?;
            let dst = destination_index(instruction);
            cpu.fpu_set_st(dst, rhs - cpu.fpu_st(dst));
        }
        Mnemonic::Fmul => {
            let rhs = read_arith_operand(cpu, instruction)?;
            let dst = destination_index(instruction);
            cpu.fpu_set_st(dst, cpu.fpu_st(dst) * rhs);
        }
        Mnemonic::Fdiv => {
            let rhs = read_arith_operand(cpu, instruction)?;
            let dst = destination_index(instruction);
            cpu.fpu_set_st(dst, cpu.fpu_st(dst) / rhs);
        }
        Mnemonic::Fdivr => {
            let rhs = read_arith_operand(cpu, instruction)?;
            let dst = destination_index(instruction);
            cpu.fpu_set_st(dst, rhs / cpu.fpu_st(dst));
        }
        Mnemonic::Fiadd => {
            let rhs = read_fp_memory(cpu, instruction)?;
            cpu.fpu_set_st(0, cpu.fpu_st(0) + rhs);
        }
        Mnemonic::Fisub => {
            let rhs = read_fp_memory(cpu, instruction)?;
            cpu.fpu_set_st(0, cpu.fpu_st(0) - rhs);
        }
        Mnemonic::Fimul => {
            let rhs = read_fp_memory(cpu, instruction)?;
            cpu.fpu_set_st(0, cpu.fpu_st(0) * rhs);
        }
        Mnemonic::Fidiv => {
            let rhs = read_fp_memory(cpu, instruction)?;
            cpu.fpu_set_st(0, cpu.fpu_st(0) / rhs);
        }

        // ---- arithmetic-and-pop (ST(i) op= ST(0); pop) ----
        Mnemonic::Faddp => {
            let index = source_index(instruction);
            cpu.fpu_set_st(index, cpu.fpu_st(index) + cpu.fpu_st(0));
            cpu.fpu_pop();
        }
        Mnemonic::Fsubp => {
            let index = source_index(instruction);
            cpu.fpu_set_st(index, cpu.fpu_st(index) - cpu.fpu_st(0));
            cpu.fpu_pop();
        }
        Mnemonic::Fsubrp => {
            let index = source_index(instruction);
            cpu.fpu_set_st(index, cpu.fpu_st(0) - cpu.fpu_st(index));
            cpu.fpu_pop();
        }
        Mnemonic::Fmulp => {
            let index = source_index(instruction);
            cpu.fpu_set_st(index, cpu.fpu_st(index) * cpu.fpu_st(0));
            cpu.fpu_pop();
        }
        Mnemonic::Fdivp => {
            let index = source_index(instruction);
            cpu.fpu_set_st(index, cpu.fpu_st(index) / cpu.fpu_st(0));
            cpu.fpu_pop();
        }
        Mnemonic::Fdivrp => {
            let index = source_index(instruction);
            cpu.fpu_set_st(index, cpu.fpu_st(0) / cpu.fpu_st(index));
            cpu.fpu_pop();
        }

        // ---- unary ----
        Mnemonic::Fchs => cpu.fpu_set_st(0, -cpu.fpu_st(0)),
        Mnemonic::Fabs => cpu.fpu_set_st(0, cpu.fpu_st(0).abs()),
        Mnemonic::Fsqrt => cpu.fpu_set_st(0, cpu.fpu_st(0).sqrt()),
        Mnemonic::Frndint => cpu.fpu_set_st(0, cpu.fpu_st(0).round_ties_even()),
        Mnemonic::Fxch => {
            let index = source_index(instruction);
            let top = cpu.fpu_st(0);
            let other = cpu.fpu_st(index);
            cpu.fpu_set_st(0, other);
            cpu.fpu_set_st(index, top);
        }

        // ---- compares that set the status word ----
        Mnemonic::Fcom | Mnemonic::Fucom => {
            let rhs = read_arith_operand(cpu, instruction)?;
            set_compare_status(cpu, cpu.fpu_st(0), rhs);
        }
        Mnemonic::Fcomp | Mnemonic::Fucomp => {
            let rhs = read_arith_operand(cpu, instruction)?;
            set_compare_status(cpu, cpu.fpu_st(0), rhs);
            cpu.fpu_pop();
        }
        Mnemonic::Fcompp | Mnemonic::Fucompp => {
            set_compare_status(cpu, cpu.fpu_st(0), cpu.fpu_st(1));
            cpu.fpu_pop();
            cpu.fpu_pop();
        }
        Mnemonic::Ficom => {
            let rhs = read_fp_memory(cpu, instruction)?;
            set_compare_status(cpu, cpu.fpu_st(0), rhs);
        }
        Mnemonic::Ficomp => {
            let rhs = read_fp_memory(cpu, instruction)?;
            set_compare_status(cpu, cpu.fpu_st(0), rhs);
            cpu.fpu_pop();
        }

        // ---- compares that set EFLAGS ----
        Mnemonic::Fcomi | Mnemonic::Fucomi => {
            let index = source_index(instruction);
            set_compare_flags(cpu, cpu.fpu_st(0), cpu.fpu_st(index));
        }
        Mnemonic::Fcomip | Mnemonic::Fucomip => {
            let index = source_index(instruction);
            set_compare_flags(cpu, cpu.fpu_st(0), cpu.fpu_st(index));
            cpu.fpu_pop();
        }

        // ---- classification ----
        Mnemonic::Fxam => {
            let value = cpu.fpu_st(0);
            let (c3, c2, c0) = if value.is_nan() {
                (0, 0, 1)
            } else if value.is_infinite() {
                (0, 1, 1)
            } else if value == 0.0 {
                (1, 0, 0)
            } else {
                (0, 1, 0)
            };
            let c1 = u16::from(value.is_sign_negative());
            let mut status = cpu.fpu_status_word & !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
            status |= (c3 << 14) | (c2 << 10) | (c1 << 9) | (c0 << 8);
            cpu.fpu_status_word = status;
        }
        Mnemonic::Ftst => set_compare_status(cpu, cpu.fpu_st(0), 0.0),

        // ---- control / status words ----
        Mnemonic::Fldcw => {
            cpu.fpu_control_word = cpu.read_operand(instruction, 0, 2)? as u16;
        }
        Mnemonic::Fnstcw | Mnemonic::Fstcw => {
            cpu.write_operand(instruction, 0, 2, u64::from(cpu.fpu_control_word))?;
        }
        Mnemonic::Fnstsw | Mnemonic::Fstsw => {
            let status = (cpu.fpu_status_word & !0x3800) | ((u16::from(cpu.fpu_top) & 7) << 11);
            if instruction.op0_kind() == OpKind::Register {
                crate::ops::write_register(&mut cpu.regs, Register::AX, 2, u64::from(status));
            } else {
                cpu.write_operand(instruction, 0, 2, u64::from(status))?;
            }
        }

        // ---- stack pointer housekeeping ----
        Mnemonic::Ffree | Mnemonic::Ffreep => {
            if instruction.mnemonic() == Mnemonic::Ffreep {
                cpu.fpu_pop();
            }
        }
        Mnemonic::Fincstp => cpu.fpu_top = (cpu.fpu_top + 1) & 7,
        Mnemonic::Fdecstp => cpu.fpu_top = cpu.fpu_top.wrapping_sub(1) & 7,

        // ---- environment / init: modelled as state resets, memory ignored ----
        Mnemonic::Finit | Mnemonic::Fninit => {
            cpu.fpu_control_word = 0x037F;
            cpu.fpu_status_word = 0;
            cpu.fpu_top = 0;
            cpu.fpu_stack = [0.0; 8];
        }
        Mnemonic::Fclex | Mnemonic::Fnclex => {
            cpu.fpu_status_word &= !0x80ff;
        }
        Mnemonic::Fldenv | Mnemonic::Frstor => {}
        Mnemonic::Fnstenv | Mnemonic::Fstenv | Mnemonic::Fnsave | Mnemonic::Fsave => {}
        Mnemonic::Wait => {}

        other => {
            return Err(CpuError::UnimplementedInstruction {
                code: format!("{other:?}"),
                address: cpu.regs.rip,
                bytes: Vec::new(),
            });
        }
    }
    Ok(())
}

/// The destination ST register for a two-operand arithmetic form: the explicit
/// register operand, or ST(0) for the memory / no-operand forms.
fn destination_index(instruction: &Instruction) -> usize {
    if instruction.op0_kind() == OpKind::Register {
        st_index(instruction.op0_register()).unwrap_or(0)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu() -> Cpu {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.efer |= crate::arch::registers::Efer::LMA;
        cpu.regs.cs = crate::arch::segments::SegmentRegister {
            base: 0,
            long_mode: true,
            code: true,
            limit: u32::MAX,
            granularity: true,
            writable_or_readable: true,
            ..Default::default()
        };
        cpu
    }

    fn run(cpu: &mut Cpu, bytes: &[u8]) {
        cpu.memory.write(0x1000, bytes).unwrap();
        let instruction =
            iced_x86::Decoder::with_ip(64, bytes, 0x1000, iced_x86::DecoderOptions::NONE).decode();
        cpu.regs.rip = 0x1000 + instruction.len() as u64;
        cpu.dispatch(&instruction).unwrap();
    }

    #[test]
    fn fild_fld1_and_faddp_walk_the_stack() {
        let mut cpu = cpu();
        cpu.memory.write_u32(0x2000, 5).unwrap();
        cpu.regs.set_gpr(crate::arch::registers::index::RBX, 0x2000);
        run(&mut cpu, &[0xDB, 0x03]); // fild dword [rbx] -> push 5.0
        assert_eq!(cpu.fpu_st(0), 5.0);
        run(&mut cpu, &[0xD9, 0xE8]); // fld1 -> push 1.0
        assert_eq!(cpu.fpu_st(0), 1.0);
        run(&mut cpu, &[0xDE, 0xC1]); // faddp st(1), st(0): st1 = 6.0, pop
        assert_eq!(cpu.fpu_st(0), 6.0);
    }

    #[test]
    fn fistp_rounds_and_stores_an_integer() {
        let mut cpu = cpu();
        cpu.fpu_push(42.7);
        cpu.regs.set_gpr(crate::arch::registers::index::RBX, 0x2000);
        run(&mut cpu, &[0xDB, 0x1B]); // fistp dword [rbx]
        assert_eq!(cpu.memory.read_u32(0x2000).unwrap(), 43);
    }

    #[test]
    fn extended80_round_trips_a_double() {
        for value in [1.0_f64, -2.5, 1000.0, 0.0, std::f64::consts::PI] {
            let bytes = f64_to_extended80(value);
            assert_eq!(extended80_to_f64(&bytes), value, "round-trip {value}");
        }
    }
}
