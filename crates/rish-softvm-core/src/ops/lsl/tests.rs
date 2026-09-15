use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
use crate::arch::segments::{Descriptor, SegmentSelector};
use crate::{Cpu, CpuError};

const CODE: u64 = 0x1000;
const GDT: u64 = 0x3000;
const LDT: u64 = 0x8000;
const SELECTOR: u16 = 0x28;
const ORIGINAL: u64 = 0xABCD_9876_0123_4567;
const REG32: &[u8] = &[0x0F, 0x03, 0xC1]; // lsl eax, ecx
const REG64: &[u8] = &[0x48, 0x0F, 0x03, 0xC1]; // lsl rax, ecx

fn descriptor(limit: u32, kind: u8, system: bool, dpl: u8, granular: bool) -> u64 {
    u64::from(limit & 0xFFFF)
        | (u64::from((limit >> 16) & 0xF) << 48)
        | (u64::from(kind) << 40)
        | (u64::from(!system) << 44)
        | (u64::from(dpl) << 45)
        | (1 << 47)
        | (u64::from(granular) << 55)
}

fn cpu(bitness: u32) -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.cr0 = Cr0::PE;
    let code = match bitness {
        64 => {
            cpu.regs.efer |= Efer::LMA;
            0x00AF_9B00_0000_FFFF
        }
        32 => 0x00CF_9B00_0000_FFFF,
        _ => 0x0000_9B00_0000_FFFF,
    };
    cpu.regs.cs = Descriptor::decode(code).load(SegmentSelector(8));
    cpu.regs.ds = Descriptor::decode(0x00CF_9300_0000_FFFF).load(SegmentSelector(16));
    cpu.regs.ss = cpu.regs.ds;
    cpu.regs.gdt_base = GDT;
    cpu.regs.gdt_limit = 0xFFFF;
    cpu.memory.write_u64(GDT + 8, code).unwrap();
    cpu.memory
        .write_u64(
            GDT + u64::from(SELECTOR),
            descriptor(0xABCDE, 2, false, 3, false),
        )
        .unwrap();
    cpu.regs.set_gpr(index::RAX, ORIGINAL);
    cpu.regs.set_gpr(index::RCX, u64::from(SELECTOR));
    cpu.regs.set_rsp(0x9000);
    cpu.regs.rflags = RFlags::CF | RFlags::PF | RFlags::SF | RFlags::OF | RFlags::DF | RFlags::ZF;
    cpu
}

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step()
}

fn flags_without_zf(cpu: &Cpu) -> RFlags {
    cpu.regs.rflags - RFlags::ZF
}

fn assert_rejected(cpu: &Cpu, previous_flags: RFlags) {
    assert_eq!(
        cpu.regs.gpr[index::RAX],
        ORIGINAL,
        "failure preserves the entire destination"
    );
    assert_eq!(cpu.regs.rflags, previous_flags - RFlags::ZF);
    assert_eq!(cpu.exceptions_raised, 0);
}

#[test]
fn lsl_destination_widths_and_source_low_16_bits_are_exact() {
    for (bytes, expected) in [
        (&[0x66, 0x0F, 0x03, 0xC1][..], (ORIGINAL & !0xFFFF) | 0xBCDE),
        (REG32, 0xABCDE),
        (REG64, 0xABCDE),
    ] {
        let mut cpu = cpu(64);
        cpu.regs
            .set_gpr(index::RCX, 0xFFFF_8888_ABCD_0000 | u64::from(SELECTOR));
        cpu.regs.rflags -= RFlags::ZF;
        let flags = flags_without_zf(&cpu);
        run(&mut cpu, bytes).unwrap();
        assert_eq!(cpu.regs.gpr[index::RAX], expected);
        assert!(cpu.regs.rflags.contains(RFlags::ZF));
        assert_eq!(flags_without_zf(&cpu), flags);
        assert_eq!(cpu.regs.gpr[index::RCX], 0xFFFF_8888_ABCD_0028);
    }
}

#[test]
fn lsl_rex_registers_and_same_source_destination_work() {
    let mut cpu = cpu(64);
    cpu.regs
        .set_gpr(index::R9, 0xFFFF_0000 | u64::from(SELECTOR));
    run(&mut cpu, &[0x4D, 0x0F, 0x03, 0xC1]).unwrap(); // lsl r8, r9d
    assert_eq!(cpu.regs.gpr[index::R8], 0xABCDE);
    cpu.regs.set_gpr(index::RAX, u64::from(SELECTOR));
    run(&mut cpu, &[0x48, 0x0F, 0x03, 0xC0]).unwrap(); // lsl rax, eax
    assert_eq!(cpu.regs.gpr[index::RAX], 0xABCDE);
}

#[test]
fn lsl_legacy_operand_sizes_and_page_granularity() {
    for bitness in [16, 32, 64] {
        for granular in [false, true] {
            let mut cpu = cpu(bitness);
            let entry = descriptor(0xABCDE, 2, false, 3, granular);
            cpu.memory
                .write_u64(GDT + u64::from(SELECTOR), entry)
                .unwrap();
            let expected = if granular { 0xABCD_EFFF_u64 } else { 0xABCDE };
            run(&mut cpu, REG32).unwrap();
            assert_eq!(
                cpu.regs.gpr[index::RAX],
                if bitness == 16 {
                    (ORIGINAL & !0xFFFF) | (expected & 0xFFFF)
                } else {
                    expected
                }
            );
            assert_eq!(
                cpu.memory.read_u64(GDT + u64::from(SELECTOR)).unwrap(),
                entry
            );
        }
    }
}

#[test]
fn lsl_null_selectors_never_touch_descriptor_memory() {
    for selector in 0..4 {
        let mut cpu = cpu(64);
        cpu.regs.set_gpr(index::RCX, selector);
        cpu.regs.gdt_base = 0xDEAD_0000; // A read would fail.
        let flags = cpu.regs.rflags;
        run(&mut cpu, REG32).unwrap();
        assert_rejected(&cpu, flags);
    }
}

#[test]
fn lsl_gdt_boundary_requires_the_complete_descriptor() {
    for table_limit in [0, 0x27, 0x28, 0x2E, 0x2F] {
        let mut cpu = cpu(64);
        cpu.regs.gdt_limit = table_limit;
        let flags = cpu.regs.rflags;
        run(&mut cpu, REG64).unwrap();
        if table_limit < 0x2F {
            assert_rejected(&cpu, flags);
        } else {
            assert_eq!(cpu.regs.gpr[index::RAX], 0xABCDE);
            assert!(cpu.regs.rflags.contains(RFlags::ZF));
        }
    }
}

fn enable_ldt(cpu: &mut Cpu, limit: u32, granular: bool) {
    cpu.regs.ldtr =
        Descriptor::decode(descriptor(limit, 2, true, 0, granular)).load(SegmentSelector(24));
    cpu.regs.ldtr.base = LDT;
}

#[test]
fn lsl_ldt_zero_entry_is_non_null_and_honors_cached_limit() {
    for (limit, granular, source, valid) in [
        (7, false, 4, true),
        (6, false, 4, false),
        (0, true, 0xFFC, true),
        (0, true, 0x1004, false),
    ] {
        let mut cpu = cpu(64);
        enable_ldt(&mut cpu, limit, granular);
        let entry = descriptor(0x12345, 2, false, 3, false);
        cpu.memory.write_u64(LDT + (source & !7), entry).unwrap();
        cpu.regs.set_gpr(index::RCX, source);
        let flags = cpu.regs.rflags;
        let ldtr = cpu.regs.ldtr;
        run(&mut cpu, REG32).unwrap();
        if valid {
            assert_eq!(cpu.regs.gpr[index::RAX], 0x12345);
            assert!(cpu.regs.rflags.contains(RFlags::ZF));
        } else {
            assert_rejected(&cpu, flags);
        }
        assert_eq!(cpu.regs.ldtr, ldtr);
    }
}

#[test]
fn lsl_unavailable_ldt_does_not_read_memory() {
    for null in [true, false] {
        let mut cpu = cpu(64);
        enable_ldt(&mut cpu, 0xFFFF, false);
        cpu.regs.ldtr.base = 0xDEAD_0000;
        if null {
            cpu.regs.ldtr.selector = SegmentSelector(0);
        } else {
            cpu.regs.ldtr.attributes.present = false;
        }
        cpu.regs.set_gpr(index::RCX, 4);
        let flags = cpu.regs.rflags;
        run(&mut cpu, REG32).unwrap();
        assert_rejected(&cpu, flags);
    }
}

#[test]
fn lsl_all_code_data_types_ignore_present_and_long_default_bits() {
    for kind in 0..16 {
        for modifier in [0, 1 << 53, 1 << 54, (1 << 53) | (1 << 54)] {
            for present in [false, true] {
                let mut cpu = cpu(64);
                let entry = (descriptor(0xACE, kind, false, 3, false) & !(1 << 47))
                    | (u64::from(present) << 47)
                    | modifier;
                cpu.memory
                    .write_u64(GDT + u64::from(SELECTOR), entry)
                    .unwrap();
                let segments = [
                    cpu.regs.cs,
                    cpu.regs.ds,
                    cpu.regs.ss,
                    cpu.regs.ldtr,
                    cpu.regs.tr,
                ];
                run(&mut cpu, REG64).unwrap();
                assert_eq!(cpu.regs.gpr[index::RAX], 0xACE, "type {kind}");
                assert!(cpu.regs.rflags.contains(RFlags::ZF));
                assert_eq!(
                    cpu.memory.read_u64(GDT + u64::from(SELECTOR)).unwrap(),
                    entry
                );
                assert_eq!(
                    [
                        cpu.regs.cs,
                        cpu.regs.ds,
                        cpu.regs.ss,
                        cpu.regs.ldtr,
                        cpu.regs.tr
                    ],
                    segments
                );
            }
        }
    }
}

#[test]
fn lsl_system_type_matrix_matches_legacy_and_ia32e() {
    for bitness in [32, 64] {
        for kind in 0..16 {
            for present in [false, true] {
                let mut cpu = cpu(bitness);
                let entry = (descriptor(0x1357, kind, true, 3, false) & !(1 << 47))
                    | (u64::from(present) << 47);
                cpu.memory
                    .write_u64(GDT + u64::from(SELECTOR), entry)
                    .unwrap();
                let flags = cpu.regs.rflags;
                run(&mut cpu, REG32).unwrap();
                let valid = if bitness == 64 {
                    matches!(kind, 2 | 9 | 11)
                } else {
                    matches!(kind, 1 | 2 | 3 | 9 | 11)
                };
                if valid {
                    assert_eq!(cpu.regs.gpr[index::RAX], 0x1357);
                    assert!(cpu.regs.rflags.contains(RFlags::ZF));
                } else {
                    assert_rejected(&cpu, flags);
                }
            }
        }
    }
}

#[test]
fn lsl_ia32e_system_descriptor_needs_full_16_bytes_and_valid_upper_type() {
    for kind in [2, 9, 11] {
        for limit in [0x2F, 0x36, 0x37] {
            for upper in [0, 1 << 40, 1 << 41, 1 << 42, 1 << 43, 1 << 44, 1 << 63] {
                let mut cpu = cpu(64);
                cpu.memory
                    .write_u64(
                        GDT + u64::from(SELECTOR),
                        descriptor(0x567, kind, true, 3, false),
                    )
                    .unwrap();
                cpu.memory
                    .write_u64(GDT + u64::from(SELECTOR) + 8, upper)
                    .unwrap();
                cpu.regs.gdt_limit = limit;
                let flags = cpu.regs.rflags;
                run(&mut cpu, REG32).unwrap();
                if limit == 0x37 && upper & (0x1F << 40) == 0 {
                    assert_eq!(cpu.regs.gpr[index::RAX], 0x567);
                    assert!(cpu.regs.rflags.contains(RFlags::ZF));
                } else {
                    assert_rejected(&cpu, flags);
                }
            }
        }
    }
}

#[test]
fn lsl_privilege_matrix_and_conforming_code_exception() {
    // Data, nonconforming code, conforming code, and system segments: the
    // conforming-code exemption must not accidentally include system type B.
    for (kind, system) in [(2, false), (10, false), (14, false), (11, true)] {
        for cpl in 0..4 {
            for rpl in 0..4 {
                for dpl in 0..4 {
                    let mut cpu = cpu(64);
                    cpu.regs.cs.selector = SegmentSelector(8 | u16::from(cpl));
                    cpu.regs
                        .set_gpr(index::RCX, u64::from(SELECTOR | u16::from(rpl)));
                    cpu.memory
                        .write_u64(
                            GDT + u64::from(SELECTOR),
                            descriptor(0xABC, kind, system, dpl, false),
                        )
                        .unwrap();
                    let flags = cpu.regs.rflags;
                    run(&mut cpu, REG32).unwrap();
                    if (!system && kind == 14) || (cpl <= dpl && rpl <= dpl) {
                        assert!(cpu.regs.rflags.contains(RFlags::ZF));
                        assert_eq!(cpu.regs.gpr[index::RAX], 0xABC);
                    } else {
                        assert_rejected(&cpu, flags);
                    }
                    assert_eq!(flags_without_zf(&cpu), flags - RFlags::ZF);
                }
            }
        }
    }
}

fn paging(cpu: &mut Cpu) {
    cpu.regs.cr0 |= Cr0::PG | Cr0::WP;
    cpu.regs.cr4 |= Cr4::PAE;
    cpu.regs.cr3 = 0x10000;
    cpu.memory.write_u64(0x10000, 0x11007).unwrap();
    cpu.memory.write_u64(0x11000, 0x12007).unwrap();
    cpu.memory.write_u64(0x12000, 0x13007).unwrap();
    for page in 0..16_u64 {
        cpu.memory
            .write_u64(0x13000 + page * 8, (page << 12) | 7)
            .unwrap();
    }
}

#[test]
fn lsl_cpl3_reads_supervisor_gdt_like_linux_vdso_getcpu() {
    let mut cpu = cpu(64);
    paging(&mut cpu);
    cpu.regs.cs.selector = SegmentSelector(11);
    cpu.regs.set_gpr(index::RCX, u64::from(SELECTOR | 3));
    cpu.memory.write_u64(0x13000 + 3 * 8, GDT | 3).unwrap(); // Supervisor GDT.
    run(&mut cpu, REG32).unwrap();
    assert_eq!(cpu.regs.gpr[index::RAX], 0xABCDE);
    assert!(cpu.regs.rflags.contains(RFlags::ZF));
    assert_eq!(cpu.exceptions_raised, 0);
}

#[test]
fn lsl_m16_operand_does_not_read_past_the_page_and_uses_normal_cpl() {
    let mut cpu = cpu(64);
    paging(&mut cpu);
    cpu.regs.cs.selector = SegmentSelector(11);
    cpu.regs.set_gpr(index::RBX, 0x40FFE);
    cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50007).unwrap();
    cpu.memory.write_u16(0x50FFE, SELECTOR | 3).unwrap();
    // lsl rax, word [rbx] requires only the last two bytes of this mapped page.
    run(&mut cpu, &[0x48, 0x0F, 0x03, 0x03]).unwrap();
    assert_eq!(cpu.regs.gpr[index::RAX], 0xABCDE);
}

#[test]
fn lsl_memory_sib_rex_and_segment_override_resolve_normally() {
    let mut cpu = cpu(64);
    cpu.regs.fs.base = 0x20000;
    cpu.regs.set_gpr(index::R12, 0x1000);
    cpu.regs.set_gpr(index::R13, 3);
    cpu.memory.write_u16(0x2101D, SELECTOR).unwrap();
    // lsl r8, fs:word [r12 + r13*4 + 0x11]
    run(&mut cpu, &[0x64, 0x4F, 0x0F, 0x03, 0x44, 0xAC, 0x11]).unwrap();
    assert_eq!(cpu.regs.gpr[index::R8], 0xABCDE);
}

#[test]
fn lsl_descriptor_read_follows_nonadjacent_pages_without_side_effects() {
    let mut cpu = cpu(64);
    paging(&mut cpu);
    cpu.regs.gdt_base = 0x40FD4; // Entry 5 begins at 0x40FFC and spans pages.
    cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50003).unwrap();
    cpu.memory.write_u64(0x13000 + 0x41 * 8, 0x60003).unwrap();
    let entry = descriptor(0xDECAF, 2, false, 3, true).to_le_bytes();
    cpu.memory.write(0x50FFC, &entry[..4]).unwrap();
    cpu.memory.write(0x60000, &entry[4..]).unwrap();
    run(&mut cpu, REG64).unwrap();
    assert_eq!(cpu.regs.gpr[index::RAX], 0xDECA_FFFF);
    assert!(cpu.regs.rflags.contains(RFlags::ZF));
}

fn dispatch_fault(cpu: &mut Cpu, bytes: &[u8]) -> CpuError {
    let mut decoder = iced_x86::Decoder::with_ip(64, bytes, CODE, iced_x86::DecoderOptions::NONE);
    let instruction = decoder.decode();
    cpu.instruction_start = CODE;
    cpu.regs.rip = instruction.next_ip();
    cpu.dispatch(&instruction).unwrap_err()
}

#[test]
fn lsl_source_and_descriptor_page_faults_preserve_destination_and_all_flags() {
    for memory_source in [false, true] {
        let mut cpu = cpu(64);
        paging(&mut cpu);
        cpu.regs.cs.selector = SegmentSelector(11);
        cpu.regs.set_gpr(index::RBX, 0x40FFF);
        cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50007).unwrap();
        cpu.memory.write_u8(0x50FFF, SELECTOR as u8).unwrap();
        if !memory_source {
            cpu.regs.gdt_base = 0x40FD4; // Descriptor crosses unmapped page.
        }
        let flags = cpu.regs.rflags;
        let gpr = cpu.regs.gpr;
        let error = dispatch_fault(
            &mut cpu,
            if memory_source {
                &[0x0F, 0x03, 0x03]
            } else {
                REG32
            },
        );
        assert_eq!(
            error,
            CpuError::PageFault {
                linear: 0x41000,
                error_code: if memory_source { 4 } else { 0 }
            }
        );
        assert_eq!(cpu.regs.gpr, gpr);
        assert_eq!(cpu.regs.rflags, flags);
    }
}

#[test]
fn lsl_memory_source_cannot_read_supervisor_page_at_cpl3() {
    let mut cpu = cpu(64);
    paging(&mut cpu);
    cpu.regs.cs.selector = SegmentSelector(11);
    cpu.regs.set_gpr(index::RBX, 0x40000);
    cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50003).unwrap();
    cpu.memory.write_u16(0x50000, SELECTOR).unwrap();
    let flags = cpu.regs.rflags;
    let error = dispatch_fault(&mut cpu, &[0x0F, 0x03, 0x03]);
    assert_eq!(
        error,
        CpuError::PageFault {
            linear: 0x40000,
            error_code: 5
        }
    );
    assert_eq!(cpu.regs.gpr[index::RAX], ORIGINAL);
    assert_eq!(cpu.regs.rflags, flags);
}

#[test]
fn lsl_upper_system_descriptor_page_fault_does_not_clear_zf() {
    let mut cpu = cpu(64);
    paging(&mut cpu);
    cpu.regs.gdt_base = 0x40FD0; // Low qword at 0x40FF8, upper unmapped.
    cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50003).unwrap();
    cpu.memory
        .write_u64(0x50FF8, descriptor(0xFF, 9, true, 3, false))
        .unwrap();
    let flags = cpu.regs.rflags;
    let error = dispatch_fault(&mut cpu, REG64);
    assert_eq!(
        error,
        CpuError::PageFault {
            linear: 0x41000,
            error_code: 0
        }
    );
    assert_eq!(cpu.regs.gpr[index::RAX], ORIGINAL);
    assert_eq!(cpu.regs.rflags, flags);
}

#[test]
fn lsl_register_source_gdt_fault_delivers_restartable_instruction_frame() {
    let mut cpu = cpu(64);
    paging(&mut cpu);
    cpu.regs.gdt_base = 0x3FFB;
    cpu.memory.write_u64(0x4003, 0x00AF_9B00_0000_FFFF).unwrap(); // Handler CS.
    cpu.regs.set_gpr(index::RCX, 0x1000); // Descriptor begins at 0x4FFB.
    let entry = descriptor(0xACE, 2, false, 3, false).to_le_bytes();
    cpu.memory.write(0x4FFB, &entry[..5]).unwrap();
    cpu.memory.write_u64(0x13000 + 5 * 8, 0).unwrap(); // Tail of descriptor absent.
    cpu.regs.idt_base = 0x6000;
    cpu.regs.idt_limit = 0xFFF;
    cpu.memory
        .write_u64(0x6000 + 14 * 16, (8 << 16) | 0x8E00_0000_0000 | 0xA000)
        .unwrap();
    cpu.memory.write_u64(0x6000 + 14 * 16 + 8, 0).unwrap();
    cpu.memory.write_u8(0xA000, 0xF4).unwrap();
    let flags = cpu.regs.rflags;
    run(&mut cpu, REG32).unwrap();
    assert!(cpu.halted, "the guest page-fault handler ran");
    assert_eq!(cpu.regs.gpr[index::RAX], ORIGINAL);
    assert_eq!(cpu.regs.cr2, 0x5000);
    assert_eq!(cpu.memory.read_u64(0x9000 - 40).unwrap(), CODE);
    assert_eq!(cpu.memory.read_u64(0x9000 - 24).unwrap(), flags.bits());
    assert_eq!(cpu.fault_log.back().unwrap().vector, 14);
    assert_eq!(cpu.fault_log.back().unwrap().rip, CODE);
}

#[test]
fn lsl_legacy_memory_uses_implicit_ss_and_checks_entire_source_limit() {
    let mut cpu = cpu(32);
    cpu.regs.ss.base = 0x20000;
    cpu.regs.ss.limit = 0x31;
    cpu.regs.ss.granularity = false;
    cpu.regs.set_gpr(index::RBP, 0x30);
    cpu.memory.write_u16(0x20030, SELECTOR).unwrap();
    run(&mut cpu, &[0x0F, 0x03, 0x45, 0]).unwrap(); // lsl eax, word [ebp]
    assert_eq!(cpu.regs.gpr[index::RAX], 0xABCDE);
    cpu.regs.set_gpr(index::RAX, ORIGINAL);
    cpu.regs.ss.limit = 0x30;
    let mut decoder = iced_x86::Decoder::with_ip(
        32,
        &[0x0F, 0x03, 0x45, 0],
        CODE,
        iced_x86::DecoderOptions::NONE,
    );
    let flags = cpu.regs.rflags;
    let error = cpu.dispatch(&decoder.decode()).unwrap_err();
    assert!(error.to_string().contains("vector 12"), "{error}");
    assert_eq!(cpu.regs.gpr[index::RAX], ORIGINAL);
    assert_eq!(cpu.regs.rflags, flags);
}

#[test]
fn lsl_real_and_virtual_8086_modes_raise_ud_before_reading_source() {
    for virtual_8086 in [false, true] {
        let mut cpu = cpu(16);
        if virtual_8086 {
            cpu.regs.rflags |= RFlags::VM;
        } else {
            cpu.regs.cr0 = Cr0::empty();
        }
        cpu.regs.set_gpr(index::RBX, 0xDEAD_0000);
        let flags = cpu.regs.rflags;
        let mut decoder = iced_x86::Decoder::with_ip(
            16,
            &[0x0F, 0x03, 0x00],
            CODE,
            iced_x86::DecoderOptions::NONE,
        );
        let error = cpu.dispatch(&decoder.decode()).unwrap_err();
        assert!(error.to_string().contains("vector 6"), "{error}");
        assert_eq!(cpu.regs.gpr[index::RAX], ORIGINAL);
        assert_eq!(cpu.regs.rflags, flags);
    }
}
