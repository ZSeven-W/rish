//! Instruction implementations, grouped by family.

pub mod arithmetic;
pub mod branch;
pub mod data;
pub mod logic;
pub mod sse;
pub mod sse_horizontal;
pub mod sse_integer;
pub mod sse_round;
pub mod sse_widen;
pub mod stack;
pub mod string;
pub mod system;
pub mod x87;

#[cfg(test)]
mod flags_tests;

use iced_x86::{Instruction, OpKind, Register};

use crate::arch::registers::{RFlags, Registers};
use crate::cpu::register_index;

/// Reads a register operand at the natural width as a zero-extended u64.
#[inline]
pub fn read_register(regs: &Registers, register: Register, size: u8) -> u64 {
    let value = regs.gpr(register_index(register));
    // The only sub-register whose bits are not the low `size` bytes is a legacy
    // high byte (AH/CH/DH/BH); every other width is the low 1/2/4/8 bytes, and
    // `size` already carries that width, so a single mask covers them all.
    match register {
        Register::AH | Register::CH | Register::DH | Register::BH => (value >> 8) & 0xFF,
        _ => match size {
            1 => value & 0xFF,
            2 => value & 0xFFFF,
            4 => value & 0xFFFF_FFFF,
            _ => value,
        },
    }
}

/// Writes a register operand at the natural width, preserving other bits.
#[inline]
pub fn write_register(regs: &mut Registers, register: Register, size: u8, value: u64) {
    let index = register_index(register);
    let old = regs.gpr(index);
    // Mirrors read_register: a legacy high byte patches bits 8..16, and every
    // other width is governed by `size` — 8/16-bit writes preserve the upper
    // bits while a 32-bit write zeroes the whole upper half, per x86-64.
    let new = match register {
        Register::AH | Register::CH | Register::DH | Register::BH => {
            (old & !0xFF00) | ((value & 0xFF) << 8)
        }
        _ => match size {
            1 => (old & !0xFF) | (value & 0xFF),
            2 => (old & !0xFFFF) | (value & 0xFFFF),
            4 => value & 0xFFFF_FFFF,
            _ => value,
        },
    };
    regs.set_gpr(index, new);
}

/// Operand size in bytes for the instruction's natural width.
pub fn operand_size(instruction: &Instruction, operand: u32) -> u8 {
    match instruction.op_kind(operand) {
        OpKind::Register => register_size(instruction.op_register(operand)),
        OpKind::Memory => memory_size(instruction),
        _ => 8,
    }
}

pub fn register_size(register: Register) -> u8 {
    match register {
        Register::AL
        | Register::CL
        | Register::DL
        | Register::BL
        | Register::AH
        | Register::CH
        | Register::DH
        | Register::BH
        | Register::R8L
        | Register::R9L
        | Register::R10L
        | Register::R11L
        | Register::R12L
        | Register::R13L
        | Register::R14L
        | Register::R15L
        | Register::SPL
        | Register::BPL
        | Register::SIL
        | Register::DIL => 1,
        Register::AX
        | Register::CX
        | Register::DX
        | Register::BX
        | Register::SP
        | Register::BP
        | Register::SI
        | Register::DI
        | Register::R8W
        | Register::R9W
        | Register::R10W
        | Register::R11W
        | Register::R12W
        | Register::R13W
        | Register::R14W
        | Register::R15W => 2,
        Register::EAX
        | Register::ECX
        | Register::EDX
        | Register::EBX
        | Register::ESP
        | Register::EBP
        | Register::ESI
        | Register::EDI
        | Register::R8D
        | Register::R9D
        | Register::R10D
        | Register::R11D
        | Register::R12D
        | Register::R13D
        | Register::R14D
        | Register::R15D => 4,
        _ => 8,
    }
}

pub fn memory_size(instruction: &Instruction) -> u8 {
    match instruction.memory_size() {
        iced_x86::MemorySize::UInt8 | iced_x86::MemorySize::Int8 => 1,
        iced_x86::MemorySize::UInt16 | iced_x86::MemorySize::Int16 => 2,
        iced_x86::MemorySize::UInt32 | iced_x86::MemorySize::Int32 => 4,
        iced_x86::MemorySize::UInt64 | iced_x86::MemorySize::Int64 => 8,
        iced_x86::MemorySize::UInt128 | iced_x86::MemorySize::Int128 => 16,
        iced_x86::MemorySize::UInt256 | iced_x86::MemorySize::Int256 => 32,
        iced_x86::MemorySize::UInt512 | iced_x86::MemorySize::Int512 => 64,
        // iced reports signed memory sizes (Int*) for idiv/imul memory
        // operands; they are the same width as the unsigned forms and must
        // not fall through to the 8-byte default.
        _ => 8,
    }
}

/// Reads the first explicit operand (register or memory) as u64.
pub fn read_operand0(
    cpu: &mut crate::Cpu,
    instruction: &Instruction,
) -> Result<u64, crate::CpuError> {
    if instruction.op0_kind() == OpKind::Memory {
        cpu.read_operand(instruction, 0, memory_size(instruction))
    } else {
        Ok(read_register(
            &cpu.regs,
            instruction.op0_register(),
            operand_size(instruction, 0),
        ))
    }
}

/// Reads the second explicit operand (register or memory or immediate) as u64.
pub fn read_operand1(
    cpu: &mut crate::Cpu,
    instruction: &Instruction,
) -> Result<u64, crate::CpuError> {
    match instruction.op1_kind() {
        OpKind::Memory => cpu.read_operand(instruction, 1, memory_size(instruction)),
        OpKind::Register => Ok(read_register(
            &cpu.regs,
            instruction.op1_register(),
            operand_size(instruction, 1),
        )),
        _ => Ok(instruction.immediate(1)),
    }
}

/// Writes the first explicit operand (register or memory).
pub fn write_operand0(
    cpu: &mut crate::Cpu,
    instruction: &Instruction,
    value: u64,
) -> Result<(), crate::CpuError> {
    if instruction.op0_kind() == OpKind::Memory {
        cpu.write_operand(instruction, 0, memory_size(instruction), value)
    } else {
        write_register(
            &mut cpu.regs,
            instruction.op0_register(),
            operand_size(instruction, 0),
            value,
        );
        Ok(())
    }
}

// ---- flag helpers ----

#[derive(Clone, Copy)]
pub struct AddResult {
    pub result: u64,
    pub carry: bool,
    pub overflow: bool,
    pub adjust: bool,
}

pub fn add_with_flags(left: u64, right: u64, carry: bool, bits: u32) -> AddResult {
    let carry_in = u64::from(carry);
    let result = left.wrapping_add(right).wrapping_add(carry_in);
    let mask = bits_mask(bits);
    let truncated = result & mask;
    let left_t = left & mask;
    let right_t = right & mask;
    let wide = u128::from(left_t) + u128::from(right_t) + u128::from(carry_in);
    AddResult {
        result,
        carry: wide > u128::from(mask),
        overflow: (!(left_t ^ right_t) & (left_t ^ truncated) & sign_bit(bits)) != 0,
        adjust: ((left_t ^ right_t ^ truncated) & 0x10) != 0,
    }
}

pub fn sub_with_flags(left: u64, right: u64, carry: bool, bits: u32) -> AddResult {
    let borrow = u64::from(carry);
    let result = left.wrapping_sub(right).wrapping_sub(borrow);
    let mask = bits_mask(bits);
    let truncated = result & mask;
    let left_t = left & mask;
    let right_t = right & mask;
    AddResult {
        result,
        carry: left_t < right_t || (carry && left_t == right_t),
        overflow: ((left_t ^ right_t) & (left_t ^ truncated) & sign_bit(bits)) != 0,
        adjust: ((left_t ^ right_t ^ truncated) & 0x10) != 0,
    }
}

fn bits_mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1_u64 << bits) - 1
    }
}

fn sign_bit(bits: u32) -> u64 {
    1_u64 << (bits - 1)
}

/// Applies SF/ZF/PF after a logic or arithmetic result at the given width.
pub fn set_szp(regs: &mut Registers, result: u64, bits: u32) {
    let mask = bits_mask(bits);
    let truncated = result & mask;
    let mut flags = regs.rflags;
    flags.set(RFlags::SF, truncated & sign_bit(bits) != 0);
    flags.set(RFlags::ZF, truncated == 0);
    flags.set(RFlags::PF, (truncated as u8).count_ones() % 2 == 0);
    regs.rflags = flags;
}

pub fn set_carry(regs: &mut Registers, carry: bool) {
    regs.rflags.set(RFlags::CF, carry);
}

pub fn set_overflow(regs: &mut Registers, overflow: bool) {
    regs.rflags.set(RFlags::OF, overflow);
}

pub fn set_adjust(regs: &mut Registers, adjust: bool) {
    regs.rflags.set(RFlags::AF, adjust);
}

pub fn carry(regs: &Registers) -> bool {
    regs.rflags.contains(RFlags::CF)
}
