// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A form whose value is only tested contributes its primary value alone. An
//! IF, WHEN, UNLESS or COND test, or a non-final AND/OR form, left its other
//! values pending, so a branch yielding a single value returned them too:
//! `(if (values x 99) x (list x))` gave X and 99, and compiled UNLESS even
//! answered the test's value instead of NIL. The expected results are SBCL's.

use std::process::Command;

const PROGRAM: &str = r#"
(defun tv-two (x) (values x 99))
(defun tv-if (x) (if (tv-two x) x (list x)))
(defun tv-when (x) (when (tv-two x) (list x)))
(defun tv-unless (x) (unless (tv-two x) (list x)))
(defun tv-and (x) (and (tv-two x) (list x)))
(defun tv-or (x) (or (tv-two x) 7))
(defun tv-cond (x) (cond ((tv-two x) x) (t (list x))))
(defun tv-final-and (x) (and t (tv-two x)))
(defun tv-final-or (x) (or nil (tv-two x)))
(defun tv-report ()
  (list (multiple-value-list (tv-if 5)) (multiple-value-list (tv-when nil))
        (multiple-value-list (tv-unless 5)) (multiple-value-list (tv-and nil))
        (multiple-value-list (tv-or nil)) (multiple-value-list (tv-cond 5))
        (multiple-value-list (tv-final-and 3)) (multiple-value-list (tv-final-or 4))))
(dotimes (i 300) (tv-report))
(format t "~&TEST-VALUES ~S~%" (tv-report))
"#;

const EXPECTED: &str = "TEST-VALUES ((5) (NIL) (NIL) (NIL) (7) (5) (3 99) (4 99))";

fn check(tier: &str) {
    let output = Command::new("timeout")
        .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", PROGRAM])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{tier}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains(EXPECTED), "{tier}: {stdout}");
}

#[test]
fn tested_forms_return_one_value_interpreted() {
    check("interp");
}

#[test]
fn tested_forms_return_one_value_t0() {
    check("t0");
}

#[test]
fn tested_forms_return_one_value_t1() {
    check("t1");
}

#[test]
fn tested_forms_return_one_value_t2() {
    check("t2");
}
