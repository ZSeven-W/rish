//! SSE/SSE2 instruction subset used by the kernel boot path.
//!
//! All operations are scalar Rust on 128-bit lanes; semantics match the
//! hardware for the register/memory forms the decompressor and early kernel
//! use. Memory operands may be unaligned in this interpreter.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::ops::{operand_size, read_register, write_register};
use crate::{CpuError, cpu::Cpu};

#[path = "sse_numeric.rs"]
mod numeric;
use numeric::*;

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
        Mnemonic::Pminsb => packed_arith(cpu, instruction, 8, PackedOp::MinS),
        Mnemonic::Pmaxsb => packed_arith(cpu, instruction, 8, PackedOp::MaxS),
        Mnemonic::Pminsd => packed_arith(cpu, instruction, 32, PackedOp::MinS),
        Mnemonic::Pmaxsd => packed_arith(cpu, instruction, 32, PackedOp::MaxS),
        Mnemonic::Pminuw => packed_arith(cpu, instruction, 16, PackedOp::MinU),
        Mnemonic::Pmaxuw => packed_arith(cpu, instruction, 16, PackedOp::MaxU),
        Mnemonic::Pminud => packed_arith(cpu, instruction, 32, PackedOp::MinU),
        Mnemonic::Pmaxud => packed_arith(cpu, instruction, 32, PackedOp::MaxU),
        Mnemonic::Paddusb => packed_arith(cpu, instruction, 8, PackedOp::AddSatU),
        Mnemonic::Paddusw => packed_arith(cpu, instruction, 16, PackedOp::AddSatU),
        Mnemonic::Psubusb => packed_arith(cpu, instruction, 8, PackedOp::SubSatU),
        Mnemonic::Psubusw => packed_arith(cpu, instruction, 16, PackedOp::SubSatU),
        Mnemonic::Paddsb => packed_arith(cpu, instruction, 8, PackedOp::AddSatS),
        Mnemonic::Paddsw => packed_arith(cpu, instruction, 16, PackedOp::AddSatS),
        Mnemonic::Psubsb => packed_arith(cpu, instruction, 8, PackedOp::SubSatS),
        Mnemonic::Psubsw => packed_arith(cpu, instruction, 16, PackedOp::SubSatS),
        Mnemonic::Pinsrw => pinsrw(cpu, instruction),
        Mnemonic::Pextrw => super::sse_integer::extract(cpu, instruction, 2),
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
            write_xmm(&mut cpu.regs, instruction.op0_register(), u128::from(value));
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
    let source = match instruction.op1_kind() {
        OpKind::Memory => read_mem128(cpu, instruction, 1)?,
        _ => read_xmm(&cpu.regs, instruction.op1_register()),
    };
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

fn shift_count(cpu: &mut Cpu, instruction: &Instruction) -> Result<u64, CpuError> {
    let address = cpu.regs.rip;
    let unsupported = || CpuError::UnimplementedInstruction {
        code: format!("{:?}", instruction.mnemonic()),
        address,
        bytes: Vec::new(),
    };
    if super::sse_integer::xmm_index(instruction.op0_register()).is_none() {
        return Err(unsupported());
    }
    match instruction.op1_kind() {
        OpKind::Immediate8 => Ok(instruction.immediate(1)),
        OpKind::Register => {
            let index = super::sse_integer::xmm_index(instruction.op1_register())
                .ok_or_else(unsupported)?;
            Ok(cpu.regs.xmm[index] as u64)
        }
        OpKind::Memory => Ok(read_mem128(cpu, instruction, 1)? as u64),
        _ => Err(unsupported()),
    }
}

fn psll(cpu: &mut Cpu, instruction: &Instruction, lane_bits: u32) -> Result<(), CpuError> {
    let left = read_xmm(&cpu.regs, instruction.op0_register());
    let count = shift_count(cpu, instruction)?.min(u64::from(lane_bits));
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
    let count = shift_count(cpu, instruction)?.min(u64::from(lane_bits));
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
    let count = shift_count(cpu, instruction)?.min(u64::from(lane_bits)) as u32;
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
        if count >= 128 { 0 } else { value << count },
    );
    Ok(())
}

fn psrldq(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let value = read_xmm(&cpu.regs, instruction.op0_register());
    let count = instruction.immediate(1) as u32 * 8;
    write_xmm(
        &mut cpu.regs,
        instruction.op0_register(),
        if count >= 128 { 0 } else { value >> count },
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

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sse_shift_tests.rs"]
mod shift_tests;
