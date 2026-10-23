// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A function containing LOAD-TIME-VALUE compiles in the running image, its
//! forms evaluated once when it is compiled (bliss-jz86). The bytecode lowerer
//! could not publish such a function outside COMPILE-FILE, so it stayed
//! tree-walked and re-evaluated each form on every call: CL-UNICODE rebuilt a
//! literal cl-ppcre scanner per call. COMPILE-FILE still defers the forms to
//! the fasl's load.
use std::path::PathBuf;
use std::process::Command;

fn run(tier: &str, program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{tier}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

const DEFINITIONS: &str = r#"
  (defvar *n* 0)
  (defun ltv () (load-time-value (progn (incf *n*) (list :cell))))
  (defun ltv-table (x) (cons x (load-time-value (make-hash-table))))
  (defun ltv-nested () (mapcar (lambda (x) (cons x (load-time-value (list :inner)))) '(1 2)))
  (defun ltv-bad () (load-time-value (error "initializer failed")))"#;

#[test]
fn forced_compilation_evaluates_each_form_once() {
    for tier in ["t0", "t2"] {
        let stdout = run(
            tier,
            &format!(
                r#"(progn {DEFINITIONS}
                     (let ((a (ltv)))
                       (dotimes (i 30) (ltv) (ltv-table i) (ltv-nested))
                       (format t "~&RESULT ~s~%"
                               (list *n* (eq a (ltv))
                                     (eq (cdr (ltv-table 1)) (cdr (ltv-table 2)))
                                     (let ((r (ltv-nested))) (eq (cdr (first r)) (cdr (second r))))
                                     (handler-case (ltv-bad) (error () :signalled)))))
                     (disassemble 'ltv))"#
            ),
        );
        assert!(stdout.contains("RESULT (1 T T T :SIGNALLED)"), "{tier}: {stdout}");
        assert!(stdout.contains("LTV —"), "{tier}: {stdout}");
    }
}

#[test]
fn a_tiered_up_function_keeps_one_value() {
    let stdout = run(
        "",
        &format!(
            r#"(progn {DEFINITIONS}
                 (dotimes (i 50) (ltv))
                 (let ((n *n*))
                   (format t "~&RESULT ~s~%" (list (eq (ltv) (ltv)) (= n *n*)))))"#
        ),
    );
    assert!(stdout.contains("RESULT (T T)"), "{stdout}");
}

#[test]
fn compile_file_still_defers_load_time_values_to_load() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("live-load-time-value-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("ltv.lisp");
    std::fs::write(
        &source,
        "(defvar *loads* 0)\n(defun from-fasl () (load-time-value (incf *loads*)))\n",
    )
    .unwrap();
    let stdout = run(
        "",
        &format!(
            r#"(progn (compile-file "{}")
                 (format t "~&COMPILED ~s~%" (boundp '*loads*))
                 (load "{}")
                 (format t "~&LOADED ~s~%" (list (from-fasl) (from-fasl) *loads*)))"#,
            source.display(),
            dir.join("ltv.fasl").display()
        ),
    );
    assert!(stdout.contains("COMPILED NIL"), "{stdout}");
    assert!(stdout.contains("LOADED (1 1 1)"), "{stdout}");
}
