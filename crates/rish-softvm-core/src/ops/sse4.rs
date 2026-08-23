//! SSE4.1 floating-point rounding and variable-mask blending.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use super::sse::{read_mem128, read_scalar_mem, read_xmm, write_xmm};
use crate::{CpuError, cpu::Cpu};

pub fn sse4_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Roundsd => round_scalar(cpu, instruction, true),
        Mnemonic::Roundss => round_scalar(cpu, instruction, false),
        Mnemonic::Roundpd => round_packed(cpu, instruction, true),
        Mnemonic::Roundps => round_packed(cpu, instruction, false),
        Mnemonic::Blendvpd => blend_variable(cpu, instruction, 64),
        Mnemonic::Blendvps => blend_variable(cpu, instruction, 32),
        Mnemonic::Pblendvb => blend_variable(cpu, instruction, 8),
        _ => Err(unimplemented(cpu, instruction)),
    }
}

fn round_scalar(cpu: &mut Cpu, instruction: &Instruction, double: bool) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let source = match instruction.op1_kind() {
        OpKind::Register => read_xmm(&cpu.regs, instruction.op1_register()),
        OpKind::Memory => {
            let bytes = if double { 8 } else { 4 };
            u128::from(read_scalar_mem(cpu, instruction, 1, bytes)?)
        }
        _ => return Err(unimplemented(cpu, instruction)),
    };
    let control = instruction.immediate(2) as u8;
    let destination_register = instruction.op0_register();
    let destination = read_xmm(&cpu.regs, destination_register);
    let result = if double {
        let rounded = round_f64(f64::from_bits(source as u64), rounding_mode(cpu, control));
        (destination & (u128::from(u64::MAX) << 64)) | u128::from(rounded.to_bits())
    } else {
        let rounded = round_f32(f32::from_bits(source as u32), rounding_mode(cpu, control));
        (destination & !u128::from(u32::MAX)) | u128::from(rounded.to_bits())
    };
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn round_packed(cpu: &mut Cpu, instruction: &Instruction, double: bool) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let source = read_source(cpu, instruction)?;
    let mode = rounding_mode(cpu, instruction.immediate(2) as u8);
    let result = if double {
        let low = round_f64(f64::from_bits(source as u64), mode).to_bits();
        let high = round_f64(f64::from_bits((source >> 64) as u64), mode).to_bits();
        u128::from(low) | (u128::from(high) << 64)
    } else {
        let mut packed = 0_u128;
        for lane in 0..4 {
            let value = f32::from_bits((source >> (lane * 32)) as u32);
            packed |= u128::from(round_f32(value, mode).to_bits()) << (lane * 32);
        }
        packed
    };
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn rounding_mode(cpu: &Cpu, control: u8) -> u8 {
    if control & 0b100 != 0 {
        ((cpu.mxcsr >> 13) & 0b11) as u8
    } else {
        control & 0b11
    }
}

fn round_f64(value: f64, mode: u8) -> f64 {
    match mode {
        0 => value.round_ties_even(),
        1 => value.floor(),
        2 => value.ceil(),
        _ => value.trunc(),
    }
}

fn round_f32(value: f32, mode: u8) -> f32 {
    match mode {
        0 => value.round_ties_even(),
        1 => value.floor(),
        2 => value.ceil(),
        _ => value.trunc(),
    }
}

fn blend_variable(
    cpu: &mut Cpu,
    instruction: &Instruction,
    lane_bits: u32,
) -> Result<(), CpuError> {
    if instruction.op0_kind() != OpKind::Register {
        return Err(unimplemented(cpu, instruction));
    }
    let destination_register = instruction.op0_register();
    let destination = read_xmm(&cpu.regs, destination_register);
    let source = read_source(cpu, instruction)?;
    let selection = read_xmm(&cpu.regs, Register::XMM0);
    let lanes = 128 / lane_bits;
    let lane_mask = if lane_bits == 64 {
        u128::from(u64::MAX)
    } else {
        (1_u128 << lane_bits) - 1
    };
    let mut result = 0_u128;
    for lane in 0..lanes {
        let shift = lane * lane_bits;
        let selected = if selection & (1_u128 << (shift + lane_bits - 1)) != 0 {
            source
        } else {
            destination
        };
        result |= ((selected >> shift) & lane_mask) << shift;
    }
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn read_source(cpu: &mut Cpu, instruction: &Instruction) -> Result<u128, CpuError> {
    match instruction.op1_kind() {
        OpKind::Register => Ok(read_xmm(&cpu.regs, instruction.op1_register())),
        OpKind::Memory => read_mem128(cpu, instruction, 1),
        _ => Err(unimplemented(cpu, instruction)),
    }
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

    #[test]
    fn roundsd_floors_the_source_and_preserves_the_high_lane() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = (0xDEAD_BEEF_CAFE_F00D_u128 << 64) | u128::from(99.0_f64.to_bits());
        cpu.regs.xmm[1] = u128::from((-3.25_f64).to_bits());

        // 66 0F 3A 0B C1 09: roundsd xmm0, xmm1, floor|no-exception
        run(&mut cpu, &[0x66, 0x0F, 0x3A, 0x0B, 0xC1, 0x09]).unwrap();

        assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), -4.0);
        assert_eq!(cpu.regs.xmm[0] >> 64, 0xDEAD_BEEF_CAFE_F00D);
    }

    #[test]
    fn blendvpd_selects_each_qword_from_the_xmm0_sign_bits() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[3] = (20_u128 << 64) | 10;
        cpu.regs.xmm[2] = (40_u128 << 64) | 30;
        cpu.regs.xmm[0] = 1_u128 << 127;

        // 66 0F 38 15 DA: blendvpd xmm3, xmm2
        run(&mut cpu, &[0x66, 0x0F, 0x38, 0x15, 0xDA]).unwrap();

        assert_eq!(cpu.regs.xmm[3], (40_u128 << 64) | 10);
    }
}
