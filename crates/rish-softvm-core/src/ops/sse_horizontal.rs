//! Horizontal SSE3 arithmetic, using the interpreter's scalar FP backend.

use crate::{CpuError, cpu::Cpu};
use iced_x86::{Instruction, Mnemonic, OpKind};

pub fn execute(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let address = cpu.regs.rip;
    let unsupported = || CpuError::UnimplementedInstruction {
        code: format!("{:?}", instruction.mnemonic()),
        address,
        bytes: Vec::new(),
    };
    if cpu.mxcsr & 0x6000 != 0 || cpu.mxcsr & 0x1f80 != 0x1f80 {
        return Err(CpuError::GuestFault(
            "horizontal FP mode is not implemented".into(),
        ));
    }
    let destination =
        super::sse_integer::xmm_index(instruction.op0_register()).ok_or_else(unsupported)?;
    let left = cpu.regs.xmm[destination].to_le_bytes();
    let mut right = [0; 16];
    match instruction.op1_kind() {
        OpKind::Register => {
            let index = super::sse_integer::xmm_index(instruction.op1_register())
                .ok_or_else(unsupported)?;
            right = cpu.regs.xmm[index].to_le_bytes();
        }
        OpKind::Memory => {
            let address = cpu.effective_address(instruction, 1);
            cpu.read_linear_bytes(address, &mut right)?;
        }
        _ => return Err(unsupported()),
    }
    let width = match instruction.mnemonic() {
        Mnemonic::Haddpd | Mnemonic::Hsubpd => 8,
        _ => 4,
    };
    let subtract = matches!(instruction.mnemonic(), Mnemonic::Hsubpd | Mnemonic::Hsubps);
    let mut result = [0; 16];
    let mut exceptions = 0;
    for (source, output_offset) in [(&left, 0), (&right, 8)] {
        for pair in 0..8 / width {
            let offset = pair * width * 2;
            let read = |offset: usize| {
                if width == 8 {
                    f64::from_le_bytes(source[offset..offset + 8].try_into().unwrap())
                } else {
                    f64::from(f32::from_le_bytes(
                        source[offset..offset + 4].try_into().unwrap(),
                    ))
                }
            };
            let mut a = read(offset);
            let mut b = read(offset + width);
            if !a.is_finite() || !b.is_finite() {
                return Err(CpuError::GuestFault(
                    "horizontal non-finite FP operands are not implemented".into(),
                ));
            }
            let subnormal = |value: f64| {
                if width == 8 {
                    value.is_subnormal()
                } else {
                    (value as f32).is_subnormal()
                }
            };
            for value in [&mut a, &mut b] {
                if subnormal(*value) {
                    if cpu.mxcsr & 0x40 != 0 {
                        *value = 0.0_f64.copysign(*value);
                    } else {
                        exceptions |= 2;
                    }
                }
            }
            if subtract {
                b = -b;
            }
            let sum = a + b;
            let mut rounded = if width == 8 {
                sum
            } else {
                f64::from((a as f32) + (b as f32))
            };
            if !rounded.is_finite() {
                exceptions |= 0x28;
            } else {
                let virtual_b = sum - a;
                let residual = (a - (sum - virtual_b)) + (b - virtual_b);
                if rounded != sum || residual != 0.0 {
                    exceptions |= 0x20;
                }
                if cpu.mxcsr & 0x8000 != 0 && subnormal(rounded) {
                    rounded = 0.0_f64.copysign(rounded);
                    exceptions |= 0x30;
                }
            }
            let out = output_offset + pair * width;
            if width == 8 {
                result[out..out + 8].copy_from_slice(&rounded.to_le_bytes());
            } else {
                result[out..out + 4].copy_from_slice(&(rounded as f32).to_le_bytes());
            }
        }
    }
    cpu.mxcsr |= exceptions;
    cpu.regs.xmm[destination] = u128::from_le_bytes(result);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{CpuError, cpu::Cpu};
    use iced_x86::{Decoder, DecoderOptions};

    fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
        cpu.regs.efer |= crate::arch::registers::Efer::LMA;
        cpu.regs.cs.long_mode = true;
        cpu.dispatch(&Decoder::with_ip(64, bytes, 0x1000, DecoderOptions::NONE).decode())
    }

    #[test]
    fn horizontal_double_add_and_subtract_keep_operand_order() {
        for (opcode, expected) in [(0x7c, [4.0, 30.0]), (0x7d, [-1.0, -10.0])] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.xmm[0] = u128::from(1.5_f64.to_bits()) | (u128::from(2.5_f64.to_bits()) << 64);
            cpu.regs.xmm[1] = u128::from(10_f64.to_bits()) | (u128::from(20_f64.to_bits()) << 64);
            let flags = cpu.regs.rflags;
            run(&mut cpu, &[0x66, 0x0f, opcode, 0xc1]).unwrap();
            assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), expected[0]);
            assert_eq!(f64::from_bits((cpu.regs.xmm[0] >> 64) as u64), expected[1]);
            assert_eq!(cpu.regs.rflags, flags);
        }
    }

    #[test]
    fn horizontal_single_precision_groups_adjacent_pairs() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        let mut left = [0; 16];
        let mut right = [0; 16];
        for (i, value) in [1_f32, 2.0, 3.0, 4.0].iter().enumerate() {
            left[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
            right[i * 4..i * 4 + 4].copy_from_slice(&(value * 10.0).to_le_bytes());
        }
        cpu.regs.xmm[0] = u128::from_le_bytes(left);
        cpu.regs.xmm[1] = u128::from_le_bytes(right);
        run(&mut cpu, &[0xf2, 0x0f, 0x7c, 0xc1]).unwrap();
        let bytes = cpu.regs.xmm[0].to_le_bytes();
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        assert_eq!(values, [3.0, 7.0, 30.0, 70.0]);
    }
}
