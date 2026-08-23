//! SSE3 horizontal arithmetic and alternating add/subtract instructions.

use iced_x86::{Instruction, Mnemonic, OpKind};

use super::sse::{read_mem128, read_xmm, write_xmm};
use crate::{CpuError, cpu::Cpu};

pub fn sse3_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Haddpd => horizontal_f64(cpu, instruction, false),
        Mnemonic::Hsubpd => horizontal_f64(cpu, instruction, true),
        Mnemonic::Haddps => horizontal_f32(cpu, instruction, false),
        Mnemonic::Hsubps => horizontal_f32(cpu, instruction, true),
        Mnemonic::Addsubpd => alternating_f64(cpu, instruction),
        Mnemonic::Addsubps => alternating_f32(cpu, instruction),
        _ => Err(unimplemented(cpu, instruction)),
    }
}

fn horizontal_f64(
    cpu: &mut Cpu,
    instruction: &Instruction,
    subtract: bool,
) -> Result<(), CpuError> {
    let destination_register = require_destination(cpu, instruction)?;
    let destination = read_xmm(&cpu.regs, destination_register);
    let source = read_source(cpu, instruction)?;
    let combine = |value: u128| {
        let low = f64::from_bits(value as u64);
        let high = f64::from_bits((value >> 64) as u64);
        if subtract { low - high } else { low + high }
    };
    let low = combine(destination).to_bits();
    let high = combine(source).to_bits();
    write_xmm(
        &mut cpu.regs,
        destination_register,
        u128::from(low) | (u128::from(high) << 64),
    );
    Ok(())
}

fn horizontal_f32(
    cpu: &mut Cpu,
    instruction: &Instruction,
    subtract: bool,
) -> Result<(), CpuError> {
    let destination_register = require_destination(cpu, instruction)?;
    let destination = read_xmm(&cpu.regs, destination_register);
    let source = read_source(cpu, instruction)?;
    let combine = |value: u128, low_lane: u32| {
        let left = f32::from_bits((value >> (low_lane * 32)) as u32);
        let right = f32::from_bits((value >> ((low_lane + 1) * 32)) as u32);
        if subtract { left - right } else { left + right }
    };
    let lanes = [
        combine(destination, 0),
        combine(destination, 2),
        combine(source, 0),
        combine(source, 2),
    ];
    let mut result = 0_u128;
    for (lane, value) in lanes.into_iter().enumerate() {
        result |= u128::from(value.to_bits()) << (lane * 32);
    }
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn alternating_f64(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let destination_register = require_destination(cpu, instruction)?;
    let destination = read_xmm(&cpu.regs, destination_register);
    let source = read_source(cpu, instruction)?;
    let low = f64::from_bits(destination as u64) - f64::from_bits(source as u64);
    let high = f64::from_bits((destination >> 64) as u64) + f64::from_bits((source >> 64) as u64);
    write_xmm(
        &mut cpu.regs,
        destination_register,
        u128::from(low.to_bits()) | (u128::from(high.to_bits()) << 64),
    );
    Ok(())
}

fn alternating_f32(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let destination_register = require_destination(cpu, instruction)?;
    let destination = read_xmm(&cpu.regs, destination_register);
    let source = read_source(cpu, instruction)?;
    let mut result = 0_u128;
    for lane in 0..4 {
        let left = f32::from_bits((destination >> (lane * 32)) as u32);
        let right = f32::from_bits((source >> (lane * 32)) as u32);
        let value = if lane % 2 == 0 {
            left - right
        } else {
            left + right
        };
        result |= u128::from(value.to_bits()) << (lane * 32);
    }
    write_xmm(&mut cpu.regs, destination_register, result);
    Ok(())
}

fn require_destination(
    cpu: &Cpu,
    instruction: &Instruction,
) -> Result<iced_x86::Register, CpuError> {
    if instruction.op0_kind() == OpKind::Register {
        Ok(instruction.op0_register())
    } else {
        Err(unimplemented(cpu, instruction))
    }
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
    fn haddpd_sums_each_operand_into_one_result_lane() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::from(2.0_f64.to_bits()) | (u128::from(3.0_f64.to_bits()) << 64);
        cpu.regs.xmm[1] = u128::from(5.0_f64.to_bits()) | (u128::from(7.0_f64.to_bits()) << 64);

        // 66 0F 7C C1: haddpd xmm0, xmm1
        run(&mut cpu, &[0x66, 0x0F, 0x7C, 0xC1]).unwrap();

        assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), 5.0);
        assert_eq!(f64::from_bits((cpu.regs.xmm[0] >> 64) as u64), 12.0);
    }
}
