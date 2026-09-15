use crate::arch::registers::{Cr0, Cr4, Efer, RFlags, index};
use crate::{Cpu, CpuError};

const CODE: u64 = 0x1000;
const DATA: u64 = 0x2000;

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

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    cpu.memory.write(CODE, bytes).unwrap();
    cpu.regs.rip = CODE;
    cpu.step()
}

#[test]
fn pmulld_computes_all_four_low_dwords_without_saturation_or_lane_carry() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = packed([7, u32::MAX, 0x8000_0000, 0x8000_0001]);
    cpu.regs.xmm[1] = packed([3, 2, 3, 0x8000_0001]);
    let mut xmm = cpu.regs.xmm;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;

    // 66 0F 38 40 C1: pmulld xmm0, xmm1 (legacy SSE4.1).
    run(&mut cpu, &[0x66, 0x0F, 0x38, 0x40, 0xC1]).unwrap();

    xmm[0] = packed([21, 0xFFFF_FFFE, 0x8000_0000, 1]);
    assert_eq!(cpu.regs.xmm, xmm, "only the destination may change");
    assert_eq!(cpu.regs.rflags, flags);
    assert_eq!(cpu.mxcsr, mxcsr);
    assert_eq!(cpu.regs.rip, CODE + 5);
}

#[test]
fn pmulld_zero_and_negative_lanes_have_exact_twos_complement_results() {
    let mut cpu = cpu();
    cpu.regs.xmm[2] = packed([0, 0xFFFF_FFFD, 0x8000_0000, 0x0001_0000]);
    cpu.regs.xmm[3] = packed([u32::MAX, 7, u32::MAX, 0x0001_0000]);
    let source = cpu.regs.xmm[3];

    run(&mut cpu, &[0x66, 0x0F, 0x38, 0x40, 0xD3]).unwrap();

    assert_eq!(cpu.regs.xmm[2], packed([0, 0xFFFF_FFEB, 0x8000_0000, 0]));
    assert_eq!(cpu.regs.xmm[3], source);
}

#[test]
fn pmulld_rex_selects_every_register_and_handles_same_register_operands() {
    for destination in 0..16 {
        for source in 0..16 {
            let mut cpu = cpu();
            for (register, value) in cpu.regs.xmm.iter_mut().enumerate() {
                let seed = register as u32 + 1;
                *value = packed([seed, 0x8000_0000 + seed, u32::MAX - seed, 0x10000 + seed]);
            }
            let mut expected = cpu.regs.xmm;
            let a = expected[destination].to_le_bytes();
            let b = expected[source].to_le_bytes();
            let mut result = [0_u32; 4];
            for lane in 0..4 {
                // An independently widened signed product also defines the
                // low 32 bits for negative values and self multiplication.
                let left = i32::from_le_bytes(a[lane * 4..lane * 4 + 4].try_into().unwrap());
                let right = i32::from_le_bytes(b[lane * 4..lane * 4 + 4].try_into().unwrap());
                result[lane] = (i64::from(left) * i64::from(right)) as u32;
            }
            expected[destination] = packed(result);
            let flags = cpu.regs.rflags;
            let gpr = cpu.regs.gpr;
            let rex = 0x40 | ((destination >> 3) as u8 * 4) | (source >> 3) as u8;
            let modrm = 0xC0 | ((destination & 7) as u8 * 8) | (source & 7) as u8;

            run(&mut cpu, &[0x66, rex, 0x0F, 0x38, 0x40, modrm]).unwrap();

            assert_eq!(cpu.regs.xmm, expected, "xmm{destination}, xmm{source}");
            assert_eq!(cpu.regs.rflags, flags);
            assert_eq!(cpu.regs.gpr, gpr);
        }
    }
}

#[test]
fn pmulld_memory_source_uses_rex_sib_scale_and_displacement() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::R12, DATA);
    cpu.regs.set_gpr(index::R13, 3);
    cpu.regs.xmm[14] = packed([2, 3, 4, 5]);
    let source = packed([7, 11, 13, 17]).to_le_bytes();
    cpu.memory.write(DATA + 0x30, &source).unwrap();
    let gpr = cpu.regs.gpr;
    let flags = cpu.regs.rflags;

    // pmulld xmm14, [r12 + r13*4 + 0x24]
    run(&mut cpu, &[0x66, 0x47, 0x0F, 0x38, 0x40, 0x74, 0xAC, 0x24]).unwrap();

    assert_eq!(cpu.regs.xmm[14], packed([14, 33, 52, 85]));
    let mut after = [0; 16];
    cpu.memory.read(DATA + 0x30, &mut after).unwrap();
    assert_eq!(after, source);
    assert_eq!(cpu.regs.gpr, gpr);
    assert_eq!(cpu.regs.rflags, flags);
}

#[test]
fn pmulld_rip_relative_memory_honors_fs_base_and_full_instruction_length() {
    let mut cpu = cpu();
    cpu.regs.fs.base = 0x10000;
    cpu.regs.xmm[15] = packed([2, 4, 6, 8]);
    let bytes = [0x64, 0x66, 0x44, 0x0F, 0x38, 0x40, 0x3D, 0x25, 0, 0, 0];
    cpu.memory
        .write(0x11030, &packed([3, 5, 7, 9]).to_le_bytes())
        .unwrap();

    // pmulld xmm15, fs:[rip + 0x25], next RIP is CODE + 11.
    run(&mut cpu, &bytes).unwrap();

    assert_eq!(cpu.regs.xmm[15], packed([6, 20, 42, 72]));
    assert_eq!(cpu.regs.rip, CODE + bytes.len() as u64);
}

#[test]
fn pmulld_address_size_override_uses_the_low_32_bit_address() {
    let mut cpu = cpu();
    cpu.regs.set_gpr(index::RAX, 0xABCD_0000_0000 | DATA);
    cpu.regs.xmm[0] = packed([13, 17, 19, 23]);
    cpu.memory
        .write(DATA, &packed([2, 3, 5, 7]).to_le_bytes())
        .unwrap();

    // pmulld xmm0, [eax]
    run(&mut cpu, &[0x67, 0x66, 0x0F, 0x38, 0x40, 0x00]).unwrap();

    assert_eq!(cpu.regs.xmm[0], packed([26, 51, 95, 161]));
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
fn pmulld_memory_fault_after_reading_low_lanes_preserves_all_registers() {
    let mut cpu = cpu();
    paging(&mut cpu);
    cpu.regs.xmm[0] = packed([2, 3, 4, 5]);
    cpu.memory.write_u64(0x50FF8, u64::MAX).unwrap();
    let xmm = cpu.regs.xmm;
    let gpr = cpu.regs.gpr;
    let flags = cpu.regs.rflags;
    let mxcsr = cpu.mxcsr;
    let bytes = [0x66, 0x0F, 0x38, 0x40, 0x03]; // pmulld xmm0, [rbx]
    let instruction =
        iced_x86::Decoder::with_ip(64, &bytes, CODE, iced_x86::DecoderOptions::NONE).decode();

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
fn pmulld_memory_source_reads_both_noncontiguous_physical_pages() {
    let mut cpu = cpu();
    paging(&mut cpu);
    cpu.memory.write_u64(0x13000 + 0x41 * 8, 0x60003).unwrap();
    cpu.regs.xmm[0] = packed([2, 3, 4, 5]);
    let source = packed([7, 11, 13, 17]).to_le_bytes();
    cpu.memory.write(0x50FF8, &source[..8]).unwrap();
    cpu.memory.write(0x60000, &source[8..]).unwrap();

    run(&mut cpu, &[0x66, 0x0F, 0x38, 0x40, 0x03]).unwrap();

    assert_eq!(cpu.regs.xmm[0], packed([14, 33, 52, 85]));
}
