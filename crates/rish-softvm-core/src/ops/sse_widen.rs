//! SSE4.1 packed sign/zero extension from the low source lanes.

use crate::{CpuError, cpu::Cpu};
use iced_x86::{Instruction, Mnemonic, OpKind};

pub fn execute(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let address = cpu.regs.rip;
    let unsupported = || CpuError::UnimplementedInstruction {
        code: format!("{:?}", instruction.mnemonic()),
        address,
        bytes: Vec::new(),
    };
    let (source_width, destination_width, signed) = match instruction.mnemonic() {
        Mnemonic::Pmovsxbw => (1, 2, true),
        Mnemonic::Pmovsxbd => (1, 4, true),
        Mnemonic::Pmovsxbq => (1, 8, true),
        Mnemonic::Pmovsxwd => (2, 4, true),
        Mnemonic::Pmovsxwq => (2, 8, true),
        Mnemonic::Pmovsxdq => (4, 8, true),
        Mnemonic::Pmovzxbw => (1, 2, false),
        Mnemonic::Pmovzxbd => (1, 4, false),
        Mnemonic::Pmovzxbq => (1, 8, false),
        Mnemonic::Pmovzxwd => (2, 4, false),
        Mnemonic::Pmovzxwq => (2, 8, false),
        Mnemonic::Pmovzxdq => (4, 8, false),
        _ => return Err(unsupported()),
    };
    let destination =
        super::sse_integer::xmm_index(instruction.op0_register()).ok_or_else(unsupported)?;
    let count = 16 / destination_width;
    let mut source = [0; 16];
    match instruction.op1_kind() {
        OpKind::Register => {
            let index = super::sse_integer::xmm_index(instruction.op1_register())
                .ok_or_else(unsupported)?;
            source = cpu.regs.xmm[index].to_le_bytes();
        }
        OpKind::Memory => {
            let address = cpu.effective_address(instruction, 1);
            cpu.read_linear_bytes(address, &mut source[..count * source_width])?;
        }
        _ => return Err(unsupported()),
    }
    let mut result = [0; 16];
    for lane in 0..count {
        let mut bytes = [0; 8];
        bytes[..source_width]
            .copy_from_slice(&source[lane * source_width..(lane + 1) * source_width]);
        let mut value = u64::from_le_bytes(bytes);
        if signed {
            let shift = 64 - source_width * 8;
            value = ((value << shift) as i64 >> shift) as u64;
        }
        result[lane * destination_width..(lane + 1) * destination_width]
            .copy_from_slice(&value.to_le_bytes()[..destination_width]);
    }
    cpu.regs.xmm[destination] = u128::from_le_bytes(result);
    Ok(())
}
