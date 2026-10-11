// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{program}\n{stdout}\n{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(
        stdout.lines().any(|line| line == "APROPOS-PASS"),
        "{stdout}"
    );
}

#[test]
fn apropos_entry_points_signal_program_error_for_wrong_arity() {
    for name in ["apropos", "apropos-list"] {
        let forms = format!(
            "(({name}) ({name} \"x\" :cl nil)
               (funcall #'{name}) (funcall #'{name} \"x\" :cl nil)
               (apply #'{name} nil) (apply #'{name} '(\"x\" :cl nil)))"
        );
        check(&format!(
            "(progn (dolist (form '{forms})
                (assert (eq :program-error
                  (handler-case (progn (eval form) :no-error)
                    (program-error () :program-error)))))
               (format t \"APROPOS-PASS~%\"))"
        ));
    }
}

#[test]
fn apropos_prints_special_symbols_and_returns_no_values() {
    check(
        r#"(progn
        (defpackage :apropos-special-probe (:use) (:import-from :cl :nil :t))
        (let ((symbols (apropos-list "" :apropos-special-probe)))
          (assert (= 2 (length symbols)))
          (assert (member nil symbols))
          (assert (member t symbols)))
        (let ((text (with-output-to-string (*standard-output*)
                     (assert (null (multiple-value-list
                       (apropos "" :apropos-special-probe)))))))
          (assert (search "NIL (value)" text))
          (assert (search "T (value)" text)))
        (format t "APROPOS-PASS~%"))"#,
    );
}
