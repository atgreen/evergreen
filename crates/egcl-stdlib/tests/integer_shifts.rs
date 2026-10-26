// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bignum::{BigInt, FIXNUM_MIN};
use egcl_rt::error::EgclError;
use egcl_rt::value::EgclVal;
use egcl_stdlib::numbers::ash;

// Heap allocation statistics are process-wide, so allocation assertions from
// these tests must not race each other's construction of bignum inputs.
static SHIFT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn direct_shift_kernel_boundaries_and_allocation() {
    let _lock = SHIFT_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let before = egcl_rt::heap_stats().bytes_allocated;
    for n in -1000..1000 {
        for count in -65..10 {
            let actual = ash(EgclVal::from_fixnum(n), EgclVal::from_fixnum(count)).unwrap();
            let expected = if count < 0 {
                n >> (-count).min(63)
            } else {
                n << count
            };
            assert_eq!(actual.as_fixnum(), expected);
        }
    }
    assert_eq!(
        egcl_rt::heap_stats().bytes_allocated,
        before,
        "fixnum shifts must not allocate Lisp temporaries"
    );
    assert_eq!(
        ash(
            EgclVal::from_fixnum(FIXNUM_MIN),
            EgclVal::from_fixnum(FIXNUM_MIN)
        )
        .unwrap()
        .as_fixnum(),
        -1
    );

    // Neither gigantic positive counts nor operand validation can be skipped
    // accidentally. Reject an unrepresentable nonzero result without trying to
    // reserve an astronomical vector; zero remains exactly zero.
    let huge = BigInt::from_mag(1, vec![0, 1]).to_val();
    egcl_rt::rooted!(huge = huge);
    assert!(matches!(
        ash(EgclVal::from_fixnum(1), *huge),
        Err(EgclError::Oom)
    ));
    assert_eq!(ash(EgclVal::from_fixnum(0), *huge).unwrap().as_fixnum(), 0);
    assert!(matches!(
        ash(EgclVal::from_fixnum(0), EgclVal::from_char('x')),
        Err(EgclError::TypeError { .. })
    ));
}

#[test]
fn one_limb_right_shift_leaf_matches_signed_arithmetic_without_allocation() {
    let _lock = SHIFT_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    use egcl_rt::bignum::{FIXNUM_MAX, try_small_integer_right_shift};
    // Root the input immediately: later test cases allocate new bignums.
    for magnitude in [1u64 << 60, (1u64 << 63) - 1, 1u64 << 63, u64::MAX] {
        for sign in [-1, 1] {
            egcl_rt::rooted!(input = BigInt::from_mag(sign, vec![magnitude]).to_val());
            let before = egcl_rt::heap_stats().bytes_allocated;
            for bits in [1u64, 2, 3, 4, 8, 60, 63, 64, 65, u64::MAX] {
                let integer = magnitude as i128 * sign as i128;
                let expected = integer >> bits.min(127);
                let actual = try_small_integer_right_shift(*input, bits);
                if (FIXNUM_MIN as i128..=FIXNUM_MAX as i128).contains(&expected) {
                    assert_eq!(actual.unwrap().as_fixnum() as i128, expected);
                    let count = if bits > (1u64 << 60) {
                        FIXNUM_MIN
                    } else {
                        -(bits as i64)
                    };
                    assert_eq!(
                        ash(*input, EgclVal::from_fixnum(count)).unwrap(),
                        actual.unwrap()
                    );
                } else {
                    assert!(actual.is_none(), "non-fixnum result must retain fallback");
                }
            }
            assert_eq!(egcl_rt::heap_stats().bytes_allocated, before);
        }
    }
    egcl_rt::rooted!(large = BigInt::from_mag(1, vec![0, 1]).to_val());
    assert!(try_small_integer_right_shift(*large, 8).is_none());
    assert_eq!(
        ash(*large, EgclVal::from_fixnum(-8)).unwrap().as_fixnum(),
        1i64 << 56
    );
    assert!(matches!(
        ash(*large, EgclVal::from_char('x')),
        Err(EgclError::TypeError { .. })
    ));
    assert!(matches!(
        ash(EgclVal::from_char('x'), EgclVal::from_fixnum(-8)),
        Err(EgclError::TypeError { .. })
    ));
}
