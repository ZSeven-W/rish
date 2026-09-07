use crate::{CpuError, cpu::Cpu};

fn run(cpu: &mut Cpu, bytes: &[u8]) -> Result<(), CpuError> {
    let mut decoder = iced_x86::Decoder::with_ip(64, bytes, 0x1000, iced_x86::DecoderOptions::NONE);
    let instruction = decoder.decode();
    cpu.regs.rip = 0x1000 + instruction.len() as u64;
    cpu.dispatch(&instruction)
}

#[test]
fn roundsd_floors_the_source_and_preserves_the_high_lane() {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.xmm[0] = (0xDEAD_BEEF_CAFE_F00D_u128 << 64) | u128::from(99.0_f64.to_bits());
    cpu.regs.xmm[1] = u128::from((-3.25_f64).to_bits());

    // 66 0F 3A 0B C1 09: roundsd xmm0, xmm1, floor|no-exception
    run(&mut cpu, &[0x66, 0x0F, 0x3A, 0x0B, 0xC1, 0x09]).unwrap();

    assert_eq!(f64::from_bits(cpu.regs.xmm[0] as u64), -4.0);
    assert_eq!(cpu.regs.xmm[0] >> 64, 0xDEAD_BEEF_CAFE_F00D);
}

#[test]
fn blendvpd_selects_each_qword_from_the_xmm0_sign_bits() {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.xmm[3] = (20_u128 << 64) | 10;
    cpu.regs.xmm[2] = (40_u128 << 64) | 30;
    cpu.regs.xmm[0] = 1_u128 << 127;

    // 66 0F 38 15 DA: blendvpd xmm3, xmm2
    run(&mut cpu, &[0x66, 0x0F, 0x38, 0x15, 0xDA]).unwrap();

    assert_eq!(cpu.regs.xmm[3], (40_u128 << 64) | 10);
}
