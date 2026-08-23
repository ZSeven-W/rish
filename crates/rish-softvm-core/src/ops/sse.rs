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
        Mnemonic::Shufps => shufps(cpu, instruction),
        Mnemonic::Shufpd => shufpd(cpu, instruction),
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
        Mnemonic::Psraw => psra(cpu, instruction, 16),
        Mnemonic::Psrad => psra(cpu, instruction, 32),
        Mnemonic::Pslldq => pslldq(cpu, instruction),
        Mnemonic::Psrldq => psrldq(cpu, instruction),
        Mnemonic::Paddb => packed_arith(cpu, instruction, 8, PackedOp::AddWrap),
        Mnemonic::Paddw => packed_arith(cpu, instruction, 16, PackedOp::AddWrap),
        Mnemonic::Paddd => packed_arith(cpu, instruction, 32, PackedOp::AddWrap),
        Mnemonic::Paddq => packed_arith(cpu, instruction, 64, PackedOp::AddWrap),
        Mnemonic::Psubb => packed_arith(cpu, instruction, 8, PackedOp::SubWrap),
        Mnemonic::Psubw => packed_arith(cpu, instruction, 16, PackedOp::SubWrap),
        Mnemonic::Psubd => packed_arith(cpu, instruction, 32, PackedOp::SubWrap),
        Mnemonic::Psubq => packed_arith(cpu, instruction, 64, PackedOp::SubWrap),
        Mnemonic::Pcmpgtb => packed_arith(cpu, instruction, 8, PackedOp::CmpGt),
        Mnemonic::Pcmpgtw => packed_arith(cpu, instruction, 16, PackedOp::CmpGt),
        Mnemonic::Pcmpgtd => packed_arith(cpu, instruction, 32, PackedOp::CmpGt),
        Mnemonic::Pminub => packed_arith(cpu, instruction, 8, PackedOp::MinU),
        Mnemonic::Pmaxub => packed_arith(cpu, instruction, 8, PackedOp::MaxU),
        Mnemonic::Pminsw => packed_arith(cpu, instruction, 16, PackedOp::MinS),
        Mnemonic::Pmaxsw => packed_arith(cpu, instruction, 16, PackedOp::MaxS),
        Mnemonic::Paddusb => packed_arith(cpu, instruction, 8, PackedOp::AddSatU),
        Mnemonic::Paddusw => packed_arith(cpu, instruction, 16, PackedOp::AddSatU),
        Mnemonic::Psubusb => packed_arith(cpu, instruction, 8, PackedOp::SubSatU),
        Mnemonic::Psubusw => packed_arith(cpu, instruction, 16, PackedOp::SubSatU),
        Mnemonic::Paddsb => packed_arith(cpu, instruction, 8, PackedOp::AddSatS),
        Mnemonic::Paddsw => packed_arith(cpu, instruction, 16, PackedOp::AddSatS),
        Mnemonic::Psubsb => packed_arith(cpu, instruction, 8, PackedOp::SubSatS),
        Mnemonic::Psubsw => packed_arith(cpu, instruction, 16, PackedOp::SubSatS),
        Mnemonic::Pinsrw => pinsrw(cpu, instruction),
        Mnemonic::Pextrw => pextrw(cpu, instruction),
        Mnemonic::Pmovmskb => pmovmskb(cpu, instruction),
        Mnemonic::Movmskps => movmsk(cpu, instruction, 32),
        Mnemonic::Movmskpd => movmsk(cpu, instruction, 64),
        Mnemonic::Ucomisd | Mnemonic::Comisd => comis(cpu, instruction, true),
        Mnemonic::Ucomiss | Mnemonic::Comiss => comis(cpu, instruction, false),
        Mnemonic::Addsd => scalar_float(cpu, instruction, true, FloatOp::Add),
        Mnemonic::Subsd => scalar_float(cpu, instruction, true, FloatOp::Sub),
        Mnemonic::Mulsd => scalar_float(cpu, instruction, true, FloatOp::Mul),
        Mnemonic::Divsd => scalar_float(cpu, instruction, true, FloatOp::Div),
        Mnemonic::Minsd => scalar_float(cpu, instruction, true, FloatOp::Min),
        Mnemonic::Maxsd => scalar_float(cpu, instruction, true, FloatOp::Max),
        Mnemonic::Addss => scalar_float(cpu, instruction, false, FloatOp::Add),
        Mnemonic::Subss => scalar_float(cpu, instruction, false, FloatOp::Sub),
        Mnemonic::Mulss => scalar_float(cpu, instruction, false, FloatOp::Mul),
        Mnemonic::Divss => scalar_float(cpu, instruction, false, FloatOp::Div),
        Mnemonic::Minss => scalar_float(cpu, instruction, false, FloatOp::Min),
        Mnemonic::Maxss => scalar_float(cpu, instruction, false, FloatOp::Max),
        Mnemonic::Sqrtsd => sqrt_scalar(cpu, instruction, true),
        Mnemonic::Sqrtss => sqrt_scalar(cpu, instruction, false),
        Mnemonic::Addps => packed_float(cpu, instruction, false, FloatOp::Add),
        Mnemonic::Subps => packed_float(cpu, instruction, false, FloatOp::Sub),
        Mnemonic::Mulps => packed_float(cpu, instruction, false, FloatOp::Mul),
        Mnemonic::Divps => packed_float(cpu, instruction, false, FloatOp::Div),
        Mnemonic::Minps => packed_float(cpu, instruction, false, FloatOp::Min),
        Mnemonic::Maxps => packed_float(cpu, instruction, false, FloatOp::Max),
        Mnemonic::Addpd => packed_float(cpu, instruction, true, FloatOp::Add),
        Mnemonic::Subpd => packed_float(cpu, instruction, true, FloatOp::Sub),
        Mnemonic::Mulpd => packed_float(cpu, instruction, true, FloatOp::Mul),
        Mnemonic::Divpd => packed_float(cpu, instruction, true, FloatOp::Div),
        Mnemonic::Minpd => packed_float(cpu, instruction, true, FloatOp::Min),
        Mnemonic::Maxpd => packed_float(cpu, instruction, true, FloatOp::Max),
        Mnemonic::Cmpsd => cmp_float(cpu, instruction, true, true),
        Mnemonic::Cmpss => cmp_float(cpu, instruction, true, false),
        Mnemonic::Cmppd => cmp_float(cpu, instruction, false, true),
        Mnemonic::Cmpps => cmp_float(cpu, instruction, false, false),
        Mnemonic::Pmuludq => pmuludq(cpu, instruction),
        Mnemonic::Movhlps => movhl(cpu, instruction, true),
        Mnemonic::Movlhps => movhl(cpu, instruction, false),
        Mnemonic::Unpcklps => unpck_float(cpu, instruction, 32, false),
        Mnemonic::Unpckhps => unpck_float(cpu, instruction, 32, true),
        Mnemonic::Unpcklpd => unpck_float(cpu, instruction, 64, false),
        Mnemonic::Unpckhpd => unpck_float(cpu, instruction, 64, true),
        Mnemonic::Cvtsi2sd | Mnemonic::Cvtsi2ss => cvtsi2s(cpu, instruction),
        Mnemonic::Cvttsd2si | Mnemonic::Cvttss2si => cvtts2si(cpu, instruction),
        Mnemonic::Cvtsd2si | Mnemonic::Cvtss2si => cvt2si_round(cpu, instruction),
        Mnemonic::Cvtss2sd => cvt_precision(cpu, instruction, true),
        Mnemonic::Cvtsd2ss => cvt_precision(cpu, instruction, false),
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
    let mut bytes = [0_u8; 16];
    cpu.read_linear_bytes(linear, &mut bytes)?;
    Ok(u128::from_le_bytes(bytes))
}

fn write_mem128(
    cpu: &mut Cpu,
    instruction: &Instruction,
    operand: u32,
    value: u128,
) -> Result<(), CpuError> {
    let linear = cpu.effective_address(instruction, operand);
    cpu.write_linear_bytes(linear, &value.to_le_bytes())
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
            // Page-crossing aware: a misaligned qword load may span two pages
            // whose physical frames are not contiguous.
            let linear = cpu.effective_address(instruction, 1);
            let mut bytes = [0_u8; 8];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            write_xmm(
                &mut cpu.regs,
                instruction.op0_register(),
                u128::from(u64::from_le_bytes(bytes)),
            );
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            let linear = cpu.effective_address(instruction, 0);
            cpu.write_linear_bytes(linear, &value.to_le_bytes())?;
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
            // Page-crossing aware: a misaligned dword load (musl's memcpy
            // realign carry `movd -0x3(%rax)`) may span two pages.
            let linear = cpu.effective_address(instruction, 1);
            let mut bytes = [0_u8; 4];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            write_xmm(
                &mut cpu.regs,
                instruction.op0_register(),
                u128::from(u32::from_le_bytes(bytes)),
            );
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u32;
            let linear = cpu.effective_address(instruction, 0);
            cpu.write_linear_bytes(linear, &value.to_le_bytes())?;
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
            // Page-crossing aware scalar load.
            let linear = cpu.effective_address(instruction, 1);
            let mut bytes = [0_u8; 8];
            cpu.read_linear_bytes(linear, &mut bytes[..size as usize])?;
            let value = u64::from_le_bytes(bytes);
            let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
            destination = (destination & !((1_u128 << (size * 8)) - 1)) | u128::from(value);
            write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            let linear = cpu.effective_address(instruction, 0);
            cpu.write_linear_bytes(linear, &value.to_le_bytes()[..size as usize])?;
        }
        _ => return Err(bad("movss/movsd", cpu, instruction)),
    }
    Ok(())
}

fn mov_low(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Memory) => {
            let linear = cpu.effective_address(instruction, 1);
            let mut bytes = [0_u8; 8];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            let value = u64::from_le_bytes(bytes);
            let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
            destination = (destination & !u128::from(u64::MAX)) | u128::from(value);
            write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = read_xmm(&cpu.regs, instruction.op1_register()) as u64;
            let linear = cpu.effective_address(instruction, 0);
            cpu.write_linear_bytes(linear, &value.to_le_bytes())?;
        }
        _ => return Err(bad("movlps/movlpd", cpu, instruction)),
    }
    Ok(())
}

fn mov_high(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    match (instruction.op0_kind(), instruction.op1_kind()) {
        (OpKind::Register, OpKind::Memory) => {
            let linear = cpu.effective_address(instruction, 1);
            let mut bytes = [0_u8; 8];
            cpu.read_linear_bytes(linear, &mut bytes)?;
            let value = u64::from_le_bytes(bytes);
            let mut destination = read_xmm(&cpu.regs, instruction.op0_register());
            destination = (destination & u128::from(u64::MAX)) | (u128::from(value) << 64);
            write_xmm(&mut cpu.regs, instruction.op0_register(), destination);
        }
        (OpKind::Memory, OpKind::Register) => {
            let value = (read_xmm(&cpu.regs, instruction.op1_register()) >> 64) as u64;
            let linear = cpu.effective_address(instruction, 0);
            cpu.write_linear_bytes(linear, &value.to_le_bytes())?;
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
        let index = u32::from((control >> (lane * 2)) & 0b11);
        // Result lanes 0 and 1 select any dword of the destination; lanes 2
        // and 3 select any dword of the source.
        let source = if lane < 2 { left } else { right };
        let word = (source >> (index * 32)) & 0xFFFF_FFFF;
        result |= word << (lane * 32);
    }
    write_xmm(&mut cpu.regs, instruction.op0_register(), result);
    Ok(())
}

fn shufpd(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let right = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
    let control = instruction.immediate(2) as u8;
    let low_index = u32::from(control & 0b1);
    let high_index = u32::from((control >> 1) & 0b1);
    let low = (left >> (low_index * 64)) & u128::from(u64::MAX);
    let high = (right >> (high_index * 64)) & u128::from(u64::MAX);
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        low | (high << 64),
    );
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
        // Shift within the lane, clamp to the lane width, then reposition.
        // Parentheses are load-bearing: `<<` binds tighter than `&`, so
        // dropping them ANDs the low-bit shifted value against a mask sitting
        // at the lane offset, zeroing every lane above lane 0.
        result |= ((value << count) & mask) << (lane * lane_bits);
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

fn psra(cpu: &mut Cpu, instruction: &Instruction, lane_bits: u32) -> Result<(), CpuError> {
    // Arithmetic right shift: each lane shifts in copies of its sign bit. A
    // count at or above the lane width saturates every lane to all-sign.
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let count = shift_count(cpu, instruction).min(u64::from(lane_bits)) as u32;
    let mask = (1_u128 << lane_bits) - 1;
    let sign = 1_u128 << (lane_bits - 1);
    let lanes = 128 / lane_bits;
    let mut result = 0_u128;
    for lane in 0..lanes {
        let value = (left >> (lane * lane_bits)) & mask;
        let shifted = if count >= lane_bits {
            if value & sign != 0 { mask } else { 0 }
        } else {
            let logical = value >> count;
            if value & sign != 0 {
                // Fill the vacated high bits with ones, inside the lane.
                logical | ((mask << (lane_bits - count)) & mask)
            } else {
                logical
            }
        };
        result |= (shifted & mask) << (lane * lane_bits);
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

#[derive(Clone, Copy)]
enum PackedOp {
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
fn packed_arith(
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
fn read_scalar_mem(
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
fn comis(cpu: &mut Cpu, instruction: &Instruction, double: bool) -> Result<(), CpuError> {
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
fn pinsrw(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
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

/// PEXTRW: extract one 16-bit word lane, selected by imm8[2:0], into a
/// general-purpose register, zero-extended.
fn pextrw(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let lane = u32::from(instruction.immediate(2) as u8 & 0b111);
    let source = read_xmm(&cpu.regs, instruction.op1_register());
    let word = ((source >> (lane * 16)) & 0xFFFF) as u64;
    write_register(
        &mut cpu.regs,
        instruction.op0_register(),
        operand_size(instruction, 0),
        word,
    );
    Ok(())
}

/// PMOVMSKB: gather the top bit of each of the 16 source bytes into the low
/// 16 bits of a general-purpose register, zero-extended.
fn pmovmskb(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
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
fn movmsk(cpu: &mut Cpu, instruction: &Instruction, lane_bits: u32) -> Result<(), CpuError> {
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
enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

fn apply_float(op: FloatOp, a: f64, b: f64) -> f64 {
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
fn read_scalar_src(
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
fn write_scalar_result(cpu: &mut Cpu, register: Register, double: bool, value: f64) {
    let mut destination = read_xmm(&cpu.regs, register);
    if double {
        destination = (destination & !u128::from(u64::MAX)) | u128::from(value.to_bits());
    } else {
        destination = (destination & !u128::from(u32::MAX)) | u128::from((value as f32).to_bits());
    }
    write_xmm(&mut cpu.regs, register, destination);
}

fn scalar_float(
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
fn float_compare(a: f64, b: f64, predicate: u8) -> bool {
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
fn cmp_float(
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

fn sqrt_scalar(cpu: &mut Cpu, instruction: &Instruction, double: bool) -> Result<(), CpuError> {
    let raw = read_scalar_src(cpu, instruction, double)?;
    let value = if double {
        f64::from_bits(raw).sqrt()
    } else {
        f64::from(f32::from_bits(raw as u32).sqrt())
    };
    write_scalar_result(cpu, instruction.op0_register(), double, value);
    Ok(())
}

fn packed_float(
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
fn pmuludq(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
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
fn movhl(cpu: &mut Cpu, instruction: &Instruction, high: bool) -> Result<(), CpuError> {
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
fn unpck_float(
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
fn cvt_precision(
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
fn cvt2si_round(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
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

    // Diagnostic oracle: execute real musl leaf routines (dumped to /tmp/*.bin)
    // through the interpreter and compare against a trivial reference. Localizes
    // interpreter instruction bugs without a full guest boot.
    fn run_leaf(code_path: &str, rdi: u64, rsi: u64, rdx: u64) -> Cpu {
        use crate::arch::registers::index;
        let code = std::fs::read(code_path).expect("dump the bytes first");
        let mut cpu = cpu();
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
        const CODE: u64 = 0x10000;
        const SENTINEL: u64 = 0x0040_0000;
        cpu.memory.write(CODE, &code).unwrap();
        let rsp = 0x4_FFF8_u64;
        cpu.memory.write(rsp, &SENTINEL.to_le_bytes()).unwrap();
        cpu.regs.gpr[index::RSP] = rsp;
        cpu.regs.gpr[index::RDI] = rdi;
        cpu.regs.gpr[index::RSI] = rsi;
        cpu.regs.gpr[index::RDX] = rdx;
        cpu.regs.rip = CODE;
        for _ in 0..1_000_000 {
            if cpu.regs.rip == SENTINEL {
                break;
            }
            let mut buf = [0_u8; 16];
            cpu.memory.read(cpu.regs.rip, &mut buf).unwrap();
            let insn = decode(64, &buf, cpu.regs.rip);
            cpu.regs.rip = cpu.regs.rip.wrapping_add(insn.len() as u64);
            cpu.dispatch(&insn).unwrap();
        }
        cpu
    }

    fn hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    #[ignore = "diagnostic; needs /tmp/memcpy.bin and --nocapture"]
    fn musl_memcpy_misaligned_oracle() {
        const SRC: u64 = 0x20000; // 16-aligned
        const DST: u64 = 0x30001; // dst % 4 == 1 -> shufps realign loop
        const LEN: usize = 64;
        let src_bytes: Vec<u8> = (0..LEN)
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(3))
            .collect();
        let cpu = run_leaf_seeded("/tmp/memcpy.bin", DST, SRC, LEN as u64, SRC, &src_bytes);
        let mut dst_bytes = vec![0_u8; LEN];
        cpu.memory.read(DST, &mut dst_bytes).unwrap();
        eprintln!("src: {}", hex(&src_bytes));
        eprintln!("dst: {}", hex(&dst_bytes));
        assert_eq!(dst_bytes, src_bytes, "misaligned memcpy corrupted the copy");
    }

    #[test]
    #[ignore = "diagnostic; needs /tmp/memset.bin and --nocapture"]
    fn musl_memset_misaligned_oracle() {
        const DST: u64 = 0x30001;
        const LEN: usize = 96;
        const FILL: u64 = 0xAB;
        let cpu = run_leaf("/tmp/memset.bin", DST, FILL, LEN as u64);
        let mut dst = vec![0_u8; LEN];
        cpu.memory.read(DST, &mut dst).unwrap();
        eprintln!("memset dst: {}", hex(&dst));
        let first_bad = (0..LEN).find(|&i| dst[i] != FILL as u8);
        eprintln!("first non-0xAB byte: {first_bad:?}");
        assert!(
            dst.iter().all(|&b| b == FILL as u8),
            "memset did not fill uniformly"
        );
    }

    #[test]
    #[ignore = "diagnostic; needs /tmp/memmove.bin and --nocapture"]
    fn musl_memmove_misaligned_oracle() {
        const SRC: u64 = 0x20000;
        const DST: u64 = 0x30001;
        const LEN: usize = 96;
        let src_bytes: Vec<u8> = (0..LEN)
            .map(|i| (i as u8).wrapping_mul(11).wrapping_add(5))
            .collect();
        let cpu = run_leaf_seeded("/tmp/memmove.bin", DST, SRC, LEN as u64, SRC, &src_bytes);
        let mut dst = vec![0_u8; LEN];
        cpu.memory.read(DST, &mut dst).unwrap();
        eprintln!("memmove dst: {}", hex(&dst));
        assert_eq!(dst, src_bytes, "memmove corrupted the copy");
    }

    fn run_leaf_seeded(
        code_path: &str,
        rdi: u64,
        rsi: u64,
        rdx: u64,
        seed_at: u64,
        seed: &[u8],
    ) -> Cpu {
        use crate::arch::registers::index;
        let code = std::fs::read(code_path).expect("dump the bytes first");
        let mut cpu = cpu();
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
        const CODE: u64 = 0x10000;
        const SENTINEL: u64 = 0x0040_0000;
        cpu.memory.write(CODE, &code).unwrap();
        cpu.memory.write(seed_at, seed).unwrap();
        let rsp = 0x4_FFF8_u64;
        cpu.memory.write(rsp, &SENTINEL.to_le_bytes()).unwrap();
        cpu.regs.gpr[index::RSP] = rsp;
        cpu.regs.gpr[index::RDI] = rdi;
        cpu.regs.gpr[index::RSI] = rsi;
        cpu.regs.gpr[index::RDX] = rdx;
        cpu.regs.rip = CODE;
        for _ in 0..1_000_000 {
            if cpu.regs.rip == SENTINEL {
                break;
            }
            let mut buf = [0_u8; 16];
            cpu.memory.read(cpu.regs.rip, &mut buf).unwrap();
            let insn = decode(64, &buf, cpu.regs.rip);
            cpu.regs.rip = cpu.regs.rip.wrapping_add(insn.len() as u64);
            cpu.dispatch(&insn).unwrap();
        }
        cpu
    }

    #[test]
    fn pinsrw_inserts_a_word_lane() {
        let mut cpu = cpu();
        cpu.regs.gpr[1] = 0xBEEF; // rcx
        // 66 0F C4 C1 02: pinsrw xmm0, ecx, 2  -> word lane 2
        run(&mut cpu, 64, &[0x66, 0x0F, 0xC4, 0xC1, 0x02]).unwrap();
        assert_eq!((cpu.regs.xmm[0] >> 32) & 0xFFFF, 0xBEEF);
    }

    #[test]
    fn pextrw_extracts_a_word_lane() {
        let mut cpu = cpu();
        cpu.regs.xmm[1] = 0xDEAD_0000_0000_0000_0000_0000_0000_0000;
        // 66 0F C5 C1 07: pextrw eax, xmm1, 7  -> top word lane
        run(&mut cpu, 64, &[0x66, 0x0F, 0xC5, 0xC1, 0x07]).unwrap();
        assert_eq!(cpu.regs.gpr[0] & 0xFFFF, 0xDEAD);
    }

    #[test]
    fn pmovmskb_gathers_byte_sign_bits() {
        let mut cpu = cpu();
        // Top bit set in bytes 0, 2, and 15.
        cpu.regs.xmm[1] = 0x8000_0000_0000_0000_0000_0000_0080_0080;
        // 66 0F D7 C1: pmovmskb eax, xmm1
        run(&mut cpu, 64, &[0x66, 0x0F, 0xD7, 0xC1]).unwrap();
        assert_eq!(cpu.regs.gpr[0] & 0xFFFF, 0b1000_0000_0000_0101);
    }

    #[test]
    fn psubb_wraps_per_byte_lane() {
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0x0000_0000_0000_0000_0000_0000_0000_0500;
        cpu.regs.xmm[1] = 0x0000_0000_0000_0000_0000_0000_0000_0001;
        // 66 0F F8 C1: psubb xmm0, xmm1  (lane0: 0x00-0x01=0xFF, lane1: 0x05-0x00=0x05)
        run(&mut cpu, 64, &[0x66, 0x0F, 0xF8, 0xC1]).unwrap();
        assert_eq!(cpu.regs.xmm[0] & 0xFFFF, 0x05FF);
    }

    #[test]
    fn addsd_adds_the_low_double_and_keeps_the_high_lane() {
        let mut cpu = cpu();
        cpu.regs.xmm[0] = (u128::from(2.0_f64.to_bits())) | (0xDEAD_u128 << 64);
        cpu.regs.xmm[1] = u128::from(3.0_f64.to_bits());
        // F2 0F 58 C1: addsd xmm0, xmm1
        run(&mut cpu, 64, &[0xF2, 0x0F, 0x58, 0xC1]).unwrap();
        assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), 5.0);
        assert_eq!(cpu.regs.xmm[0] >> 64, 0xDEAD);
    }

    #[test]
    fn ucomisd_sets_flags_for_less_than() {
        use crate::arch::registers::RFlags;
        let mut cpu = cpu();
        cpu.regs.xmm[0] = u128::from(1.0_f64.to_bits());
        cpu.regs.xmm[1] = u128::from(2.0_f64.to_bits());
        // 66 0F 2E C1: ucomisd xmm0, xmm1  (1.0 < 2.0 => CF=1, ZF=0)
        run(&mut cpu, 64, &[0x66, 0x0F, 0x2E, 0xC1]).unwrap();
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
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
    fn shufps_selects_dest_then_source_lanes() {
        // musl's misaligned SSE memcpy realigns bytes with `shufps` (imm 0x00
        // and 0x98). Lanes 0/1 come from the destination, lanes 2/3 from the
        // source, each choosing any dword of that operand (Intel SELECT4).
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0x0000_000D_0000_000C_0000_000B_0000_000A;
        cpu.regs.xmm[1] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
        // 0F C6 C1 98: shufps xmm0, xmm1, 0x98
        run(&mut cpu, 64, &[0x0F, 0xC6, 0xC1, 0x98]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0],
            0x0000_0003_0000_0002_0000_000C_0000_000A_u128
        );
    }

    #[test]
    fn shufpd_selects_qwords_from_each_operand() {
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0xAAAA_AAAA_AAAA_AAAA_BBBB_BBBB_BBBB_BBBB;
        cpu.regs.xmm[1] = 0xCCCC_CCCC_CCCC_CCCC_DDDD_DDDD_DDDD_DDDD;
        // 66 0F C6 C1 01: shufpd xmm0, xmm1, 1 -> low=dest[1], high=src[0]
        run(&mut cpu, 64, &[0x66, 0x0F, 0xC6, 0xC1, 0x01]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0],
            0xDDDD_DDDD_DDDD_DDDD_AAAA_AAAA_AAAA_AAAA_u128
        );
    }

    #[test]
    fn cmpsd_writes_an_all_ones_mask_on_a_true_predicate() {
        // Go's math.archLog uses cmpnltsd; an unimplemented compare left the
        // result register stale and crashed the `docker` CLI.
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0x1111_1111_1111_1111_4008_0000_0000_0000; // low = 3.0
        cpu.regs.xmm[2] = 0x4014_0000_0000_0000; // low = 5.0
        // F2 0F C2 C2 01: cmpltsd xmm0, xmm2 (3.0 < 5.0 -> true)
        run(&mut cpu, 64, &[0xF2, 0x0F, 0xC2, 0xC2, 0x01]).unwrap();
        assert_eq!(cpu.regs.xmm[0] & u128::from(u64::MAX), u128::from(u64::MAX));
        assert_eq!(
            cpu.regs.xmm[0] >> 64,
            0x1111_1111_1111_1111,
            "upper preserved"
        );
        // NLT of the same pair is false -> zero mask.
        cpu.regs.xmm[0] = 0x4008_0000_0000_0000;
        run(&mut cpu, 64, &[0xF2, 0x0F, 0xC2, 0xC2, 0x05]).unwrap();
        assert_eq!(cpu.regs.xmm[0] & u128::from(u64::MAX), 0);
    }

    #[test]
    fn pslld_shifts_every_dword_lane() {
        // Regression: a precedence slip once left every lane above lane 0
        // zeroed, which garbled musl's misaligned SSE memcpy (uname -a).
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
        // 66 0F 72 F0 08: pslld xmm0, 8
        run(&mut cpu, 64, &[0x66, 0x0F, 0x72, 0xF0, 0x08]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0],
            0x0000_0400_0000_0300_0000_0200_0000_0100_u128
        );
    }

    #[test]
    fn psrld_shifts_every_dword_lane() {
        let mut cpu = cpu();
        cpu.regs.xmm[0] = 0x0000_0400_0000_0300_0000_0200_0000_0100;
        // 66 0F 72 D0 08: psrld xmm0, 8
        run(&mut cpu, 64, &[0x66, 0x0F, 0x72, 0xD0, 0x08]).unwrap();
        assert_eq!(
            cpu.regs.xmm[0],
            0x0000_0004_0000_0003_0000_0002_0000_0001_u128
        );
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
    #[test]
    fn psrad_sign_extends_each_dword_lane() {
        let mut cpu = cpu();
        // Two negative and two positive dwords.
        let value = (0xFFFF_FFF0_u128)
            | (0x8000_0000_u128 << 32)
            | (0x0000_0010_u128 << 64)
            | (0x7FFF_FFFF_u128 << 96);
        write_xmm(&mut cpu.regs, iced_x86::Register::XMM1, value);
        // psrad xmm1, 4
        run(&mut cpu, 64, &[0x66, 0x0F, 0x72, 0xE1, 0x04]).unwrap();
        let r = read_xmm(&cpu.regs, iced_x86::Register::XMM1);
        assert_eq!(r & 0xFFFF_FFFF, 0xFFFF_FFFF, "negative lane keeps sign");
        assert_eq!(
            (r >> 32) & 0xFFFF_FFFF,
            0xF800_0000,
            "0x80000000>>4 arithmetic"
        );
        assert_eq!(
            (r >> 64) & 0xFFFF_FFFF,
            0x0000_0001,
            "positive lane logical"
        );
        assert_eq!((r >> 96) & 0xFFFF_FFFF, 0x07FF_FFFF, "positive top lane");
    }
}
