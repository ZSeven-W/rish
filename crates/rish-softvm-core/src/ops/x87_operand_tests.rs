use crate::Cpu;
use crate::arch::registers::{Cr0, Efer, RFlags, index};
use crate::arch::segments::{Descriptor, SegmentSelector};

const CODE: u64 = 0x1000;
const DATA: u64 = 0x2000;

#[test]
fn x87_fxch_swaps_explicit_second_register_without_popping_or_touching_integer_flags() {
    for top in 0..8_u8 {
        for index in 0..8_u8 {
            let mut cpu = cpu(12.0, 5.0);
            cpu.fpu_top = top;
            // Swap must preserve exact payload bits, including signed zero
            // and NaNs, rather than calculating with either register value.
            cpu.fpu_stack[2] = -0.0;
            cpu.fpu_stack[4] = f64::from_bits(0x7ff0_0000_0000_0123);
            cpu.fpu_status_word = 0x5a5a;
            let flags = cpu.regs.rflags;
            let before = cpu.fpu_stack.map(f64::to_bits);
            let target = usize::from((top + index) & 7);
            let mut expected = before;
            expected.swap(usize::from(top), target);
            run(&mut cpu, &[0xd9, 0xc8 + index]);
            assert_eq!(
                cpu.fpu_stack.map(f64::to_bits),
                expected,
                "top={top} ST({index})"
            );
            assert_eq!(cpu.fpu_top, top);
            assert_eq!(cpu.regs.rflags, flags);
            assert_eq!(cpu.fpu_status_word, 0x5a5a & !(1 << 9));
        }
    }
}

fn cpu(a: f64, b: f64) -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cr0 |= Cr0::PE | Cr0::NE;
    cpu.regs.cs = Descriptor::decode(0x00af_9b00_0000_ffff).load(SegmentSelector(8));
    cpu.regs.set_gpr(index::RBX, DATA);
    cpu.fpu_top = 6;
    cpu.fpu_stack = [29.0, b, 37.0, 41.0, 43.0, 47.0, a, 23.0];
    cpu
}

fn run(cpu: &mut Cpu, bytes: &[u8]) {
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step().unwrap();
}

#[test]
fn x87_arithmetic_st0_uses_explicit_second_operand() {
    for (modrm, expected) in [
        (0xc3, 17.0),
        (0xcb, 60.0),
        (0xe3, 7.0),
        (0xeb, -7.0),
        (0xf3, 12.0 / 5.0),
        (0xfb, 5.0 / 12.0),
    ] {
        let mut cpu = cpu(12.0, 5.0);
        let old = cpu.fpu_stack;
        run(&mut cpu, &[0xd8, modrm]);
        assert_eq!(cpu.fpu_stack[6], expected, "D8 {modrm:02X}");
        assert_eq!(cpu.fpu_top, 6);
        for (slot, previous) in old.iter().enumerate() {
            if slot != 6 {
                assert_eq!(cpu.fpu_stack[slot], *previous);
            }
        }
    }
}

#[test]
fn x87_arithmetic_sti_uses_st0_source_without_overwriting_it() {
    // DC reverses the SUB/SUBR and DIV/DIVR opcode groups relative to D8.
    for (modrm, expected) in [
        (0xc3, 17.0),
        (0xcb, 60.0),
        (0xe3, 7.0),
        (0xeb, -7.0),
        (0xf3, 12.0 / 5.0),
        (0xfb, 5.0 / 12.0),
    ] {
        let mut cpu = cpu(12.0, 5.0);
        let old = cpu.fpu_stack;
        run(&mut cpu, &[0xdc, modrm]);
        assert_eq!(cpu.fpu_stack[1], expected, "DC {modrm:02X}");
        assert_eq!(cpu.fpu_top, 6);
        for (slot, previous) in old.iter().enumerate() {
            if slot != 1 {
                assert_eq!(cpu.fpu_stack[slot], *previous);
            }
        }
    }
}

#[test]
fn x87_pop_arithmetic_keeps_first_operand_as_destination() {
    for (modrm, expected) in [
        (0xc3, 17.0),
        (0xcb, 60.0),
        (0xe3, 7.0),
        (0xeb, -7.0),
        (0xf3, 12.0 / 5.0),
        (0xfb, 5.0 / 12.0),
    ] {
        let mut cpu = cpu(12.0, 5.0);
        run(&mut cpu, &[0xde, modrm]);
        assert_eq!(cpu.fpu_stack[1], expected, "DE {modrm:02X}");
        assert_eq!(cpu.fpu_top, 7);
        assert_eq!(cpu.fpu_stack[6], 12.0);
    }
}

#[test]
fn x87_memory_arithmetic_keeps_using_memory_source() {
    for opcode in [0xd8, 0xdc] {
        for (modrm, expected) in [
            (0x03, 17.0),
            (0x0b, 60.0),
            (0x23, 7.0),
            (0x2b, -7.0),
            (0x33, 12.0 / 5.0),
            (0x3b, 5.0 / 12.0),
        ] {
            let mut cpu = cpu(12.0, 41.0);
            if opcode == 0xd8 {
                cpu.memory.write(DATA, &5.0_f32.to_le_bytes()).unwrap();
            } else {
                cpu.memory.write(DATA, &5.0_f64.to_le_bytes()).unwrap();
            }
            run(&mut cpu, &[opcode, modrm]);
            assert_eq!(cpu.fpu_stack[6], expected, "{opcode:02X} {modrm:02X}");
            assert_eq!(cpu.fpu_stack[1], 41.0);
            assert_eq!(cpu.fpu_top, 6);
        }
    }
}

#[test]
fn x87_status_compares_use_named_source_and_preserve_pop_counts() {
    for (bytes, pops) in [
        ([0xd8, 0xd3], 0),
        ([0xd8, 0xdb], 1),
        ([0xdd, 0xe3], 0),
        ([0xdd, 0xeb], 1),
    ] {
        for (a, b, expected) in [(12.0, 5.0, 0), (5.0, 12.0, 0x100), (5.0, 5.0, 0x4000)] {
            let mut cpu = cpu(a, b);
            cpu.fpu_status_word = 0x4700;
            run(&mut cpu, &bytes);
            assert_eq!(cpu.fpu_status_word & 0x4700, expected, "{bytes:02X?}");
            assert_eq!(cpu.fpu_top, 6 + pops);
        }
    }
}

#[test]
fn x87_flag_compares_use_explicit_second_operand_and_preserve_pop_counts() {
    for (bytes, pops) in [
        ([0xdb, 0xf3], 0),
        ([0xdb, 0xeb], 0),
        ([0xdf, 0xf3], 1),
        ([0xdf, 0xeb], 1),
    ] {
        for (a, b, expected) in [
            (12.0, 5.0, RFlags::empty()),
            (5.0, 12.0, RFlags::CF),
            (5.0, 5.0, RFlags::ZF),
            (5.0, f64::NAN, RFlags::ZF | RFlags::PF | RFlags::CF),
        ] {
            let mut cpu = cpu(a, b);
            cpu.regs.rflags |= RFlags::ZF | RFlags::PF | RFlags::CF | RFlags::OF | RFlags::SF;
            run(&mut cpu, &bytes);
            assert_eq!(
                cpu.regs.rflags & (RFlags::ZF | RFlags::PF | RFlags::CF),
                expected,
                "{bytes:02X?}"
            );
            assert!(!cpu.regs.rflags.intersects(RFlags::OF | RFlags::SF));
            assert_eq!(cpu.fpu_top, 6 + pops);
        }
    }
}
