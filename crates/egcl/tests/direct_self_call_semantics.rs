// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The direct self-call (T2 register entry) must not change what a program
//! means (bliss-w6aki). Two things it got wrong, both reproduced natively on
//! s390x and x86-64 before the fix:
//!
//! * a self-call with the wrong argument count jumped to the register entry
//!   and bound whatever the argument registers held, answering 0 where every
//!   other tier signals PROGRAM-ERROR;
//! * a guard failing at an inner level of the recursion recorded a resume
//!   state that run_native would only consume after every outer native level
//!   had continued with the deopt stub's NIL, so a multiplication overflowing
//!   to a bignum 60 levels deep answered TYPE-ERROR instead of the bignum.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str, tier: &str) -> String {
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .expect("run egcl");
    assert!(
        out.status.success(),
        "egcl failed at {tier}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|line| line.starts_with("RESULT"))
        .unwrap_or("")
        .to_string()
}

/// The warm-up never reaches the wrong-arity branch; the probe call does.
#[test]
fn wrong_arity_self_call_signals_program_error_at_every_tier() {
    let program = "(progn \
        (defun g (n) (cond ((<= n 0) 0) ((= n 1) (g 0 0)) (t (g (- n 2))))) \
        (dotimes (i 100) (g 4)) \
        (format t \"RESULT ~A ~A~%\" \
          (handler-case (g 1) (program-error () :program-error) (error (e) (type-of e))) \
          (egcl-ext:function-tier (quote g))))";
    for (tier, expected_tier) in [("t0", "0"), ("t1", "1"), ("t2", "2")] {
        let line = run(program, tier);
        assert_eq!(
            line,
            format!("RESULT PROGRAM-ERROR {expected_tier}"),
            "tier {tier}: a wrong-arity self-call must signal, not bind the registers"
        );
    }
}

/// `(pow2 70)` overflows the fixnum range at an inner level of the direct
/// recursion. Each tier must answer 2^70. The x86-64 half of the fix is still
/// open on bliss-w6aki, so the T2 row runs on s390x only for now.
#[test]
fn inner_overflow_inside_direct_recursion_returns_the_bignum() {
    let program = "(progn \
        (defun pow2 (n) (if (= n 0) 1 (* 2 (pow2 (1- n))))) \
        (dotimes (i 100) (pow2 10)) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"RESULT ~A ~A ~A~%\" \
            (handler-case (pow2 70) (error (e) (type-of e))) \
            (egcl-ext:function-tier (quote pow2)) \
            (if (> (egcl-ext:deopt-count) before) :deopted :no-deopt))))";
    let expected = "1180591620717411303424";
    for (tier, expected_tier) in [("t0", "0"), ("t1", "1")] {
        let line = run(program, tier);
        assert!(
            line.starts_with(&format!("RESULT {expected} {expected_tier}")),
            "tier {tier}: {line:?}"
        );
    }
    // Every native level above the overflow deopts in turn as the bignum
    // climbs, so the per-symbol threshold retires the T2 code by the end;
    // the tier afterwards is a policy, not part of this contract.
    if cfg!(target_arch = "s390x") {
        let line = run(program, "t2");
        assert!(
            line.starts_with(&format!("RESULT {expected} ")) && line.ends_with("DEOPTED"),
            "T2 must deopt at the overflow and still answer the bignum: {line:?}"
        );
    }
}
