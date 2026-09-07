use super::*;

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    cpu.regs.efer |= crate::arch::registers::Efer::LMA;
    cpu.regs.cs.long_mode = true;
    cpu.dispatch(
        &iced_x86::Decoder::with_ip(64, bytes, 0x1000, iced_x86::DecoderOptions::NONE).decode(),
    )
}

#[test]
fn variable_shift_reads_low_xmm_qword_instead_of_same_numbered_gpr() {
    for (opcode, input, count, expected) in [
        (
            0xf1,
            0x1234_1234_1234_1234_1234_1234_1234_1234_u128,
            1,
            0x2468_2468_2468_2468_2468_2468_2468_2468,
        ),
        (
            0xd3,
            u128::MAX,
            1,
            0x7fff_ffff_ffff_ffff_7fff_ffff_ffff_ffff,
        ),
        (
            0xe1,
            0xfff0_fff0_fff0_fff0_fff0_fff0_fff0_fff0,
            4,
            u128::MAX,
        ),
    ] {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = input;
        cpu.regs.xmm[1] = (u128::from(u64::MAX) << 64) | count;
        cpu.regs.gpr[1] = 16;
        let flags = cpu.regs.rflags;
        run(&mut cpu, &[0x66, 0x0f, opcode, 0xc1]).unwrap();
        assert_eq!(cpu.regs.xmm[0], expected);
        assert_eq!(cpu.regs.rflags, flags);
    }
}

#[test]
fn memory_shift_count_is_loaded_and_memory_fault_preserves_destination() {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.xmm[0] = 0x0000_0001_0000_0001_0000_0001_0000_0001;
    cpu.regs.gpr[0] = 0x2000;
    cpu.memory.write(0x2000, &2_u128.to_le_bytes()).unwrap();
    run(&mut cpu, &[0x66, 0x0f, 0xf2, 0x00]).unwrap();
    assert_eq!(cpu.regs.xmm[0], 0x0000_0004_0000_0004_0000_0004_0000_0004);
    let previous = cpu.regs.xmm[0];
    cpu.regs.gpr[0] = 0xffff8;
    assert!(run(&mut cpu, &[0x66, 0x0f, 0xf2, 0x00]).is_err());
    assert_eq!(cpu.regs.xmm[0], previous);
}

#[test]
fn byte_shifts_at_or_above_16_clear_the_register_without_wrapping() {
    for (modrm, left) in [(0xf8, true), (0xd8, false)] {
        for count in [0, 1, 15, 16, 255] {
            let mut cpu = Cpu::new(1, 0).unwrap();
            cpu.regs.xmm[0] = u128::MAX;
            run(&mut cpu, &[0x66, 0x0f, 0x73, modrm, count]).unwrap();
            let expected = if count >= 16 {
                0
            } else if left {
                u128::MAX << (count * 8)
            } else {
                u128::MAX >> (count * 8)
            };
            assert_eq!(cpu.regs.xmm[0], expected);
        }
    }
}

#[test]
fn scalar_memory_loads_clear_upper_lanes_but_register_moves_preserve_them() {
    for (prefix, width) in [(0xf3, 4), (0xf2, 8)] {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.xmm[0] = u128::MAX;
        cpu.regs.gpr[0] = 0x2000;
        cpu.memory.write(0x2000, &0x1234_u64.to_le_bytes()).unwrap();
        run(&mut cpu, &[prefix, 0x0f, 0x10, 0x00]).unwrap();
        assert_eq!(cpu.regs.xmm[0], 0x1234);
        cpu.regs.xmm[0] = u128::MAX;
        cpu.regs.xmm[1] = 0x1234;
        run(&mut cpu, &[prefix, 0x0f, 0x10, 0xc1]).unwrap();
        let mask = (1_u128 << (width * 8)) - 1;
        assert_eq!(cpu.regs.xmm[0], !mask | 0x1234);
    }
}

#[test]
fn word_shuffle_loads_memory_and_preserves_the_unshuffled_half() {
    for (prefix, expected) in [
        (0xf2, [3_u16, 2, 1, 0, 4, 5, 6, 7]),
        (0xf3, [0_u16, 1, 2, 3, 7, 6, 5, 4]),
    ] {
        let mut cpu = Cpu::new(1, 0).unwrap();
        cpu.regs.gpr[0] = 0x2000;
        let mut data = [0; 16];
        for i in 0..8 {
            data[i * 2..i * 2 + 2].copy_from_slice(&(i as u16).to_le_bytes());
        }
        cpu.memory.write(0x2000, &data).unwrap();
        run(&mut cpu, &[prefix, 0x0f, 0x70, 0x00, 0x1b]).unwrap();
        let result = cpu.regs.xmm[0].to_le_bytes();
        let words: Vec<u16> = result
            .chunks_exact(2)
            .map(|x| u16::from_le_bytes(x.try_into().unwrap()))
            .collect();
        assert_eq!(words, expected);
    }
}
