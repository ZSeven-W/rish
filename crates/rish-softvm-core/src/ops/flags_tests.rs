use super::*;

#[test]
fn subtract_with_borrow_matches_a_wide_unsigned_oracle_at_limb_boundaries() {
    for bits in [8, 16, 32, 64] {
        let mask = bits_mask(bits);
        let values = [0, 1, mask / 2, mask - 1, mask];
        for left in values {
            for right in values {
                for borrow in [false, true] {
                    let wide_right = u128::from(right) + u128::from(borrow);
                    let expected_borrow = u128::from(left) < wide_right;
                    let expected = u128::from(left).wrapping_sub(wide_right) as u64 & mask;
                    let result = sub_with_flags(left, right, borrow, bits);
                    assert_eq!(result.result & mask, expected);
                    assert_eq!(
                        result.carry, expected_borrow,
                        "width={bits} left={left:#x} right={right:#x} borrow={borrow}"
                    );
                }
            }
        }
    }
}

#[test]
fn parity_uses_only_the_low_byte_while_sign_and_zero_use_the_full_width() {
    let mut regs = Registers::default();
    regs.rflags |= RFlags::CF;
    for (value, parity, sign, zero) in [
        (0x100, true, false, false),
        (0x101, false, false, false),
        (1_u64 << 63, true, true, false),
        (0, true, false, true),
    ] {
        set_szp(&mut regs, value, 64);
        assert_eq!(regs.rflags.contains(RFlags::PF), parity);
        assert_eq!(regs.rflags.contains(RFlags::SF), sign);
        assert_eq!(regs.rflags.contains(RFlags::ZF), zero);
        assert!(regs.rflags.contains(RFlags::CF));
    }
}
