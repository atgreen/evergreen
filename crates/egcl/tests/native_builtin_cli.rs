// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", unix))]
use std::process::Command;

#[test]
fn native_builtins_preserve_values_allocating_arguments_and_errors() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
      (defun builtin-code (x) (char-code x))
      (defun builtin-values (a b c d) (values a b c d))
      (defun builtin-reverse (x) (reverse x))
      (dotimes (i 40)
        (let ((input (list i (+ i 1) (+ i 2))))
          (assert (equal (multiple-value-list (builtin-values input 2 3 4))
                         (list input 2 3 4)))
          (assert (equal (multiple-value-list (builtin-code #\A)) '(65)))
          (assert (equal (builtin-reverse input) (list (+ i 2) (+ i 1) i)))))
      (assert (= 2 (egcl-ext:function-tier 'builtin-code)))
      (assert (= 2 (egcl-ext:function-tier 'builtin-values)))
      (assert (= 2 (egcl-ext:function-tier 'builtin-reverse)))
      (egcl-ext:gc :full t)
      (dotimes (i 3)
        (assert (handler-case (progn (builtin-code (list :bad)) nil)
                  (type-error () t)))
        (assert (= (builtin-code #\B) 66))
        (assert (equal (multiple-value-list (builtin-values 1 2 3 4)) '(1 2 3 4))))
      (format t "NATIVE-BUILTIN-OK~%")
    "#,
        ])
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE-BUILTIN-OK"));
}
