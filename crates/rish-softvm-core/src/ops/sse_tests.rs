use super::*;

#[test]
fn sse41_min_max_select_signedness_and_lane_width() {
    for (opcode, width, signed, maximum) in [
        (0x38, 1, true, false),
        (0x39, 4, true, false),
        (0x3a, 2, false, false),
        (0x3b, 4, false, false),
        (0x3c, 1, true, true),
        (0x3d, 4, true, true),
        (0x3e, 2, false, true),
        (0x3f, 4, false, true),
    ] {
        let mut cpu = cpu();
        let bits = width * 8;
        let mask = (1_u64 << bits) - 1;
        let sign = 1_u64 << (bits - 1);
        let a = [mask, sign, 1, 0];
        let b = [1, sign - 1, 0, mask];
        let mut left = [0; 16];
        let mut right = [0; 16];
        let mut expected = [0; 16];
        for lane in 0..16 / width {
            let av = a[lane % 4];
            let bv = b[lane % 4];
            left[lane * width..(lane + 1) * width].copy_from_slice(&av.to_le_bytes()[..width]);
            right[lane * width..(lane + 1) * width].copy_from_slice(&bv.to_le_bytes()[..width]);
            let signed_value = |v: u64| {
                if v & sign != 0 {
                    (v | !mask) as i64
                } else {
                    v as i64
                }
            };
            let less = if signed {
                signed_value(av) < signed_value(bv)
            } else {
                av < bv
            };
            let value = if less == maximum { bv } else { av };
            expected[lane * width..(lane + 1) * width]
                .copy_from_slice(&value.to_le_bytes()[..width]);
        }
        cpu.regs.xmm[0] = u128::from_le_bytes(left);
        cpu.regs.xmm[1] = u128::from_le_bytes(right);
        run(&mut cpu, 64, &[0x66, 0x0f, 0x38, opcode, 0xc1]).unwrap();
        assert_eq!(cpu.regs.xmm[0].to_le_bytes(), expected, "opcode {opcode:x}");
    }
}

fn cpu() -> Cpu {
    Cpu::new(1, 0).unwrap()
}

fn decode(bitness: u32, bytes: &[u8], ip: u64) -> Instruction {
    let mut decoder =
        iced_x86::Decoder::with_ip(bitness, bytes, ip, iced_x86::DecoderOptions::NONE);
    decoder.decode()
}

fn run(cpu: &mut Cpu, bitness: u32, bytes: &[u8]) -> Result<(), CpuError> {
    cpu.memory.write(0x1000, bytes).unwrap();
    cpu.regs.rip = 0x1000;
    if bitness == 64 {
        cpu.regs.efer |= crate::arch::registers::Efer::LMA;
    }
    cpu.regs.cs = crate::arch::segments::SegmentRegister {
        base: 0,
        long_mode: bitness == 64,
        default_32: bitness == 32,
        code: true,
        limit: u32::MAX,
        granularity: true,
        writable_or_readable: true,
        ..Default::default()
    };
    let instruction = decode(bitness, bytes, 0x1000);
    let size = instruction.len();
    cpu.regs.rip = 0x1000 + size as u64;
    cpu.dispatch(&instruction)
}

// Diagnostic oracle: execute real musl leaf routines (dumped to /tmp/*.bin)
// through the interpreter and compare against a trivial reference. Localizes
// interpreter instruction bugs without a full guest boot.
fn run_leaf(code_path: &str, rdi: u64, rsi: u64, rdx: u64) -> Cpu {
    use crate::arch::registers::index;
    let code = std::fs::read(code_path).expect("dump the bytes first");
    let mut cpu = cpu();
    cpu.regs.efer |= crate::arch::registers::Efer::LMA;
    cpu.regs.cs = crate::arch::segments::SegmentRegister {
        base: 0,
        long_mode: true,
        code: true,
        limit: u32::MAX,
        granularity: true,
        writable_or_readable: true,
        ..Default::default()
    };
    const CODE: u64 = 0x10000;
    const SENTINEL: u64 = 0x0040_0000;
    cpu.memory.write(CODE, &code).unwrap();
    let rsp = 0x4_FFF8_u64;
    cpu.memory.write(rsp, &SENTINEL.to_le_bytes()).unwrap();
    cpu.regs.gpr[index::RSP] = rsp;
    cpu.regs.gpr[index::RDI] = rdi;
    cpu.regs.gpr[index::RSI] = rsi;
    cpu.regs.gpr[index::RDX] = rdx;
    cpu.regs.rip = CODE;
    for _ in 0..1_000_000 {
        if cpu.regs.rip == SENTINEL {
            break;
        }
        let mut buf = [0_u8; 16];
        cpu.memory.read(cpu.regs.rip, &mut buf).unwrap();
        let insn = decode(64, &buf, cpu.regs.rip);
        cpu.regs.rip = cpu.regs.rip.wrapping_add(insn.len() as u64);
        cpu.dispatch(&insn).unwrap();
    }
    cpu
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
#[ignore = "diagnostic; needs /tmp/memcpy.bin and --nocapture"]
fn musl_memcpy_misaligned_oracle() {
    const SRC: u64 = 0x20000; // 16-aligned
    const DST: u64 = 0x30001; // dst % 4 == 1 -> shufps realign loop
    const LEN: usize = 64;
    let src_bytes: Vec<u8> = (0..LEN)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(3))
        .collect();
    let cpu = run_leaf_seeded("/tmp/memcpy.bin", DST, SRC, LEN as u64, SRC, &src_bytes);
    let mut dst_bytes = vec![0_u8; LEN];
    cpu.memory.read(DST, &mut dst_bytes).unwrap();
    eprintln!("src: {}", hex(&src_bytes));
    eprintln!("dst: {}", hex(&dst_bytes));
    assert_eq!(dst_bytes, src_bytes, "misaligned memcpy corrupted the copy");
}

#[test]
#[ignore = "diagnostic; needs /tmp/memset.bin and --nocapture"]
fn musl_memset_misaligned_oracle() {
    const DST: u64 = 0x30001;
    const LEN: usize = 96;
    const FILL: u64 = 0xAB;
    let cpu = run_leaf("/tmp/memset.bin", DST, FILL, LEN as u64);
    let mut dst = vec![0_u8; LEN];
    cpu.memory.read(DST, &mut dst).unwrap();
    eprintln!("memset dst: {}", hex(&dst));
    let first_bad = (0..LEN).find(|&i| dst[i] != FILL as u8);
    eprintln!("first non-0xAB byte: {first_bad:?}");
    assert!(
        dst.iter().all(|&b| b == FILL as u8),
        "memset did not fill uniformly"
    );
}

#[test]
#[ignore = "diagnostic; needs /tmp/memmove.bin and --nocapture"]
fn musl_memmove_misaligned_oracle() {
    const SRC: u64 = 0x20000;
    const DST: u64 = 0x30001;
    const LEN: usize = 96;
    let src_bytes: Vec<u8> = (0..LEN)
        .map(|i| (i as u8).wrapping_mul(11).wrapping_add(5))
        .collect();
    let cpu = run_leaf_seeded("/tmp/memmove.bin", DST, SRC, LEN as u64, SRC, &src_bytes);
    let mut dst = vec![0_u8; LEN];
    cpu.memory.read(DST, &mut dst).unwrap();
    eprintln!("memmove dst: {}", hex(&dst));
    assert_eq!(dst, src_bytes, "memmove corrupted the copy");
}

fn run_leaf_seeded(
    code_path: &str,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    seed_at: u64,
    seed: &[u8],
) -> Cpu {
    use crate::arch::registers::index;
    let code = std::fs::read(code_path).expect("dump the bytes first");
    let mut cpu = cpu();
    cpu.regs.efer |= crate::arch::registers::Efer::LMA;
    cpu.regs.cs = crate::arch::segments::SegmentRegister {
        base: 0,
        long_mode: true,
        code: true,
        limit: u32::MAX,
        granularity: true,
        writable_or_readable: true,
        ..Default::default()
    };
    const CODE: u64 = 0x10000;
    const SENTINEL: u64 = 0x0040_0000;
    cpu.memory.write(CODE, &code).unwrap();
    cpu.memory.write(seed_at, seed).unwrap();
    let rsp = 0x4_FFF8_u64;
    cpu.memory.write(rsp, &SENTINEL.to_le_bytes()).unwrap();
    cpu.regs.gpr[index::RSP] = rsp;
    cpu.regs.gpr[index::RDI] = rdi;
    cpu.regs.gpr[index::RSI] = rsi;
    cpu.regs.gpr[index::RDX] = rdx;
    cpu.regs.rip = CODE;
    for _ in 0..1_000_000 {
        if cpu.regs.rip == SENTINEL {
            break;
        }
        let mut buf = [0_u8; 16];
        cpu.memory.read(cpu.regs.rip, &mut buf).unwrap();
        let insn = decode(64, &buf, cpu.regs.rip);
        cpu.regs.rip = cpu.regs.rip.wrapping_add(insn.len() as u64);
        cpu.dispatch(&insn).unwrap();
    }
    cpu
}

#[test]
fn pinsrw_inserts_a_word_lane() {
    let mut cpu = cpu();
    cpu.regs.gpr[1] = 0xBEEF; // rcx
    // 66 0F C4 C1 02: pinsrw xmm0, ecx, 2  -> word lane 2
    run(&mut cpu, 64, &[0x66, 0x0F, 0xC4, 0xC1, 0x02]).unwrap();
    assert_eq!((cpu.regs.xmm[0] >> 32) & 0xFFFF, 0xBEEF);
}

#[test]
fn pextrw_extracts_a_word_lane() {
    let mut cpu = cpu();
    cpu.regs.xmm[1] = 0xDEAD_0000_0000_0000_0000_0000_0000_0000;
    // 66 0F C5 C1 07: pextrw eax, xmm1, 7  -> top word lane
    run(&mut cpu, 64, &[0x66, 0x0F, 0xC5, 0xC1, 0x07]).unwrap();
    assert_eq!(cpu.regs.gpr[0] & 0xFFFF, 0xDEAD);
}

#[test]
fn pmovmskb_gathers_byte_sign_bits() {
    let mut cpu = cpu();
    // Top bit set in bytes 0, 2, and 15.
    cpu.regs.xmm[1] = 0x8000_0000_0000_0000_0000_0000_0080_0080;
    // 66 0F D7 C1: pmovmskb eax, xmm1
    run(&mut cpu, 64, &[0x66, 0x0F, 0xD7, 0xC1]).unwrap();
    assert_eq!(cpu.regs.gpr[0] & 0xFFFF, 0b1000_0000_0000_0101);
}

#[test]
fn psubb_wraps_per_byte_lane() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0x0000_0000_0000_0000_0000_0000_0000_0500;
    cpu.regs.xmm[1] = 0x0000_0000_0000_0000_0000_0000_0000_0001;
    // 66 0F F8 C1: psubb xmm0, xmm1  (lane0: 0x00-0x01=0xFF, lane1: 0x05-0x00=0x05)
    run(&mut cpu, 64, &[0x66, 0x0F, 0xF8, 0xC1]).unwrap();
    assert_eq!(cpu.regs.xmm[0] & 0xFFFF, 0x05FF);
}

#[test]
fn addsd_adds_the_low_double_and_keeps_the_high_lane() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = (u128::from(2.0_f64.to_bits())) | (0xDEAD_u128 << 64);
    cpu.regs.xmm[1] = u128::from(3.0_f64.to_bits());
    // F2 0F 58 C1: addsd xmm0, xmm1
    run(&mut cpu, 64, &[0xF2, 0x0F, 0x58, 0xC1]).unwrap();
    assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), 5.0);
    assert_eq!(cpu.regs.xmm[0] >> 64, 0xDEAD);
}

#[test]
fn ucomisd_sets_flags_for_less_than() {
    use crate::arch::registers::RFlags;
    let mut cpu = cpu();
    cpu.regs.xmm[0] = u128::from(1.0_f64.to_bits());
    cpu.regs.xmm[1] = u128::from(2.0_f64.to_bits());
    // 66 0F 2E C1: ucomisd xmm0, xmm1  (1.0 < 2.0 => CF=1, ZF=0)
    run(&mut cpu, 64, &[0x66, 0x0F, 0x2E, 0xC1]).unwrap();
    assert!(cpu.regs.rflags.contains(RFlags::CF));
    assert!(!cpu.regs.rflags.contains(RFlags::ZF));
}

#[test]
fn pxor_zeroes_a_register() {
    let mut cpu = cpu();
    cpu.regs.xmm[3] = 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF;
    // 66 0F EF DB: pxor xmm3, xmm3
    run(&mut cpu, 64, &[0x66, 0x0F, 0xEF, 0xDB]).unwrap();
    assert_eq!(cpu.regs.xmm[3], 0);
}

#[test]
fn movdqu_loads_unaligned_memory() {
    let mut cpu = cpu();
    let mut payload = [0_u8; 16];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = index as u8;
    }
    cpu.memory.write(0x2001, &payload).unwrap();
    // F3 0F 6F 04 25 01 20 00 00: movdqu xmm0, [0x2001]
    run(
        &mut cpu,
        64,
        &[0xF3, 0x0F, 0x6F, 0x04, 0x25, 0x01, 0x20, 0x00, 0x00],
    )
    .unwrap();
    assert_eq!(
        cpu.regs.xmm[0],
        u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
    );
}

#[test]
fn movdqu_stores_unaligned_memory() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    // F3 0F 7F 04 25 00 20 00 00: movdqu [0x2000], xmm0
    run(
        &mut cpu,
        64,
        &[0xF3, 0x0F, 0x7F, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00],
    )
    .unwrap();
    assert_eq!(cpu.memory.read_u64(0x2000).unwrap(), 0x090A_0B0C_0D0E_0F10);
    assert_eq!(cpu.memory.read_u64(0x2008).unwrap(), 0x0102_0304_0506_0708);
}

#[test]
fn pshufd_broadcasts_a_dword() {
    let mut cpu = cpu();
    cpu.regs.xmm[1] = 0x0000_0000_DEAD_BEEF_0000_0000_0000_0001;
    // 66 0F 70 C1 00: pshufd xmm0, xmm1, 0
    run(&mut cpu, 64, &[0x66, 0x0F, 0x70, 0xC1, 0x00]).unwrap();
    let expected = 0x0000_0001_0000_0001_0000_0001_0000_0001_u128;
    assert_eq!(cpu.regs.xmm[0], expected);
}

#[test]
fn shufps_selects_dest_then_source_lanes() {
    // musl's misaligned SSE memcpy realigns bytes with `shufps` (imm 0x00
    // and 0x98). Lanes 0/1 come from the destination, lanes 2/3 from the
    // source, each choosing any dword of that operand (Intel SELECT4).
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0x0000_000D_0000_000C_0000_000B_0000_000A;
    cpu.regs.xmm[1] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
    // 0F C6 C1 98: shufps xmm0, xmm1, 0x98
    run(&mut cpu, 64, &[0x0F, 0xC6, 0xC1, 0x98]).unwrap();
    assert_eq!(
        cpu.regs.xmm[0],
        0x0000_0003_0000_0002_0000_000C_0000_000A_u128
    );
}

#[test]
fn shufpd_selects_qwords_from_each_operand() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0xAAAA_AAAA_AAAA_AAAA_BBBB_BBBB_BBBB_BBBB;
    cpu.regs.xmm[1] = 0xCCCC_CCCC_CCCC_CCCC_DDDD_DDDD_DDDD_DDDD;
    // 66 0F C6 C1 01: shufpd xmm0, xmm1, 1 -> low=dest[1], high=src[0]
    run(&mut cpu, 64, &[0x66, 0x0F, 0xC6, 0xC1, 0x01]).unwrap();
    assert_eq!(
        cpu.regs.xmm[0],
        0xDDDD_DDDD_DDDD_DDDD_AAAA_AAAA_AAAA_AAAA_u128
    );
}

#[test]
fn cmpsd_writes_an_all_ones_mask_on_a_true_predicate() {
    // Go's math.archLog uses cmpnltsd; an unimplemented compare left the
    // result register stale and crashed the `docker` CLI.
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0x1111_1111_1111_1111_4008_0000_0000_0000; // low = 3.0
    cpu.regs.xmm[2] = 0x4014_0000_0000_0000; // low = 5.0
    // F2 0F C2 C2 01: cmpltsd xmm0, xmm2 (3.0 < 5.0 -> true)
    run(&mut cpu, 64, &[0xF2, 0x0F, 0xC2, 0xC2, 0x01]).unwrap();
    assert_eq!(cpu.regs.xmm[0] & u128::from(u64::MAX), u128::from(u64::MAX));
    assert_eq!(
        cpu.regs.xmm[0] >> 64,
        0x1111_1111_1111_1111,
        "upper preserved"
    );
    // NLT of the same pair is false -> zero mask.
    cpu.regs.xmm[0] = 0x4008_0000_0000_0000;
    run(&mut cpu, 64, &[0xF2, 0x0F, 0xC2, 0xC2, 0x05]).unwrap();
    assert_eq!(cpu.regs.xmm[0] & u128::from(u64::MAX), 0);
}

#[test]
fn pslld_shifts_every_dword_lane() {
    // Regression: a precedence slip once left every lane above lane 0
    // zeroed, which garbled musl's misaligned SSE memcpy (uname -a).
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
    // 66 0F 72 F0 08: pslld xmm0, 8
    run(&mut cpu, 64, &[0x66, 0x0F, 0x72, 0xF0, 0x08]).unwrap();
    assert_eq!(
        cpu.regs.xmm[0],
        0x0000_0400_0000_0300_0000_0200_0000_0100_u128
    );
}

#[test]
fn psrld_shifts_every_dword_lane() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0x0000_0400_0000_0300_0000_0200_0000_0100;
    // 66 0F 72 D0 08: psrld xmm0, 8
    run(&mut cpu, 64, &[0x66, 0x0F, 0x72, 0xD0, 0x08]).unwrap();
    assert_eq!(
        cpu.regs.xmm[0],
        0x0000_0004_0000_0003_0000_0002_0000_0001_u128
    );
}

#[test]
fn punpcklqdq_duplicates_the_low_qword() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0xAAAA_AAAA_AAAA_AAAA_BBBB_BBBB_BBBB_BBBB;
    cpu.regs.xmm[1] = 0xCCCC_CCCC_CCCC_CCCC_DDDD_DDDD_DDDD_DDDD;
    // 66 0F 6C C1: punpcklqdq xmm0, xmm1
    run(&mut cpu, 64, &[0x66, 0x0F, 0x6C, 0xC1]).unwrap();
    assert_eq!(cpu.regs.xmm[0], 0xDDDD_DDDD_DDDD_DDDD_BBBB_BBBB_BBBB_BBBB);
}

#[test]
fn movq_round_trips_via_memory() {
    let mut cpu = cpu();
    cpu.regs.xmm[0] = 0xDEAD_BEEF_CAFE_F00D;
    // 66 0F D6 04 25 00 20 00 00: movq [0x2000], xmm0
    run(
        &mut cpu,
        64,
        &[0x66, 0x0F, 0xD6, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00],
    )
    .unwrap();
    assert_eq!(cpu.memory.read_u64(0x2000).unwrap(), 0xDEAD_BEEF_CAFE_F00D);
}
#[test]
fn psrad_sign_extends_each_dword_lane() {
    let mut cpu = cpu();
    // Two negative and two positive dwords.
    let value = (0xFFFF_FFF0_u128)
        | (0x8000_0000_u128 << 32)
        | (0x0000_0010_u128 << 64)
        | (0x7FFF_FFFF_u128 << 96);
    write_xmm(&mut cpu.regs, iced_x86::Register::XMM1, value);
    // psrad xmm1, 4
    run(&mut cpu, 64, &[0x66, 0x0F, 0x72, 0xE1, 0x04]).unwrap();
    let r = read_xmm(&cpu.regs, iced_x86::Register::XMM1);
    assert_eq!(r & 0xFFFF_FFFF, 0xFFFF_FFFF, "negative lane keeps sign");
    assert_eq!(
        (r >> 32) & 0xFFFF_FFFF,
        0xF800_0000,
        "0x80000000>>4 arithmetic"
    );
    assert_eq!(
        (r >> 64) & 0xFFFF_FFFF,
        0x0000_0001,
        "positive lane logical"
    );
    assert_eq!((r >> 96) & 0xFFFF_FFFF, 0x07FF_FFFF, "positive top lane");
}
