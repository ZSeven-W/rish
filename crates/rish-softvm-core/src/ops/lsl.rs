//! LSL descriptor inspection (Intel SDM Vol. 2A, LSL and Table 3-59).

use iced_x86::{Instruction, OpKind, Register};

use crate::arch::paging::AccessKind;
use crate::arch::registers::{CpuMode, Cr0, Cr4, Efer, RFlags};
use crate::arch::segments::{Descriptor, SegmentRegister, SegmentSelector};
use crate::cpu::{VECTOR_GENERAL_PROTECTION, VECTOR_INVALID_OPCODE};
use crate::{Cpu, CpuError};

pub fn execute(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    if cpu.regs.mode() == CpuMode::Real
        || cpu.regs.rflags.contains(RFlags::VM)
        || instruction.has_lock_prefix()
    {
        return cpu.raise(VECTOR_INVALID_OPCODE, 0, false);
    }
    let selector = match instruction.op1_kind() {
        OpKind::Register => {
            SegmentSelector(super::read_register(&cpu.regs, instruction.op1_register(), 2) as u16)
        }
        OpKind::Memory => {
            let Some(value) = read_selector(cpu, instruction)? else {
                return Ok(()); // A source-address fault has been delivered.
            };
            SegmentSelector(value)
        }
        _ => return cpu.raise(VECTOR_INVALID_OPCODE, 0, false),
    };
    // Do not clear ZF until all descriptor reads have succeeded: a page fault
    // must preserve both the old flags and the entire destination register.
    let limit = descriptor_limit(cpu, selector)?;
    if let Some(limit) = limit {
        super::write_register(
            &mut cpu.regs,
            instruction.op0_register(),
            super::operand_size(instruction, 0),
            limit,
        );
    }
    cpu.regs.rflags.set(RFlags::ZF, limit.is_some());
    Ok(())
}

fn descriptor_limit(cpu: &Cpu, selector: SegmentSelector) -> Result<Option<u64>, CpuError> {
    if selector.0 & !3 == 0 {
        return Ok(None); // GDT entry zero is null at every RPL; LDT[0] is not.
    }
    let (base, table_limit) = if selector.table() == 0 {
        (cpu.regs.gdt_base, u64::from(cpu.regs.gdt_limit))
    } else {
        let ldt = cpu.regs.ldtr;
        if ldt.selector.0 & !3 == 0 || !ldt.attributes.present {
            return Ok(None);
        }
        (ldt.base, ldt.effective_limit())
    };
    let offset = u64::from(selector.index()) * 8;
    if offset + 7 > table_limit {
        return Ok(None);
    }
    let address = base.wrapping_add(offset);
    let descriptor = Descriptor::decode(read_descriptor_qword(cpu, address)?);
    let ia32e = cpu.regs.efer.contains(Efer::LMA);
    if descriptor.system {
        let valid = if ia32e {
            matches!(descriptor.descriptor_type, 2 | 9 | 11)
        } else {
            matches!(descriptor.descriptor_type, 1 | 2 | 3 | 9 | 11)
        };
        if !valid {
            return Ok(None);
        }
        if ia32e {
            if offset + 15 > table_limit {
                return Ok(None);
            }
            let upper = read_descriptor_qword(cpu, address.wrapping_add(8))?;
            if upper & (0x1F_u64 << 40) != 0 {
                return Ok(None); // Table 3-59: upper dword bits 12:8 reserved.
            }
        }
    }
    let conforming_code = !descriptor.system && descriptor.code && descriptor.conforming;
    if !conforming_code && (cpu.regs.cpl() > descriptor.dpl || selector.rpl() > descriptor.dpl) {
        return Ok(None);
    }
    // LSL inspects the stored limit; P=0 and code L/D bits do not reject an
    // otherwise valid descriptor. No accessed/busy bit or segment cache changes.
    Ok(Some(descriptor.load(selector).effective_limit()))
}

fn read_descriptor_qword(cpu: &Cpu, linear: u64) -> Result<u64, CpuError> {
    let mut bytes = [0; 8];
    let mut read = 0;
    while read < bytes.len() {
        // Descriptor-table reads are supervisor accesses even for CPL3 LSL
        // (e.g. Linux vDSO getcpu). An unaligned GDT can span physical frames.
        let physical =
            cpu.translate_privileged(linear.wrapping_add(read as u64), AccessKind::Read)?;
        let count = (4096 - (physical & 0xFFF) as usize).min(bytes.len() - read);
        cpu.memory.read(physical, &mut bytes[read..read + count])?;
        read += count;
    }
    Ok(u64::from_le_bytes(bytes))
}

fn read_selector(cpu: &mut Cpu, instruction: &Instruction) -> Result<Option<u16>, CpuError> {
    let segment_id = instruction.memory_segment();
    let segment = memory_segment(cpu, segment_id);
    let shared_address = cpu.effective_address(instruction, 1);
    let linear = if cpu.regs.mode() == CpuMode::Long {
        let width = if cpu.regs.cr4.contains(Cr4::LA57) {
            57
        } else {
            48
        };
        if !canonical(shared_address, width) || !canonical(shared_address.wrapping_add(1), width) {
            return address_fault(cpu, segment_id);
        }
        shared_address
    } else {
        // The common EA helper defaults to DS; apply implicit SS for BP/SP
        // forms as well as explicit segment overrides in legacy modes.
        let offset = shared_address.wrapping_sub(cpu.regs.data_base(instruction.segment_prefix()));
        let end = offset.checked_add(1);
        let in_limit = if !segment.code && segment.expand_down {
            let maximum = if segment.default_32 {
                u32::MAX as u64
            } else {
                0xFFFF
            };
            offset > segment.effective_limit() && end.is_some_and(|end| end <= maximum)
        } else {
            end.is_some_and(|end| end <= segment.effective_limit())
        };
        if segment.selector.0 & !3 == 0 || !in_limit {
            return address_fault(cpu, segment_id);
        }
        segment.base.wrapping_add(offset)
    };
    // Check paging before #AC; the source is always m16, even with REX.W.
    cpu.translate(linear, AccessKind::Read)?;
    if linear & 0xFFF == 0xFFF {
        cpu.translate(linear.wrapping_add(1), AccessKind::Read)?;
    }
    if linear & 1 != 0
        && cpu.regs.cpl() == 3
        && cpu.regs.cr0.contains(Cr0::AM)
        && cpu.regs.rflags.contains(RFlags::AC)
    {
        cpu.raise(17, 0, true)?;
        return Ok(None);
    }
    let mut bytes = [0; 2];
    cpu.read_linear_bytes(linear, &mut bytes)?;
    Ok(Some(u16::from_le_bytes(bytes)))
}

fn memory_segment(cpu: &Cpu, register: Register) -> SegmentRegister {
    match register {
        Register::ES => cpu.regs.es,
        Register::CS => cpu.regs.cs,
        Register::SS => cpu.regs.ss,
        Register::FS => cpu.regs.fs,
        Register::GS => cpu.regs.gs,
        _ => cpu.regs.ds,
    }
}

fn address_fault(cpu: &mut Cpu, segment: Register) -> Result<Option<u16>, CpuError> {
    cpu.raise(
        if segment == Register::SS {
            12
        } else {
            VECTOR_GENERAL_PROTECTION
        },
        0,
        true,
    )?;
    Ok(None)
}

fn canonical(address: u64, width: u32) -> bool {
    ((address << (64 - width)) as i64 >> (64 - width)) as u64 == address
}

#[cfg(test)]
mod tests;
