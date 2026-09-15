use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
use crate::arch::segments::{Descriptor, SegmentSelector};
use crate::{Cpu, CpuError};

const CODE: u64 = 0x1000;
const DATA: u64 = 0x2000;
const STACK: u64 = 0x9000;
const HANDLER: u64 = 0xA000;
const LOAD: &[u8] = &[0x0F, 0xAE, 0x10]; // ldmxcsr [rax]
const STORE: &[u8] = &[0x0F, 0xAE, 0x18]; // stmxcsr [rax]

fn cpu() -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cr0 |= Cr0::PE;
    cpu.regs.cr4 |= Cr4::OSFXSR;
    cpu.regs.cs = Descriptor::decode(0x00AF_9B00_0000_FFFF).load(SegmentSelector(8));
    cpu.regs.set_gpr(index::RAX, DATA);
    cpu.regs.set_rsp(STACK);
    cpu
}

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step()
}

fn install_handler(cpu: &mut Cpu, vector: u8) {
    cpu.regs.gdt_base = 0x3000;
    cpu.regs.gdt_limit = 0xFFFF;
    cpu.memory.write_u64(0x3008, 0x00AF_9B00_0000_FFFF).unwrap();
    cpu.regs.idt_base = 0x4000;
    cpu.regs.idt_limit = 0xFFF;
    let gate = 0x4000 + u64::from(vector) * 16;
    cpu.memory
        .write_u64(gate, (8 << 16) | 0x8E00_0000_0000 | HANDLER)
        .unwrap();
    cpu.memory.write_u64(gate + 8, 0).unwrap();
    cpu.memory.write(HANDLER, &[0xF4]).unwrap(); // hlt
}

fn assert_fault(cpu: &Cpu, vector: u8, has_error: bool) {
    assert_eq!(cpu.exceptions_raised, 1);
    let event = cpu.fault_log.back().unwrap();
    assert_eq!(event.vector, vector);
    assert_eq!(event.rip, CODE, "fault return points to the instruction");
    assert_eq!(event.error_code, 0);
    assert_eq!(cpu.memory.read_u64(STACK - 40).unwrap(), CODE);
    assert_eq!(cpu.regs.rsp(), STACK - if has_error { 48 } else { 40 });
    if has_error {
        assert_eq!(cpu.memory.read_u64(STACK - 48).unwrap(), 0);
    }
}

#[test]
fn mxcsr_default_and_encoded_four_byte_round_trip() {
    let mut cpu = cpu();
    assert_eq!(cpu.mxcsr, 0x1F80);
    cpu.memory.write(DATA - 1, &[0xCC; 6]).unwrap();
    run(&mut cpu, STORE).unwrap();
    assert_eq!(cpu.memory.read_u32(DATA).unwrap(), 0x1F80);
    assert_eq!(cpu.memory.read_u8(DATA - 1).unwrap(), 0xCC);
    assert_eq!(cpu.memory.read_u8(DATA + 4).unwrap(), 0xCC);
    assert_eq!(cpu.regs.rip, CODE + 3);

    // Include DAZ/FTZ, both rounding bits, every status bit, and unmasked
    // exception flags. LDMXCSR itself must never deliver a SIMD exception.
    for value in [0, 0x0041, 0x8040, 0x6000, 0xFFFF] {
        cpu.memory.write_u32(DATA, value).unwrap();
        let gpr = cpu.regs.gpr;
        let flags = cpu.regs.rflags;
        run(&mut cpu, LOAD).unwrap();
        assert_eq!(cpu.mxcsr, value);
        assert_eq!(cpu.regs.gpr, gpr);
        assert_eq!(cpu.regs.rflags, flags);
        run(&mut cpu, STORE).unwrap();
        assert_eq!(cpu.memory.read_u32(DATA).unwrap(), value);
        assert_eq!(cpu.exceptions_raised, 0);
    }
}

#[test]
fn mxcsr_store_clears_reserved_bits_without_mutating_register() {
    let mut cpu = cpu();
    // Defensively enforce architectural output even if an older snapshot
    // imported state with reserved bits set.
    cpu.mxcsr = 0xABCD_FF40;
    run(&mut cpu, STORE).unwrap();
    assert_eq!(cpu.memory.read_u32(DATA).unwrap(), 0xFF40);
    assert_eq!(cpu.mxcsr, 0xABCD_FF40);
}

#[test]
fn mxcsr_sib_rex_displacement_and_unaligned_access() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::R12, DATA);
    cpu.regs.set_gpr(index::R13, 3);
    cpu.mxcsr = 0xAFC0;
    // stmxcsr [r12 + r13*4 + 0x21], an unaligned m32 operand.
    run(&mut cpu, &[0x43, 0x0F, 0xAE, 0x5C, 0xAC, 0x21]).unwrap();
    assert_eq!(cpu.memory.read_u32(DATA + 12 + 0x21).unwrap(), 0xAFC0);
    cpu.mxcsr = 0;
    run(&mut cpu, &[0x43, 0x0F, 0xAE, 0x54, 0xAC, 0x21]).unwrap();
    assert_eq!(cpu.mxcsr, 0xAFC0);
}

#[test]
fn mxcsr_rip_relative_and_fs_override() {
    let mut cpu = cpu();
    cpu.regs.fs.base = 0x10000;
    cpu.mxcsr = 0x3F80;
    // fs:stmxcsr [rip + 0x20]. The complete instruction is eight bytes.
    run(&mut cpu, &[0x64, 0x0F, 0xAE, 0x1D, 0x20, 0, 0, 0]).unwrap();
    assert_eq!(cpu.memory.read_u32(0x11028).unwrap(), 0x3F80);
    cpu.mxcsr = 0;
    run(&mut cpu, &[0x64, 0x0F, 0xAE, 0x15, 0x20, 0, 0, 0]).unwrap();
    assert_eq!(cpu.mxcsr, 0x3F80);
}

#[test]
fn mxcsr_address_size_override_truncates_base_to_32_bits() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::RAX, 0xABCD_0000_0000 | DATA);
    cpu.mxcsr = 0x5F80;
    run(&mut cpu, &[0x67, 0x0F, 0xAE, 0x18]).unwrap();
    assert_eq!(cpu.memory.read_u32(DATA).unwrap(), 0x5F80);
    cpu.mxcsr = 0;
    run(&mut cpu, &[0x67, 0x0F, 0xAE, 0x10]).unwrap();
    assert_eq!(cpu.mxcsr, 0x5F80);
}

#[test]
fn mxcsr_legacy_16_and_32_bit_addressing_uses_m32() {
    for bitness in [16, 32] {
        let mut cpu = cpu();
        cpu.regs.efer = Efer::empty();
        cpu.regs.cs.long_mode = false;
        cpu.regs.cs.default_32 = bitness == 32;
        cpu.regs.ds.base = DATA;
        cpu.regs.set_gpr(index::RBX, 0x30);
        cpu.regs.set_gpr(index::RSI, 0x10);
        cpu.regs.set_gpr(index::RAX, 0x40);
        // The same ModRM encodes [bx + si] in 16-bit mode and [eax] in
        // 32-bit mode. Both resolve DS:0x40 and transfer exactly four bytes.
        cpu.mxcsr = 0xBFC0;
        run(&mut cpu, STORE).unwrap();
        assert_eq!(cpu.memory.read_u32(DATA + 0x40).unwrap(), 0xBFC0);
        cpu.mxcsr = 0;
        run(&mut cpu, LOAD).unwrap();
        assert_eq!(cpu.mxcsr, 0xBFC0);
    }
}

#[test]
fn mxcsr_load_rejects_every_reserved_bit_with_gp_and_preserves_state() {
    for bit in 16..32 {
        let mut cpu = cpu();
        install_handler(&mut cpu, 13);
        cpu.regs.set_gpr(index::RBX, 0x1234);
        cpu.regs.xmm[0] = 0xABCD;
        cpu.memory.write_u32(DATA, 0x1F80 | (1 << bit)).unwrap();
        let memory_before = cpu.memory.read_u32(DATA).unwrap();
        let gpr = cpu.regs.gpr;
        let xmm = cpu.regs.xmm;
        let flags = cpu.regs.rflags;
        run(&mut cpu, LOAD).unwrap();
        assert_fault(&cpu, 13, true);
        assert_eq!(cpu.mxcsr, 0x1F80);
        assert_eq!(cpu.memory.read_u32(DATA).unwrap(), memory_before);
        assert_eq!(cpu.regs.xmm, xmm);
        for (i, expected) in gpr.iter().enumerate() {
            if i != index::RSP {
                assert_eq!(cpu.regs.gpr[i], *expected);
            }
        }
        assert_eq!(cpu.memory.read_u64(STACK - 24).unwrap(), flags.bits());
    }
}

#[test]
fn mxcsr_sse_control_gates_deliver_ud_or_nm_before_operand_access() {
    for bytes in [LOAD, STORE] {
        for (cr0, osfxsr, vector) in [
            (Cr0::EM, true, 6),
            (Cr0::empty(), false, 6),
            (Cr0::TS, true, 7),
            (Cr0::EM | Cr0::TS, true, 6),
            (Cr0::TS, false, 6),
        ] {
            let mut cpu = cpu();
            install_handler(&mut cpu, vector);
            cpu.regs.cr0 |= cr0;
            cpu.regs.cr4.set(Cr4::OSFXSR, osfxsr);
            // This would be invalid memory if the gate didn't run first.
            cpu.regs.set_gpr(index::RAX, u64::MAX - 8);
            cpu.memory.write_u32(DATA, 0xFEED_FACE).unwrap();
            run(&mut cpu, bytes).unwrap();
            assert_fault(&cpu, vector, false);
            assert_eq!(cpu.mxcsr, 0x1F80);
            assert_eq!(cpu.memory.read_u32(DATA).unwrap(), 0xFEED_FACE);
        }
    }
}

fn enable_paging(cpu: &mut Cpu, second_pte: u64) {
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
    // Deliberately nonadjacent physical frames for consecutive virtual pages.
    cpu.memory.write_u64(0x13000 + 0x40 * 8, 0x50003).unwrap();
    cpu.memory
        .write_u64(0x13000 + 0x41 * 8, second_pte)
        .unwrap();
    cpu.regs.set_gpr(index::RAX, 0x40FFE);
}

#[test]
fn mxcsr_cross_page_transfers_follow_both_physical_frames() {
    let mut cpu = cpu();
    enable_paging(&mut cpu, 0x60003);
    cpu.mxcsr = 0xFFC0;
    cpu.memory.write_u32(0x51000, 0xDEAD_BEEF).unwrap();
    run(&mut cpu, STORE).unwrap();
    assert_eq!(cpu.memory.read_u16(0x50FFE).unwrap(), 0xFFC0);
    assert_eq!(cpu.memory.read_u16(0x60000).unwrap(), 0);
    assert_eq!(cpu.memory.read_u32(0x51000).unwrap(), 0xDEAD_BEEF);
    cpu.mxcsr = 0;
    run(&mut cpu, LOAD).unwrap();
    assert_eq!(cpu.mxcsr, 0xFFC0);
}

#[test]
fn mxcsr_cross_page_faults_leave_register_and_destination_unchanged() {
    for (bytes, second_pte, error_code) in [
        (LOAD, 0, 0),
        (STORE, 0, 2),
        (STORE, 0x60001, 3), // Present, read-only and CR0.WP enabled.
    ] {
        let mut cpu = cpu();
        install_handler(&mut cpu, 14);
        enable_paging(&mut cpu, second_pte);
        cpu.memory.write_u16(0x50FFE, 0xCAFE).unwrap();
        cpu.memory.write_u16(0x60000, 0xBEEF).unwrap();
        cpu.regs.xmm[0] = 0x5678;
        run(&mut cpu, bytes).unwrap();
        let event = cpu.fault_log.back().unwrap();
        assert_eq!(event.vector, 14);
        assert_eq!(event.rip, CODE);
        assert_eq!(event.error_code, error_code);
        assert_eq!(cpu.regs.cr2, 0x41000);
        assert_eq!(cpu.memory.read_u64(STACK - 40).unwrap(), CODE);
        assert_eq!(cpu.mxcsr, 0x1F80);
        assert_eq!(cpu.regs.xmm[0], 0x5678);
        assert_eq!(cpu.memory.read_u16(0x50FFE).unwrap(), 0xCAFE);
        assert_eq!(cpu.memory.read_u16(0x60000).unwrap(), 0xBEEF);
        assert!(cpu.halted, "the guest page-fault handler ran");
    }
}

#[test]
fn mxcsr_noncanonical_operands_raise_gp_or_ss_without_access() {
    for (bytes, base, vector) in [
        (STORE, 0x0000_8000_0000_0000, 13),
        (LOAD, 0x0000_7FFF_FFFF_FFFE, 13),
        (&[0x0F, 0xAE, 0x55, 0][..], 0x0000_8000_0000_0000, 12), // [rbp]
    ] {
        let mut cpu = cpu();
        install_handler(&mut cpu, vector);
        cpu.regs.set_gpr(index::RAX, base);
        cpu.regs.set_gpr(index::RBP, base);
        run(&mut cpu, bytes).unwrap();
        assert_fault(&cpu, vector, true);
        assert_eq!(cpu.mxcsr, 0x1F80);
    }
}

#[test]
fn mxcsr_user_alignment_check_is_optional_and_faults_before_store() {
    let mut cpu = cpu();
    cpu.regs.cs.selector = SegmentSelector(11);
    cpu.regs.cr0 |= Cr0::AM;
    cpu.regs.rflags |= RFlags::AC;
    cpu.regs.set_gpr(index::RAX, DATA + 1);
    cpu.memory.write_u32(DATA + 1, 0xDEAD_BEEF).unwrap();
    install_handler(&mut cpu, 17);
    cpu.regs.tr_base = 0xB000;
    cpu.memory.write_u64(0xB004, STACK).unwrap(); // Ring-0 stack for #AC.
    run(&mut cpu, STORE).unwrap();
    assert_fault(&cpu, 17, true);
    assert_eq!(cpu.memory.read_u32(DATA + 1).unwrap(), 0xDEAD_BEEF);
    assert_eq!(cpu.mxcsr, 0x1F80);
}
