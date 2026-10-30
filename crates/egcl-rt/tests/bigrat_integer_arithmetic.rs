// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bignum::{
    BigInt, BigRat, bigrat_add, bigrat_cmp, bigrat_div, bigrat_mul, bigrat_neg, bigrat_sub,
};
use std::cmp::Ordering;

fn expect(value: &BigRat, sign: i8, numerator: &[u64], denominator: &[u64]) {
    assert_eq!(value.num.sign, sign);
    assert_eq!(value.num.mag, numerator);
    assert_eq!(value.den.sign, 1);
    assert_eq!(value.den.mag, denominator);
}

#[test]
fn large_integer_arithmetic_preserves_exact_results_in_every_sign() {
    // (2^128 - 1) + (2^128 + 1) = 2^129
    // (2^128 - 1) * (2^128 + 1) = 2^256 - 1
    for a_sign in [-1, 1] {
        for b_sign in [-1, 1] {
            let a = BigRat::from_bigint(BigInt::from_mag(a_sign, vec![u64::MAX; 2]));
            let b = BigRat::from_bigint(BigInt::from_mag(b_sign, vec![1, 0, 1]));
            let sum = bigrat_add(&a, &b);
            let difference = bigrat_sub(&a, &b);
            if a_sign == b_sign {
                expect(&sum, a_sign, &[0, 0, 2], &[1]);
                expect(&difference, -a_sign, &[2], &[1]);
            } else {
                expect(&sum, b_sign, &[2], &[1]);
                expect(&difference, a_sign, &[0, 0, 2], &[1]);
            }
            expect(&bigrat_mul(&a, &b), a_sign * b_sign, &[u64::MAX; 4], &[1]);
            let order = if a_sign != b_sign {
                a_sign.cmp(&b_sign)
            } else if a_sign > 0 {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            assert_eq!(bigrat_cmp(&a, &b), order);
            assert_eq!(bigrat_cmp(&a, &a), Ordering::Equal);
            assert!(sum.is_integer() && difference.is_integer());
        }
    }
}

#[test]
fn integer_cancellation_zero_and_negative_unit_denominators_are_canonical() {
    for sign in [-1, 1] {
        let number = BigInt::from_mag(sign, vec![7, 0, 0, 1]);
        let integer = BigRat::new(number.clone(), BigInt::one());
        expect(&integer, sign, &[7, 0, 0, 1], &[1]);
        let negated = BigRat::new(number, BigInt::from_i64(-1));
        expect(&negated, -sign, &[7, 0, 0, 1], &[1]);
        let zero = BigRat::from_i64(0);
        for result in [
            bigrat_add(&integer, &bigrat_neg(&integer)),
            bigrat_sub(&integer, &integer),
            bigrat_mul(&integer, &zero),
            bigrat_mul(&zero, &integer),
        ] {
            expect(&result, 0, &[], &[1]);
            assert!(result.is_integer());
        }
        assert_eq!(bigrat_cmp(&integer, &zero), sign.cmp(&0));
        assert_eq!(bigrat_cmp(&zero, &integer), 0.cmp(&sign));
    }
    for denominator in [1, -1, -9] {
        expect(
            &BigRat::new(BigInt::zero(), BigInt::from_i64(denominator)),
            0,
            &[],
            &[1],
        );
    }
}

#[test]
fn noninteger_and_mixed_rational_neighbors_still_reduce_and_compare() {
    let negative_three_quarters = BigRat::new(BigInt::from_i64(6), BigInt::from_i64(-8));
    expect(&negative_three_quarters, -1, &[3], &[4]);
    assert!(!negative_three_quarters.is_integer());
    let two = BigRat::from_i64(2);
    expect(&bigrat_add(&two, &negative_three_quarters), 1, &[5], &[4]);
    expect(&bigrat_sub(&two, &negative_three_quarters), 1, &[11], &[4]);
    expect(&bigrat_sub(&negative_three_quarters, &two), -1, &[11], &[4]);
    expect(&bigrat_mul(&two, &negative_three_quarters), -1, &[3], &[2]);
    expect(&bigrat_div(&two, &negative_three_quarters), -1, &[8], &[3]);
    assert_eq!(
        bigrat_cmp(&two, &negative_three_quarters),
        Ordering::Greater
    );
    assert_eq!(bigrat_cmp(&negative_three_quarters, &two), Ordering::Less);
    let half = BigRat::new(BigInt::one(), BigInt::from_i64(2));
    expect(&bigrat_add(&half, &half), 1, &[1], &[1]);
    expect(&bigrat_sub(&half, &half), 0, &[], &[1]);
    // Common factors span multiple limbs: (6 * 2^128)/(8 * 2^128) = 3/4.
    let large_ratio = BigRat::new(
        BigInt::from_mag(1, vec![0, 0, 6]),
        BigInt::from_mag(1, vec![0, 0, 8]),
    );
    expect(&large_ratio, 1, &[3], &[4]);
    assert_eq!(
        bigrat_cmp(&large_ratio, &bigrat_neg(&negative_three_quarters)),
        Ordering::Equal
    );
}
