// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check_typep_values(tier: &str, expected_tier: u8) {
    let program = format!(
        r#"
        (defun mv-string-p (x) (values 1 2) (typep x 'string))
        (defun mv-symbol-p (x) (values 1 2) (typep x 'symbol))
        (defun mv-null-p (x) (values) (typep x 'null))
        (defun mv-operand-p (x) (typep (values x :extra) 'symbol))
        (defun mv-constant-p () (values 1 2) (typep "x" 'string))
        (dotimes (i 30)
          (mv-string-p "x") (mv-symbol-p :yes) (mv-null-p nil)
          (mv-operand-p :yes) (mv-constant-p))
        (dolist (name '(mv-symbol-p mv-null-p mv-operand-p))
          (format t "TIER ~a ~a~%" name (egcl-ext:function-tier name))
          (assert (= (egcl-ext:function-tier name) {expected_tier})))
        (assert (equal (multiple-value-list (mv-string-p "x")) '(t)))
        (assert (equal (multiple-value-list (mv-string-p 42)) '(nil)))
        (assert (equal (multiple-value-list (mv-symbol-p :yes)) '(t)))
        (assert (equal (multiple-value-list (mv-symbol-p 42)) '(nil)))
        (assert (equal (multiple-value-list (mv-null-p nil)) '(t)))
        (assert (equal (multiple-value-list (mv-null-p 42)) '(nil)))
        (assert (equal (multiple-value-list (mv-operand-p :yes)) '(t)))
        (assert (equal (multiple-value-list (mv-operand-p 42)) '(nil)))
        (assert (equal (multiple-value-list (mv-constant-p)) '(t)))
        (format t "TYPEP-SINGLE-VALUE-OK~%")
        "#
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", tier)
        .env_remove("EGCL_T2_NO_QUEUE")
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("run TYPEP multiple-values regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    assert!(stdout.contains("TYPEP-SINGLE-VALUE-OK"), "{stdout}");
}

#[test]
fn typep_returns_one_value_in_t0() {
    check_typep_values("t0", 0);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn typep_returns_one_value_in_t1() {
    check_typep_values("t1", 1);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn typep_returns_one_value_in_t2() {
    check_typep_values("t2", 2);
}
