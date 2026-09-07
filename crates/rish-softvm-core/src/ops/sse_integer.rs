//! Packed integer SSE2 operations used by unmodified Linux Harness binaries.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::{CpuError, cpu::Cpu};

pub(super) fn xmm_index(register: Register) -> Option<usize> {
    let index = (register as usize).checked_sub(Register::XMM0 as usize)?;
    (index < 16).then_some(index)
}

pub fn execute(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Pextrb => return extract(cpu, instruction, 1),
        Mnemonic::Pextrd | Mnemonic::Extractps => return extract(cpu, instruction, 4),
        Mnemonic::Pextrq => return extract(cpu, instruction, 8),
        _ => {}
    }
    let instruction_address = cpu.regs.rip;
    let unsupported = || CpuError::UnimplementedInstruction {
        code: format!("{:?}", instruction.mnemonic()),
        address: instruction_address,
        bytes: Vec::new(),
    };
    // The legacy MMX form aliases x87 state, not XMM state. Leave it closed
    // until that distinct register contract is implemented.
    let destination = xmm_index(instruction.op0_register()).ok_or_else(unsupported)?;
    if let Some(width) = match instruction.mnemonic() {
        Mnemonic::Pinsrb => Some(1),
        Mnemonic::Pinsrd => Some(4),
        Mnemonic::Pinsrq => Some(8),
        _ => None,
    } {
        let mut source = [0; 8];
        match instruction.op1_kind() {
            OpKind::Register => {
                source =
                    crate::ops::read_register(&cpu.regs, instruction.op1_register(), width as u8)
                        .to_le_bytes();
            }
            OpKind::Memory => {
                let address = cpu.effective_address(instruction, 1);
                cpu.read_linear_bytes(address, &mut source[..width])?;
            }
            _ => return Err(unsupported()),
        }
        let mut result = cpu.regs.xmm[destination].to_le_bytes();
        let lane = usize::from(instruction.immediate8()) & (16 / width - 1);
        result[lane * width..(lane + 1) * width].copy_from_slice(&source[..width]);
        cpu.regs.xmm[destination] = u128::from_le_bytes(result);
        return Ok(());
    }
    let left = cpu.regs.xmm[destination].to_le_bytes();
    let right = match instruction.op1_kind() {
        OpKind::Register => {
            let source = xmm_index(instruction.op1_register()).ok_or_else(unsupported)?;
            cpu.regs.xmm[source].to_le_bytes()
        }
        OpKind::Memory => {
            let address = cpu.effective_address(instruction, 1);
            let mut bytes = [0; 16];
            cpu.read_linear_bytes(address, &mut bytes)?;
            bytes
        }
        _ => return Err(unsupported()),
    };
    let mut result = [0; 16];
    match instruction.mnemonic() {
        Mnemonic::Pblendw | Mnemonic::Blendps | Mnemonic::Blendpd => {
            let width = match instruction.mnemonic() {
                Mnemonic::Pblendw => 2,
                Mnemonic::Blendps => 4,
                _ => 8,
            };
            for lane in 0..16 / width {
                let offset = lane * width;
                let source = if instruction.immediate8() & (1 << lane) != 0 {
                    &right
                } else {
                    &left
                };
                result[offset..offset + width].copy_from_slice(&source[offset..offset + width]);
            }
        }
        Mnemonic::Palignr => {
            let mut combined = [0; 32];
            combined[..16].copy_from_slice(&right);
            combined[16..].copy_from_slice(&left);
            let shift = usize::from(instruction.immediate8());
            for (lane, byte) in result.iter_mut().enumerate() {
                *byte = combined.get(shift + lane).copied().unwrap_or(0);
            }
        }
        Mnemonic::Pblendvb | Mnemonic::Blendvps | Mnemonic::Blendvpd => {
            let width = match instruction.mnemonic() {
                Mnemonic::Pblendvb => 1,
                Mnemonic::Blendvps => 4,
                _ => 8,
            };
            let mask = cpu.regs.xmm[0].to_le_bytes();
            for lane in 0..16 / width {
                let offset = lane * width;
                let source = if mask[offset + width - 1] & 0x80 != 0 {
                    &right
                } else {
                    &left
                };
                result[offset..offset + width].copy_from_slice(&source[offset..offset + width]);
            }
        }
        Mnemonic::Pmaddwd => {
            for lane in 0..4 {
                let offset = lane * 4;
                let a = i32::from(i16::from_le_bytes([left[offset], left[offset + 1]]));
                let b = i32::from(i16::from_le_bytes([right[offset], right[offset + 1]]));
                let c = i32::from(i16::from_le_bytes([left[offset + 2], left[offset + 3]]));
                let d = i32::from(i16::from_le_bytes([right[offset + 2], right[offset + 3]]));
                result[offset..offset + 4]
                    .copy_from_slice(&(a * b).wrapping_add(c * d).to_le_bytes());
            }
        }
        Mnemonic::Pmaddubsw => {
            for lane in 0..8 {
                let offset = lane * 2;
                let value = i32::from(left[offset]) * i32::from(right[offset] as i8)
                    + i32::from(left[offset + 1]) * i32::from(right[offset + 1] as i8);
                let value = value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
                result[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        Mnemonic::Pmulhuw | Mnemonic::Pmulhw | Mnemonic::Pmullw => {
            for lane in 0..8 {
                let offset = lane * 2;
                let a = u16::from_le_bytes([left[offset], left[offset + 1]]);
                let b = u16::from_le_bytes([right[offset], right[offset + 1]]);
                let product = match instruction.mnemonic() {
                    Mnemonic::Pmulhw => (i32::from(a as i16) * i32::from(b as i16)) as u32,
                    _ => u32::from(a) * u32::from(b),
                };
                let word = if instruction.mnemonic() == Mnemonic::Pmullw {
                    product as u16
                } else {
                    (product >> 16) as u16
                };
                result[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
            }
        }
        Mnemonic::Cvttps2dq => {
            let mut exceptions = 0;
            for (lane, bytes) in right.chunks_exact(4).enumerate() {
                let mut value = f32::from_le_bytes(bytes.try_into().expect("four-byte lane"));
                if cpu.mxcsr & 0x40 != 0 && value.is_subnormal() {
                    value = 0.0;
                }
                let value = f64::from(value);
                let integer = if !(-2147483648.0..2147483648.0).contains(&value) {
                    exceptions |= 1; // Invalid, including NaN and infinities.
                    i32::MIN
                } else {
                    let truncated = value.trunc();
                    if value != truncated {
                        exceptions |= 0x20;
                    }
                    truncated as i32
                };
                result[lane * 4..lane * 4 + 4].copy_from_slice(&integer.to_le_bytes());
            }
            cpu.mxcsr |= exceptions;
            if exceptions & !(cpu.mxcsr >> 7) != 0 {
                // Exception delivery is outside this subset; do not silently
                // commit a result when the guest requests an unmasked fault.
                return Err(CpuError::GuestFault(
                    "unmasked SIMD conversion exception".into(),
                ));
            }
        }
        Mnemonic::Packssdw | Mnemonic::Packsswb => {
            let width = if instruction.mnemonic() == Mnemonic::Packssdw {
                4
            } else {
                2
            };
            for (source, offset) in [(&left, 0), (&right, 8)] {
                for (lane, value) in source.chunks_exact(width).enumerate() {
                    if width == 4 {
                        let value = i32::from_le_bytes([value[0], value[1], value[2], value[3]])
                            .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                            as i16;
                        result[offset + lane * 2..offset + lane * 2 + 2]
                            .copy_from_slice(&value.to_le_bytes());
                    } else {
                        result[offset + lane] = i16::from_le_bytes([value[0], value[1]])
                            .clamp(i16::from(i8::MIN), i16::from(i8::MAX))
                            as i8 as u8;
                    }
                }
            }
        }
        Mnemonic::Pshufb => {
            for (lane, control) in right.iter().enumerate() {
                result[lane] = if control & 0x80 != 0 {
                    0
                } else {
                    left[usize::from(control & 0x0f)]
                };
            }
        }
        Mnemonic::Ptest => {
            use crate::arch::registers::RFlags;
            let left = u128::from_le_bytes(left);
            let right = u128::from_le_bytes(right);
            cpu.regs
                .rflags
                .remove(RFlags::OF | RFlags::SF | RFlags::AF | RFlags::PF);
            cpu.regs.rflags.set(RFlags::ZF, left & right == 0);
            cpu.regs.rflags.set(RFlags::CF, !left & right == 0);
            return Ok(());
        }
        Mnemonic::Psadbw => {
            for group in 0..2 {
                let start = group * 8;
                let sum: u64 = left[start..start + 8]
                    .iter()
                    .zip(&right[start..start + 8])
                    .map(|(a, b)| u64::from(a.abs_diff(*b)))
                    .sum();
                result[start..start + 8].copy_from_slice(&sum.to_le_bytes());
            }
        }
        Mnemonic::Packuswb => {
            for (source, offset) in [(&left, 0), (&right, 8)] {
                for (lane, word) in source.chunks_exact(2).enumerate() {
                    result[offset + lane] =
                        i16::from_le_bytes([word[0], word[1]]).clamp(0, 255) as u8;
                }
            }
        }
        _ => return Err(unsupported()),
    }
    // Commit only after all source bytes have been read successfully.
    cpu.regs.xmm[destination] = u128::from_le_bytes(result);
    Ok(())
}

pub(super) fn extract(
    cpu: &mut Cpu,
    instruction: &Instruction,
    width: usize,
) -> Result<(), CpuError> {
    let source = xmm_index(instruction.op1_register()).ok_or_else(|| {
        CpuError::UnimplementedInstruction {
            code: format!("{:?}", instruction.mnemonic()),
            address: cpu.regs.rip,
            bytes: Vec::new(),
        }
    })?;
    let lane = usize::from(instruction.immediate8()) & (16 / width - 1);
    let bytes = cpu.regs.xmm[source].to_le_bytes();
    let mut value = [0; 8];
    value[..width].copy_from_slice(&bytes[lane * width..(lane + 1) * width]);
    if instruction.op0_kind() == OpKind::Memory {
        let address = cpu.effective_address(instruction, 0);
        cpu.write_linear_bytes(address, &value[..width])?;
    } else {
        crate::ops::write_register(
            &mut cpu.regs,
            instruction.op0_register(),
            if width == 8 { 8 } else { 4 },
            u64::from_le_bytes(value),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{CpuError, cpu::Cpu};
    use iced_x86::{Decoder, DecoderOptions};

    fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
        cpu.regs.efer |= crate::arch::registers::Efer::LMA;
        cpu.regs.cs.long_mode = true;
        let instruction = Decoder::with_ip(64, bytes, 0x1000, DecoderOptions::NONE).decode();
        cpu.dispatch(&instruction)
    }

    fn words(values: [i16; 8]) -> u128 {
        let mut bytes = [0; 16];
        for (index, value) in values.iter().enumerate() {
            bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        u128::from_le_bytes(bytes)
    }

    #[test]
    fn psadbw_sums_two_unsigned_groups_and_clears_unused_bits() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[8] = u128::from_le_bytes([
            255, 255, 255, 255, 255, 255, 255, 255, 1, 2, 3, 4, 5, 6, 7, 8,
        ]);
        cpu.regs.xmm[9] = u128::from_le_bytes([0, 0, 0, 0, 0, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1]);
        let flags = cpu.regs.rflags;
        run(&mut cpu, &[0x66, 0x45, 0x0f, 0xf6, 0xc1]).unwrap();
        assert_eq!(cpu.regs.xmm[8], 2040 | (32_u128 << 64));
        assert_eq!(cpu.regs.rflags, flags);
    }

    #[test]
    fn psadbw_reads_memory_and_self_alias_is_zero() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::MAX;
        cpu.regs.gpr[0] = 0x2000;
        cpu.memory.write(0x2000, &[128; 16]).unwrap();
        run(&mut cpu, &[0x66, 0x0f, 0xf6, 0x00]).unwrap();
        assert_eq!(cpu.regs.xmm[0], 1016 | (1016_u128 << 64));
        run(&mut cpu, &[0x66, 0x0f, 0xf6, 0xc0]).unwrap();
        assert_eq!(cpu.regs.xmm[0], 0);
    }

    #[test]
    fn packuswb_saturates_signed_words_and_preserves_source() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = words([-32768, -1, 0, 1, 254, 255, 256, 32767]);
        cpu.regs.xmm[1] = words([4, 300, -300, 0, 42, 255, 256, 32767]);
        let source = cpu.regs.xmm[1];
        let flags = cpu.regs.rflags;
        run(&mut cpu, &[0x66, 0x0f, 0x67, 0xc1]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [
                0, 0, 0, 1, 254, 255, 255, 255, 4, 255, 0, 0, 42, 255, 255, 255
            ]
        );
        assert_eq!(cpu.regs.xmm[1], source);
        assert_eq!(cpu.regs.rflags, flags);
    }

    #[test]
    fn packuswb_memory_and_alias_forms_keep_lane_order() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = words([1, 2, 3, 4, 5, 6, 7, 8]);
        cpu.regs.gpr[0] = 0x2000;
        cpu.memory
            .write(
                0x2000,
                &words([9, 10, 11, 12, 13, 14, 15, 16]).to_le_bytes(),
            )
            .unwrap();
        run(&mut cpu, &[0x66, 0x0f, 0x67, 0x00]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        cpu.regs.xmm[0] = words([-1, 0, 1, 255, 256, 20, 30, 40]);
        run(&mut cpu, &[0x66, 0x0f, 0x67, 0xc0]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [0, 0, 1, 255, 255, 20, 30, 40, 0, 0, 1, 255, 255, 20, 30, 40]
        );
    }

    #[test]
    fn memory_fault_and_unsupported_mmx_do_not_mutate_xmm() {
        for opcode in [0xf6, 0x67] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.xmm[0] = u128::MAX;
            cpu.regs.gpr[0] = 0xffff8;
            assert!(run(&mut cpu, &[0x66, 0x0f, opcode, 0x00]).is_err());
            assert_eq!(cpu.regs.xmm[0], u128::MAX);
            assert!(matches!(
                run(&mut cpu, &[0x0f, opcode, 0xc1]),
                Err(CpuError::UnimplementedInstruction { .. })
            ));
            assert_eq!(cpu.regs.xmm[0], u128::MAX);
        }
    }

    #[test]
    fn pinsr_register_forms_mask_lane_and_preserve_other_bytes() {
        for (bytes, width) in [
            (vec![0x66, 0x0f, 0x3a, 0x20, 0xc1, 0xff], 1),
            (vec![0x66, 0x0f, 0x3a, 0x22, 0xc1, 0xff], 4),
            (vec![0x66, 0x48, 0x0f, 0x3a, 0x22, 0xc1, 0xff], 8),
        ] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.xmm[0] = u128::from_le_bytes([0x11; 16]);
            cpu.regs.gpr[1] = 0xaabb_ccdd_89ab_cdef;
            let flags = cpu.regs.rflags;
            run(&mut cpu, &bytes).unwrap();
            let mut expected = [0x11; 16];
            expected[16 - width..].copy_from_slice(&cpu.regs.gpr[1].to_le_bytes()[..width]);
            assert_eq!(cpu.regs.xmm[0].to_le_bytes(), expected);
            assert_eq!(cpu.regs.rflags, flags);
        }
    }

    #[test]
    fn pinsrd_memory_reads_only_four_bytes_and_fault_is_atomic() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::MAX;
        cpu.regs.gpr[0] = 0xffffc;
        cpu.memory.write(0xffffc, &[1, 2, 3, 4]).unwrap();
        run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x22, 0x00, 2]).unwrap();
        let mut expected = [255; 16];
        expected[8..12].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(cpu.regs.xmm[0].to_le_bytes(), expected);
        cpu.regs.gpr[0] = 0xffffd;
        assert!(run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x22, 0x00, 2]).is_err());
        assert_eq!(cpu.regs.xmm[0].to_le_bytes(), expected);
    }

    #[test]
    fn ptest_updates_zf_cf_and_clears_only_defined_flags() {
        use crate::arch::registers::RFlags;
        for (left, right, zero, carry) in [
            (1, 3, false, false),
            (3, 1, false, true),
            (2, 1, true, false),
            (0, 0, true, true),
        ] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.xmm[0] = left << 100;
            cpu.regs.xmm[1] = right << 100;
            cpu.regs.rflags |= RFlags::IF | RFlags::OF | RFlags::SF | RFlags::AF | RFlags::PF;
            let original = cpu.regs.xmm;
            run(&mut cpu, &[0x66, 0x0f, 0x38, 0x17, 0xc1]).unwrap();
            assert_eq!(cpu.regs.rflags.contains(RFlags::ZF), zero);
            assert_eq!(cpu.regs.rflags.contains(RFlags::CF), carry);
            assert!(cpu.regs.rflags.contains(RFlags::IF));
            assert!(
                !cpu.regs
                    .rflags
                    .intersects(RFlags::OF | RFlags::SF | RFlags::AF | RFlags::PF)
            );
            assert_eq!(cpu.regs.xmm, original);
        }
    }

    #[test]
    fn cvttps2dq_truncates_and_marks_inexact_ignoring_rounding_mode() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        let floats = [-1.9_f32, 1.9, 0.0, -2147483648.0];
        let mut bytes = [0; 16];
        for (i, value) in floats.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        cpu.regs.xmm[1] = u128::from_le_bytes(bytes);
        cpu.mxcsr |= 0x4000;
        let flags = cpu.regs.rflags;
        run(&mut cpu, &[0xf3, 0x0f, 0x5b, 0xc1]).unwrap();
        let result = cpu.regs.xmm[0].to_le_bytes();
        let values: Vec<i32> = result
            .chunks_exact(4)
            .map(|x| i32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        assert_eq!(values, [-1, 1, 0, i32::MIN]);
        assert_eq!(cpu.mxcsr & 0x21, 0x20);
        assert_eq!(cpu.regs.rflags, flags);
    }

    #[test]
    fn cvttps2dq_invalid_returns_indefinite_and_unmasked_fault_preserves_destination() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[1] = u128::from(f32::NAN.to_bits());
        run(&mut cpu, &[0xf3, 0x0f, 0x5b, 0xc1]).unwrap();
        assert_eq!(cpu.regs.xmm[0], u128::from(i32::MIN as u32));
        assert_eq!(cpu.mxcsr & 1, 1);
        cpu.regs.xmm[0] = u128::MAX;
        cpu.mxcsr &= !0x80;
        assert!(run(&mut cpu, &[0xf3, 0x0f, 0x5b, 0xc1]).is_err());
        assert_eq!(cpu.regs.xmm[0], u128::MAX);
    }

    #[test]
    fn widening_moves_select_correct_source_width_and_extension() {
        for (opcode, source_width, destination_width) in [
            (0x20, 1, 2),
            (0x21, 1, 4),
            (0x22, 1, 8),
            (0x23, 2, 4),
            (0x24, 2, 8),
            (0x25, 4, 8),
        ] {
            for signed in [false, true] {
                let mut cpu = Cpu::new(1, 0).unwrap();
                let mask = (1_u64 << (source_width * 8)) - 1;
                let sign = 1_u64 << (source_width * 8 - 1);
                let values = [sign, mask, 1, 0];
                let mut input = [0; 16];
                let mut expected = [0; 16];
                for lane in 0..16 / destination_width {
                    let value = values[lane % 4];
                    input[lane * source_width..(lane + 1) * source_width]
                        .copy_from_slice(&value.to_le_bytes()[..source_width]);
                    let value = if signed && value & sign != 0 {
                        value | !mask
                    } else {
                        value
                    };
                    expected[lane * destination_width..(lane + 1) * destination_width]
                        .copy_from_slice(&value.to_le_bytes()[..destination_width]);
                }
                cpu.regs.xmm[1] = u128::from_le_bytes(input);
                run(
                    &mut cpu,
                    &[
                        0x66,
                        0x0f,
                        0x38,
                        opcode + if signed { 0 } else { 0x10 },
                        0xc1,
                    ],
                )
                .unwrap();
                assert_eq!(cpu.regs.xmm[0].to_le_bytes(), expected);
            }
        }
    }

    #[test]
    fn pmovzxdq_memory_reads_eight_bytes_not_sixteen() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.gpr[0] = 0xffff8;
        cpu.memory
            .write(0xffff8, &[255, 255, 255, 255, 0, 0, 0, 128])
            .unwrap();
        run(&mut cpu, &[0x66, 0x0f, 0x38, 0x35, 0x00]).unwrap();
        assert_eq!(cpu.regs.xmm[0], 0x8000_0000_u128 << 64 | 0xffff_ffff);
    }

    #[test]
    fn packed_extraction_masks_lane_and_zero_extends_gpr_destinations() {
        for (prefix, opcode, expected) in [
            (vec![0x66], 0x16, 0x0f0e0d0c_u64),
            (vec![0x66], 0x14, 15),
            (vec![0x66, 0x48], 0x16, 0x0f0e0d0c0b0a0908),
            (vec![0x66], 0x17, 0x0f0e0d0c),
        ] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.gpr[0] = u64::MAX;
            cpu.regs.xmm[1] =
                u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
            let mut bytes = prefix;
            bytes.extend_from_slice(&[0x0f, 0x3a, opcode, 0xc8, 0xff]);
            run(&mut cpu, &bytes).unwrap();
            assert_eq!(cpu.regs.gpr[0], expected);
        }
    }

    #[test]
    fn pextrw_memory_form_writes_only_the_selected_word() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.gpr[0] = 0x2000;
        cpu.regs.xmm[1] =
            u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        cpu.memory.write(0x2000, &[0xa5; 8]).unwrap();
        run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x15, 0x08, 9]).unwrap();
        let mut result = [0; 8];
        cpu.memory.read(0x2000, &mut result).unwrap();
        assert_eq!(result, [2, 3, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5]);
        assert_eq!(cpu.regs.gpr[0], 0x2000);
    }

    #[test]
    fn palignr_orders_the_sources_and_zero_fills_past_32_bytes() {
        for shift in [0, 1, 15, 16, 17, 31, 32, 255] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.xmm[0] = u128::from_le_bytes([
                16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
            ]);
            cpu.regs.xmm[1] =
                u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
            run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x0f, 0xc1, shift]).unwrap();
            let mut expected = [0; 16];
            for (i, byte) in expected.iter_mut().enumerate() {
                if i + usize::from(shift) < 32 {
                    *byte = (i + usize::from(shift)) as u8;
                }
            }
            assert_eq!(cpu.regs.xmm[0].to_le_bytes(), expected);
        }
    }

    #[test]
    fn immediate_blends_select_complete_lanes_for_all_masks() {
        for (opcode, width) in [(0x0e, 2), (0x0c, 4), (0x0d, 8)] {
            for mask in 0..=255u8 {
                let mut cpu = Cpu::new(1, 0).unwrap();
                cpu.regs.xmm[0] = u128::from_le_bytes([0x11; 16]);
                cpu.regs.xmm[1] = u128::from_le_bytes([0x82; 16]);
                let flags = cpu.regs.rflags;
                run(&mut cpu, &[0x66, 0x0f, 0x3a, opcode, 0xc1, mask]).unwrap();
                let mut expected = [0x11; 16];
                for lane in 0..16 / width {
                    if mask & (1 << lane) != 0 {
                        expected[lane * width..(lane + 1) * width].fill(0x82);
                    }
                }
                assert_eq!(cpu.regs.xmm[0].to_le_bytes(), expected);
                assert_eq!(cpu.regs.rflags, flags);
            }
        }
    }

    #[test]
    fn variable_blends_select_by_each_lane_sign_bit_of_xmm_zero() {
        for (opcode, width) in [(0x10, 1), (0x14, 4), (0x15, 8)] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            let left = [1; 16];
            let right = [2; 16];
            let mut mask = [255; 16];
            let mut expected = left;
            for lane in 0..16 / width {
                mask[(lane + 1) * width - 1] = if lane % 2 == 0 { 128 } else { 127 };
                if lane % 2 == 0 {
                    expected[lane * width..(lane + 1) * width].fill(2);
                }
            }
            cpu.regs.xmm[0] = u128::from_le_bytes(mask);
            cpu.regs.xmm[1] = u128::from_le_bytes(right);
            cpu.regs.xmm[2] = u128::from_le_bytes(left);
            let flags = cpu.regs.rflags;
            run(&mut cpu, &[0x66, 0x0f, 0x38, opcode, 0xd1]).unwrap();
            assert_eq!(cpu.regs.xmm[2].to_le_bytes(), expected);
            assert_eq!(cpu.regs.xmm[0].to_le_bytes(), mask);
            assert_eq!(cpu.regs.rflags, flags);
        }
    }

    #[test]
    fn pmaddwd_wraps_the_signed_overflow_pair() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = words([-32768, -32768, 2, -3, 32767, -32768, 0, 1]);
        cpu.regs.xmm[1] = words([-32768, -32768, 4, 5, -1, 1, 32767, -1]);
        run(&mut cpu, &[0x66, 0x0f, 0xf5, 0xc1]).unwrap();
        let bytes = cpu.regs.xmm[0].to_le_bytes();
        let result: Vec<i32> = bytes
            .chunks_exact(4)
            .map(|x| i32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        assert_eq!(result, [i32::MIN, -7, -65535, -1]);
    }

    #[test]
    fn pmaddubsw_uses_unsigned_left_signed_right_and_saturates() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::from_le_bytes([
            255, 255, 255, 255, 1, 2, 0, 3, 128, 128, 10, 20, 42, 0, 1, 1,
        ]);
        cpu.regs.xmm[1] = u128::from_le_bytes([
            127, 127, 128, 128, 3, 254, 128, 2, 255, 1, 255, 1, 1, 128, 127, 128,
        ]);
        run(&mut cpu, &[0x66, 0x0f, 0x38, 0x04, 0xc1]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0],
            words([32767, -32768, -1, 6, 0, 10, 42, -1])
        );
    }

    #[test]
    fn packed_word_multiplication_selects_signed_or_unsigned_half() {
        for (opcode, left, right, expected) in [
            (
                0xe4,
                [-1, -32768, -1, 2, 1, 0, 256, 256],
                [-1, 2, 1, -32768, -1, 42, 256, 255],
                [-2, 1, 0, 1, 0, 0, 1, 0],
            ),
            (
                0xe5,
                [-32768, -1, 32767, 2, -2, 0, 256, -256],
                [2, -1, 32767, -32768, 32767, 42, 256, 256],
                [-1, 0, 16383, -1, -1, 0, 1, -1],
            ),
            (
                0xd5,
                [-1, -32768, -1, 2, 1, 0, 256, 256],
                [-1, 2, 1, -32768, -1, 42, 256, 255],
                [1, 0, -1, 0, -1, 0, 0, -256],
            ),
        ] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.xmm[0] = words(left);
            cpu.regs.xmm[1] = words(right);
            let flags = cpu.regs.rflags;
            run(&mut cpu, &[0x66, 0x0f, opcode, 0xc1]).unwrap();
            assert_eq!(cpu.regs.xmm[0], words(expected));
            assert_eq!(cpu.regs.rflags, flags);
        }
    }

    #[test]
    fn packssdw_clamps_signed_dwords_to_signed_words() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        let pack = |values: [i32; 4]| {
            let mut bytes = [0; 16];
            for (i, value) in values.iter().enumerate() {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            u128::from_le_bytes(bytes)
        };
        cpu.regs.xmm[0] = pack([-32769, -32768, 32767, 32768]);
        cpu.regs.xmm[1] = pack([0, -1, i32::MIN, i32::MAX]);
        let flags = cpu.regs.rflags;
        run(&mut cpu, &[0x66, 0x0f, 0x6b, 0xc1]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0],
            words([-32768, -32768, 32767, 32767, 0, -1, -32768, 32767])
        );
        assert_eq!(cpu.regs.rflags, flags);
    }

    #[test]
    fn packsswb_clamps_signed_words_and_self_aliases() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = words([-129, -128, -1, 0, 1, 127, 128, 32767]);
        run(&mut cpu, &[0x66, 0x0f, 0x63, 0xc0]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [
                128, 128, 255, 0, 1, 127, 127, 127, 128, 128, 255, 0, 1, 127, 127, 127
            ]
        );
    }

    #[test]
    fn pshufb_masks_indices_and_zeroes_high_bit_controls() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::from_le_bytes([
            10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
        ]);
        cpu.regs.xmm[1] = u128::from_le_bytes([
            15, 14, 13, 12, 11, 10, 9, 8, 0x70, 0x71, 0x80, 0xff, 4, 4, 0x7f, 0x9f,
        ]);
        let flags = cpu.regs.rflags;
        run(&mut cpu, &[0x66, 0x0f, 0x38, 0x00, 0xc1]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [25, 24, 23, 22, 21, 20, 19, 18, 10, 11, 0, 0, 14, 14, 25, 0]
        );
        assert_eq!(cpu.regs.rflags, flags);
        cpu.regs.xmm[0] =
            u128::from_le_bytes([15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
        run(&mut cpu, &[0x66, 0x0f, 0x38, 0x00, 0xc0]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }
}
