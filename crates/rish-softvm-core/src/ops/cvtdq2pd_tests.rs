use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
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

fn dwords(lanes: [i32; 4]) -> u128 {
    u128::from_le_bytes(lanes.map(i32::to_le_bytes).concat().try_into().unwrap())
}

fn doubles(low: u64, high: u64) -> u128 {
    u128::from(low) | (u128::from(high) << 64)
}

fn decode(bytes: &[u8]) -> iced_x86::Instruction {
    let instruction =
        iced_x86::Decoder::with_ip(64, bytes, CODE, iced_x86::DecoderOptions::NONE).decode();
    assert_eq!(instruction.mnemonic(), iced_x86::Mnemonic::Cvtdq2pd);
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
fn cvtdq2pd_widens_signed_integer_extremes_into_the_whole_destination() {
    let mut cpu = cpu();
    cpu.regs.xmm[1] = dwords([i32::MIN, i32::MAX, 0x7F80_0001, 1]);
    let mut xmm = cpu.regs.xmm;
    let gpr = cpu.regs.gpr;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;

    // Legacy SSE2: cvtdq2pd xmm0, xmm1.
    run(&mut cpu, &[0xF3, 0x0F, 0xE6, 0xC1]).unwrap();

    xmm[0] = doubles(0xC1E0_0000_0000_0000, 0x41DF_FFFF_FFC0_0000);
    assert_eq!(cpu.regs.xmm, xmm);
    assert_eq!(cpu.regs.gpr, gpr);
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.mxcsr, mxcsr);
    assert_eq!(cpu.regs.rip, CODE + 4);
}

#[test]
fn cvtdq2pd_integer_bits_ignore_all_mxcsr_controls_and_preserve_sticky_status() {
    for input in [[0, -1], [1, 0x7F80_0001], [16_777_217, -16_777_217]] {
        for rounding in 0..4 {
            for daz in [0, 1 << 6] {
                for ftz in [0, 1 << 15] {
                    for masks in [0, 0x1F80] {
                        for sticky in [0, 0x3F] {
                            let mut cpu = cpu();
                            cpu.mxcsr = (rounding << 13) | daz | ftz | masks | sticky;
                            // The high lanes are irrelevant; low-lane 0x7F800001
                            // must be an integer even though its bits are sNaN.
                            cpu.regs.xmm[1] = dwords([input[0], input[1], i32::MIN, 0x7F80_0001]);
                            let mut xmm = cpu.regs.xmm;
                            let mxcsr = cpu.mxcsr;
                            let flags = cpu.regs.rflags;

                            run(&mut cpu, &[0xF3, 0x0F, 0xE6, 0xC1]).unwrap();

                            xmm[0] = doubles(
                                f64::from(input[0]).to_bits(),
                                f64::from(input[1]).to_bits(),
                            );
                            assert_eq!(cpu.regs.xmm, xmm);
                            assert_eq!(cpu.mxcsr, mxcsr);
                            assert_eq!(cpu.regs.rflags, flags);
                            assert_eq!(cpu.exceptions_raised, 0);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn cvtdq2pd_high_xmm_and_self_alias_read_both_original_dwords() {
    for (destination, source) in [(0, 0), (15, 15), (14, 9), (9, 14)] {
        let mut cpu = cpu();
        cpu.regs.xmm[destination] = SENTINEL;
        cpu.regs.xmm[source] = dwords([-42, 16_777_217, i32::MIN, i32::MAX]);
        let mut xmm = cpu.regs.xmm;
        let gpr = cpu.regs.gpr;
        let flags = cpu.regs.rflags;
        let rex = 0x40 | ((destination >> 3) as u8 * 4) | (source >> 3) as u8;
        let modrm = 0xC0 | ((destination & 7) as u8 * 8) | (source & 7) as u8;

        run(&mut cpu, &[0xF3, rex, 0x0F, 0xE6, modrm]).unwrap();

        xmm[destination] = doubles((-42_f64).to_bits(), 16_777_217_f64.to_bits());
        assert_eq!(cpu.regs.xmm, xmm);
        assert_eq!(cpu.regs.gpr, gpr);
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
fn cvtdq2pd_page_end_memory_source_reads_only_eight_bytes() {
    let mut cpu = cpu();
    paging(&mut cpu, 0x40FF8);
    let source = 0x7FFF_FFFF_8000_0000;
    cpu.memory.write_u64(0x50FF8, source).unwrap();
    let flags = cpu.regs.rflags;
    let gpr = cpu.regs.gpr;

    run(&mut cpu, &[0xF3, 0x0F, 0xE6, 0x03]).unwrap(); // cvtdq2pd xmm0, m64 [rbx]

    assert_eq!(
        cpu.regs.xmm[0],
        doubles(0xC1E0_0000_0000_0000, 0x41DF_FFFF_FFC0_0000)
    );
    assert_eq!(cpu.memory.read_u64(0x50FF8).unwrap(), source);
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.regs.gpr, gpr);
    assert_eq!(cpu.mxcsr, 0x1F80);
}

#[test]
fn cvtdq2pd_cross_page_fault_preserves_destination_and_mxcsr() {
    for mxcsr in [0, 0xFFFF] {
        let mut cpu = cpu();
        paging(&mut cpu, 0x40FFC);
        cpu.mxcsr = mxcsr;
        cpu.memory.write_u32(0x50FFC, 0xDEAD_BEEF).unwrap();
        let xmm = cpu.regs.xmm;
        let gpr = cpu.regs.gpr;
        let flags = cpu.regs.rflags;
        let instruction = decode(&[0xF3, 0x0F, 0xE6, 0x03]);

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
fn cvtdq2pd_reads_two_signed_dwords_from_noncontiguous_physical_pages() {
    let mut cpu = cpu();
    paging(&mut cpu, 0x40FFC);
    cpu.memory.write_u64(0x13000 + 0x41 * 8, 0x60003).unwrap();
    cpu.memory.write_u32(0x50FFC, u32::MAX).unwrap();
    cpu.memory.write_u32(0x60000, 0x7FFF_FFFF).unwrap();

    run(&mut cpu, &[0xF3, 0x0F, 0xE6, 0x03]).unwrap();

    assert_eq!(
        cpu.regs.xmm[0],
        doubles((-1_f64).to_bits(), 0x41DF_FFFF_FFC0_0000)
    );
    assert_eq!(cpu.mxcsr, 0x1F80);
}
