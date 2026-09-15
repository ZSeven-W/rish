use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
use crate::{Cpu, CpuError};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

const CODE: u64 = 0x1000;

fn cpu() -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cs.long_mode = true;
    cpu.regs.rflags = RFlags::CF | RFlags::PF | RFlags::AF | RFlags::ZF | RFlags::SF | RFlags::OF;
    cpu
}

fn packed(lanes: [u32; 4]) -> u128 {
    u128::from_le_bytes(lanes.map(u32::to_le_bytes).concat().try_into().unwrap())
}

fn decode(bytes: &[u8], mnemonic: Mnemonic) -> Instruction {
    let instruction = Decoder::with_ip(64, bytes, CODE, DecoderOptions::NONE).decode();
    assert_eq!(instruction.mnemonic(), mnemonic);
    assert_eq!(instruction.len(), bytes.len());
    instruction
}

fn run(cpu: &mut Cpu, bytes: &[u8], mnemonic: Mnemonic) -> Result<(), CpuError> {
    decode(bytes, mnemonic);
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step()
}

#[test]
fn pavgb_rounds_up_each_unsigned_byte_without_overflow() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = u128::from_le_bytes([
        0, 0, 1, 1, 254, 254, 255, 255, 0, 255, 127, 128, 2, 3, 42, 200,
    ]);
    cpu.regs.xmm[1] = u128::from_le_bytes([
        0, 1, 0, 1, 254, 255, 254, 255, 255, 0, 128, 127, 3, 2, 43, 201,
    ]);
    let mut xmm = cpu.regs.xmm;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;

    run(&mut cpu, &[0x66, 0x0f, 0xe0, 0xc1], Mnemonic::Pavgb).unwrap();

    xmm[0] = u128::from_le_bytes([
        0, 1, 1, 1, 254, 255, 255, 255, 128, 128, 128, 128, 3, 3, 43, 201,
    ]);
    assert_eq!(cpu.regs.xmm, xmm);
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.mxcsr, mxcsr);
    assert_eq!(cpu.regs.rip, CODE + 4);
}

#[test]
fn phaddd_concatenates_adjacent_destination_and_source_sums_with_wrapping() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = packed([1, 2, 0x7fff_ffff, 1]);
    cpu.regs.xmm[1] = packed([u32::MAX, 2, 0x8000_0000, 0x8000_0000]);
    let mut xmm = cpu.regs.xmm;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;

    run(&mut cpu, &[0x66, 0x0f, 0x38, 0x02, 0xc1], Mnemonic::Phaddd).unwrap();

    xmm[0] = packed([3, 0x8000_0000, 1, 0]);
    assert_eq!(cpu.regs.xmm, xmm);
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.mxcsr, mxcsr);
    assert_eq!(cpu.regs.rip, CODE + 5);
}

fn opcodes(mnemonic: Mnemonic) -> &'static [u8] {
    match mnemonic {
        Mnemonic::Pavgb => &[0x0f, 0xe0],
        Mnemonic::Phaddd => &[0x0f, 0x38, 0x02],
        _ => panic!("unexpected test instruction"),
    }
}

fn encoding(mnemonic: Mnemonic, prefix: &[u8], suffix: &[u8]) -> Vec<u8> {
    [prefix, opcodes(mnemonic), suffix].concat()
}

fn expected(mnemonic: Mnemonic, left: u128, right: u128) -> u128 {
    let a = left.to_le_bytes();
    let b = right.to_le_bytes();
    match mnemonic {
        Mnemonic::Pavgb => {
            // ceil((a+b)/2) = max bit union minus half the differing bits.
            // This oracle cannot overflow and does not mirror widened addition.
            u128::from_le_bytes(std::array::from_fn(|i| {
                (a[i] | b[i]) - ((a[i] ^ b[i]) >> 1)
            }))
        }
        Mnemonic::Phaddd => {
            let mut lanes = [0; 4];
            for (index, pair) in a.chunks_exact(8).chain(b.chunks_exact(8)).enumerate() {
                let lo = i32::from_le_bytes(pair[..4].try_into().unwrap());
                let hi = i32::from_le_bytes(pair[4..].try_into().unwrap());
                lanes[index] = (i64::from(lo) + i64::from(hi)) as u32;
            }
            packed(lanes)
        }
        _ => panic!("unexpected test instruction"),
    }
}

#[test]
fn encodings_distinguish_xmm_from_legacy_mmx_and_memory_operands() {
    for mnemonic in [Mnemonic::Pavgb, Mnemonic::Phaddd] {
        let register = decode(&encoding(mnemonic, &[0x66], &[0xc1]), mnemonic);
        assert_eq!(register.op0_register(), Register::XMM0);
        assert_eq!(register.op1_register(), Register::XMM1);
        let memory = decode(&encoding(mnemonic, &[0x66], &[0x03]), mnemonic);
        assert_eq!(memory.op0_register(), Register::XMM0);
        assert_eq!(memory.op1_kind(), OpKind::Memory);
        assert_eq!(memory.memory_base(), Register::RBX);
        let mmx = decode(&encoding(mnemonic, &[], &[0xc1]), mnemonic);
        assert_eq!(mmx.op0_register(), Register::MM0);
        assert_eq!(mmx.op1_register(), Register::MM1);
    }
}

fn register_pairs(mnemonic: Mnemonic) {
    for destination in 0..16 {
        for source in 0..16 {
            let mut cpu = cpu();
            for (register, value) in cpu.regs.xmm.iter_mut().enumerate() {
                let seed = register as u32 + 1;
                *value = packed([
                    seed,
                    0x7fff_ffff + seed,
                    u32::MAX - seed,
                    0xffff_ff00 + seed,
                ]);
            }
            let mut xmm = cpu.regs.xmm;
            xmm[destination] = expected(mnemonic, xmm[destination], xmm[source]);
            let flags = cpu.regs.rflags;
            let mxcsr = cpu.mxcsr;
            let gpr = cpu.regs.gpr;
            let rex = 0x40 | ((destination >> 3) as u8 * 4) | (source >> 3) as u8;
            let modrm = 0xc0 | ((destination & 7) as u8 * 8) | (source & 7) as u8;
            let bytes = encoding(mnemonic, &[0x66, rex], &[modrm]);
            let instruction = decode(&bytes, mnemonic);
            assert_eq!(
                instruction.op0_register() as usize,
                Register::XMM0 as usize + destination
            );
            assert_eq!(
                instruction.op1_register() as usize,
                Register::XMM0 as usize + source
            );

            run(&mut cpu, &bytes, mnemonic).unwrap();

            assert_eq!(
                cpu.regs.xmm, xmm,
                "{mnemonic:?} xmm{destination}, xmm{source}"
            );
            assert_eq!(cpu.regs.gpr, gpr);
            assert_eq!(cpu.regs.rflags, flags);
            assert_eq!(cpu.mxcsr, mxcsr);
            assert_eq!(cpu.regs.rip, CODE + bytes.len() as u64);
        }
    }
}

#[test]
fn pavgb_selects_all_xmm_register_pairs_and_self_alias_is_identity() {
    register_pairs(Mnemonic::Pavgb);
}

#[test]
fn phaddd_selects_all_xmm_register_pairs_and_self_alias_duplicates_original_pair_sums() {
    register_pairs(Mnemonic::Phaddd);
}

#[test]
fn memory_sources_use_rex_sib_and_preserve_source_flags_and_other_registers() {
    for mnemonic in [Mnemonic::Pavgb, Mnemonic::Phaddd] {
        let mut cpu = cpu();
        cpu.regs.set_gpr(index::R12, 0x2000);
        cpu.regs.set_gpr(index::R13, 3);
        cpu.regs.xmm[14] = packed([0x7fff_ffff, 0xffff_ffff, 0xff00_00ff, 0xff00_0100]);
        let source = packed([0x8000_0000, 0x8000_0001, 0xff00_ffff, 0x00ff_ff00]);
        cpu.memory.write(0x2030, &source.to_le_bytes()).unwrap();
        let mut xmm = cpu.regs.xmm;
        xmm[14] = expected(mnemonic, xmm[14], source);
        let gpr = cpu.regs.gpr;
        let flags = cpu.regs.rflags;
        let mxcsr = cpu.mxcsr;

        // xmm14, [r12 + r13*4 + 0x24]
        run(
            &mut cpu,
            &encoding(mnemonic, &[0x66, 0x47], &[0x74, 0xac, 0x24]),
            mnemonic,
        )
        .unwrap();

        assert_eq!(cpu.regs.xmm, xmm);
        assert_eq!(cpu.regs.gpr, gpr);
        assert_eq!(cpu.regs.rflags, flags);
        assert_eq!(cpu.mxcsr, mxcsr);
        let mut after = [0; 16];
        cpu.memory.read(0x2030, &mut after).unwrap();
        assert_eq!(u128::from_le_bytes(after), source);
    }
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
    cpu.regs.set_gpr(index::RBX, 0x40ff8);
}

#[test]
fn cross_page_fault_after_reading_low_half_preserves_all_registers() {
    for mnemonic in [Mnemonic::Pavgb, Mnemonic::Phaddd] {
        let mut cpu = cpu();
        paging(&mut cpu);
        cpu.regs.xmm[0] = packed([0x7fff_ffff, 1, 0x8000_0000, 0x8000_0000]);
        cpu.memory.write_u64(0x50ff8, u64::MAX).unwrap();
        let xmm = cpu.regs.xmm;
        let gpr = cpu.regs.gpr;
        let flags = cpu.regs.rflags;
        let mxcsr = cpu.mxcsr;
        let instruction = decode(&encoding(mnemonic, &[0x66], &[0x03]), mnemonic);

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
fn memory_sources_read_both_noncontiguous_physical_pages() {
    for mnemonic in [Mnemonic::Pavgb, Mnemonic::Phaddd] {
        let mut cpu = cpu();
        paging(&mut cpu);
        cpu.memory.write_u64(0x13000 + 0x41 * 8, 0x60003).unwrap();
        cpu.regs.xmm[0] = packed([0x7fff_ffff, 1, 0x8000_0000, 0x8000_0000]);
        let source = packed([0xffff_ffff, 2, 0x0080_00ff, 0xff00_00ff]);
        let bytes = source.to_le_bytes();
        cpu.memory.write(0x50ff8, &bytes[..8]).unwrap();
        cpu.memory.write(0x60000, &bytes[8..]).unwrap();
        let mut xmm = cpu.regs.xmm;
        xmm[0] = expected(mnemonic, xmm[0], source);

        run(&mut cpu, &encoding(mnemonic, &[0x66], &[0x03]), mnemonic).unwrap();

        assert_eq!(cpu.regs.xmm, xmm);
    }
}

#[test]
fn legacy_mmx_register_and_memory_forms_fail_closed_without_touching_xmm_or_x87() {
    for mnemonic in [Mnemonic::Pavgb, Mnemonic::Phaddd] {
        for suffix in [0xc1, 0x03] {
            let mut cpu = cpu();
            cpu.regs.xmm[0] = u128::MAX;
            cpu.regs.xmm[1] = packed([1, 2, 3, 4]);
            cpu.regs.set_gpr(index::RBX, 0xffff8);
            cpu.fpu_stack = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
            cpu.fpu_top = 6;
            cpu.fpu_status_word = 0x5a5a;
            let xmm = cpu.regs.xmm;
            let stack = cpu.fpu_stack.map(f64::to_bits);
            let flags = cpu.regs.rflags;
            let mxcsr = cpu.mxcsr;
            let control = cpu.fpu_control_word;
            let instruction = decode(&encoding(mnemonic, &[], &[suffix]), mnemonic);
            assert_eq!(instruction.op0_register(), Register::MM0);

            assert!(matches!(
                cpu.dispatch(&instruction),
                Err(CpuError::UnimplementedInstruction { .. })
            ));

            assert_eq!(cpu.regs.xmm, xmm);
            assert_eq!(cpu.regs.rflags, flags);
            assert_eq!(cpu.mxcsr, mxcsr);
            assert_eq!(cpu.fpu_stack.map(f64::to_bits), stack);
            assert_eq!(cpu.fpu_top, 6);
            assert_eq!(cpu.fpu_status_word, 0x5a5a);
            assert_eq!(cpu.fpu_control_word, control);
        }
    }
}
