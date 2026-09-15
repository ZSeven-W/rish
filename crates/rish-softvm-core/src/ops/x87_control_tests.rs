use crate::Cpu;
use crate::arch::registers::{Cr0, Cr4, Efer, index};
use crate::arch::segments::{Descriptor, SegmentSelector};

const CODE: u64 = 0x1000;
const DATA: u64 = 0x2000;
const STACK: u64 = 0x9000;
const HANDLER: u64 = 0xa000;
const CASES: &[(f64, [i64; 4])] = &[
    (1.75, [2, 1, 2, 1]),
    (-1.75, [-2, -2, -1, -1]),
    (2.5, [2, 2, 3, 2]),
    (-2.5, [-2, -3, -2, -2]),
    (3.5, [4, 3, 4, 3]),
    (-3.5, [-4, -4, -3, -3]),
    (0.5, [0, 0, 1, 0]),
    (-0.5, [0, -1, 0, 0]),
];

fn cpu(value: f64) -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cr0 |= Cr0::PE | Cr0::NE;
    cpu.regs.cs = Descriptor::decode(0x00af_9b00_0000_ffff).load(SegmentSelector(8));
    cpu.regs.set_rsp(STACK);
    cpu.fpu_top = 7;
    cpu.fpu_stack = [17.0, 19.0, 23.0, 29.0, 31.0, 37.0, 41.0, value];
    cpu
}

fn run(cpu: &mut Cpu, bytes: &[u8]) {
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step().unwrap();
}

fn set_rounding(cpu: &mut Cpu, mode: u16) {
    cpu.regs.set_gpr(index::RBX, DATA);
    cpu.memory.write_u16(DATA, 0x037f | (mode << 10)).unwrap();
    run(cpu, &[0xd9, 0x2b]); // fldcw word [rbx]
    cpu.regs.set_gpr(index::RBX, DATA + 16);
}

fn integer(cpu: &Cpu, width: u8) -> i64 {
    match width {
        2 => i64::from(cpu.memory.read_u16(DATA + 16).unwrap() as i16),
        4 => i64::from(cpu.memory.read_u32(DATA + 16).unwrap() as i32),
        8 => cpu.memory.read_u64(DATA + 16).unwrap() as i64,
        _ => unreachable!(),
    }
}

#[test]
fn x87_fist_and_fistp_honor_all_control_word_rounding_modes() {
    for (bytes, width, pop) in [
        ([0xdf, 0x13], 2, false),
        ([0xdb, 0x13], 4, false),
        ([0xdf, 0x1b], 2, true),
        ([0xdb, 0x1b], 4, true),
        ([0xdf, 0x3b], 8, true),
    ] {
        for &(value, expected) in CASES {
            for mode in 0..4 {
                let mut cpu = cpu(value);
                set_rounding(&mut cpu, mode);
                run(&mut cpu, &bytes);
                assert_eq!(
                    integer(&cpu, width),
                    expected[usize::from(mode)],
                    "{bytes:02x?} {value} mode{mode}"
                );
                assert_eq!(cpu.fpu_top, if pop { 0 } else { 7 });
                assert_eq!(cpu.fpu_stack[7], value);
                assert_eq!(cpu.fpu_control_word, 0x037f | (mode << 10));
            }
        }
    }
}

#[test]
fn x87_fisttp_ignores_control_word_and_always_truncates() {
    for (bytes, width) in [([0xdf, 0x0b], 2), ([0xdb, 0x0b], 4), ([0xdd, 0x0b], 8)] {
        for &(value, expected) in CASES {
            for mode in 0..4 {
                let mut cpu = cpu(value);
                set_rounding(&mut cpu, mode);
                run(&mut cpu, &bytes);
                assert_eq!(
                    integer(&cpu, width),
                    expected[3],
                    "{bytes:02x?} {value} mode{mode}"
                );
                assert_eq!(cpu.fpu_top, 0);
            }
        }
    }
}

#[test]
fn x87_frndint_honors_all_control_word_rounding_modes_and_zero_sign() {
    for &(value, expected) in CASES {
        for mode in 0..4 {
            let mut cpu = cpu(value);
            set_rounding(&mut cpu, mode);
            run(&mut cpu, &[0xd9, 0xfc]);
            let result = cpu.fpu_stack[7];
            let expected = expected[usize::from(mode)] as f64;
            let expected = if expected == 0.0 {
                expected.copysign(value)
            } else {
                expected
            };
            assert_eq!(result.to_bits(), expected.to_bits(), "{value} mode{mode}");
            assert_eq!(cpu.fpu_top, 7);
        }
    }
}

fn map_code_stack_and_page_fault_handler(cpu: &mut Cpu) {
    cpu.regs.gdt_base = 0x3000;
    cpu.regs.gdt_limit = 0xffff;
    cpu.memory.write_u64(0x3008, 0x00af_9b00_0000_ffff).unwrap();
    cpu.regs.idt_base = 0x4000;
    cpu.regs.idt_limit = 0xfff;
    cpu.memory
        .write_u64(0x4000 + 14 * 16, (8 << 16) | 0x8e00_0000_0000 | HANDLER)
        .unwrap();
    cpu.memory.write_u64(0x4000 + 14 * 16 + 8, 0).unwrap();
    cpu.memory.write(HANDLER, &[0xf4]).unwrap();
    cpu.regs.cr0 |= Cr0::PG | Cr0::WP;
    cpu.regs.cr4 |= Cr4::PAE;
    cpu.regs.cr3 = 0x10000;
    cpu.memory.write_u64(0x10000, 0x11003).unwrap();
    cpu.memory.write_u64(0x11000, 0x12003).unwrap();
    cpu.memory.write_u64(0x12000, 0x13003).unwrap();
    for page in 0..16_u64 {
        cpu.memory
            .write_u64(0x13000 + page * 8, (page << 12) | 3)
            .unwrap();
    }
    cpu.regs.set_gpr(index::RBX, 0x40000); // Unmapped destination.
}

#[test]
fn x87_fistp_and_fisttp_page_fault_preserves_stack_and_top() {
    for bytes in [
        [0xdf, 0x1b],
        [0xdb, 0x1b],
        [0xdf, 0x3b],
        [0xdf, 0x0b],
        [0xdb, 0x0b],
        [0xdd, 0x0b],
    ] {
        let mut cpu = cpu(1.75);
        map_code_stack_and_page_fault_handler(&mut cpu);
        let stack = cpu.fpu_stack;
        run(&mut cpu, &bytes);
        assert_eq!(cpu.fault_log.back().unwrap().vector, 14);
        assert_eq!(cpu.fault_log.back().unwrap().rip, CODE);
        assert_eq!(cpu.regs.cr2, 0x40000);
        assert_eq!(cpu.fpu_stack, stack);
        assert_eq!(cpu.fpu_top, 7, "{bytes:02x?}: store fault must not pop");
    }
}
