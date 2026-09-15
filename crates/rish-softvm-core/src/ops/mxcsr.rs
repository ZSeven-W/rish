//! Legacy SSE MXCSR transfers (Intel SDM Vol. 2, LDMXCSR/STMXCSR, Type 5).

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::arch::paging::AccessKind;
use crate::arch::registers::{CpuMode, Cr0, Cr4, RFlags};
use crate::cpu::{VECTOR_GENERAL_PROTECTION, VECTOR_INVALID_OPCODE};
use crate::{Cpu, CpuError};

// Matches the implemented MXCSR_MASK advertised by FXSAVE: DAZ is supported,
// and bits 16..31 are reserved. CPUID always advertises legacy SSE support.
const MXCSR_MASK: u32 = 0x0000_FFFF;
const VECTOR_DEVICE_NOT_AVAILABLE: u8 = 7;
const VECTOR_STACK: u8 = 12;
const VECTOR_ALIGNMENT_CHECK: u8 = 17;

pub fn execute(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    // #UD takes precedence over #NM and operand faults. These instructions
    // have no XMM operand, so they need an explicit legacy SSE access gate.
    if instruction.op0_kind() != OpKind::Memory
        || instruction.has_lock_prefix()
        || cpu.regs.cr0.contains(Cr0::EM)
        || !cpu.regs.cr4.contains(Cr4::OSFXSR)
    {
        return cpu.raise(VECTOR_INVALID_OPCODE, 0, false);
    }
    if cpu.regs.cr0.contains(Cr0::TS) {
        return cpu.raise(VECTOR_DEVICE_NOT_AVAILABLE, 0, false);
    }

    let linear = cpu.effective_address(instruction, 0);
    // Paging's walker assumes canonical addresses. Reject the entire m32
    // range first, including a range straddling the canonical-address hole.
    if cpu.regs.mode() == CpuMode::Long {
        let width = if cpu.regs.cr4.contains(Cr4::LA57) {
            57
        } else {
            48
        };
        if !canonical(linear, width) || !canonical(linear.wrapping_add(3), width) {
            let vector = if instruction.memory_segment() == Register::SS {
                VECTOR_STACK
            } else {
                VECTOR_GENERAL_PROTECTION
            };
            return cpu.raise(vector, 0, true);
        }
    }

    let access = match instruction.mnemonic() {
        Mnemonic::Ldmxcsr => AccessKind::Read,
        Mnemonic::Stmxcsr => AccessKind::Write,
        _ => {
            return Err(CpuError::UnimplementedInstruction {
                code: format!("{:?}", instruction.mnemonic()),
                address: cpu.regs.rip,
                bytes: Vec::new(),
            });
        }
    };
    // Resolve BOTH pages before any store. The generic byte writer translates
    // as it writes, which would partially overwrite a cross-page destination
    // before discovering an unmapped/read-only second page.
    cpu.translate(linear, access)?;
    if linear & 0xFFF > 0xFFC {
        cpu.translate(linear.wrapping_add(3) & !0xFFF, access)?;
    }
    // Type 5 permits unaligned operands unless user-mode #AC is enabled.
    if linear & 3 != 0
        && cpu.regs.cpl() == 3
        && cpu.regs.cr0.contains(Cr0::AM)
        && cpu.regs.rflags.contains(RFlags::AC)
    {
        return cpu.raise(VECTOR_ALIGNMENT_CHECK, 0, true);
    }

    match instruction.mnemonic() {
        Mnemonic::Ldmxcsr => {
            let value = cpu.read_operand(instruction, 0, 4)? as u32;
            if value & !MXCSR_MASK != 0 {
                return cpu.raise(VECTOR_GENERAL_PROTECTION, 0, true);
            }
            // A newly unmasked, already-set exception flag does not raise a
            // SIMD exception here. Commit only after every check succeeds.
            cpu.mxcsr = value;
            Ok(())
        }
        Mnemonic::Stmxcsr => {
            cpu.write_operand(instruction, 0, 4, u64::from(cpu.mxcsr & MXCSR_MASK))
        }
        _ => unreachable!("mnemonic checked above"),
    }
}

fn canonical(address: u64, width: u32) -> bool {
    ((address << (64 - width)) as i64 >> (64 - width)) as u64 == address
}

#[cfg(test)]
mod tests;
