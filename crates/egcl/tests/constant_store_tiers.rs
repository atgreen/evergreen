// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check_late_constant(tier: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", tier)
        .args([
            "--no-init",
            "--eval",
            r#"
              (defvar *after-late-store* 0)
              (defvar *late-store-cleanups* 0)
              (defun late-constant-store (x)
                (setq +late-constant+ x)
                (incf *after-late-store*)
                x)
              (dotimes (i 128) (late-constant-store 9))
              (setq *after-late-store* 0)
              (disassemble #'late-constant-store)
              (defconstant +late-constant+ 7)
              (format t "RESULT:~S~%"
                (list
                  (handler-case
                    (unwind-protect (late-constant-store 11)
                      (incf *late-store-cleanups*))
                    (program-error () :caught))
                  (symbol-value '+late-constant+)
                  *after-late-store*
                  *late-store-cleanups*))
            "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "tier={tier}: {stdout}\n{stderr}");
    if cfg!(target_arch = "x86_64") && tier != "interp" {
        let marker = format!("; {}", tier.to_ascii_uppercase());
        assert!(stdout.contains(&marker), "missing {marker}: {stdout}");
    }
    assert!(
        stdout.lines().any(|line| line == "RESULT:(:CAUGHT 7 0 1)"),
        "tier={tier}: {stdout}\n{stderr}"
    );
}

#[test]
fn interpreted_assignment_rejects_a_late_constant() {
    check_late_constant("interp");
}

#[test]
fn bytecode_assignment_rejects_a_late_constant() {
    check_late_constant("t0");
}

#[test]
fn baseline_native_assignment_rejects_a_late_constant() {
    check_late_constant("t1");
}

#[test]
fn optimizing_native_assignment_rejects_a_late_constant() {
    check_late_constant("t2");
}

fn check_definition_during_value_evaluation(operator: &str) {
    let program = format!(
        r#"
          (format t "RESULT:~S~%"
            (list
              (handler-case
                ({operator} +constant-during-rhs+
                  (progn (eval '(defconstant +constant-during-rhs+ 7)) 9))
                (program-error () :caught))
              (symbol-value '+constant-during-rhs+)))
        "#
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "interp")
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("RESULT:(:CAUGHT 7)"), "{stdout}\n{stderr}");
}

#[test]
fn interpreted_assignment_rechecks_after_evaluating_the_value() {
    check_definition_during_value_evaluation("setq");
}

#[test]
fn interpreted_setf_rechecks_after_evaluating_the_value() {
    check_definition_during_value_evaluation("setf");
}
