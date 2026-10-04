// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn terminal_error_retains_its_own_pre_unwind_chain() {
    for (tier, number) in [("interp", 0), ("t0", 0), ("t1", 1), ("t2", 2)] {
        // Dynamic calls keep three physical activations in this test. Inline
        // frame reconstruction remains compiler debug-map coverage.
        let program = format!(
            r#"
          (defun previous-handled-error () (error "Already handled"))
          (handler-case (previous-handled-error) (error () nil))
          (defun bt-leaf (value fail) (if fail (error "Bad value ~D" value) value))
          (defparameter *bt-leaf* #'bt-leaf)
          (defun bt-middle (value fail) (funcall *bt-leaf* (+ value 1) fail))
          (defparameter *bt-middle* #'bt-middle)
          (defun bt-outer (value fail) (funcall *bt-middle* value fail))
          (dotimes (i 100) (assert (= 42 (bt-outer 41 nil))))
          (assert (= {number} (egcl-ext:function-tier 'bt-leaf)))
          (assert (= {number} (egcl-ext:function-tier 'bt-middle)))
          (assert (= {number} (egcl-ext:function-tier 'bt-outer)))
          (format t "TIERS-VERIFIED~%")
          (unwind-protect (bt-outer 41 t)
            (handler-case (previous-handled-error) (error () nil))
            (make-list 100 :initial-element "cleanup allocation"))
        "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env_remove("EGCL_DISABLE_T2")
            .env_remove("EGCL_T2_DISCARD")
            .output()
            .expect("run pre-unwind trace probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("TIERS-VERIFIED"),
            "{tier}: {stdout}\n{stderr}"
        );
        assert!(stderr.contains("Bad value 42"), "{tier}: {stderr}");
        assert!(
            !stderr.contains("PREVIOUS-HANDLED-ERROR"),
            "{tier}: {stderr}"
        );
        let mut previous = 0;
        for name in ["BT-LEAF", "BT-MIDDLE", "BT-OUTER"] {
            assert_eq!(stderr.matches(name).count(), 1, "{tier}: {stderr}");
            let position = stderr.find(name).unwrap();
            assert!(position > previous, "{tier}: {stderr}");
            previous = position;
        }
        if number == 0 {
            assert!(stderr.contains("BT-LEAF 42 T)"), "{tier}: {stderr}");
            assert!(stderr.contains("BT-MIDDLE 41 T)"), "{tier}: {stderr}");
            assert!(stderr.contains("BT-OUTER 41 T)"), "{tier}: {stderr}");
        }
    }
}

#[test]
fn handled_error_does_not_print_a_backtrace_or_run_its_report() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defvar *reported* nil)
          (define-condition quiet-error (error) ()
            (:report (lambda (condition stream)
                       (declare (ignore condition stream)) (setq *reported* t))))
          (assert (eq :caught (handler-case (error 'quiet-error) (error () :caught))))
          (assert (not *reported*))
          (format t "HANDLED-OK~%")
        "#,
        ])
        .output()
        .expect("run handled-error probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("HANDLED-OK"), "{stdout}\n{stderr}");
    assert!(!stderr.contains("Backtrace"), "{stderr}");
}

#[test]
fn failing_argument_printer_does_not_replace_the_terminal_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defclass broken-printer () ())
          (defmethod print-object ((object broken-printer) stream)
            (declare (ignore object stream)) (error "Secondary printer failure"))
          (defun failed-with-object (object) (error "Original terminal failure"))
          (failed-with-object (make-instance 'broken-printer))
        "#,
        ])
        .env("EGCL_FORCE_TIER", "interp")
        .output()
        .expect("run printer-failure probe");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("Original terminal failure"), "{stderr}");
    assert!(stderr.contains("FAILED-WITH-OBJECT #<"), "{stderr}");
    assert!(stderr.contains("BROKEN-PRINTER"), "{stderr}");
    assert!(!stderr.contains("Secondary printer failure"), "{stderr}");
}

#[test]
fn anonymous_interpreted_call_retains_its_arguments() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            "(funcall (lambda (value) (error \"anonymous failure\")) 42)",
        ])
        .env("EGCL_FORCE_TIER", "interp")
        .output()
        .expect("run anonymous trace");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("(<anonymous function> 42)"), "{stderr}");
}
