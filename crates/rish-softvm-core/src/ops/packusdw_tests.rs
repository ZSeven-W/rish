use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
use crate::{Cpu, CpuError};

const CODE: u64 = 0x1000;

fn cpu() -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cs.long_mode = true;
    cpu.regs.rflags = RFlags::CF | RFlags::PF | RFlags::AF | RFlags::ZF | RFlags::SF | RFlags::OF;
    cpu
}

fn dwords(lanes: [i32; 4]) -> u128 {
    u128::from_le_bytes(lanes.map(i32::to_le_bytes).concat().try_into().unwrap())
}

fn words(lanes: [u16; 8]) -> u128 {
    u128::from_le_bytes(lanes.map(u16::to_le_bytes).concat().try_into().unwrap())
}

fn decode(bytes: &[u8]) -> iced_x86::Instruction {
    let instruction =
        iced_x86::Decoder::with_ip(64, bytes, CODE, iced_x86::DecoderOptions::NONE).decode();
    assert_eq!(instruction.mnemonic(), iced_x86::Mnemonic::Packusdw);
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
fn packusdw_saturates_signed_dwords_and_appends_source_after_destination() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = dwords([i32::MIN, -1, 0, 1]);
    cpu.regs.xmm[1] = dwords([65_534, 65_535, 65_536, i32::MAX]);
    let mut xmm = cpu.regs.xmm;
    let gpr = cpu.regs.gpr;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;

    // Legacy SSE4.1: packusdw xmm0, xmm1.
    run(&mut cpu, &[0x66, 0x0F, 0x38, 0x2B, 0xC1]).unwrap();

    xmm[0] = words([0, 0, 0, 1, 65_534, 65_535, 65_535, 65_535]);
    assert_eq!(
        cpu.regs.xmm, xmm,
        "source and unrelated registers stay unchanged"
    );
    assert_eq!(cpu.regs.gpr, gpr);
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.mxcsr, mxcsr);
    assert_eq!(cpu.regs.rip, CODE + 5);
}

#[test]
fn packusdw_rex_high_registers_and_self_alias_preserve_original_input_lanes() {
    for (destination, source) in [(0, 0), (15, 15), (14, 9), (9, 14), (0, 15), (15, 0)] {
        let mut cpu = cpu();
        cpu.regs.xmm[destination] = dwords([-4, 1, 32_768, 65_535]);
        cpu.regs.xmm[source] = dwords([17, -2, 65_536, i32::MAX]);
        let mut xmm = cpu.regs.xmm;
        let gpr = cpu.regs.gpr;
        let flags = cpu.regs.rflags;
        let rex = 0x40 | ((destination >> 3) as u8 * 4) | (source >> 3) as u8;
        let modrm = 0xC0 | ((destination & 7) as u8 * 8) | (source & 7) as u8;

        run(&mut cpu, &[0x66, rex, 0x0F, 0x38, 0x2B, modrm]).unwrap();

        xmm[destination] = if destination == source {
            words([17, 0, 65_535, 65_535, 17, 0, 65_535, 65_535])
        } else {
            words([0, 1, 32_768, 65_535, 17, 0, 65_535, 65_535])
        };
        assert_eq!(cpu.regs.xmm, xmm, "xmm{destination}, xmm{source}");
        assert_eq!(cpu.regs.gpr, gpr);
        assert_eq!(cpu.regs.rflags, flags);
    }
}

#[test]
fn packusdw_memory_source_uses_rex_sib_and_remains_unchanged() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::R12, 0x2000);
    cpu.regs.set_gpr(index::R13, 3);
    cpu.regs.xmm[14] = dwords([-1, 10, 65_536, 20]);
    let source = dwords([-2, 30, 65_535, i32::MAX]).to_le_bytes();
    cpu.memory.write(0x2030, &source).unwrap();
    let mut xmm = cpu.regs.xmm;
    let gpr = cpu.regs.gpr;
    let flags = cpu.regs.rflags;

    // packusdw xmm14, [r12 + r13*4 + 0x24]
    run(&mut cpu, &[0x66, 0x47, 0x0F, 0x38, 0x2B, 0x74, 0xAC, 0x24]).unwrap();

    xmm[14] = words([0, 10, 65_535, 20, 0, 30, 65_535, 65_535]);
    assert_eq!(cpu.regs.xmm, xmm);
    assert_eq!(cpu.regs.gpr, gpr);
    assert_eq!(cpu.regs.rflags, flags);
    let mut after = [0; 16];
    cpu.memory.read(0x2030, &mut after).unwrap();
    assert_eq!(after, source);
}

fn paging(cpu: &mut Cpu) {
    cpu.regs.cr0 |= Cr0::PG | Cr0::WP;
    cpu.regs.cr4 |= Cr4::PAE;
    cpu.regs.cr3 = 0x10000;
    cpu.memory.write_u64(0x10000, 0x11003).unwrap();
    cpu.memory.write_u64(0x11000, 0x12003).unwrap();
    cpu.memory.write_u64(0x12000, 0x13003).unwrap();
    cpu.memory.write_u64(0x13000 + 8, 0x1003).unwrap();
    cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50003).unwrap();
    cpu.regs.set_gpr(index::RBX, 0x40FF8);
}

#[test]
fn packusdw_cross_page_fault_preserves_destination_before_packing_any_lanes() {
    let mut cpu = cpu();
    paging(&mut cpu);
    cpu.regs.xmm[0] = dwords([-1, 10, 65_536, 20]);
    cpu.memory.write_u64(0x50FF8, u64::MAX).unwrap();
    let xmm = cpu.regs.xmm;
    let gpr = cpu.regs.gpr;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;
    let instruction = decode(&[0x66, 0x0F, 0x38, 0x2B, 0x03]); // packusdw xmm0, [rbx]

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

#[test]
fn packusdw_memory_source_reads_both_noncontiguous_physical_pages() {
    let mut cpu = cpu();
    paging(&mut cpu);
    cpu.memory.write_u64(0x13000 + 0x41 * 8, 0x60003).unwrap();
    cpu.regs.xmm[0] = dwords([-1, 10, 65_536, 20]);
    let source = dwords([-2, 30, 65_535, i32::MAX]).to_le_bytes();
    cpu.memory.write(0x50FF8, &source[..8]).unwrap();
    cpu.memory.write(0x60000, &source[8..]).unwrap();

    run(&mut cpu, &[0x66, 0x0F, 0x38, 0x2B, 0x03]).unwrap();

    assert_eq!(
        cpu.regs.xmm[0],
        words([0, 10, 65_535, 20, 0, 30, 65_535, 65_535])
    );
}
