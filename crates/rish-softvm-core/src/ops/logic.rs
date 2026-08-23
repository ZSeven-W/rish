//! Bitwise logic, shifts, rotates, and bit scan/test.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use crate::ops::{
    carry, operand_size, read_operand0, read_operand1, read_register, set_adjust, set_carry,
    set_overflow, set_szp, write_operand0, write_register,
};
use crate::{CpuError, cpu::Cpu};

pub fn and_or_xor(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    let result = match instruction.mnemonic() {
        Mnemonic::Or => left | right,
        Mnemonic::Xor => left ^ right,
        _ => left & right,
    };
    set_szp(&mut cpu.regs, result, u32::from(size) * 8);
    set_carry(&mut cpu.regs, false);
    set_overflow(&mut cpu.regs, false);
    set_adjust(&mut cpu.regs, false);
    write_operand0(cpu, instruction, result)
}

pub fn test(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let size = operand_size(instruction, 0);
    let left = read_operand0(cpu, instruction)?;
    let right = read_operand1(cpu, instruction)?;
    let result = left & right;
    set_szp(&mut cpu.regs, result, u32::from(size) * 8);
    set_carry(&mut cpu.regs, false);
    set_overflow(&mut cpu.regs, false);
    set_adjust(&mut cpu.regs, false);
    Ok(())
}

/// Shifts and rotates.
///
/// The count is masked by operand width before anything else: five bits for
/// 8, 16 and 32-bit operands and six bits for 64-bit ones, so `shl eax, 32`
/// is a no-op rather than a wipe. Rotates leave SF, ZF, PF and AF alone,
/// which is why a compiler is free to schedule one between a compare and the
/// branch that reads its flags.
pub fn shift_rotate(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    if mnemonic == Mnemonic::Not {
        let value = read_operand0(cpu, instruction)?;
        return write_operand0(cpu, instruction, !value);
    }
    let size = operand_size(instruction, 0);
    let value = read_operand0(cpu, instruction)?;
    let count_mask = if size == 8 { 0x3F } else { 0x1F };
    let raw_count = match instruction.op1_kind() {
        OpKind::Immediate8 => instruction.immediate(1),
        OpKind::Register => read_register(&cpu.regs, Register::CL, 1),
        _ => 1,
    };
    let count = raw_count & count_mask;
    let bits = u32::from(size) * 8;
    let mask = crate::ops::bits_mask(bits);
    let truncated = value & mask;

    let outcome = match mnemonic {
        Mnemonic::Rol | Mnemonic::Ror => rotate(truncated, count, bits, mask, mnemonic),
        Mnemonic::Rcl | Mnemonic::Rcr => {
            rotate_through_carry(truncated, count, bits, mask, carry(&cpu.regs), mnemonic)
        }
        _ => shift(truncated, count, bits, mask, mnemonic),
    };
    let Some(outcome) = outcome else {
        // A zero count leaves both the operand and every flag untouched.
        return Ok(());
    };
    set_carry(&mut cpu.regs, outcome.carry);
    set_overflow(&mut cpu.regs, outcome.overflow);
    if outcome.affects_sign_zero_parity {
        set_szp(&mut cpu.regs, outcome.result, bits);
    }
    write_operand0(cpu, instruction, outcome.result)
}

/// One shift or rotate result plus the flags it defines.
struct ShiftOutcome {
    result: u64,
    carry: bool,
    overflow: bool,
    affects_sign_zero_parity: bool,
}

fn rotate(
    value: u64,
    count: u64,
    bits: u32,
    mask: u64,
    mnemonic: Mnemonic,
) -> Option<ShiftOutcome> {
    if count == 0 {
        return None;
    }
    let steps = (count % u64::from(bits)) as u32;
    let result = if steps == 0 {
        value
    } else if mnemonic == Mnemonic::Rol {
        ((value << steps) | (value >> (bits - steps))) & mask
    } else {
        ((value >> steps) | (value << (bits - steps))) & mask
    };
    let top = (result >> (bits - 1)) & 1 != 0;
    let carry = if mnemonic == Mnemonic::Rol {
        result & 1 != 0
    } else {
        top
    };
    // OF is defined only for a single-step rotate: it is the exclusive or of
    // the two most significant result bits for ROL, and of the carry and the
    // new top bit for ROR.
    let overflow = if count == 1 {
        if mnemonic == Mnemonic::Rol {
            top != carry
        } else {
            top != ((result >> (bits - 2)) & 1 != 0)
        }
    } else {
        false
    };
    Some(ShiftOutcome {
        result,
        carry,
        overflow,
        affects_sign_zero_parity: false,
    })
}

fn rotate_through_carry(
    value: u64,
    count: u64,
    bits: u32,
    mask: u64,
    carry_in: bool,
    mnemonic: Mnemonic,
) -> Option<ShiftOutcome> {
    // RCL and RCR rotate the operand together with CF, so the period is one
    // bit wider than the operand.
    let width = u64::from(bits) + 1;
    let steps = count % width;
    if steps == 0 {
        return None;
    }
    let mut result = value;
    let mut carry = carry_in;
    for _ in 0..steps {
        if mnemonic == Mnemonic::Rcl {
            let top = (result >> (bits - 1)) & 1 != 0;
            result = ((result << 1) & mask) | u64::from(carry);
            carry = top;
        } else {
            let bottom = result & 1 != 0;
            result = (result >> 1) | (u64::from(carry) << (bits - 1));
            carry = bottom;
        }
    }
    let top = (result >> (bits - 1)) & 1 != 0;
    let overflow = if count == 1 {
        if mnemonic == Mnemonic::Rcl {
            top != carry
        } else {
            top != ((result >> (bits - 2)) & 1 != 0)
        }
    } else {
        false
    };
    Some(ShiftOutcome {
        result: result & mask,
        carry,
        overflow,
        affects_sign_zero_parity: false,
    })
}

fn shift(value: u64, count: u64, bits: u32, mask: u64, mnemonic: Mnemonic) -> Option<ShiftOutcome> {
    if count == 0 {
        return None;
    }
    let steps = count as u32;
    let (result, carry) = match mnemonic {
        Mnemonic::Shl => {
            if steps >= bits {
                (0, steps == bits && value & 1 != 0)
            } else {
                (
                    (value << steps) & mask,
                    value & (1_u64 << (bits - steps)) != 0,
                )
            }
        }
        Mnemonic::Sar => {
            let negative = value & (1_u64 << (bits - 1)) != 0;
            let filled = if steps >= bits {
                if negative { mask } else { 0 }
            } else if negative {
                ((u64::MAX << (bits - steps)) | (value >> steps)) & mask
            } else {
                value >> steps
            };
            let last_out = if steps >= bits {
                negative
            } else {
                value & (1_u64 << (steps - 1)) != 0
            };
            (filled, last_out)
        }
        _ => {
            if steps >= bits {
                (0, steps == bits && value & (1_u64 << (bits - 1)) != 0)
            } else {
                (value >> steps, value & (1_u64 << (steps - 1)) != 0)
            }
        }
    };
    // OF is defined only for a single-bit shift.
    let overflow = if count == 1 {
        match mnemonic {
            Mnemonic::Shl => ((result >> (bits - 1)) & 1 != 0) != carry,
            Mnemonic::Sar => false,
            _ => value & (1_u64 << (bits - 1)) != 0,
        }
    } else {
        false
    };
    Some(ShiftOutcome {
        result,
        carry,
        overflow,
        affects_sign_zero_parity: true,
    })
}

pub fn bit_scan_test(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    let size = operand_size(instruction, 0);
    // The bit-test family computes its own bit-string address; only the
    // scans read a plain operand here.
    if !matches!(
        mnemonic,
        Mnemonic::Bsf | Mnemonic::Bsr | Mnemonic::Tzcnt | Mnemonic::Lzcnt
    ) {
        return bit_test(cpu, instruction);
    }
    let source = read_operand1(cpu, instruction)?;
    match mnemonic {
        Mnemonic::Bsf => {
            // When the source is zero, BSF sets ZF and leaves the destination
            // register UNCHANGED. Linux's ffs loop depends on this: it seeds
            // the destination with -1 before BSF so that `bsf; add 1; jz`
            // exits on an empty mask. Writing a count here (trailing_zeros of
            // zero is the operand width) makes the loop overrun.
            if source == 0 {
                cpu.regs.rflags |= crate::arch::registers::RFlags::ZF;
            } else {
                cpu.regs.rflags -= crate::arch::registers::RFlags::ZF;
                write_register(
                    &mut cpu.regs,
                    instruction.op0_register(),
                    size,
                    u64::from(source.trailing_zeros()),
                );
            }
        }
        Mnemonic::Bsr => {
            // BSR likewise leaves the destination unchanged when the source
            // is zero.
            if source == 0 {
                cpu.regs.rflags |= crate::arch::registers::RFlags::ZF;
            } else {
                cpu.regs.rflags -= crate::arch::registers::RFlags::ZF;
                write_register(
                    &mut cpu.regs,
                    instruction.op0_register(),
                    size,
                    u64::from(63 - source.leading_zeros()),
                );
            }
        }
        Mnemonic::Tzcnt => {
            let bits = u64::from(size) * 8;
            let mask = if bits >= 64 {
                u64::MAX
            } else {
                (1_u64 << bits) - 1
            };
            let masked = source & mask;
            let index = if masked == 0 {
                bits
            } else {
                u64::from(masked.trailing_zeros())
            };
            cpu.regs
                .rflags
                .set(crate::arch::registers::RFlags::CF, source == 0);
            cpu.regs
                .rflags
                .set(crate::arch::registers::RFlags::ZF, index == 0);
            write_register(&mut cpu.regs, instruction.op0_register(), size, index);
        }
        Mnemonic::Lzcnt => {
            let bits = u64::from(size) * 8;
            let mask = if bits >= 64 {
                u64::MAX
            } else {
                (1_u64 << bits) - 1
            };
            let masked = source & mask;
            let index = if masked == 0 {
                bits
            } else {
                u64::from(masked.leading_zeros()) - (64 - bits)
            };
            cpu.regs
                .rflags
                .set(crate::arch::registers::RFlags::CF, source == 0);
            cpu.regs
                .rflags
                .set(crate::arch::registers::RFlags::ZF, index == 0);
            write_register(&mut cpu.regs, instruction.op0_register(), size, index);
        }
        _ => unreachable!("bit-test mnemonics are handled above"),
    }
    Ok(())
}

/// BT, BTS, BTR, BTC.
///
/// With a register bit offset and a memory operand, the offset is a signed
/// bit-string displacement: the addressed unit is
/// `ea + (offset >> log2(w)) * (w / 8)` and the bit is `offset & (w - 1)`.
/// Linux's `test_bit`/`set_bit` pass bit numbers far beyond one word
/// (feature 323 lives five dwords in), so folding the offset into the
/// effective address is load-bearing: without it every large bit number
/// reads word zero and CPU feature tests return garbage. An immediate
/// offset does not extend the address; it wraps inside the addressed unit.
fn bit_test(cpu: &mut Cpu, instruction: &Instruction) -> Result<(), CpuError> {
    let mnemonic = instruction.mnemonic();
    let size = operand_size(instruction, 0);
    let width_bits = u64::from(size) * 8;
    let writes = !matches!(mnemonic, Mnemonic::Bt);
    if instruction.op0_kind() == OpKind::Memory {
        let base = cpu.effective_address(instruction, 0);
        let (address, bit_in_unit) = match instruction.op1_kind() {
            OpKind::Register => {
                let raw = read_register(&cpu.regs, instruction.op1_register(), size);
                // Sign-extend the offset at the operand width, then split it
                // into a signed unit displacement and a bit inside the unit.
                let bits = u32::from(size) * 8;
                let signed = ((raw as i64) << (64 - bits)) >> (64 - bits);
                let unit = signed.div_euclid(width_bits as i64);
                let bit = signed.rem_euclid(width_bits as i64) as u64;
                (base.wrapping_add((unit * i64::from(size)) as u64), bit)
            }
            _ => (base, instruction.immediate(1) % width_bits),
        };
        let physical = cpu.translate(
            address,
            if writes {
                crate::arch::paging::AccessKind::Write
            } else {
                crate::arch::paging::AccessKind::Read
            },
        )?;
        let value = match size {
            1 => u64::from(cpu.memory.read_u8(physical)?),
            2 => u64::from(cpu.memory.read_u16(physical)?),
            4 => u64::from(cpu.memory.read_u32(physical)?),
            _ => cpu.memory.read_u64(physical)?,
        };
        let mask = 1_u64 << bit_in_unit;
        set_carry(&mut cpu.regs, value & mask != 0);
        if writes {
            let updated = match mnemonic {
                Mnemonic::Bts => value | mask,
                Mnemonic::Btr => value & !mask,
                _ => value ^ mask,
            };
            match size {
                1 => cpu.memory.write_u8(physical, updated as u8)?,
                2 => cpu.memory.write_u16(physical, updated as u16)?,
                4 => cpu.memory.write_u32(physical, updated as u32)?,
                _ => cpu.memory.write_u64(physical, updated)?,
            }
        }
        return Ok(());
    }
    // Register destination: the offset wraps at the operand width.
    let bit = match instruction.op1_kind() {
        OpKind::Immediate8 => instruction.immediate(1),
        OpKind::Register => read_register(&cpu.regs, instruction.op1_register(), size),
        _ => 0,
    } % width_bits;
    let value = read_register(&cpu.regs, instruction.op0_register(), size);
    let mask = 1_u64 << bit;
    set_carry(&mut cpu.regs, value & mask != 0);
    if writes {
        let updated = match mnemonic {
            Mnemonic::Bts => value | mask,
            Mnemonic::Btr => value & !mask,
            _ => value ^ mask,
        };
        write_register(&mut cpu.regs, instruction.op0_register(), size, updated);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::registers::{RFlags, index};

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
    fn and_sets_flags_and_clears_carry() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xFF);
        cpu.regs.rflags |= RFlags::CF;
        run(&mut cpu, 64, &[0x48, 0x83, 0xE0, 0x0F]).unwrap(); // and rax, 0xF
        assert_eq!(cpu.regs.gpr(index::RAX), 0x0F);
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn xor_register_with_self_zeroes() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 42);
        run(&mut cpu, 64, &[0x48, 0x31, 0xC0]).unwrap(); // xor rax, rax
        assert_eq!(cpu.regs.gpr(index::RAX), 0);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn shl_shifts_in_zero_and_sets_carry() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x8000_0000_0000_0000);
        run(&mut cpu, 64, &[0x48, 0xD1, 0xE0]).unwrap(); // shl rax, 1
        assert_eq!(cpu.regs.gpr(index::RAX), 0);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        assert!(cpu.regs.rflags.contains(RFlags::OF));
    }

    #[test]
    fn sar_preserves_sign() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x8000_0000_0000_0000);
        run(&mut cpu, 64, &[0x48, 0xD1, 0xF8]).unwrap(); // sar rax, 1
        assert_eq!(cpu.regs.gpr(index::RAX), 0xC000_0000_0000_0000);
    }

    #[test]
    fn test_instruction_only_sets_flags() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xF0);
        run(&mut cpu, 64, &[0x48, 0xA9, 0x0F, 0x00, 0x00, 0x00]).unwrap(); // test rax, 0xF
        assert_eq!(cpu.regs.gpr(index::RAX), 0xF0);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn bsf_finds_lowest_set_bit() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x40);
        run(&mut cpu, 64, &[0x48, 0x0F, 0xBC, 0xD8]).unwrap(); // bsf rbx, rax
        assert_eq!(cpu.regs.gpr(index::RBX), 6);
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn bts_imm8_sets_bit_58() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RSI, 0x35E_4063);
        // bts rsi, 0x3a (48 0f ba ee 3a)
        run(&mut cpu, 64, &[0x48, 0x0F, 0xBA, 0xEE, 0x3A]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RSI), 0x35E_4063 | (1_u64 << 58));
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
    }
    #[test]
    fn a_rotate_leaves_the_zero_flag_alone() {
        // The compiler schedules rotates between a compare and its branch
        // precisely because ROL and ROR do not touch SF, ZF, PF or AF.
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RBX, 0x0123_4567_89AB_CDEF);
        cpu.regs.rflags |= RFlags::ZF | RFlags::SF | RFlags::PF;
        // rol rbx, 4
        run(&mut cpu, 64, &[0x48, 0xC1, 0xC3, 0x04]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RBX), 0x1234_5678_9ABC_DEF0);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        assert!(cpu.regs.rflags.contains(RFlags::SF));
        assert!(cpu.regs.rflags.contains(RFlags::PF));
    }

    #[test]
    fn ror_wraps_the_low_bits_to_the_top() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RBX, 0x0123_4567_89AB_CDEF);
        // ror rbx, 0x10
        run(&mut cpu, 64, &[0x48, 0xC1, 0xCB, 0x10]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RBX), 0xCDEF_0123_4567_89AB);
    }

    #[test]
    fn a_shift_count_is_masked_to_the_operand_width() {
        // `shl eax, 32` masks to a count of zero, so nothing changes.
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xDEAD_BEEF);
        run(&mut cpu, 64, &[0xC1, 0xE0, 0x20]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0xDEAD_BEEF);
        // The same masking applies to the CL form.
        cpu.regs.set_gpr(index::RCX, 0x20);
        run(&mut cpu, 64, &[0xD3, 0xE0]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0xDEAD_BEEF);
        // 64-bit operands mask to six bits instead.
        cpu.regs.set_gpr(index::RAX, 0xFF);
        cpu.regs.set_gpr(index::RCX, 0x20);
        run(&mut cpu, 64, &[0x48, 0xD3, 0xE0]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0xFF << 32);
    }

    #[test]
    fn a_zero_count_shift_changes_no_flags() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xF0);
        cpu.regs.set_gpr(index::RCX, 0);
        cpu.regs.rflags |= RFlags::CF | RFlags::ZF;
        run(&mut cpu, 64, &[0xD3, 0xE0]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0xF0);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn rcl_rotates_the_carry_flag_through_the_operand() {
        let mut cpu = cpu();
        // 0x80 with CF=0: rcl al, 1 shifts the top bit into CF and zero in.
        cpu.regs.set_gpr(index::RAX, 0x80);
        cpu.regs.rflags -= RFlags::CF;
        run(&mut cpu, 64, &[0xD0, 0xD0]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX) & 0xFF, 0x00);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        // Rotating again brings the carry back into bit 0.
        run(&mut cpu, 64, &[0xD0, 0xD0]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX) & 0xFF, 0x01);
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn rcr_is_the_inverse_of_rcl() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0x01);
        cpu.regs.rflags -= RFlags::CF;
        // rcr al, 1: bit 0 goes to CF, CF comes in at the top.
        run(&mut cpu, 64, &[0xD0, 0xD8]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX) & 0xFF, 0x00);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        run(&mut cpu, 64, &[0xD0, 0xD8]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX) & 0xFF, 0x80);
    }

    #[test]
    fn sar_keeps_the_sign_and_reports_the_last_bit_out() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xFFFF_FFFF_FFFF_FF83);
        // sar rax, 2 fills from the sign; the last bit shifted out is bit 1
        // of 0x83, which is set.
        run(&mut cpu, 64, &[0x48, 0xC1, 0xF8, 0x02]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0xFFFF_FFFF_FFFF_FFE0);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        // Shifting a positive value reports the last bit out as clear.
        cpu.regs.set_gpr(index::RAX, 0x8);
        run(&mut cpu, 64, &[0x48, 0xC1, 0xF8, 0x02]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0x2);
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
    }
    #[test]
    fn bt_with_a_register_offset_addresses_beyond_the_first_word() {
        // Linux test_bit(323, addr): bit 323 lives at dword 10, bit 3. A BT
        // that wraps the offset inside word zero reads CPU feature garbage.
        let mut cpu = cpu();
        cpu.memory.write_u32(0x2000 + 40, 1 << 3).unwrap();
        cpu.regs.set_gpr(index::RSI, 0x2000);
        cpu.regs.set_gpr(index::RAX, 323);
        // bt [rsi], eax
        run(&mut cpu, 64, &[0x0F, 0xA3, 0x06]).unwrap();
        assert!(cpu.regs.rflags.contains(RFlags::CF));
        cpu.regs.set_gpr(index::RAX, 322);
        run(&mut cpu, 64, &[0x0F, 0xA3, 0x06]).unwrap();
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn bts_with_a_register_offset_writes_the_addressed_word_only() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RSI, 0x2000);
        cpu.regs.set_gpr(index::RAX, 100);
        // bts [rsi], rax  (64-bit unit: bit 100 = qword 1 bit 36)
        run(&mut cpu, 64, &[0x48, 0x0F, 0xAB, 0x06]).unwrap();
        assert_eq!(cpu.memory.read_u64(0x2000).unwrap(), 0);
        assert_eq!(cpu.memory.read_u64(0x2008).unwrap(), 1 << 36);
        assert!(!cpu.regs.rflags.contains(RFlags::CF));
        run(&mut cpu, 64, &[0x48, 0x0F, 0xAB, 0x06]).unwrap();
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn bt_with_a_negative_register_offset_addresses_backwards() {
        let mut cpu = cpu();
        cpu.memory.write_u32(0x2000 - 4, 1 << 31).unwrap();
        cpu.regs.set_gpr(index::RSI, 0x2000);
        cpu.regs.set_gpr(index::RAX, (-1_i64) as u64);
        // bt [rsi], eax: bit -1 = previous dword, bit 31.
        run(&mut cpu, 64, &[0x0F, 0xA3, 0x06]).unwrap();
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn bt_with_an_immediate_offset_wraps_in_place() {
        let mut cpu = cpu();
        cpu.memory.write_u32(0x2000, 1 << 3).unwrap();
        cpu.memory.write_u32(0x2004, 0).unwrap();
        cpu.regs.set_gpr(index::RSI, 0x2000);
        // bt dword [rsi], 35: immediate wraps to bit 3 of the same dword.
        run(&mut cpu, 64, &[0x0F, 0xBA, 0x26, 0x23]).unwrap();
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }

    #[test]
    fn btr_on_a_register_wraps_at_the_operand_width() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xFFFF_FFFF);
        cpu.regs.set_gpr(index::RCX, 35);
        // btr eax, ecx: clears bit 3.
        run(&mut cpu, 64, &[0x0F, 0xB3, 0xC8]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0xFFFF_FFF7);
        assert!(cpu.regs.rflags.contains(RFlags::CF));
    }
    #[test]
    fn bsf_leaves_the_destination_unchanged_when_the_source_is_zero() {
        // Linux's softirq ffs loop seeds the dest with -1 and relies on BSF
        // leaving it there for an empty mask so `bsf; inc; jz` exits.
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RBX, 0xFFFF_FFFF);
        cpu.regs.set_gpr(index::RBP, 0);
        // bsf ebx, ebp
        run(&mut cpu, 64, &[0x0F, 0xBC, 0xDD]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RBX), 0xFFFF_FFFF);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        // A non-zero source writes the index and clears ZF.
        cpu.regs.set_gpr(index::RBP, 0x200);
        run(&mut cpu, 64, &[0x0F, 0xBC, 0xDD]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RBX), 9);
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn bsr_leaves_the_destination_unchanged_when_the_source_is_zero() {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::RAX, 0xDEAD);
        cpu.regs.set_gpr(index::RCX, 0);
        // bsr rax, rcx
        run(&mut cpu, 64, &[0x48, 0x0F, 0xBD, 0xC1]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 0xDEAD);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        cpu.regs.set_gpr(index::RCX, 0x8000_0000);
        run(&mut cpu, 64, &[0x48, 0x0F, 0xBD, 0xC1]).unwrap();
        assert_eq!(cpu.regs.gpr(index::RAX), 31);
        assert!(!cpu.regs.rflags.contains(RFlags::ZF));
    }

    #[test]
    fn bsf_reports_the_lowest_set_bit_of_a_32_bit_operand() {
        let mut cpu = cpu();
        // A 32-bit source with only bit 20 set; the full-width trailing_zeros
        // of the zero-extended value must still be 20, not 64.
        cpu.regs.set_gpr(index::RCX, 1 << 20);
        run(&mut cpu, 64, &[0x0F, 0xBC, 0xC1]).unwrap(); // bsf eax, ecx
        assert_eq!(cpu.regs.gpr(index::RAX), 20);
    }
}
