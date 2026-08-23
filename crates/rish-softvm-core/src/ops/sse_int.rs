//! Packed integer instructions shared by SSE2 and later CPU profiles.

use iced_x86::{Instruction, Mnemonic, OpKind};

use super::sse::{read_mem128, read_scalar_mem, read_xmm, write_xmm};
use super::{operand_size, read_register, write_register};
use crate::{CpuError, cpu::Cpu};

pub fn packed_integer_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Packuswb => pack_signed_words_to_unsigned_bytes(cpu, instruction),
        Mnemonic::Palignr => align_right(cpu, instruction),
        Mnemonic::Pshufb => shuffle_bytes(cpu, instruction),
        Mnemonic::Pminsb => packed_min_max(cpu, instruction, 8, true, false),
        Mnemonic::Pminsd => packed_min_max(cpu, instruction, 32, true, false),
        Mnemonic::Pminuw => packed_min_max(cpu, instruction, 16, false, false),
        Mnemonic::Pminud => packed_min_max(cpu, instruction, 32, false, false),
        Mnemonic::Pmaxsb => packed_min_max(cpu, instruction, 8, true, true),
        Mnemonic::Pmaxsd => packed_min_max(cpu, instruction, 32, true, true),
        Mnemonic::Pmaxuw => packed_min_max(cpu, instruction, 16, false, true),
        Mnemonic::Pmaxud => packed_min_max(cpu, instruction, 32, false, true),
        Mnemonic::Ptest => packed_test(cpu, instruction),
        Mnemonic::Pmovsxbw => packed_extend(cpu, instruction, 8, 16, true),
        Mnemonic::Pmovsxbd => packed_extend(cpu, instruction, 8, 32, true),
        Mnemonic::Pmovsxbq => packed_extend(cpu, instruction, 8, 64, true),
        Mnemonic::Pmovsxwd => packed_extend(cpu, instruction, 16, 32, true),
        Mnemonic::Pmovsxwq => packed_extend(cpu, instruction, 16, 64, true),
        Mnemonic::Pmovsxdq => packed_extend(cpu, instruction, 32, 64, true),
        Mnemonic::Pmovzxbw => packed_extend(cpu, instruction, 8, 16, false),
        Mnemonic::Pmovzxbd => packed_extend(cpu, instruction, 8, 32, false),
        Mnemonic::Pmovzxbq => packed_extend(cpu, instruction, 8, 64, false),
        Mnemonic::Pmovzxwd => packed_extend(cpu, instruction, 16, 32, false),
        Mnemonic::Pmovzxwq => packed_extend(cpu, instruction, 16, 64, false),
        Mnemonic::Pmovzxdq => packed_extend(cpu, instruction, 32, 64, false),
        Mnemonic::Pinsrb => insert_integer(cpu, instruction, 8),
        Mnemonic::Pinsrd => insert_integer(cpu, instruction, 32),
        Mnemonic::Pinsrq => insert_integer(cpu, instruction, 64),
        Mnemonic::Pextrb => extract_integer(cpu, instruction, 8),
        Mnemonic::Pextrd | Mnemonic::Extractps => extract_integer(cpu, instruction, 32),
        Mnemonic::Pextrq => extract_integer(cpu, instruction, 64),
        Mnemonic::Insertps => insert_single(cpu, instruction),
        _ => Err(unimplemented(cpu, instruction)),
    }
}

fn packed_extend(
    cpu: &mut Cpu,
    instruction: &Instruction,
    source_bits: u32,
    destination_bits: u32,
    signed: bool,
) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let lanes = 128 / destination_bits;
    let source_bytes = (lanes * source_bits / 8) as usize;
    let source = match instruction.op1_kind() {
        OpKind::Register => read_xmm(&cpu.regs, instruction.op1_register()),
        OpKind::Memory => u128::from(read_scalar_mem(cpu, instruction, 1, source_bytes)?),
        _ => return Err(unimplemented(cpu, instruction)),
    };
    let source_mask = (1_u128 << source_bits) - 1;
    let destination_mask = if destination_bits == 64 {
        u128::from(u64::MAX)
    } else {
        (1_u128 << destination_bits) - 1
    };
    let mut result = 0_u128;
    for lane in 0..lanes {
        let value = (source >> (lane * source_bits)) & source_mask;
        let extended = if signed {
            signed_lane(value, source_bits) as i128 as u128
        } else {
            value
        };
        result |= (extended & destination_mask) << (lane * destination_bits);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn packed_test(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    use crate::arch::registers::RFlags;

    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let destination = read_xmm(&cpu.regs, instruction.op0_register());
    let source = read_vector_source(cpu, instruction)?;
    cpu.regs
        .rflags
        .remove(RFlags::AF | RFlags::OF | RFlags::PF | RFlags::SF | RFlags::ZF | RFlags::CF);
    if destination & source == 0 {
        cpu.regs.rflags.insert(RFlags::ZF);
    }
    if !destination & source == 0 {
        cpu.regs.rflags.insert(RFlags::CF);
    }
    Ok(())
}

fn packed_min_max(
    cpu: &mut Cpu,
    instruction: &Instruction,
    lane_bits: u32,
    signed: bool,
    maximum: bool,
) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let destination_register = instruction.op0_register();
    let destination = read_xmm(&cpu.regs, destination_register);
    let source = read_vector_source(cpu, instruction)?;
    let lane_mask = (1_u128 << lane_bits) - 1;
    let mut result = 0_u128;
    for lane in 0..(128 / lane_bits) {
        let shift = lane * lane_bits;
        let left = (destination >> shift) & lane_mask;
        let right = (source >> shift) & lane_mask;
        let ordering = if signed {
            signed_lane(left, lane_bits).cmp(&signed_lane(right, lane_bits))
        } else {
            left.cmp(&right)
        };
        let take_left = if maximum {
            ordering.is_ge()
        } else {
            ordering.is_le()
        };
        result |= (if take_left { left } else { right }) << shift;
    }
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn signed_lane(value: u128, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((value as u64) << shift) as i64 >> shift
}

fn align_right(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let destination_register = instruction.op0_register();
    let destination = read_xmm(&cpu.regs, destination_register).to_le_bytes();
    let source = read_vector_source(cpu, instruction)?.to_le_bytes();
    let offset = instruction.immediate(2) as usize;
    let mut result = [0_u8; 16];
    for (index, byte) in result.iter_mut().enumerate() {
        let source_index = offset + index;
        *byte = match source_index {
            0..=15 => source[source_index],
            16..=31 => destination[source_index - 16],
            _ => 0,
        };
    }
    write_xmm(
        &mut cpu.regs,
        destination_register,
        u128::from_le_bytes(result),
    );
    Ok(())
}

fn shuffle_bytes(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let destination_register = instruction.op0_register();
    let input = read_xmm(&cpu.regs, destination_register).to_le_bytes();
    let control = read_vector_source(cpu, instruction)?.to_le_bytes();
    let mut result = [0_u8; 16];
    for lane in 0..16 {
        result[lane] = if control[lane] & 0x80 != 0 {
            0
        } else {
            input[usize::from(control[lane] & 0x0F)]
        };
    }
    write_xmm(
        &mut cpu.regs,
        destination_register,
        u128::from_le_bytes(result),
    );
    Ok(())
}

fn read_vector_source(cpu: &mut Cpu, instruction: &Instruction) -> Result<u128, CpuError> {
    match instruction.op1_kind() {
        OpKind::Register => Ok(read_xmm(&cpu.regs, instruction.op1_register())),
        OpKind::Memory => read_mem128(cpu, instruction, 1),
        _ => Err(unimplemented(cpu, instruction)),
    }
}

fn insert_integer(
    cpu: &mut Cpu,
    instruction: &Instruction,
    lane_bits: u32,
) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let lanes = 128 / lane_bits;
    let lane = u32::from(instruction.immediate(2) as u8) & (lanes - 1);
    let bytes = (lane_bits / 8) as u8;
    let value = match instruction.op1_kind() {
        OpKind::Register => read_register(&cpu.regs, instruction.op1_register(), bytes),
        OpKind::Memory => read_scalar_mem(cpu, instruction, 1, usize::from(bytes))?,
        _ => return Err(unimplemented(cpu, instruction)),
    };
    let shift = lane * lane_bits;
    let mask = if lane_bits == 64 {
        u128::from(u64::MAX)
    } else {
        (1_u128 << lane_bits) - 1
    };
    let destination_register = instruction.op0_register();
    let destination = read_xmm(&cpu.regs, destination_register);
    let result = (destination & !(mask << shift)) | ((u128::from(value) & mask) << shift);
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn extract_integer(
    cpu: &mut Cpu,
    instruction: &Instruction,
    lane_bits: u32,
) -> Result<(), CpuError> {
    if instruction.op1_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let lanes = 128 / lane_bits;
    let lane = u32::from(instruction.immediate(2) as u8) & (lanes - 1);
    let mask = if lane_bits == 64 {
        u128::from(u64::MAX)
    } else {
        (1_u128 << lane_bits) - 1
    };
    let value =
        ((read_xmm(&cpu.regs, instruction.op1_register()) >> (lane * lane_bits)) & mask) as u64;
    let bytes = (lane_bits / 8) as u8;
    match instruction.op0_kind() {
        OpKind::Register => write_register(
            &mut cpu.regs,
            instruction.op0_register(),
            operand_size(instruction, 0),
            value,
        ),
        OpKind::Memory => cpu.write_operand(instruction, 0, bytes, value)?,
        _ => return Err(unimplemented(cpu, instruction)),
    }
    Ok(())
}

fn insert_single(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let control = instruction.immediate(2) as u8;
    let source = match instruction.op1_kind() {
        OpKind::Register => {
            let lane = u32::from(control >> 6);
            (read_xmm(&cpu.regs, instruction.op1_register()) >> (lane * 32)) as u32
        }
        OpKind::Memory => read_scalar_mem(cpu, instruction, 1, 4)? as u32,
        _ => return Err(unimplemented(cpu, instruction)),
    };
    let destination_register = instruction.op0_register();
    let destination_lane = u32::from((control >> 4) & 0b11);
    let mut result = read_xmm(&cpu.regs, destination_register);
    result = (result & !(u128::from(u32::MAX) << (destination_lane * 32)))
        | (u128::from(source) << (destination_lane * 32));
    for lane in 0..4 {
        if control & (1 << lane) != 0 {
            result &= !(u128::from(u32::MAX) << (lane * 32));
        }
    }
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn pack_signed_words_to_unsigned_bytes(
    cpu: &mut Cpu,
    instruction: &Instruction,
) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let destination_register = instruction.op0_register();
    let destination = read_xmm(&cpu.regs, destination_register);
    let source = match instruction.op1_kind() {
        OpKind::Register => read_xmm(&cpu.regs, instruction.op1_register()),
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => return Err(unimplemented(cpu, instruction)),
    };

    let mut result = 0_u128;
    for lane in 0..8 {
        let value = signed_word(destination, lane).clamp(0, 255) as u8;
        result |= u128::from(value) << (lane * 8);
    }
    for lane in 0..8 {
        let value = signed_word(source, lane).clamp(0, 255) as u8;
        result |= u128::from(value) << ((lane + 8) * 8);
    }
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn signed_word(value: u128, lane: u32) -> i16 {
    ((value >> (lane * 16)) as u16) as i16
}

fn unimplemented(cpu: &Cpu, instruction: &Instruction) -> CpuError {
    CpuError::UnimplementedInstruction {
        code: format!(
            "{:?} ({:?}, {:?})",
            instruction.mnemonic(),
            instruction.op0_kind(),
            instruction.op1_kind()
        ),
        address: cpu.regs.rip,
        bytes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
        let mut decoder =
            iced_x86::Decoder::with_ip(64, bytes, 0x1000, iced_x86::DecoderOptions::NONE);
        let instruction = decoder.decode();
        cpu.regs.rip = 0x1000 + instruction.len() as u64;
        cpu.dispatch(&instruction)
    }

    fn words(values: [i16; 8]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0_u128, |packed, (lane, value)| {
                packed | (u128::from(value as u16) << (lane * 16))
            })
    }

    #[test]
    fn packuswb_saturates_signed_words_from_both_operands() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = words([-1, 0, 1, 254, 255, 256, i16::MAX, i16::MIN]);
        cpu.regs.xmm[1] = words([42, -42, 300, 128, 127, 1024, -2, 7]);

        // 66 0F 67 C1: packuswb xmm0, xmm1
        run(&mut cpu, &[0x66, 0x0F, 0x67, 0xC1]).unwrap();

        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [
                0, 0, 1, 254, 255, 255, 255, 0, 42, 0, 255, 128, 127, 255, 0, 7
            ]
        );
    }

    #[test]
    fn pinsrd_replaces_only_the_selected_dword() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[3] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        cpu.regs.gpr[10] = 0xFFFF_FFFF_1234_5678;

        // 66 41 0F 3A 22 DA 02: pinsrd xmm3, r10d, 2
        run(&mut cpu, &[0x66, 0x41, 0x0F, 0x3A, 0x22, 0xDA, 0x02]).unwrap();

        assert_eq!(cpu.regs.xmm[3], 0xAAAA_AAAA_1234_5678_CCCC_CCCC_DDDD_DDDD);
    }

    #[test]
    fn pinsrq_replaces_only_the_selected_qword() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[1] = 0xAAAA_AAAA_AAAA_AAAA_BBBB_BBBB_BBBB_BBBB;
        cpu.regs.gpr[6] = 0x0123_4567_89AB_CDEF;

        // 66 48 0F 3A 22 CE 00: pinsrq xmm1, rsi, 0
        run(&mut cpu, &[0x66, 0x48, 0x0F, 0x3A, 0x22, 0xCE, 0x00]).unwrap();

        assert_eq!(cpu.regs.xmm[1], 0xAAAA_AAAA_AAAA_AAAA_0123_4567_89AB_CDEF);
    }

    #[test]
    fn pextrd_zero_extends_the_selected_dword() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        cpu.regs.gpr[0] = u64::MAX;

        // 66 0F 3A 16 C8 02: pextrd eax, xmm1, 2
        run(&mut cpu, &[0x66, 0x0F, 0x3A, 0x16, 0xC8, 0x02]).unwrap();

        assert_eq!(cpu.regs.gpr[0], 0xBBBB_BBBB);
    }

    #[test]
    fn insertps_selects_source_and_destination_lanes_then_zeroes() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
        cpu.regs.xmm[1] = 0x0000_0008_0000_0007_0000_0006_0000_0005;

        // Source lane 2 -> destination lane 1, then clear lanes 0 and 3.
        // 66 0F 3A 21 C1 99: insertps xmm0, xmm1, 0x99
        run(&mut cpu, &[0x66, 0x0F, 0x3A, 0x21, 0xC1, 0x99]).unwrap();

        assert_eq!(cpu.regs.xmm[0], 0x0000_0000_0000_0003_0000_0007_0000_0000);
    }

    #[test]
    fn palignr_splices_source_tail_with_destination_head() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] =
            u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        cpu.regs.xmm[1] = u128::from_le_bytes([
            16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
        ]);

        // 66 0F 3A 0F C8 0C: palignr xmm1, xmm0, 12
        run(&mut cpu, &[0x66, 0x0F, 0x3A, 0x0F, 0xC8, 0x0C]).unwrap();

        assert_eq!(
            cpu.regs.xmm[1].to_le_bytes(),
            [
                12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27
            ]
        );
    }

    #[test]
    fn pshufb_indexes_destination_bytes_and_zeroes_high_bit_controls() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[1] =
            u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        cpu.regs.xmm[2] =
            u128::from_le_bytes([15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0x80]);

        // 66 0F 38 00 CA: pshufb xmm1, xmm2
        run(&mut cpu, &[0x66, 0x0F, 0x38, 0x00, 0xCA]).unwrap();

        assert_eq!(
            cpu.regs.xmm[1].to_le_bytes(),
            [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]
        );
    }

    #[test]
    fn pmaxsd_compares_signed_dword_lanes() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::from_le_bytes([
            0xFF, 0xFF, 0xFF, 0xFF, // -1
            10, 0, 0, 0, // 10
            0, 0, 0, 0x80, // i32::MIN
            100, 0, 0, 0, // 100
        ]);
        cpu.regs.xmm[1] = u128::from_le_bytes([
            0xFE, 0xFF, 0xFF, 0xFF, // -2
            20, 0, 0, 0, // 20
            0xFF, 0xFF, 0xFF, 0x7F, // i32::MAX
            50, 0, 0, 0, // 50
        ]);

        // 66 0F 38 3D C1: pmaxsd xmm0, xmm1
        run(&mut cpu, &[0x66, 0x0F, 0x38, 0x3D, 0xC1]).unwrap();

        assert_eq!(
            cpu.regs.xmm[0].to_le_bytes(),
            [
                0xFF, 0xFF, 0xFF, 0xFF, 20, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x7F, 100, 0, 0, 0,
            ]
        );
    }

    #[test]
    fn ptest_sets_zero_and_carry_from_both_mask_tests() {
        use crate::arch::registers::RFlags;

        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = 0b0011;
        cpu.regs.xmm[1] = 0b0100;

        // 66 0F 38 17 C1: ptest xmm0, xmm1
        run(&mut cpu, &[0x66, 0x0F, 0x38, 0x17, 0xC1]).unwrap();
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        assert!(!cpu.regs.rflags.contains(RFlags::CF));

        cpu.regs.xmm[1] = 0b0010;
        run(&mut cpu, &[0x66, 0x0F, 0x38, 0x17, 0xC1]).unwrap();
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn pmovzxdq_zero_extends_two_low_dwords() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[1] = 0xDEAD_BEEF_CAFE_F00D_8000_0000_FFFF_FFFF;

        // 66 0F 38 35 C1: pmovzxdq xmm0, xmm1
        run(&mut cpu, &[0x66, 0x0F, 0x38, 0x35, 0xC1]).unwrap();

        assert_eq!(cpu.regs.xmm[0], 0x0000_0000_8000_0000_0000_0000_FFFF_FFFF);
    }
}
