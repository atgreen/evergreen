// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", unix))]

use std::process::Command;

#[test]
fn captured_callbacks_promote_preserve_shared_state_and_survive_gc() {
    let program = r#"
      (defun captured-counter (seed)
        (let ((hits 0))
          (lambda (delta)
            (setq hits (+ hits 1))
            (setq seed (+ seed delta))
            (values seed hits))))
      (defun captured-cell (value)
        (list (lambda (new) (setq value new))
              (lambda () value)))
      (defun captured-snapshot (value)
        (lambda (new)
          (let ((old value))
            (setq value new)
            (egcl-ext:gc)
            (values old value))))
      (format *error-output* "CAPTURE-T2-BEGIN~%")
      (let ((a (captured-counter 10)) (b (captured-counter 100)))
        (dotimes (i 40)
          (multiple-value-bind (value hits) (funcall a 2)
            (assert (= value (+ 12 (* i 2))))
            (assert (= hits (+ i 1)))))
        (assert (= (funcall b 3) 103))
        (multiple-value-bind (value hits) (funcall a 1152921504606846975)
          (assert (= value 1152921504606847065))
          (assert (= hits 41)))
        (multiple-value-bind (value hits) (funcall a 1)
          (assert (= value 1152921504606847066))
          (assert (= hits 42))))
      (let* ((pair (captured-cell (list 1 2)))
             (writer (car pair)) (reader (car (cdr pair))))
        (dotimes (i 40)
          (assert (equal (funcall writer (list i (+ i 1))) (list i (+ i 1))))
          (egcl-ext:gc)
          (assert (equal (funcall reader) (list i (+ i 1))))))
      (let ((f (captured-snapshot (list 0))))
        (dotimes (i 40)
          (multiple-value-bind (old new) (funcall f (list (+ i 1)))
            (assert (equal old (list i)))
            (assert (equal new (list (+ i 1)))))))
      (format *error-output* "CAPTURE-T2-END~%")
      (format t "CAPTURE-STATE-OK~%")
    "#;
    for tier in ["t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_LOG", "stderr")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(stdout.contains("CAPTURE-STATE-OK"), "{tier}: {stdout}");
        if tier == "t2" {
            let warmup = stderr
                .split("CAPTURE-T2-BEGIN")
                .nth(1)
                .unwrap()
                .split("CAPTURE-T2-END")
                .next()
                .unwrap();
            assert!(
                warmup
                    .lines()
                    .any(|line| line.contains("<flet-closure>: T2 INSTALLED")),
                "{warmup}"
            );
            assert!(
                !warmup.contains("UnsupportedInstr(\"LoadEnvVar\")"),
                "{warmup}"
            );
            assert!(
                !warmup.contains("UnsupportedInstr(\"StoreEnvVar\")"),
                "{warmup}"
            );
        }
    }
}

#[test]
fn shared_template_feedback_preserves_numeric_phase_recovery() {
    let program = r#"
      (defun phase-callback (seed) (lambda (x) (+ seed x)))
      (let ((a (phase-callback 1)) (b (phase-callback 2)))
        (format *error-output* "SHARED-PHASE-BEGIN~%")
        (dotimes (i 40) (assert (= (funcall a i) (+ i 1))))
        (let ((big 2305843009213693952))
          (dotimes (i 40) (assert (= (funcall b big) (+ big 2))))
          (dotimes (i 40) (assert (= (funcall a big) (+ big 1)))))
        (format *error-output* "SHARED-PHASE-END~%"))
      (format t "SHARED-PHASE-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_LOG", "stderr")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("SHARED-PHASE-OK"));
    let trace = stderr.split("SHARED-PHASE-BEGIN").nth(1).unwrap()
        .split("SHARED-PHASE-END").next().unwrap();
    assert!(trace.contains("native guard deopt"), "probe must fail a compiled guard: {trace}");
    assert!(!trace.contains("uninstall + blacklist"), "sibling feedback must preserve phase recovery: {trace}");
    assert!(trace.contains("supported numeric phase change => generic T1 + recompile"),
        "the stale version must recover through native recompilation: {trace}");
}
