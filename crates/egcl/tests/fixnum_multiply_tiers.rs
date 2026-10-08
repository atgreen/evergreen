// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `*` on fixnums must give the same answer at every tier, including the
//! products that leave the fixnum range (a bignum) and the operands that are
//! not fixnums at all (a ratio, a float), which a speculating tier must deopt
//! on rather than compute wrongly. On s390x T1 called the numeric runtime for
//! every `*` until it had a full-width overflow check, so a bignum product
//! never deoptimized there (t1_deopt's deopt_count probe).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

#[test]
fn fixnum_products_agree_across_tiers() {
    let program = "(progn \
        (defun mul (a b) (* a b)) \
        (dotimes (i 40) (mul 3 4)) \
        (format t \"RESULT ~S ~A~%\" \
          (list (mul 3 4) (mul 3 -4) (mul -5 -6) (mul 0 -7) \
                (mul 1073741824 1073741824) (mul most-positive-fixnum 2) \
                (mul most-negative-fixnum -1) (mul 1/2 4) (mul 2.5 2) (mul 3 1/3)) \
          (egcl-ext:function-tier (quote mul))))";
    let expected = "(12 -12 30 0 1152921504606846976 2305843009213693950 \
1152921504606846976 2 5.0 1)";
    for (tier, expected_tier) in [("t0", "0"), ("t1", "1"), ("t2", "2")] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "egcl failed at {tier}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = stdout
            .lines()
            .find(|l| l.starts_with("RESULT"))
            .unwrap_or("");
        // The tier reached is a property of the emitters, not of the answer;
        // the answer is asserted everywhere, the tier where `*` is inlined.
        assert!(
            line.starts_with(&format!("RESULT {expected} ")),
            "tier {tier}: {line:?}"
        );
        if cfg!(any(target_arch = "x86_64", target_arch = "s390x")) {
            assert_eq!(line, format!("RESULT {expected} {expected_tier}"));
        }
    }
}
