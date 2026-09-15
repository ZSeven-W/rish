use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
use crate::arch::segments::{Descriptor, SegmentSelector};
use crate::{Cpu, CpuError};

const CODE: u64 = 0x1000;
const SENTINEL: u128 = 0xDEAD_BEEF_0123_4567_CAFE_BABE_89AB_CDEF;

fn cpu() -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cs.long_mode = true;
    cpu.regs.rflags = RFlags::CF | RFlags::PF | RFlags::AF | RFlags::ZF | RFlags::SF | RFlags::OF;
    cpu.regs.xmm[0] = SENTINEL;
    cpu
}

fn singles(lanes: [u32; 4]) -> u128 {
    u128::from_le_bytes(lanes.map(u32::to_le_bytes).concat().try_into().unwrap())
}

fn doubles(low: u64, high: u64) -> u128 {
    u128::from(low) | (u128::from(high) << 64)
}

fn decode(bytes: &[u8]) -> iced_x86::Instruction {
    let instruction =
        iced_x86::Decoder::with_ip(64, bytes, CODE, iced_x86::DecoderOptions::NONE).decode();
    assert_eq!(instruction.mnemonic(), iced_x86::Mnemonic::Cvtps2pd);
    assert_eq!(instruction.len(), bytes.len());
    instruction
}

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    decode(bytes);
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step()
}

#[test]
fn cvtps2pd_widens_the_low_two_floats_and_replaces_the_whole_destination() {
    let mut cpu = cpu();
    cpu.regs.xmm[1] = singles([1.5_f32.to_bits(), (-2.25_f32).to_bits(), 0, u32::MAX]);
    let mut xmm = cpu.regs.xmm;
    let gpr = cpu.regs.gpr;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;

    // Legacy SSE2: cvtps2pd xmm0, xmm1.
    run(&mut cpu, &[0x0F, 0x5A, 0xC1]).unwrap();

    xmm[0] = doubles(1.5_f64.to_bits(), (-2.25_f64).to_bits());
    assert_eq!(cpu.regs.xmm, xmm);
    assert_eq!(cpu.regs.gpr, gpr);
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.mxcsr, mxcsr);
    assert_eq!(cpu.regs.rip, CODE + 3);
}

#[test]
fn cvtps2pd_preserves_signed_zero_infinity_and_finite_extreme_bits() {
    for (input, expected) in [
        ([0, 0x8000_0000], [0, 0x8000_0000_0000_0000]),
        (
            [0x7F80_0000, 0xFF80_0000],
            [0x7FF0_0000_0000_0000, 0xFFF0_0000_0000_0000],
        ),
        (
            [0x7F7F_FFFF, 0xFF7F_FFFF],
            [0x47EF_FFFF_E000_0000, 0xC7EF_FFFF_E000_0000],
        ),
        (
            [0x0080_0000, 0x8080_0000],
            [0x3810_0000_0000_0000, 0xB810_0000_0000_0000],
        ),
    ] {
        for rounding in 0..4 {
            for ftz in [false, true] {
                let mut cpu = cpu();
                cpu.mxcsr |= (rounding << 13) | (u32::from(ftz) << 15);
                cpu.regs.xmm[1] = singles([input[0], input[1], 0, 0]);
                let mxcsr = cpu.mxcsr;

                run(&mut cpu, &[0x0F, 0x5A, 0xC1]).unwrap();

                assert_eq!(cpu.regs.xmm[0], doubles(expected[0], expected[1]));
                assert_eq!(cpu.mxcsr, mxcsr, "exact widening ignores RC and FTZ");
            }
        }
    }
}

#[test]
fn cvtps2pd_denormals_widen_exactly_or_become_signed_zero_with_daz() {
    for daz in [false, true] {
        for rounding in 0..4 {
            for ftz in [false, true] {
                let mut cpu = cpu();
                cpu.mxcsr |=
                    0x20 | (u32::from(daz) << 6) | (rounding << 13) | (u32::from(ftz) << 15);
                // Smallest positive and largest negative subnormal f32.
                cpu.regs.xmm[1] = singles([1, 0x807F_FFFF, 0, 0]);
                let before = cpu.mxcsr;

                run(&mut cpu, &[0x0F, 0x5A, 0xC1]).unwrap();

                let expected = if daz {
                    doubles(0, 0x8000_0000_0000_0000)
                } else {
                    doubles(0x36A0_0000_0000_0000, 0xB80F_FFFF_C000_0000)
                };
                assert_eq!(cpu.regs.xmm[0], expected);
                assert_eq!(cpu.mxcsr, before | if daz { 0 } else { 2 });
            }
        }
    }
}

#[test]
fn cvtps2pd_nan_payloads_and_signs_survive_and_only_signaling_nan_raises_invalid() {
    for (low, high, expected_low, expected_high, exception) in [
        (
            0x7FC1_2345,
            0xFFC1_2345,
            0x7FF8_2468_A000_0000,
            0xFFF8_2468_A000_0000,
            0,
        ),
        (
            0x7F81_2345,
            0xFFA0_0001,
            0x7FF8_2468_A000_0000,
            0xFFFC_0000_2000_0000,
            1,
        ),
    ] {
        let mut cpu = cpu();
        cpu.mxcsr |= 0x20; // An existing precision status remains sticky.
        cpu.regs.xmm[1] = singles([low, high, 0, 0]);
        let mxcsr = cpu.mxcsr;

        run(&mut cpu, &[0x0F, 0x5A, 0xC1]).unwrap();

        assert_eq!(cpu.regs.xmm[0], doubles(expected_low, expected_high));
        assert_eq!(cpu.mxcsr, mxcsr | exception);
    }
}

#[test]
fn cvtps2pd_ignores_upper_source_lanes_even_with_unmasked_invalid_and_denormal() {
    let mut cpu = cpu();
    cpu.mxcsr &= !((1 << 7) | (1 << 8));
    cpu.regs.xmm[1] = singles([1.0_f32.to_bits(), 2.0_f32.to_bits(), 0x7F80_0001, 1]);
    let mxcsr = cpu.mxcsr;

    run(&mut cpu, &[0x0F, 0x5A, 0xC1]).unwrap();

    assert_eq!(
        cpu.regs.xmm[0],
        doubles(1.0_f64.to_bits(), 2.0_f64.to_bits())
    );
    assert_eq!(cpu.mxcsr, mxcsr);
    assert_eq!(cpu.exceptions_raised, 0);
}

#[test]
fn cvtps2pd_high_xmm_and_self_alias_read_both_original_single_precision_lanes() {
    for (destination, source) in [(0, 0), (15, 15), (14, 9), (9, 14)] {
        let mut cpu = cpu();
        cpu.regs.xmm[destination] = SENTINEL;
        cpu.regs.xmm[source] = singles([(-3.5_f32).to_bits(), 42.0_f32.to_bits(), 1, 2]);
        let mut xmm = cpu.regs.xmm;
        let flags = cpu.regs.rflags;
        let rex = 0x40 | ((destination >> 3) as u8 * 4) | (source >> 3) as u8;
        let modrm = 0xC0 | ((destination & 7) as u8 * 8) | (source & 7) as u8;

        run(&mut cpu, &[rex, 0x0F, 0x5A, modrm]).unwrap();

        xmm[destination] = doubles((-3.5_f64).to_bits(), 42.0_f64.to_bits());
        assert_eq!(cpu.regs.xmm, xmm);
        assert_eq!(cpu.regs.rflags, flags);
    }
}

fn paging(cpu: &mut Cpu, source: u64) {
    cpu.regs.cr0 |= Cr0::PG | Cr0::WP;
    cpu.regs.cr4 |= Cr4::PAE;
    cpu.regs.cr3 = 0x10000;
    cpu.memory.write_u64(0x10000, 0x11003).unwrap();
    cpu.memory.write_u64(0x11000, 0x12003).unwrap();
    cpu.memory.write_u64(0x12000, 0x13003).unwrap();
    cpu.memory.write_u64(0x13000 + 8, 0x1003).unwrap();
    cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50003).unwrap();
    cpu.regs.set_gpr(index::RBX, source);
}

#[test]
fn cvtps2pd_memory_source_at_page_end_reads_only_eight_bytes() {
    let mut cpu = cpu();
    paging(&mut cpu, 0x40FF8);
    let source = u64::from((-3.5_f32).to_bits()) | (u64::from(42.0_f32.to_bits()) << 32);
    cpu.memory.write_u64(0x50FF8, source).unwrap();
    let flags = cpu.regs.rflags;

    run(&mut cpu, &[0x0F, 0x5A, 0x03]).unwrap(); // cvtps2pd xmm0, m64 [rbx]

    assert_eq!(
        cpu.regs.xmm[0],
        doubles((-3.5_f64).to_bits(), 42.0_f64.to_bits())
    );
    assert_eq!(cpu.memory.read_u64(0x50FF8).unwrap(), source);
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.mxcsr, 0x1F80);
}

#[test]
fn cvtps2pd_cross_page_fault_cannot_commit_first_lane_or_exception_status() {
    for first_lane in [1, 0x7F80_0001] {
        let mut cpu = cpu();
        paging(&mut cpu, 0x40FFC);
        cpu.memory.write_u32(0x50FFC, first_lane).unwrap();
        let xmm = cpu.regs.xmm;
        let gpr = cpu.regs.gpr;
        let flags = cpu.regs.rflags;
        let mxcsr = cpu.mxcsr;
        let instruction = decode(&[0x0F, 0x5A, 0x03]);

        assert_eq!(
            cpu.dispatch(&instruction).unwrap_err(),
            CpuError::PageFault {
                linear: 0x41000,
                error_code: 0,
            }
        );

        assert_eq!(cpu.regs.xmm, xmm);
        assert_eq!(cpu.regs.gpr, gpr);
        assert_eq!(cpu.regs.rflags, flags);
        assert_eq!(cpu.mxcsr, mxcsr);
    }
}

#[test]
fn cvtps2pd_reads_noncontiguous_pages_and_accumulates_masked_lane_exceptions() {
    let mut cpu = cpu();
    paging(&mut cpu, 0x40FFC);
    cpu.memory.write_u64(0x13000 + 0x41 * 8, 0x60003).unwrap();
    cpu.memory.write_u32(0x50FFC, 1).unwrap();
    cpu.memory.write_u32(0x60000, 0x7F80_0001).unwrap();

    run(&mut cpu, &[0x0F, 0x5A, 0x03]).unwrap();

    assert_eq!(
        cpu.regs.xmm[0],
        doubles(0x36A0_0000_0000_0000, 0x7FF8_0000_2000_0000)
    );
    assert_eq!(cpu.mxcsr, 0x1F83);
}

fn handler(cpu: &mut Cpu, vector: u8) {
    cpu.regs.cs = Descriptor::decode(0x00AF_9B00_0000_FFFF).load(SegmentSelector(8));
    cpu.regs.gdt_base = 0x3000;
    cpu.regs.gdt_limit = 0xFF;
    cpu.memory.write_u64(0x3008, 0x00AF_9B00_0000_FFFF).unwrap();
    cpu.regs.set_rsp(0x9000);
    cpu.regs.idt_base = 0x4000;
    cpu.regs.idt_limit = 0xFFF;
    cpu.memory
        .write_u64(
            0x4000 + u64::from(vector) * 16,
            (8 << 16) | 0x8E00_0000_0000 | 0xA000,
        )
        .unwrap();
    cpu.memory
        .write_u64(0x4000 + u64::from(vector) * 16 + 8, 0)
        .unwrap();
}

#[test]
fn cvtps2pd_unmasked_invalid_or_denormal_delivers_xm_or_ud_without_writing_lanes() {
    for osxmmexcpt in [false, true] {
        for (source, exception, mask) in [(0x7F80_0001, 1, 1 << 7), (1, 2, 1 << 8)] {
            for lane in 0..2 {
                let mut cpu = cpu();
                let vector = if osxmmexcpt { 19 } else { 6 };
                handler(&mut cpu, vector);
                cpu.regs.cr4.set(Cr4::OSXMMEXCPT, osxmmexcpt);
                cpu.mxcsr &= !mask;
                let mut input = [1.0_f32.to_bits(); 4];
                input[lane] = source;
                cpu.regs.xmm[1] = singles(input);
                let xmm = cpu.regs.xmm;
                let flags = cpu.regs.rflags;
                let mxcsr = cpu.mxcsr;

                run(&mut cpu, &[0x0F, 0x5A, 0xC1]).unwrap();

                assert_eq!(cpu.fault_log.back().unwrap().vector, vector);
                assert_eq!(cpu.fault_log.back().unwrap().rip, CODE);
                assert_eq!(cpu.exceptions_raised, 1);
                assert_eq!(cpu.regs.xmm, xmm);
                assert_eq!(cpu.memory.read_u64(0x9000 - 24).unwrap(), flags.bits());
                assert_eq!(cpu.mxcsr, mxcsr | exception);
            }
        }
    }
}

#[test]
fn cvtps2pd_mixed_invalid_and_denormal_summary_is_independent_of_lane_order() {
    for reversed in [false, true] {
        for unmasked in 0..4 {
            let mut cpu = cpu();
            handler(&mut cpu, 19);
            cpu.regs.cr4 |= Cr4::OSXMMEXCPT;
            cpu.mxcsr &= !(unmasked << 7);
            let mut input = [0x7F80_0001, 1, 0, 0];
            let mut output = [0x7FF8_0000_2000_0000, 0x36A0_0000_0000_0000];
            if reversed {
                input.swap(0, 1);
                output.swap(0, 1);
            }
            cpu.regs.xmm[1] = singles(input);
            let mut xmm = cpu.regs.xmm;
            let mxcsr = cpu.mxcsr;

            run(&mut cpu, &[0x0F, 0x5A, 0xC1]).unwrap();

            assert_eq!(cpu.mxcsr, mxcsr | 3, "both lane exceptions are sticky");
            if unmasked == 0 {
                xmm[0] = doubles(output[0], output[1]);
                assert_eq!(cpu.exceptions_raised, 0);
            } else {
                assert_eq!(cpu.exceptions_raised, 1);
                assert_eq!(cpu.fault_log.back().unwrap().vector, 19);
                assert_eq!(cpu.fault_log.back().unwrap().rip, CODE);
            }
            assert_eq!(cpu.regs.xmm, xmm);
        }
    }
}
