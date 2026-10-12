// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bignum::{BigInt, BigRat, big_add, big_neg, big_sub, mag_set_bit};

fn power(bit: usize) -> BigInt {
    let mut limbs = Vec::new();
    mag_set_bit(&mut limbs, bit);
    BigInt::from_mag(1, limbs)
}

fn ratio(num: BigInt, den: BigInt) -> BigRat {
    BigRat::new(num, den)
}

#[test]
fn huge_components_do_not_create_nan_or_false_zero() {
    let huge = power(2048);
    let near_one = ratio(
        big_add(&huge, &BigInt::one()),
        big_sub(&huge, &BigInt::one()),
    );
    assert_eq!(near_one.to_f64(), 1.0);
    assert_eq!(ratio(power(1024), power(1074)).to_f64(), 2.0f64.powi(-50));
    assert_eq!(
        ratio(big_add(&power(1024), &BigInt::one()), power(1074)).to_f64(),
        2.0f64.powi(-50)
    );
    assert_eq!(
        ratio(BigInt::one(), power(1024)).to_f64().to_bits(),
        1 << 50
    );
}

#[test]
fn rational_rounding_uses_nearest_even_including_subnormals() {
    let denominator = power(53);
    assert_eq!(
        ratio(big_add(&denominator, &BigInt::one()), denominator.clone()).to_f64(),
        1.0
    );
    assert_eq!(
        ratio(big_add(&denominator, &BigInt::from_i64(3)), denominator)
            .to_f64()
            .to_bits(),
        1.0f64.to_bits() + 2
    );
    assert_eq!(ratio(BigInt::one(), power(1074)).to_f64().to_bits(), 1);
    assert_eq!(ratio(BigInt::one(), power(1075)).to_f64().to_bits(), 0);
    assert_eq!(
        ratio(BigInt::from_i64(3), power(1075)).to_f64().to_bits(),
        2
    );
    // Just above the halfway point must not double-round to zero.
    assert_eq!(
        ratio(big_add(&power(1024), &BigInt::one()), power(2099))
            .to_f64()
            .to_bits(),
        1
    );
    // Halfway between the largest subnormal and the smallest normal.
    assert_eq!(
        ratio(big_sub(&power(53), &BigInt::one()), power(1075))
            .to_f64()
            .to_bits(),
        f64::MIN_POSITIVE.to_bits()
    );
}

#[test]
fn finite_overflow_boundary_and_negative_underflow_keep_ieee_values() {
    let largest = big_sub(&power(1024), &power(971));
    assert_eq!(BigRat::from_bigint(largest).to_f64(), f64::MAX);
    let below_midpoint = big_sub(&big_sub(&power(1024), &power(970)), &BigInt::one());
    assert_eq!(BigRat::from_bigint(below_midpoint).to_f64(), f64::MAX);
    let midpoint = big_sub(&power(1024), &power(970));
    assert_eq!(BigRat::from_bigint(midpoint).to_f64(), f64::INFINITY);
    assert_eq!(
        BigRat::from_bigint(big_neg(&power(2048))).to_f64(),
        f64::NEG_INFINITY
    );
    assert_eq!(
        ratio(BigInt::from_i64(-1), power(1075)).to_f64().to_bits(),
        (-0.0f64).to_bits()
    );
    assert_eq!(
        ratio(BigInt::from_i64(-1), power(1074)).to_f64().to_bits(),
        (1u64 << 63) | 1
    );
}

#[test]
fn ordinary_ratios_and_zero_preserve_expected_values() {
    for (num, den) in [(1, 3), (-22, 7), (17, 5), (0, 9)] {
        assert_eq!(
            ratio(BigInt::from_i64(num), BigInt::from_i64(den)).to_f64(),
            num as f64 / den as f64
        );
    }
}

#[test]
fn exact_float_rationals_round_trip_across_exponents_and_signs() {
    let mut state = 0x91d6_7b4a_22c5_31efu64;
    for _ in 0..2048 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bits = state;
        let biased = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & ((1 << 52) - 1);
        if biased == 0x7ff || (biased == 0 && fraction == 0) {
            continue;
        }
        let mantissa = if biased == 0 {
            fraction
        } else {
            fraction | (1 << 52)
        };
        let exponent = if biased == 0 {
            -1074
        } else {
            biased - 1023 - 52
        };
        let mut numerator = Vec::new();
        for bit in 0..53 {
            if mantissa & (1 << bit) != 0 {
                mag_set_bit(&mut numerator, bit + exponent.max(0) as usize);
            }
        }
        let numerator = BigInt::from_mag(if bits >> 63 != 0 { -1 } else { 1 }, numerator);
        let denominator = if exponent < 0 {
            power((-exponent) as usize)
        } else {
            BigInt::one()
        };
        assert_eq!(
            ratio(numerator, denominator).to_f64().to_bits(),
            bits,
            "{bits:016x}"
        );
    }
}
