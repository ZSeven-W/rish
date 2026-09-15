use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
use crate::arch::segments::{Descriptor, SegmentSelector};
use crate::{Cpu, CpuError};

const SENTINEL: u128 = 0xCAFE_BABE_0123_4567_DEAD_BEEF_89AB_CDEF;

fn cpu() -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cs.long_mode = true;
    cpu.regs.rflags = RFlags::CF | RFlags::ZF | RFlags::OF;
    cpu.regs.xmm[0] = SENTINEL;
    cpu
}

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    cpu.memory.write(0x1000, bytes).unwrap();
    cpu.regs.rip = 0x1000;
    cpu.step()
}

#[test]
fn cvtsi2ss_writes_float_bits_and_preserves_the_upper_96_bits() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::RAX, 16);
    run(&mut cpu, &[0xF3, 0x0F, 0x2A, 0xC0]).unwrap();
    assert_eq!(
        cpu.regs.xmm[0],
        (SENTINEL & !u128::from(u32::MAX)) | u128::from(16_f32.to_bits())
    );
}

#[test]
fn cvtsi2sd_sign_extends_the_actual_32_bit_source() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::RAX, u64::from((-42_i32) as u32));
    run(&mut cpu, &[0xF2, 0x0F, 0x2A, 0xC0]).unwrap();
    assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), -42.0);
}

#[test]
fn cvttss2si_rex_w_uses_a_64_bit_integer_destination() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = u128::from(4_294_967_296_f32.to_bits());
    run(&mut cpu, &[0xF3, 0x48, 0x0F, 0x2C, 0xC0]).unwrap();
    assert_eq!(cpu.regs.gpr[index::RAX], 4_294_967_296);
}

#[test]
fn cvttsd2si_reads_its_memory_source() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::RBX, 0x2000);
    cpu.memory.write_u64(0x2000, 42.75_f64.to_bits()).unwrap();
    run(&mut cpu, &[0xF2, 0x0F, 0x2C, 0x03]).unwrap();
    assert_eq!(cpu.regs.gpr[index::RAX], 42);
}

#[test]
fn hashmap_resize_threshold_keeps_capacity_times_load_factor() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::RAX, 16);
    cpu.regs.xmm[1] = u128::from(0.75_f32.to_bits());
    run(&mut cpu, &[0xF3, 0x0F, 0x2A, 0xC0]).unwrap(); // (float)capacity
    run(&mut cpu, &[0xF3, 0x0F, 0x59, 0xC1]).unwrap(); // capacity * .75f
    run(&mut cpu, &[0xF3, 0x0F, 0x2C, 0xC0]).unwrap(); // (int)threshold
    assert_eq!(cpu.regs.gpr[index::RAX], 12);
}

#[test]
fn scalar_double_nanos_division_and_multiplication_are_not_reversed() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::RAX, 2_500_000_000);
    cpu.regs.xmm[1] = u128::from(1_000_000_000_f64.to_bits());
    run(&mut cpu, &[0xF2, 0x48, 0x0F, 0x2A, 0xC0]).unwrap();
    run(&mut cpu, &[0xF2, 0x0F, 0x5E, 0xC1]).unwrap();
    assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), 2.5);
    run(&mut cpu, &[0xF2, 0x0F, 0x59, 0xC1]).unwrap();
    assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), 2_500_000_000.0);
}

#[test]
fn integer_conversion_source_width_and_scalar_width_are_independent() {
    for double in [false, true] {
        for wide in [false, true] {
            for memory in [false, true] {
                for value in [0_i64, -1, -42, i64::from(i32::MIN), 4_294_967_296, i64::MIN] {
                    let mut cpu = cpu();
                    let prefix = if double { 0xF2 } else { 0xF3 };
                    let mut bytes = vec![prefix];
                    if wide {
                        bytes.push(0x48);
                    }
                    bytes.extend_from_slice(&[0x0F, 0x2A, if memory { 0x03 } else { 0xC0 }]);
                    cpu.regs.set_gpr(index::RAX, value as u64);
                    cpu.regs.set_gpr(index::RBX, 0x2000);
                    cpu.memory.write_u64(0x2000, value as u64).unwrap();
                    let flags = cpu.regs.rflags;
                    run(&mut cpu, &bytes).unwrap();
                    let signed = if wide { value } else { i64::from(value as i32) };
                    let (mask, expected) = if double {
                        (u128::from(u64::MAX), u128::from((signed as f64).to_bits()))
                    } else {
                        (u128::from(u32::MAX), u128::from((signed as f32).to_bits()))
                    };
                    assert_eq!(cpu.regs.xmm[0], (SENTINEL & !mask) | expected);
                    assert_eq!(cpu.regs.rflags, flags);
                    assert_eq!(cpu.memory.read_u64(0x2000).unwrap(), value as u64);
                }
            }
        }
    }
}

#[test]
fn integer_conversion_obeys_all_mxcsr_rounding_modes_and_precision_status() {
    for double in [false, true] {
        let exact = if double { 1_i64 << 53 } else { 1_i64 << 24 };
        for negative in [false, true] {
            for mode in 0..4 {
                let mut cpu = cpu();
                cpu.mxcsr |= mode << 13;
                let input = if negative { -exact - 1 } else { exact + 1 };
                cpu.regs.set_gpr(index::RAX, input as u64);
                run(
                    &mut cpu,
                    &[if double { 0xF2 } else { 0xF3 }, 0x48, 0x0F, 0x2A, 0xC0],
                )
                .unwrap();
                let away = (mode == 1 && negative) || (mode == 2 && !negative);
                let rounded = exact + if away { 2 } else { 0 };
                let expected = if negative {
                    -(rounded as f64)
                } else {
                    rounded as f64
                };
                let actual = if double {
                    f64::from_bits(cpu.regs.xmm[0] as u64)
                } else {
                    f64::from(f32::from_bits(cpu.regs.xmm[0] as u32))
                };
                assert_eq!(
                    actual, expected,
                    "double={double}, negative={negative}, RC={mode}"
                );
                assert_ne!(cpu.mxcsr & 0x20, 0);
            }
        }
    }
}

#[test]
fn i64_to_f32_rounds_once_instead_of_via_f64() {
    let mut cpu = cpu();
    // One unit above a binary32 halfway point, but lost by rounding to f64.
    let input = (1_i64 << 62) + (1_i64 << 38) + 1;
    cpu.regs.set_gpr(index::RAX, input as u64);
    run(&mut cpu, &[0xF3, 0x48, 0x0F, 0x2A, 0xC0]).unwrap();
    assert_eq!(cpu.regs.xmm[0] as u32, 0x5E80_0001);
    assert_eq!((input as f64 as f32).to_bits(), 0x5E80_0000);
}

#[test]
fn cvt_and_cvtt_distinguish_rounding_and_preserve_other_registers() {
    for double in [false, true] {
        for wide in [false, true] {
            for truncating in [false, true] {
                for mode in 0..4 {
                    for value in [2.5_f64, 3.5, -2.5, -3.5, 42.0] {
                        let mut cpu = cpu();
                        cpu.mxcsr |= mode << 13;
                        cpu.regs.set_gpr(index::RAX, u64::MAX);
                        cpu.regs.xmm[0] = if double {
                            u128::from(value.to_bits())
                        } else {
                            u128::from((value as f32).to_bits())
                        };
                        let xmm = cpu.regs.xmm;
                        let flags = cpu.regs.rflags;
                        let mut bytes = vec![if double { 0xF2 } else { 0xF3 }];
                        if wide {
                            bytes.push(0x48);
                        }
                        bytes.extend_from_slice(&[
                            0x0F,
                            if truncating { 0x2C } else { 0x2D },
                            0xC0,
                        ]);
                        run(&mut cpu, &bytes).unwrap();
                        let rounded = match if truncating { 3 } else { mode } {
                            0 => value.round_ties_even(),
                            1 => value.floor(),
                            2 => value.ceil(),
                            _ => value.trunc(),
                        };
                        let expected = if wide {
                            rounded as i64 as u64
                        } else {
                            u64::from(rounded as i32 as u32)
                        };
                        assert_eq!(cpu.regs.gpr[index::RAX], expected);
                        assert_eq!(cpu.regs.xmm, xmm);
                        assert_eq!(cpu.regs.rflags, flags);
                        assert_eq!(cpu.mxcsr & 0x20 != 0, rounded != value);
                    }
                }
            }
        }
    }
}

#[test]
fn invalid_float_conversion_returns_indefinite_instead_of_saturating() {
    for double in [false, true] {
        for wide in [false, true] {
            for opcode in [0x2C, 0x2D] {
                let limit = if wide {
                    9_223_372_036_854_775_808_f64
                } else {
                    2_147_483_648_f64
                };
                for value in [
                    f64::NAN,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    limit,
                    -limit * 2.0,
                ] {
                    let mut cpu = cpu();
                    cpu.regs.xmm[0] = if double {
                        u128::from(value.to_bits())
                    } else {
                        u128::from((value as f32).to_bits())
                    };
                    let mut bytes = vec![if double { 0xF2 } else { 0xF3 }];
                    if wide {
                        bytes.push(0x48);
                    }
                    bytes.extend_from_slice(&[0x0F, opcode, 0xC0]);
                    run(&mut cpu, &bytes).unwrap();
                    assert_eq!(
                        cpu.regs.gpr[index::RAX],
                        if wide { 1 << 63 } else { 1 << 31 }
                    );
                    assert_eq!(cpu.mxcsr & 0x3F, 1, "invalid conversion sets IE, not PE");
                }
            }
        }
    }
}

#[test]
fn exact_minimum_signed_integer_is_valid_and_maximum_boundary_is_exclusive() {
    for wide in [false, true] {
        let minimum = if wide {
            -9_223_372_036_854_775_808_f64
        } else {
            -2_147_483_648_f64
        };
        let maximum = if wide {
            9_223_372_036_854_774_784_f64
        } else {
            2_147_483_647_f64
        };
        for value in [minimum, maximum] {
            let mut cpu = cpu();
            cpu.regs.xmm[0] = u128::from(value.to_bits());
            let mut bytes = vec![0xF2];
            if wide {
                bytes.push(0x48);
            }
            bytes.extend_from_slice(&[0x0F, 0x2C, 0xC0]);
            run(&mut cpu, &bytes).unwrap();
            assert_eq!(
                cpu.regs.gpr[index::RAX],
                if wide {
                    value as i64 as u64
                } else {
                    u64::from(value as i32 as u32)
                }
            );
            assert_eq!(cpu.mxcsr & 0x3F, 0);
        }
    }
}

#[test]
fn cvt_denormal_source_respects_daz_without_a_denormal_exception() {
    for double in [false, true] {
        for daz in [false, true] {
            let mut cpu = cpu();
            cpu.mxcsr |= 2 << 13; // Round toward +infinity.
            if daz {
                cpu.mxcsr |= 1 << 6;
            }
            cpu.regs.xmm[0] = 1; // Smallest positive subnormal in either precision.
            run(
                &mut cpu,
                &[if double { 0xF2 } else { 0xF3 }, 0x0F, 0x2D, 0xC0],
            )
            .unwrap();
            assert_eq!(cpu.regs.gpr[index::RAX], u64::from(!daz));
            assert_eq!(cpu.mxcsr & 0x3F, if daz { 0 } else { 0x20 });
        }
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
    cpu.regs.set_gpr(index::RBX, 0x40FFD);
}

#[test]
fn conversion_memory_reads_follow_page_boundaries_and_fault_without_mutation() {
    for bytes in [
        &[0xF3, 0x0F, 0x2A, 0x03][..],   // int32 -> float
        &[0xF2, 0x48, 0x0F, 0x2A, 0x03], // int64 -> double
        &[0xF3, 0x48, 0x0F, 0x2C, 0x03], // float -> int64
        &[0xF2, 0x0F, 0x2C, 0x03],       // double -> int32
        &[0xF3, 0x0F, 0x2D, 0x03],       // float -> int32, rounded
        &[0xF2, 0x48, 0x0F, 0x2D, 0x03], // double -> int64, rounded
    ] {
        let mut cpu = cpu();
        paging(&mut cpu);
        let gpr = cpu.regs.gpr;
        let xmm = cpu.regs.xmm;
        let flags = cpu.regs.rflags;
        let mxcsr = cpu.mxcsr;
        let mut decoder =
            iced_x86::Decoder::with_ip(64, bytes, 0x1000, iced_x86::DecoderOptions::NONE);
        let error = cpu.dispatch(&decoder.decode()).unwrap_err();
        assert_eq!(
            error,
            CpuError::PageFault {
                linear: 0x41000,
                error_code: 0
            }
        );
        assert_eq!(cpu.regs.gpr, gpr);
        assert_eq!(cpu.regs.xmm, xmm);
        assert_eq!(cpu.regs.rflags, flags);
        assert_eq!(cpu.mxcsr, mxcsr);
    }

    let mut cpu = cpu();
    paging(&mut cpu);
    cpu.memory.write_u64(0x13000 + 0x41 * 8, 0x60003).unwrap();
    let source = (-42.75_f64).to_le_bytes();
    cpu.memory.write(0x50FFD, &source[..3]).unwrap();
    cpu.memory.write(0x60000, &source[3..]).unwrap();
    run(&mut cpu, &[0xF2, 0x48, 0x0F, 0x2C, 0x03]).unwrap();
    assert_eq!(cpu.regs.gpr[index::RAX], (-42_i64) as u64);
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
fn unmasked_conversion_exceptions_deliver_xm_or_ud_without_destination_write() {
    for osxmmexcpt in [false, true] {
        for precision in [false, true] {
            let mut cpu = cpu();
            let vector = if osxmmexcpt { 19 } else { 6 };
            handler(&mut cpu, vector);
            cpu.regs.cr4.set(Cr4::OSXMMEXCPT, osxmmexcpt);
            let bytes: &[u8] = if precision {
                cpu.mxcsr &= !(1 << 12); // Unmask precision.
                cpu.regs.set_gpr(index::RAX, 16_777_217);
                &[0xF3, 0x0F, 0x2A, 0xC0]
            } else {
                cpu.mxcsr &= !(1 << 7); // Unmask invalid.
                cpu.regs.xmm[0] = u128::from(f64::INFINITY.to_bits());
                &[0xF2, 0x48, 0x0F, 0x2C, 0xC0]
            };
            let xmm = cpu.regs.xmm;
            let rax = cpu.regs.gpr[index::RAX];
            let flags = cpu.regs.rflags;
            run(&mut cpu, bytes).unwrap();
            assert_eq!(cpu.fault_log.back().unwrap().vector, vector);
            assert_eq!(cpu.fault_log.back().unwrap().rip, 0x1000);
            assert_eq!(cpu.regs.xmm, xmm);
            assert_eq!(cpu.regs.gpr[index::RAX], rax);
            assert_eq!(cpu.memory.read_u64(0x9000 - 24).unwrap(), flags.bits());
            assert_eq!(cpu.mxcsr & 0x3F, if precision { 0x20 } else { 1 });
        }
    }
}
