// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

fn run(program: &str) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "120",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .env("EGCL_NATIVE_TRANSFER", "1")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE-APPLY-OK"));
}

#[test]
fn native_apply_preserves_prefix_arguments_and_exact_closure_captures() {
    run(r#"
      (defun apply-tail (f args) (apply f args))
      (defun apply-prefix (f x args) (apply f x args))
      (defun make-apply-callback (seed)
        (lambda (&optional (a 0) (b 0) (c 0) (d 0))
          (values (+ seed a b c d) seed)))
      (let ((first (make-apply-callback 10)) (second (make-apply-callback 20)))
        (dotimes (i 40)
          (assert (equal '(10 10) (multiple-value-list (apply-tail first nil))))
          (assert (equal '(30 20) (multiple-value-list (apply-prefix second 1 (list 2 3 4)))))))
      (disassemble #'apply-prefix)
      (format t "NATIVE-APPLY-OK~%")
    "#);
}

#[test]
fn redefining_apply_replaces_warm_native_forwarding() {
    run(r#"
      (defun apply-caller (f args) (apply f args))
      (defun apply-target (x) (+ x 1))
      (dotimes (i 40) (assert (= 42 (apply-caller #'apply-target (list 41)))))
      (defun apply (f args) (declare (ignore f)) (+ (car args) 100))
      (dotimes (i 40) (assert (= 141 (apply-caller #'apply-target (list 41)))))
      (format t "NATIVE-APPLY-OK~%")
    "#);
}
