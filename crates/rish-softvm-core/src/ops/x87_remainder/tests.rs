use super::{C2, CONDITIONS, INVALID, SUMMARY_BUSY};
use crate::arch::registers::{Cr0, Efer, RFlags};
use crate::arch::segments::{Descriptor, SegmentSelector};
use crate::{Cpu, CpuError};

const CODE: u64 = 0x1000;
const STACK: u64 = 0x9000;
const HANDLER: u64 = 0xa000;
const FPREM: &[u8] = &[0xd9, 0xf8];

fn cpu(a: f64, b: f64) -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cr0 |= Cr0::PE | Cr0::NE;
    cpu.regs.cs = Descriptor::decode(0x00af_9b00_0000_ffff).load(SegmentSelector(8));
    cpu.regs.set_rsp(STACK);
    cpu.fpu_top = 7; // Exercise wrapping ST(1), and preserve every other slot.
    cpu.fpu_stack = [b, 19.0, 23.0, 29.0, 31.0, 37.0, 41.0, a];
    cpu
}

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step()
}

fn remainder(cpu: &Cpu) -> f64 {
    cpu.fpu_stack[usize::from(cpu.fpu_top)]
}

fn quotient(cpu: &Cpu) -> u16 {
    ((cpu.fpu_status_word >> 6) & 4)
        | ((cpu.fpu_status_word >> 13) & 2)
        | ((cpu.fpu_status_word >> 9) & 1)
}

fn install_handler(cpu: &mut Cpu, vector: u8) {
    cpu.regs.gdt_base = 0x3000;
    cpu.regs.gdt_limit = 0xffff;
    cpu.memory.write_u64(0x3008, 0x00af_9b00_0000_ffff).unwrap();
    cpu.regs.idt_base = 0x4000;
    cpu.regs.idt_limit = 0xfff;
    let gate = 0x4000 + u64::from(vector) * 16;
    cpu.memory
        .write_u64(gate, (8 << 16) | 0x8e00_0000_0000 | HANDLER)
        .unwrap();
    cpu.memory.write_u64(gate + 8, 0).unwrap();
    cpu.memory.write(HANDLER, &[0xf4]).unwrap();
}

#[test]
fn fprem_encoded_truncation_quotient_flags_and_stack_preservation() {
    for sign_a in [-1.0, 1.0] {
        for sign_b in [-1.0, 1.0] {
            for q in 0..16 {
                let a = sign_a * (f64::from(q) * 3.0 + 2.75);
                let mut cpu = cpu(a, sign_b * 3.0);
                let stack = cpu.fpu_stack;
                cpu.regs.rflags |= RFlags::OF | RFlags::CF | RFlags::PF;
                let flags = cpu.regs.rflags;
                cpu.fpu_status_word = CONDITIONS | 0x0020;
                run(&mut cpu, FPREM).unwrap();
                assert_eq!(remainder(&cpu), sign_a * 2.75);
                assert_eq!(quotient(&cpu), q as u16 & 7, "a={a}, b={sign_b}*3");
                assert_eq!(cpu.fpu_status_word & C2, 0);
                assert_eq!(cpu.fpu_status_word & 0xff, 0x20);
                assert_eq!(cpu.fpu_top, 7);
                assert_eq!(cpu.fpu_stack[..7], stack[..7]);
                assert_eq!(cpu.regs.rflags, flags);
                assert_eq!(cpu.regs.rip, CODE + 2);
            }
        }
    }
}

#[test]
fn fprem_large_quotient_keeps_low_bits_without_floating_point_division() {
    // Both quotients exceed 53 bits. A floating-point a - trunc(a/b)*b
    // incorrectly produces zero for these exact nonzero remainders.
    for (a, expected, q) in [(2_f64.powi(63), 2.0, 2), (2_f64.powi(64), 1.0, 5)] {
        let mut cpu = cpu(a, 3.0);
        run(&mut cpu, FPREM).unwrap();
        assert_eq!(remainder(&cpu), expected);
        assert_eq!(quotient(&cpu), q);
        assert_eq!(cpu.fpu_status_word & C2, 0);
    }
}

#[test]
fn fprem_partial_threshold_reduces_then_reports_final_quotient() {
    let mut cpu = cpu(2_f64.powi(65), 3.0); // D = 64, N = 32.
    cpu.fpu_status_word = CONDITIONS;
    run(&mut cpu, FPREM).unwrap();
    assert_eq!(remainder(&cpu), 2_f64.powi(33));
    assert_eq!(cpu.fpu_status_word & CONDITIONS, C2);
    run(&mut cpu, FPREM).unwrap();
    assert_eq!(remainder(&cpu), 2.0);
    assert_eq!(cpu.fpu_status_word & C2, 0);
    assert_eq!(quotient(&cpu), 2); // floor(2^65 / 3) mod 8
}

#[test]
fn fprem_extreme_exponents_converge_and_match_independent_fmod() {
    for (a, b) in [
        (2_f64.powi(1023), 3.0 * 2_f64.powi(-1000)),
        (-f64::MAX, 3.0 * f64::MIN_POSITIVE),
        (f64::MAX, f64::from_bits(3)),
        (f64::MIN_POSITIVE, f64::from_bits(7)),
    ] {
        let mut cpu = cpu(a, b);
        let mut rounds = 0;
        loop {
            run(&mut cpu, FPREM).unwrap();
            rounds += 1;
            if cpu.fpu_status_word & C2 == 0 {
                break;
            }
            assert!(rounds < 67, "partial reduction did not converge");
        }
        assert_eq!(remainder(&cpu).to_bits(), (a % b).to_bits(), "{a} % {b}");
        assert_eq!(cpu.fpu_status_word & 0x3f, 0);
    }
}

#[test]
fn fprem_subnormal_values_and_results_remain_exact() {
    // f64 subnormals are normal numbers in extended precision, so reducing
    // these represented values must not invent an x87 #D or #U exception.
    for (a, b, expected, q) in [(23, 3, 2, 7), (5, 7, 5, 0), (3, 3, 0, 1)] {
        let mut cpu = cpu(f64::from_bits(a), -f64::from_bits(b));
        cpu.fpu_control_word &= !0x12;
        run(&mut cpu, FPREM).unwrap();
        assert_eq!(remainder(&cpu).to_bits(), expected);
        assert_eq!(quotient(&cpu), q);
        assert_eq!(cpu.fpu_status_word & 0x3f, 0);
    }
}

#[test]
fn fprem_preserves_signed_zero_and_finite_dividend_for_infinite_divisor() {
    for a in [0.0_f64, -0.0, 12.0, -12.0] {
        for b in [3.0, -3.0, f64::INFINITY, f64::NEG_INFINITY] {
            let mut cpu = cpu(a, b);
            run(&mut cpu, FPREM).unwrap();
            let expected = if b.is_infinite() {
                a
            } else {
                0.0_f64.copysign(a)
            };
            assert_eq!(remainder(&cpu).to_bits(), expected.to_bits());
            assert_eq!(cpu.fpu_status_word & C2, 0);
            assert_eq!(
                quotient(&cpu),
                if a == 0.0 || b.is_infinite() { 0 } else { 4 }
            );
        }
    }
}

#[test]
fn fprem_ignores_precision_and_rounding_control() {
    for precision in [0, 2, 3] {
        for rounding in 0..4 {
            let mut cpu = cpu(2_f64.powi(64), 3.0);
            cpu.fpu_control_word = 0x7f | (precision << 8) | (rounding << 10);
            let control = cpu.fpu_control_word;
            run(&mut cpu, FPREM).unwrap();
            assert_eq!(remainder(&cpu), 1.0);
            assert_eq!(quotient(&cpu), 5);
            assert_eq!(cpu.fpu_status_word & 0x3f, 0);
            assert_eq!(cpu.fpu_control_word, control);
        }
    }
}

#[test]
fn fprem_invalid_masked_produces_indefinite_and_invalid_status_not_zero_divide() {
    for (a, b) in [
        (1.0, 0.0),
        (0.0, -0.0),
        (f64::INFINITY, 3.0),
        (f64::NEG_INFINITY, f64::INFINITY),
    ] {
        let mut cpu = cpu(a, b);
        run(&mut cpu, FPREM).unwrap();
        assert_eq!(remainder(&cpu).to_bits(), 0xfff8_0000_0000_0000);
        assert_eq!(cpu.fpu_status_word & 0x80ff, INVALID);
        assert_eq!(cpu.exceptions_raised, 0);
    }
}

#[test]
fn fprem_nan_quieting_payload_precedence_and_invalid_mask() {
    let quiet = f64::from_bits(0x7ff8_0000_0000_1234);
    let signaling = f64::from_bits(0x7ff0_0000_0000_5678);
    for (a, b, expected, invalid) in [
        (quiet, 0.0, quiet.to_bits(), false),
        (f64::INFINITY, quiet, quiet.to_bits(), false),
        (signaling, 3.0, signaling.to_bits() | (1 << 51), true),
        (signaling, quiet, quiet.to_bits(), true),
        (-quiet, quiet, quiet.to_bits(), false),
    ] {
        let mut cpu = cpu(a, b);
        run(&mut cpu, FPREM).unwrap();
        assert_eq!(remainder(&cpu).to_bits(), expected);
        assert_eq!(cpu.fpu_status_word & INVALID != 0, invalid);
    }
}

#[test]
fn fprem_unmasked_invalid_preserves_operand_and_defers_fault_until_next_waiting_op() {
    for (a, b) in [(9.0, 0.0), (f64::from_bits(0x7ff0_0000_0000_1234), 1.0)] {
        let mut cpu = cpu(a, b);
        cpu.fpu_control_word &= !INVALID;
        cpu.fpu_status_word = CONDITIONS;
        install_handler(&mut cpu, 16);
        run(&mut cpu, FPREM).unwrap();
        assert_eq!(remainder(&cpu).to_bits(), a.to_bits());
        assert_eq!(cpu.fpu_status_word, CONDITIONS | INVALID | SUMMARY_BUSY);
        assert_eq!(cpu.exceptions_raised, 0);
        run(&mut cpu, FPREM).unwrap();
        assert_eq!(cpu.exceptions_raised, 1);
        assert_eq!(cpu.fault_log.back().unwrap().vector, 16);
        assert_eq!(cpu.fault_log.back().unwrap().rip, CODE);
        assert_eq!(cpu.memory.read_u64(STACK - 40).unwrap(), CODE);
        assert_eq!(remainder(&cpu).to_bits(), a.to_bits());
    }
}

#[test]
fn fprem_disabled_fpu_faults_before_pending_arithmetic_exception() {
    for disabled in [Cr0::EM, Cr0::TS] {
        let mut cpu = cpu(9.0, 3.0);
        cpu.regs.cr0 |= disabled;
        cpu.fpu_control_word &= !INVALID;
        cpu.fpu_status_word = INVALID | SUMMARY_BUSY;
        install_handler(&mut cpu, 7);
        run(&mut cpu, FPREM).unwrap();
        assert_eq!(cpu.fault_log.back().unwrap().vector, 7);
        assert_eq!(cpu.fault_log.back().unwrap().rip, CODE);
        assert_eq!(remainder(&cpu), 9.0);
    }
}

#[test]
fn fprem_encoded_lock_prefix_is_invalid() {
    let mut cpu = cpu(9.0, 3.0);
    install_handler(&mut cpu, 6);
    let bytes = [0xf0, 0xd9, 0xf8];
    // The core currently fails closed on invalid encodings before dispatch.
    assert!(matches!(
        run(&mut cpu, &bytes),
        Err(CpuError::UnimplementedInstruction { code, .. }) if code == "invalid"
    ));
    assert_eq!(remainder(&cpu), 9.0);
    // Also exercise the implementation's defensive #UD gate with a decoded
    // LOCK-prefixed FPREM, bypassing only the decoder's validity check.
    let instruction =
        iced_x86::Decoder::with_ip(64, &bytes, CODE, iced_x86::DecoderOptions::NO_INVALID_CHECK)
            .decode();
    cpu.dispatch(&instruction).unwrap();
    assert_eq!(cpu.fault_log.back().unwrap().vector, 6);
    assert_eq!(cpu.fault_log.back().unwrap().rip, CODE);
    assert_eq!(remainder(&cpu), 9.0);
}

#[test]
fn fprem_random_encoded_reductions_match_fmod_and_modular_quotient_oracles() {
    let mut cpu = cpu(1.0, 1.0);
    let mut seed = 0x93b5_74ae_6132_08df_u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        seed
    };
    for _ in 0..1024 {
        let a_bits = next();
        let b_bits = next();
        let exp_a = (next() % 2046 + 1) as i32;
        let exp_b = (next() % 2046 + 1) as i32;
        let frac_mask = (1 << 52) - 1;
        let a = f64::from_bits((a_bits & (frac_mask | (1 << 63))) | ((exp_a as u64) << 52));
        let b = f64::from_bits((b_bits & (frac_mask | (1 << 63))) | ((exp_b as u64) << 52));
        cpu.fpu_stack[7] = a;
        cpu.fpu_stack[0] = b;
        cpu.fpu_status_word = 0;
        for iteration in 0..67 {
            run(&mut cpu, FPREM).unwrap();
            if cpu.fpu_status_word & C2 == 0 {
                break;
            }
            assert!(iteration < 66, "partial reduction did not converge");
        }
        assert_eq!(remainder(&cpu).to_bits(), (a % b).to_bits(), "{a} % {b}");
        // Independent oracle: modular doubling across the entire exponent
        // difference, keeping modulo 8*b, obtains floor(a/b) mod 8 without
        // using either partial-reduction implementation or float division.
        let denominator = (b_bits & frac_mask) | (1 << 52);
        let mut residue = (a_bits & frac_mask) | (1 << 52);
        let expected_q = if exp_a < exp_b {
            0
        } else {
            for _ in 0..exp_a - exp_b {
                residue = (residue << 1) % (8 * denominator);
            }
            (residue / denominator) as u16 & 7
        };
        assert_eq!(quotient(&cpu), expected_q, "quotient of {a} / {b}");
    }
}
