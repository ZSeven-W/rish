//! Scalar and packed SSE4.1 rounding with explicit MXCSR exception handling.

use crate::{CpuError, cpu::Cpu};
use iced_x86::{Instruction, Mnemonic, OpKind};

pub fn execute(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let address = cpu.regs.rip;
    let unsupported = || CpuError::UnimplementedInstruction {
        code: format!("{:?}", instruction.mnemonic()),
        address,
        bytes: Vec::new(),
    };
    let (width, packed) = match instruction.mnemonic() {
        Mnemonic::Roundsd => (8, false),
        Mnemonic::Roundss => (4, false),
        Mnemonic::Roundpd => (8, true),
        Mnemonic::Roundps => (4, true),
        _ => return Err(unsupported()),
    };
    let destination =
        super::sse_integer::xmm_index(instruction.op0_register()).ok_or_else(unsupported)?;
    let length = if packed { 16 } else { width };
    let mut source = [0; 16];
    match instruction.op1_kind() {
        OpKind::Register => {
            let index = super::sse_integer::xmm_index(instruction.op1_register())
                .ok_or_else(unsupported)?;
            source[..length].copy_from_slice(&cpu.regs.xmm[index].to_le_bytes()[..length]);
        }
        OpKind::Memory => {
            let address = cpu.effective_address(instruction, 1);
            cpu.read_linear_bytes(address, &mut source[..length])?;
        }
        _ => return Err(unsupported()),
    }
    let mut destination_bytes = cpu.regs.xmm[destination].to_le_bytes();
    let mut exceptions = 0;
    for lane in 0..length / width {
        let offset = lane * width;
        let mut raw = [0; 8];
        raw[..width].copy_from_slice(&source[offset..offset + width]);
        let (result, flags) = round_lane(
            u64::from_le_bytes(raw),
            width,
            cpu.mxcsr,
            instruction.immediate8(),
        );
        exceptions |= flags;
        destination_bytes[offset..offset + width].copy_from_slice(&result.to_le_bytes()[..width]);
    }
    cpu.mxcsr |= exceptions;
    if exceptions & !(cpu.mxcsr >> 7) != 0 {
        return Err(CpuError::GuestFault(
            "unmasked SIMD rounding exception".into(),
        ));
    }
    cpu.regs.xmm[destination] = u128::from_le_bytes(destination_bytes);
    Ok(())
}

fn round_lane(bits: u64, width: usize, mxcsr: u32, immediate: u8) -> (u64, u32) {
    let (exponent, fraction, quiet) = if width == 8 {
        (
            0x7ff0_0000_0000_0000,
            0x000f_ffff_ffff_ffff,
            0x0008_0000_0000_0000,
        )
    } else {
        (0x7f80_0000, 0x007f_ffff, 0x0040_0000)
    };
    let mut exceptions = 0;
    let result = if bits & exponent == exponent && bits & fraction != 0 {
        if bits & quiet == 0 {
            exceptions |= 1;
        }
        bits | quiet
    } else {
        let mut value = if width == 8 {
            f64::from_bits(bits)
        } else {
            f64::from(f32::from_bits(bits as u32))
        };
        if mxcsr & 0x40 != 0 && bits & exponent == 0 && bits & fraction != 0 {
            value = 0.0_f64.copysign(value);
        }
        let mode = if immediate & 4 != 0 {
            (mxcsr >> 13) & 3
        } else {
            u32::from(immediate & 3)
        };
        let rounded = match mode {
            0 => value.round_ties_even(),
            1 => value.floor(),
            2 => value.ceil(),
            _ => value.trunc(),
        };
        if immediate & 8 == 0 && rounded != value {
            exceptions |= 0x20;
        }
        if width == 8 {
            rounded.to_bits()
        } else {
            u64::from((rounded as f32).to_bits())
        }
    };
    (result, exceptions)
}

#[cfg(test)]
mod tests {
    use crate::{CpuError, cpu::Cpu};
    use iced_x86::{Decoder, DecoderOptions};

    fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
        cpu.regs.efer |= crate::arch::registers::Efer::LMA;
        cpu.regs.cs.long_mode = true;
        cpu.dispatch(&Decoder::with_ip(64, bytes, 0x1000, DecoderOptions::NONE).decode())
    }

    #[test]
    fn packed_rounding_replaces_all_lanes_with_one_rounding_mode() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[1] = u128::from(2.5_f64.to_bits()) | (u128::from((-2.5_f64).to_bits()) << 64);
        run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x09, 0xc1, 0x08]).unwrap();
        assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), 2.0);
        assert_eq!(f64::from_bits((cpu.regs.xmm[0] >> 64) as u64), -2.0);
        assert_eq!(cpu.mxcsr & 0x20, 0);
        let mut input = [0; 16];
        for (i, value) in [2.5_f32, -2.5, 1.9, -1.9].iter().enumerate() {
            input[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        cpu.regs.xmm[1] = u128::from_le_bytes(input);
        run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x08, 0xc1, 3]).unwrap();
        let bytes = cpu.regs.xmm[0].to_le_bytes();
        let output: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        assert_eq!(output, [2.0, -2.0, 1.0, -1.0]);
    }

    #[test]
    fn roundsd_modes_preserve_high_lane_and_integer_flags() {
        for (value, expected) in [
            (2.5_f64, [2.0, 2.0, 3.0, 2.0]),
            (-2.5, [-2.0, -3.0, -2.0, -2.0]),
        ] {
            for (mode, expected) in expected.into_iter().enumerate() {
                let mut cpu = Cpu::new(1, 0).unwrap();
                cpu.regs.xmm[0] = 0x1122_3344_5566_7788_u128 << 64;
                cpu.regs.xmm[1] = u128::from(value.to_bits());
                let flags = cpu.regs.rflags;
                run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x0b, 0xc1, mode as u8]).unwrap();
                assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), expected);
                assert_eq!(cpu.regs.xmm[0] >> 64, 0x1122_3344_5566_7788);
                assert_eq!(cpu.regs.rflags, flags);
                assert_eq!(cpu.mxcsr & 0x20, 0x20);
            }
        }
    }

    #[test]
    fn mxcsr_mode_precision_suppression_and_memory_width_are_honored() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.gpr[0] = 0xffff8;
        cpu.memory.write(0xffff8, &2.5_f64.to_le_bytes()).unwrap();
        cpu.mxcsr |= 0x4000; // round upward
        run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x0b, 0x00, 0x0c]).unwrap();
        assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), 3.0);
        assert_eq!(cpu.mxcsr & 0x20, 0);
        cpu.mxcsr &= !0x1000; // unmask precision
        let original = cpu.regs.xmm[0];
        assert!(run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x0b, 0x00, 0]).is_err());
        assert_eq!(cpu.regs.xmm[0], original);
    }

    #[test]
    fn roundss_quiets_signaling_nan_and_preserves_upper_96_bits() {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::MAX;
        cpu.regs.xmm[1] = 0x7f80_0001;
        run(&mut cpu, &[0x66, 0x0f, 0x3a, 0x0a, 0xc1, 0]).unwrap();
        assert_eq!(cpu.regs.xmm[0] as u32, 0x7fc0_0001);
        assert_eq!(cpu.regs.xmm[0] >> 32, u128::MAX >> 32);
        assert_eq!(cpu.mxcsr & 1, 1);
    }
}
