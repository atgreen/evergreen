// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn run(program: &str, tier: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .expect("run raw-error trace probe")
}

#[test]
fn raw_interpreted_errors_retain_the_failing_activation() {
    for tier in ["interp", "t0"] {
        for failure in ["missing-variable", "(car value)"] {
            let program = format!(
                "(defun raw-leaf (value) {failure})
                 (defparameter *raw-leaf* #'raw-leaf)
                 (defun raw-outer (value) (funcall *raw-leaf* value))
                 (raw-outer 42)"
            );
            let output = run(&program, tier);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(1), "{tier}: {stderr}");
            for name in ["RAW-LEAF 42)", "RAW-OUTER 42)"] {
                assert_eq!(stderr.matches(name).count(), 1, "{tier}: {stderr}");
            }
            assert!(stderr.find("RAW-LEAF").unwrap() < stderr.find("RAW-OUTER").unwrap());
        }
    }
}

#[test]
fn raw_error_handlers_run_once_before_dynamic_unwind() {
    let program = r#"
        (defvar *raw-state* :outside)
        (defvar *raw-events* nil)
        (defun raw-dynamic-leaf (value)
          (let ((*raw-state* :inside))
            (unwind-protect (car value)
              (push :cleanup *raw-events*))))
        (assert
          (eq :caught
            (handler-case
              (handler-bind ((type-error (lambda (condition)
                                           (declare (ignore condition))
                                           (push *raw-state* *raw-events*))))
                (raw-dynamic-leaf 42))
              (type-error () :caught))))
        (assert (equal *raw-events* '(:cleanup :inside)))
        (assert (eq *raw-state* :outside))
        (format t "DYNAMIC-HANDLER-OK~%")
    "#;
    for tier in ["interp", "t0"] {
        let output = run(program, tier);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(stdout.contains("DYNAMIC-HANDLER-OK"), "{tier}: {stdout}");
        assert!(!stderr.contains("Backtrace"), "{tier}: {stderr}");
    }
}

#[test]
fn raw_error_handler_can_use_a_restart_before_cleanup() {
    let program = r#"
        (defvar *raw-cleanups* 0)
        (defun raw-restart-leaf (value)
          (restart-case
            (unwind-protect (car value) (incf *raw-cleanups*))
            (recover () :recovered)))
        (assert
          (eq :recovered
            (handler-bind ((type-error (lambda (condition)
                                         (declare (ignore condition))
                                         (assert (= 0 *raw-cleanups*))
                                         (invoke-restart 'recover))))
              (raw-restart-leaf 42))))
        (assert (= 1 *raw-cleanups*))
        (assert (= 7 (catch 'done (throw 'done 7))))
        (assert (= 9 (block done (return-from done 9))))
        (format t "RAW-RESTART-OK~%")
    "#;
    for tier in ["interp", "t0"] {
        let output = run(program, tier);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(stdout.contains("RAW-RESTART-OK"), "{tier}: {stdout}");
        assert!(!stderr.contains("Backtrace"), "{tier}: {stderr}");
    }
}

#[test]
fn speculative_compile_seeding_does_not_signal_user_handlers() {
    let directory =
        std::env::temp_dir().join(format!("egcl-seeding-errors-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("seed.lisp");
    let fasl = directory.join("seed.fasl");
    std::fs::write(
        &source,
        r#"
      (eval-when (:compile-toplevel :load-toplevel :execute)
        (defun seed-error-helper () 42)
        (defvar *seed-error-value* (seed-error-helper)))
    "#,
    )
    .unwrap();
    let program = format!(
        r#"
      (defvar *seed-error-events* nil)
      (handler-bind ((error (lambda (condition)
                             (push (type-of condition) *seed-error-events*))))
        (multiple-value-bind (path warnings failure)
            (compile-file {source:?} :output-file {fasl:?})
          (declare (ignore warnings))
          (assert path) (assert (not failure))))
      (assert (null *seed-error-events*))
      (assert (= 42 *seed-error-value*))
      ;; Successful probing must restore normal pre-unwind signalling.
      (let ((events nil))
        (handler-case
            (handler-bind ((type-error (lambda (condition)
                                         (declare (ignore condition))
                                         (push :handler events))))
              (unwind-protect (car 42) (push :cleanup events)))
          (type-error () nil))
        (assert (equal events '(:cleanup :handler))))
      (format t "SEED-HANDLERS-OK~%")
    "#
    );
    let output = run(&program, "interp");
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("SEED-HANDLERS-OK"));
}
