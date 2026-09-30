// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bignum::{BigInt, FIXNUM_MIN};
use egcl_rt::error::EgclError;
use egcl_rt::value::EgclVal;
use egcl_stdlib::numbers::ash;

#[test]
fn direct_shift_kernel_boundaries_and_allocation() {
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
