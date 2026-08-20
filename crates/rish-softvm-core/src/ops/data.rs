//! Data movement: mov, lea, movzx/movsx, xchg, xadd, cmpxchg, xlat.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::ops::{
    memory_size, operand_size, read_operand0, read_operand1, read_register, write_operand0,
    write_register,
};
use crate::{CpuError, cpu::Cpu};

pub fn mov(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let code = instruction.code();
    // moffs forms: absolute address plus accumulator register.
    match code {
        iced_x86::Code::Mov_moffs8_AL
        | iced_x86::Code::Mov_moffs16_AX
        | iced_x86::Code::Mov_moffs32_EAX
        | iced_x86::Code::Mov_moffs64_RAX => {
            let size = memory_size(instruction);
            let value = read_accumulator(&cpu.regs, size);
            let address = instruction.memory_displacement64();
            let physical = cpu.translate(address, crate::arch::paging::AccessKind::Write)?;
            match size {
                1 => cpu.memory.write_u8(physical, value as u8)?,
                2 => cpu.memory.write_u16(physical, value as u16)?,
                4 => cpu.memory.write_u32(physical, value as u32)?,
                8 => cpu.memory.write_u64(physical, value)?,
                _ => unreachable!("moffs size"),
            }
            return Ok(());
        }
        iced_x86::Code::Mov_AL_moffs8
        | iced_x86::Code::Mov_AX_moffs16
        | iced_x86::Code::Mov_EAX_moffs32
        | iced_x86::Code::Mov_RAX_moffs64 => {
            let size = memory_size(instruction);
            let address = instruction.memory_displacement64();
            let physical = cpu.translate(address, crate::arch::paging::AccessKind::Read)?;
            let value = match size {
                1 => u64::from(cpu.memory.read_u8(physical)?),
                2 => u64::from(cpu.memory.read_u16(physical)?),
                4 => u64::from(cpu.memory.read_u32(physical)?),
                8 => cpu.memory.read_u64(physical)?,
                _ => unreachable!("moffs size"),
            };
            write_accumulator(&mut cpu.regs, size, value);
            return Ok(());
        }
        _ => {}
    }
    let _size = operand_size(instruction, 0);
    let value = read_operand1(cpu, instruction)?;
    write_operand0(cpu, instruction, value)
}

pub fn lea(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let address = cpu.effective_address(instruction, 1);
    write_register(
        &mut cpu.regs,
        instruction.op0_register(),
        operand_size(instruction, 0),
        address,
    );
    Ok(())
}

pub fn movx(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    // Source width depends on the exact form: 8/16-bit rm, or 32-bit
    // rm for movsxd r64/r32, r/m32.
    let source_size = match mnemonic {
        Mnemonic::Movzx | Mnemonic::Movsx => match instruction.memory_size() {
            iced_x86::MemorySize::UInt8 => 1,
            iced_x86::MemorySize::UInt16 => 2,
            _ => 4,
        },
        Mnemonic::Movsxd => 4,
        _ => 4,
    };
    let value = if instruction.op1_kind() == OpKind::Memory {
        cpu.read_operand(instruction, 1, source_size)?
    } else {
        read_register(&cpu.regs, instruction.op1_register(), source_size)
    };
    let sign_extend = matches!(mnemonic, Mnemonic::Movsx | Mnemonic::Movsxd);
    let value = if sign_extend {
        let bits = u32::from(source_size) * 8;
        (((value as i64) << (64 - bits)) >> (64 - bits)) as u64
    } else {
        value
    };
    write_register(
        &mut cpu.regs,
        instruction.op0_register(),
        operand_size(instruction, 0),
        value,
    );
    Ok(())
}
pub fn xchg(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    write_operand0(cpu, instruction, right)?;
    if instruction.op1_kind() == OpKind::Memory {
        cpu.write_operand(instruction, 1, memory_size(instruction), left)?;
    } else {
        write_register(&mut cpu.regs, instruction.op1_register(), size, left);
    }
    Ok(())
}

pub fn xadd(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    let sum = crate::ops::add_with_flags(left, right, false, u32::from(size) * 8);
    crate::ops::set_szp(&mut cpu.regs, sum.result, u32::from(size) * 8);
    crate::ops::set_carry(&mut cpu.regs, sum.carry);
    crate::ops::set_overflow(&mut cpu.regs, sum.overflow);
    crate::ops::set_adjust(&mut cpu.regs, sum.adjust);
    write_operand0(cpu, instruction, sum.result)?;
    if instruction.op1_kind() == OpKind::Memory {
        cpu.write_operand(instruction, 1, memory_size(instruction), left)?;
    } else {
        write_register(&mut cpu.regs, instruction.op1_register(), size, left);
    }
    Ok(())
}

pub fn cmpxchg(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    if instruction.op0_kind() == OpKind::Memory
        && matches!(crate::ops::memory_size(instruction), 8 | 16)
    {
        return cmpxchg8b(cpu, instruction);
    }
    let size = operand_size(instruction, 0);
    let accumulator = match size {
        1 => read_register(&cpu.regs, Register::AL, 1),
        2 => read_register(&cpu.regs, Register::AX, 2),
        4 => read_register(&cpu.regs, Register::EAX, 4),
        _ => read_register(&cpu.regs, Register::RAX, 8),
    };
    let memory = read_operand0(cpu, instruction)?;
    let compare = read_operand1(cpu, instruction)?;
    let flags = crate::ops::sub_with_flags(accumulator, memory, false, u32::from(size) * 8);
    crate::ops::set_szp(&mut cpu.regs, flags.result, u32::from(size) * 8);
    crate::ops::set_carry(&mut cpu.regs, flags.carry);
    crate::ops::set_overflow(&mut cpu.regs, flags.overflow);
    crate::ops::set_adjust(&mut cpu.regs, flags.adjust);
    if accumulator == memory {
        write_operand0(cpu, instruction, compare)?;
    } else {
        write_accumulator(&mut cpu.regs, size, memory);
    }
    Ok(())
}

fn cmpxchg8b(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let wide = crate::ops::memory_size(instruction) == 16;
    let (low, high) = if wide {
        (
            read_register(&cpu.regs, Register::RAX, 8),
            read_register(&cpu.regs, Register::RDX, 8),
        )
    } else {
        (
            read_register(&cpu.regs, Register::EAX, 4),
            read_register(&cpu.regs, Register::EDX, 4),
        )
    };
    let linear = cpu.effective_address(instruction, 0);
    let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
    let (memory_low, memory_high) = if wide {
        (
            cpu.memory.read_u64(physical)?,
            cpu.memory.read_u64(physical + 8)?,
        )
    } else {
        (
            u64::from(cpu.memory.read_u32(physical)?),
            u64::from(cpu.memory.read_u32(physical + 4)?),
        )
    };
    let equal = memory_low == low && memory_high == high;
    cpu.regs
        .rflags
        .set(crate::arch::registers::RFlags::ZF, equal);
    if equal {
        let (new_low, new_high) = if wide {
            (
                read_register(&cpu.regs, Register::RBX, 8),
                read_register(&cpu.regs, Register::RCX, 8),
            )
        } else {
            (
                read_register(&cpu.regs, Register::EBX, 4),
                read_register(&cpu.regs, Register::ECX, 4),
            )
        };
        let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Write)?;
        if wide {
            cpu.memory.write_u64(physical, new_low)?;
            cpu.memory.write_u64(physical + 8, new_high)?;
        } else {
            cpu.memory.write_u32(physical, new_low as u32)?;
            cpu.memory.write_u32(physical + 4, new_high as u32)?;
        }
    } else {
        write_accumulator(&mut cpu.regs, if wide { 8 } else { 4 }, memory_low);
        write_register(
            &mut cpu.regs,
            if wide { Register::RDX } else { Register::EDX },
            if wide { 8 } else { 4 },
            memory_high,
        );
    }
    Ok(())
}

pub fn xlatb(cpu: &mut Cpu, _instruction: &Instruction) -> Result<(), CpuError> {
    let al = read_register(&cpu.regs, Register::AL, 1);
    let base = read_register(&cpu.regs, Register::RBX, 8);
    let linear = base.wrapping_add(al);
    let physical = cpu.translate(linear, crate::arch::paging::AccessKind::Read)?;
    let value = cpu.memory.read_u8(physical)?;
    write_register(&mut cpu.regs, Register::AL, 1, u64::from(value));
    Ok(())
}

fn read_accumulator(regs: &crate::arch::registers::Registers, size: u8) -> u64 {
    match size {
        1 => read_register(regs, Register::AL, 1),
        2 => read_register(regs, Register::AX, 2),
        4 => read_register(regs, Register::EAX, 4),
        _ => read_register(regs, Register::RAX, 8),
    }
}

fn write_accumulator(regs: &mut crate::arch::registers::Registers, size: u8, value: u64) {
    match size {
        1 => write_register(regs, Register::AL, 1, value),
        2 => write_register(regs, Register::AX, 2, value),
        4 => write_register(regs, Register::EAX, 4, value),
        _ => write_register(regs, Register::RAX, 8, value),
    }
}
