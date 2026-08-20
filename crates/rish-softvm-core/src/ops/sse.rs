//! SSE/SSE2 instruction subset used by the kernel boot path.
//!
//! All operations are scalar Rust on 128-bit lanes; semantics match the
//! hardware for the register/memory forms the decompressor and early kernel
//! use. Memory operands may be unaligned in this interpreter.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::ops::{operand_size, read_register, write_register};
use crate::{CpuError, cpu::Cpu};

pub fn sse_op(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match instruction.mnemonic() {
        Mnemonic::Movaps
        | Mnemonic::Movups
        | Mnemonic::Movapd
        | Mnemonic::Movupd
        | Mnemonic::Movdqa
        | Mnemonic::Movdqu => mov128(cpu, instruction),
        Mnemonic::Movq => mov64(cpu, instruction),
        Mnemonic::Movd => mov32(cpu, instruction),
        Mnemonic::Movss | Mnemonic::Movsd => mov_scalar(cpu, instruction),
        Mnemonic::Movlps | Mnemonic::Movlpd => mov_low(cpu, instruction),
        Mnemonic::Movhps | Mnemonic::Movhpd => mov_high(cpu, instruction),
        Mnemonic::Movddup => movddup(cpu, instruction),
        Mnemonic::Movsldup => movsldup(cpu, instruction),
        Mnemonic::Movshdup => movshdup(cpu, instruction),
        Mnemonic::Xorps | Mnemonic::Xorpd | Mnemonic::Pxor => logic(cpu, instruction, Op::Xor),
        Mnemonic::Andps | Mnemonic::Andpd | Mnemonic::Pand => logic(cpu, instruction, Op::And),
        Mnemonic::Andnps | Mnemonic::Andnpd | Mnemonic::Pandn => logic(cpu, instruction, Op::Andn),
        Mnemonic::Orps | Mnemonic::Orpd | Mnemonic::Por => logic(cpu, instruction, Op::Or),
        Mnemonic::Pshufd => pshufd(cpu, instruction),
        Mnemonic::Pshuflw => pshuflw(cpu, instruction, false),
        Mnemonic::Pshufhw => pshuflw(cpu, instruction, true),
        Mnemonic::Shufps | Mnemonic::Shufpd => shufps(cpu, instruction),
        Mnemonic::Punpcklbw => punpck(cpu, instruction, 8, false),
        Mnemonic::Punpcklwd => punpck(cpu, instruction, 16, false),
        Mnemonic::Punpckldq => punpck(cpu, instruction, 32, false),
        Mnemonic::Punpcklqdq => punpck(cpu, instruction, 64, false),
        Mnemonic::Punpckhbw => punpck(cpu, instruction, 8, true),
        Mnemonic::Punpckhwd => punpck(cpu, instruction, 16, true),
        Mnemonic::Punpckhdq => punpck(cpu, instruction, 32, true),
        Mnemonic::Punpckhqdq => punpck(cpu, instruction, 64, true),
        Mnemonic::Movntdq | Mnemonic::Movntps | Mnemonic::Movntq => mov128(cpu, instruction),
        Mnemonic::Movnti => movnti(cpu, instruction),
        Mnemonic::Pcmpeqb => pcmpeq(cpu, instruction, 8),
        Mnemonic::Pcmpeqw => pcmpeq(cpu, instruction, 16),
        Mnemonic::Pcmpeqd => pcmpeq(cpu, instruction, 32),
        Mnemonic::Pcmpeqq => pcmpeq(cpu, instruction, 64),
        Mnemonic::Psllw => psll(cpu, instruction, 16),
        Mnemonic::Pslld => psll(cpu, instruction, 32),
        Mnemonic::Psllq => psll(cpu, instruction, 64),
        Mnemonic::Psrlw => psrl(cpu, instruction, 16),
        Mnemonic::Psrld => psrl(cpu, instruction, 32),
        Mnemonic::Psrlq => psrl(cpu, instruction, 64),
        Mnemonic::Pslldq => pslldq(cpu, instruction),
        Mnemonic::Psrldq => psrldq(cpu, instruction),
        Mnemonic::Cvtsi2sd | Mnemonic::Cvtsi2ss => cvtsi2s(cpu, instruction),
        Mnemonic::Cvttsd2si | Mnemonic::Cvttss2si => cvtts2si(cpu, instruction),
        Mnemonic::Emms | Mnemonic::Femms => Ok(()),
        _ => Err(CpuError::UnimplementedInstruction {
            code: format!("{:?}", instruction.mnemonic()),
            address: cpu.regs.rip,
            bytes: Vec::new(),
        }),
    }
}

#[derive(Clone, Copy)]
enum Op {
    And,
    Andn,
    Or,
    Xor,
}

fn read_xmm(regs: &crate::arch::registers::Registers, register: Register) -> u128 {
    match register {
        Register::XMM0 => regs.xmm[0],
        Register::XMM1 => regs.xmm[1],
        Register::XMM2 => regs.xmm[2],
        Register::XMM3 => regs.xmm[3],
        Register::XMM4 => regs.xmm[4],
        Register::XMM5 => regs.xmm[5],
        Register::XMM6 => regs.xmm[6],
        Register::XMM7 => regs.xmm[7],
        Register::XMM8 => regs.xmm[8],
        Register::XMM9 => regs.xmm[9],
        Register::XMM10 => regs.xmm[10],
        Register::XMM11 => regs.xmm[11],
        Register::XMM12 => regs.xmm[12],
        Register::XMM13 => regs.xmm[13],
        Register::XMM14 => regs.xmm[14],
        Register::XMM15 => regs.xmm[15],
        _ => 0,
    }
}

fn write_xmm(regs: &mut crate::arch::registers::Registers, register: Register, value: u128) {
    match register {
        Register::XMM0 => regs.xmm[0] = value,
        Register::XMM1 => regs.xmm[1] = value,
        Register::XMM2 => regs.xmm[2] = value,
        Register::XMM3 => regs.xmm[3] = value,
        Register::XMM4 => regs.xmm[4] = value,
        Register::XMM5 => regs.xmm[5] = value,
        Register::XMM6 => regs.xmm[6] = value,
        Register::XMM7 => regs.xmm[7] = value,
        Register::XMM8 => regs.xmm[8] = value,
        Register::XMM9 => regs.xmm[9] = value,
        Register::XMM10 => regs.xmm[10] = value,
        Register::XMM11 => regs.xmm[11] = value,
        Register::XMM12 => regs.xmm[12] = value,
        Register::XMM13 => regs.xmm[13] = value,
        Register::XMM14 => regs.xmm[14] = value,
        Register::XMM15 => regs.xmm[15] = value,
        _ => {}
    }
}

fn read_mem128(cpu: &mut Cpu, instruction: &Instruction, operand: u32) -> Result<u128, CpuError> {
    let linear = cpu.effective_address(instruction, operand);
    let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
    let mut bytes = [0_u8; 16];
    cpu.memory.read(physical, &mut bytes)?;
    Ok(u128::from_le_bytes(bytes))
}

fn write_mem128(
    cpu: &mut Cpu,
    instruction: &Instruction,
    operand: u32,
    value: u128,
) -> Result<(), CpuError> {
    let linear = cpu.effective_address(instruction, operand);
    let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
    cpu.memory.write(physical, &value.to_le_bytes())
}

fn mov128(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register());
            write_xmm(&mut cpu.regs, instruction.op0_register(), value);
        }
        (OpKind::Register, OpKind::Memory) => {
            let value = read_mem128(cpu, instruction, 1)?;
            write_xmm(&mut cpu.regs, instruction.op0_register(), value);
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register());
            write_mem128(cpu, instruction, 0, value)?;
        }
        _ => return Err(bad("mov128", cpu, instruction)),
    }
    Ok(())
}

fn mov64(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Register)
            if is_xmm(instruction.op0_register()) && !is_xmm(instruction.op1_register()) =>
        {
            // movq xmm, r/m64
            let value = read_register(&cpu.regs, instruction.op1_register(), 8);
            write_xmm(&mut cpu.regs, instruction.op0_register(), u128::from(value));
        }
        (OpKind::Register, OpKind::Register)
            if !is_xmm(instruction.op0_register()) && is_xmm(instruction.op1_register()) =>
        {
            // movq r/m64, xmm
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            write_register(&mut cpu.regs, instruction.op0_register(), 8, value);
        }
        (OpKind::Register, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            write_xmm(&mut cpu.regs, instruction.op0_register(), u128::from(value));
        }
        (OpKind::Register, OpKind::Memory) => {
            let linear = cpu.effective_address(instruction, 1);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            let value = cpu.memory.read_u64(physical)?;
            write_xmm(&mut cpu.regs, instruction.op0_register(), u128::from(value));
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
            cpu.memory.write_u64(physical, value)?;
        }
        _ => return Err(bad("movq", cpu, instruction)),
    }
    Ok(())
}
fn mov32(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Register) => {
            if is_xmm(instruction.op0_register()) {
                let value = read_register(&cpu.regs, instruction.op1_register(), 4);
                write_xmm(&mut cpu.regs, instruction.op0_register(), u128::from(value));
            } else {
                let value = read_xmm(&cpu.regs, instruction.op1_register()) as u32;
                write_register(
                    &mut cpu.regs,
                    instruction.op0_register(),
                    4,
                    u64::from(value),
                );
            }
        }
        (OpKind::Register, OpKind::Memory) => {
            let linear = cpu.effective_address(instruction, 1);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            let value = cpu.memory.read_u32(physical)?;
            write_xmm(&mut cpu.regs, instruction.op0_register(), u128::from(value));
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u32;
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
            cpu.memory.write_u32(physical, value)?;
        }
        _ => return Err(bad("movd", cpu, instruction)),
    }
    Ok(())
}

fn mov_scalar(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = if instruction.mnemonic() == Mnemonic::Movsd {
        8
    } else {
        4
    };
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
            destination = (destination & !((1_u128 << (size * 8)) - 1))
                | u128::from(
                    value
                        & if size == 8 {
                            u64::MAX
                        } else {
                            u64::from(u32::MAX)
                        },
                );
            write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
        }
        (OpKind::Register, OpKind::Memory) => {
            let linear = cpu.effective_address(instruction, 1);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            let value = if size == 8 {
                cpu.memory.read_u64(physical)?
            } else {
                u64::from(cpu.memory.read_u32(physical)?)
            };
            let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
            destination = (destination & !((1_u128 << (size * 8)) - 1)) | u128::from(value);
            write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
            if size == 8 {
                cpu.memory.write_u64(physical, value)?;
            } else {
                cpu.memory.write_u32(physical, value as u32)?;
            }
        }
        _ => return Err(bad("movss/movsd", cpu, instruction)),
    }
    Ok(())
}

fn mov_low(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Memory) => {
            let linear = cpu.effective_address(instruction, 1);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            let value = cpu.memory.read_u64(physical)?;
            let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
            destination = (destination & !u128::from(u64::MAX)) | u128::from(value);
            write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
            cpu.memory.write_u64(physical, value)?;
        }
        _ => return Err(bad("movlps/movlpd", cpu, instruction)),
    }
    Ok(())
}

fn mov_high(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Memory) => {
            let linear = cpu.effective_address(instruction, 1);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
            let value = cpu.memory.read_u64(physical)?;
            let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
            destination = (destination & u128::from(u64::MAX)) | (u128::from(value) << 64);
            write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = (read_xmm(&cpu.regs, instruction.op1_register()) >> 64) as u64;
            let linear = cpu.effective_address(instruction, 0);
            let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
            cpu.memory.write_u64(physical, value)?;
        }
        _ => return Err(bad("movhps/movhpd", cpu, instruction)),
    }
    Ok(())
}

fn movddup(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let low = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        u128::from(low) | (u128::from(low) << 64),
    );
    Ok(())
}

fn movsldup(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let value = read_xmm(&cpu.regs, instruction.op1_register());
    let a = (value & 0xFFFF_FFFF) as u64;
    let b = ((value >> 64) & 0xFFFF_FFFF) as u64;
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        u128::from(a) | (u128::from(a) << 32) | (u128::from(b) << 64) | (u128::from(b) << 96),
    );
    Ok(())
}

fn movshdup(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let value = read_xmm(&cpu.regs, instruction.op1_register());
    let a = ((value >> 32) & 0xFFFF_FFFF) as u64;
    let b = ((value >> 96) & 0xFFFF_FFFF) as u64;
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        u128::from(a) | (u128::from(a) << 32) | (u128::from(b) << 64) | (u128::from(b) << 96),
    );
    Ok(())
}

fn logic(cpu: &mut Cpu, instruction: &Instruction, op: Op) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let result = match op {
        Op::And => left & right,
        Op::Andn => !left & right,
        Op::Or => left | right,
        Op::Xor => left ^ right,
    };
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn pshufd(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let source = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let control = instruction.immediate(2) as u8;
    let mut result = 0_u128;
    for lane in 0..4 {
        let index = (control >> (lane * 2)) & 0b11;
        let word = (source >> (index * 32)) & 0xFFFF_FFFF;
        result |= word << (lane * 32);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn pshuflw(cpu: &mut Cpu, instruction: &Instruction, high: bool) -> Result<(), CpuError> {
    let source = read_xmm(&cpu.regs, instruction.op1_register());
    let control = instruction.immediate(2) as u8;
    let base_shift = if high { 64 } else { 0 };
    let mut words = [0_u64; 4];
    for (lane, word) in words.iter_mut().enumerate() {
        let index = (control >> (lane * 2)) & 0b11;
        *word = ((source >> (base_shift + index * 16)) & 0xFFFF) as u64;
    }
    let half = words[0] | (words[1] << 16) | (words[2] << 32) | (words[3] << 48);
    let result = if high {
        (source & u128::from(u64::MAX)) | (u128::from(half) << 64)
    } else {
        (source & (u128::from(u64::MAX) << 64)) | u128::from(half)
    };
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn shufps(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let control = instruction.immediate(2) as u8;
    let mut result = 0_u128;
    for lane in 0..4 {
        let index = (control >> (lane * 2)) & 0b11;
        let source = if index < 2 { left } else { right };
        let word = (source >> ((index & 1) * 32)) & 0xFFFF_FFFF;
        result |= word << (lane * 32);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn punpck(
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
    let lanes = 128 / lane_bits;
    let half = lanes / 2;
    let start = if high { half } else { 0 };
    let mask = (1_u128 << lane_bits) - 1;
    let mut result = 0_u128;
    for index in 0..half {
        let a = (left >> ((start + index) * lane_bits)) & mask;
        let b = (right >> ((start + index) * lane_bits)) & mask;
        result |= a << ((index * 2) * lane_bits);
        result |= b << ((index * 2 + 1) * lane_bits);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn movnti(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let value = read_register(
        &cpu.regs,
        instruction.op1_register(),
        operand_size(instruction, 1),
    );
    cpu.write_operand(instruction, 0, operand_size(instruction, 1), value)
}

fn pcmpeq(cpu: &mut Cpu, instruction: &Instruction, lane_bits: u32) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let mask = (1_u128 << lane_bits) - 1;
    let lanes = 128 / lane_bits;
    let mut result = 0_u128;
    for lane in 0..lanes {
        let a = (left >> (lane * lane_bits)) & mask;
        let b = (right >> (lane * lane_bits)) & mask;
        if a == b {
            result |= mask << (lane * lane_bits);
        }
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn shift_count(cpu: &Cpu, instruction: &Instruction) -> u64 {
    match instruction.op1_kind() {
        OpKind::Immediate8 => instruction.immediate(1),
        OpKind::Register => read_register(&cpu.regs, instruction.op1_register(), 8),
        _ => 0,
    }
}

fn psll(cpu: &mut Cpu, instruction: &Instruction, lane_bits: u32) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let count = shift_count(cpu, instruction).min(u64::from(lane_bits));
    let mask = (1_u128 << lane_bits) - 1;
    let lanes = 128 / lane_bits;
    let mut result = 0_u128;
    for lane in 0..lanes {
        let value = (left >> (lane * lane_bits)) & mask;
        result |= (value << count) & mask << (lane * lane_bits);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn psrl(cpu: &mut Cpu, instruction: &Instruction, lane_bits: u32) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let count = shift_count(cpu, instruction).min(u64::from(lane_bits));
    let mask = (1_u128 << lane_bits) - 1;
    let lanes = 128 / lane_bits;
    let mut result = 0_u128;
    for lane in 0..lanes {
        let value = (left >> (lane * lane_bits)) & mask;
        result |= (value >> count) << (lane * lane_bits);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn pslldq(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let value = read_xmm(&cpu.regs, instruction.op0_register());
    let count = instruction.immediate(1) as u32 * 8;
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        value << count.min(128),
    );
    Ok(())
}

fn psrldq(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let value = read_xmm(&cpu.regs, instruction.op0_register());
    let count = instruction.immediate(1) as u32 * 8;
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        value >> count.min(128),
    );
    Ok(())
}

fn cvtsi2s(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    // Integer GPR (or memory) to float scalar; stores the bit pattern of the
    // converted value. The kernel boot path uses this only for constant math.
    let bits = if instruction.mnemonic() == Mnemonic::Cvtsi2sd {
        64
    } else {
        32
    };
    let integer = match instruction.op1_kind() {
        OpKind::Memory => cpu.read_operand(instruction, 1, operand_size(instruction, 1))?,
        _ => read_register(
            &cpu.regs,
            instruction.op1_register(),
            operand_size(instruction, 1),
        ),
    };
    let value = if bits == 64 {
        (integer as i64) as f64
    } else {
        f64::from((integer as i32) as f32)
    };
    let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
    destination = (destination & !u128::from(u64::MAX)) | u128::from(value.to_bits());
    write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
    Ok(())
}

fn cvtts2si(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
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

fn is_xmm(register: Register) -> bool {
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

fn bad(label: &str, cpu: &Cpu, instruction: &Instruction) -> CpuError {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn pxor_zeroes_a_register() {
        let mut cpu = cpu();
        cpu.regs.xmm[3] = 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF;
        // 66 0F EF DB: pxor xmm3, xmm3
        run(&mut cpu, 64, &[0x66, 0x0F, 0xEF, 0xDB]).unwrap();
        assert_eq!(cpu.regs.xmm[3], 0);
    }

    #[test]
    fn movdqu_loads_unaligned_memory() {
        let mut cpu = cpu();
        let mut payload = [0_u8; 16];
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte = index as u8;
        }
        cpu.memory.write(0x2001, &payload).unwrap();
        // F3 0F 6F 04 25 01 20 00 00: movdqu xmm0, [0x2001]
        run(
            &mut cpu,
            64,
            &[0xF3, 0x0F, 0x6F, 0x04, 0x25, 0x01, 0x20, 0x00, 0x00],
        )
        .unwrap();
        assert_eq!(
            cpu.regs.xmm[0],
            u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
    }

    #[test]
    fn movdqu_stores_unaligned_memory() {
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
        // F3 0F 7F 04 25 00 20 00 00: movdqu [0x2000], xmm0
        run(
            &mut cpu,
            64,
            &[0xF3, 0x0F, 0x7F, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00],
        )
        .unwrap();
        assert_eq!(cpu.memory.read_u64(0x2000).unwrap(), 0x090A_0B0C_0D0E_0F10);
        assert_eq!(cpu.memory.read_u64(0x2008).unwrap(), 0x0102_0304_0506_0708);
    }

    #[test]
    fn pshufd_broadcasts_a_dword() {
        let mut cpu = cpu();
        cpu.regs.xmm[1] = 0x0000_0000_DEAD_BEEF_0000_0000_0000_0001;
        // 66 0F 70 C1 00: pshufd xmm0, xmm1, 0
        run(&mut cpu, 64, &[0x66, 0x0F, 0x70, 0xC1, 0x00]).unwrap();
        let expected = 0x0000_0001_0000_0001_0000_0001_0000_0001_u128;
        assert_eq!(cpu.regs.xmm[0], expected);
    }

    #[test]
    fn punpcklqdq_duplicates_the_low_qword() {
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0xAAAA_AAAA_AAAA_AAAA_BBBB_BBBB_BBBB_BBBB;
        cpu.regs.xmm[1] = 0xCCCC_CCCC_CCCC_CCCC_DDDD_DDDD_DDDD_DDDD;
        // 66 0F 6C C1: punpcklqdq xmm0, xmm1
        run(&mut cpu, 64, &[0x66, 0x0F, 0x6C, 0xC1]).unwrap();
        assert_eq!(cpu.regs.xmm[0], 0xDDDD_DDDD_DDDD_DDDD_BBBB_BBBB_BBBB_BBBB);
    }

    #[test]
    fn movq_round_trips_via_memory() {
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0xDEAD_BEEF_CAFE_F00D;
        // 66 0F D6 04 25 00 20 00 00: movq [0x2000], xmm0
        run(
            &mut cpu,
            64,
            &[0x66, 0x0F, 0xD6, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00],
        )
        .unwrap();
        assert_eq!(cpu.memory.read_u64(0x2000).unwrap(), 0xDEAD_BEEF_CAFE_F00D);
    }
}
