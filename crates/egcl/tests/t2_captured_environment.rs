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
            (egcl-ext:gc :full t)
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
          (egcl-ext:gc :full t)
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
fn instances_of_one_lambda_reuse_native_compilation() {
    let program = r#"
      (defun shared-code-counter (seed)
        (lambda (delta) (setq seed (+ seed delta))))
      (format *error-output* "SHARED-CODE-BEGIN~%")
      (let ((callbacks nil))
        (dotimes (instance 5)
          (let ((callback (shared-code-counter (* instance 100))))
            (dolist (previous callbacks) (assert (not (eq previous callback))))
            (push callback callbacks)
            (dotimes (call 40)
              (assert (= (funcall callback 1) (+ (* instance 100) call 1))))))
        (egcl-ext:gc :full t)
        (let ((expected 440))
          (dolist (callback callbacks)
            (assert (= (funcall callback 2) (+ expected 2)))
            (setq expected (- expected 100)))))
      (format *error-output* "SHARED-CODE-END~%")
      (format t "SHARED-CODE-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_LOG", "stderr")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("SHARED-CODE-OK"), "{stdout}");
    let trace = stderr
        .split("SHARED-CODE-BEGIN")
        .nth(1)
        .unwrap()
        .split("SHARED-CODE-END")
        .next()
        .unwrap();
    let installations = trace
        .lines()
        .filter(|line| line.contains("<flet-closure>: T2 INSTALLED"))
        .count();
    assert_eq!(
        installations, 1,
        "one lambda template should compile once: {trace}"
    );
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
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("SHARED-PHASE-OK"));
    let trace = stderr
        .split("SHARED-PHASE-BEGIN")
        .nth(1)
        .unwrap()
        .split("SHARED-PHASE-END")
        .next()
        .unwrap();
    assert!(
        trace.contains("native guard deopt"),
        "probe must fail a compiled guard: {trace}"
    );
    assert!(
        !trace.contains("uninstall + blacklist"),
        "sibling feedback must preserve phase recovery: {trace}"
    );
    assert!(
        trace.contains("supported numeric phase change => generic T1 + recompile"),
        "the stale version must recover through native recompilation: {trace}"
    );
}

#[test]
fn active_shared_code_survives_owner_collection_and_replacement() {
    let program = r#"
      (defvar *replace-now* nil)
      (defun replace-owner ()
        (when *replace-now*
          (setq *replace-now* nil)
          (setq *old-owner* nil)
          (egcl-ext:gc :full t)
          (dotimes (i 40) (funcall *new-owner* 1))
          (format *error-output* "REPLACEMENT-READY~%")))
      (defun evictable-counter (seed)
        (let ((hits 0))
          (lambda (delta)
            (setq hits (+ hits 1))
            (replace-owner)
            (setq seed (+ seed delta))
            (values seed hits))))
      (defvar *old-owner* (evictable-counter 10))
      (defvar *active-sibling* (evictable-counter 2305843009213694051))
      (defvar *new-owner* (evictable-counter 1000))
      (format *error-output* "EVICTION-BEGIN~%")
      (dotimes (i 40) (funcall *old-owner* 1))
      (setq *replace-now* t)
      (multiple-value-bind (value hits)
          (funcall *active-sibling* 1)
        (assert (= value 2305843009213694052))
        (assert (= hits 1)))
      (format *error-output* "OBSOLETE-RETURNED~%")
      (multiple-value-bind (value hits) (funcall *new-owner* 1)
        (assert (= value 1041)) (assert (= hits 41)))
      (format *error-output* "EVICTION-END~%")
      (format t "EVICTION-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_LOG", "stderr")
        .env("EGCL_DEOPT_PATH_DBG", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("EVICTION-OK"));
    let trace = stderr
        .split("EVICTION-BEGIN")
        .nth(1)
        .unwrap()
        .split("EVICTION-END")
        .next()
        .unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("<flet-closure>: T2 INSTALLED"))
            .count(),
        2,
        "the collected owner must be replaced while its code is active: {trace}"
    );
    let obsolete = trace
        .split("REPLACEMENT-READY")
        .nth(1)
        .unwrap()
        .split("OBSOLETE-RETURNED")
        .next()
        .unwrap();
    assert!(
        obsolete.contains("[deopt-path] Inlined"),
        "old native code must deopt: {trace}"
    );
    assert!(
        !obsolete.contains("native guard deopt #"),
        "obsolete guards must not alter the current version's counters: {trace}"
    );
}

#[test]
fn shared_t1_loop_enters_t2_and_deopts_into_the_calling_capture() {
    let program = r#"(defun make-shared-loop (seed)
  (lambda (n)
    (let ((sum seed))
      (dotimes (i n) (setq sum (+ sum 1)))
      (setq seed sum))))
(let ((a (make-shared-loop 10))
      (b (make-shared-loop 1152921504601846975)))
  (format *error-output* "SHARED-OSR-BEGIN~%")
  (funcall a 1) (funcall a 2)
  (let ((before (egcl-ext:deopt-count)))
    (format t "RESULT ~a DEOPTS ~a~%" (funcall b 6000000)
      (- (egcl-ext:deopt-count) before)))
  (format t "NEXT ~a~%" (funcall b 0))
  (format *error-output* "SHARED-OSR-END~%"))
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_FORCE_TIER")
        .env_remove("EGCL_T2")
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000")
        .env("EGCL_T1_T2_BACKEDGE_THRESHOLD", "1000")
        .env("EGCL_OSR_THRESHOLD", "100000000")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_LOG", "stderr")
        .env("EGCL_DEOPT_PATH_DBG", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("RESULT 1152921504607846975 DEOPTS "),
        "{stdout}"
    );
    assert!(stdout.contains("NEXT 1152921504607846975"), "{stdout}");
    let trace = stderr
        .split("SHARED-OSR-BEGIN")
        .nth(1)
        .unwrap()
        .split("SHARED-OSR-END")
        .next()
        .unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("<flet-closure>: T2 INSTALLED"))
            .count(),
        1,
        "loop heat must install one shared T2 version: {trace}"
    );
    assert!(trace.contains("osr_entries=1"), "{trace}");
    assert!(
        trace.contains("[deopt-path] Inlined"),
        "the running loop must enter T2 and fail its numeric guard: {trace}"
    );
}
