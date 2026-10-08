// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A native caller may enter a deopting T2 callee directly on s390x, because
//! the callee's deopt resumes T0 inline and hands back a finished value
//! (bliss-w6aki). The callee multiplies and so carries an overflow guard; the
//! probe call makes it fail, and the caller's own guard then fails on the
//! bignum. Both deopts must leave the answer right, and the compile trace must
//! show the call really went direct. On other targets only the answer is
//! checked.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

#[test]
fn direct_call_into_a_deopting_t2_callee_answers_correctly() {
    let program = "(progn \
        (defun leaf (x) (* 2 x)) \
        (defun caller (n) (+ 1 (leaf n))) \
        (dotimes (i 100) (leaf 5)) \
        (dotimes (i 100) (caller 5)) \
        (format t \"RESULT ~A ~A~%\" \
          (handler-case (caller most-positive-fixnum) (error (e) (type-of e))) \
          (caller 20)))";
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_LOG", "compile=trace")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "egcl failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    let line = stdout
        .lines()
        .find(|l| l.starts_with("RESULT"))
        .unwrap_or("");
    // 2 * most-positive-fixnum + 1 on a 60-bit fixnum.
    assert_eq!(line, "RESULT 2305843009213693951 41", "{stderr}");
    if cfg!(target_arch = "s390x") {
        assert!(
            stderr.contains("CALLER: direct call to LEAF [T2]"),
            "the deopting T2 callee must be called directly:\n{stderr}"
        );
    }
}
