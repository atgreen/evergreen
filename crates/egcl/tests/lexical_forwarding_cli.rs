// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
(defvar *lexical-forwarding-special* 100)
(declaim (notinline forwarding-maker forwarding-warm))
(defun forwarding-maker (initial)
  (let ((cell initial))
    (values
      (lambda (delta) (incf cell delta) (+ cell cell))
      (lambda (callback) (let ((before cell)) (funcall callback) (+ before cell)))
      (lambda (value) (setq cell value))
      (lambda () cell)
      (lambda (delta other) (incf cell delta) (+ cell other))
      (lambda (value) (let ((*lexical-forwarding-special* value))
                        (+ cell *lexical-forwarding-special*))))))
(multiple-value-bind (update callback set-cell get-cell checked scoped) (forwarding-maker 7)
  (defun forwarding-warm (update checked)
    (dotimes (i 30) (funcall update 1) (funcall checked 0 1)))
  (forwarding-warm update checked)
  :READY
  (assert (= 20 (progn (funcall set-cell 7) (funcall update 3))))
  (assert (= 10 (funcall get-cell)))
  ;; A callback really mutates the shared location. Its call must invalidate
  ;; the earlier available value, including through a sibling closure.
  (assert (= 51 (funcall callback (lambda () (funcall set-cell 41)))))
  (assert (= 41 (funcall get-cell)))
  ;; A later failure cannot replay an already-completed lexical store.
  (assert (eq :type-error (handler-case (funcall checked 1 "wrong")
                           (type-error () :type-error))))
  (assert (= 42 (funcall get-cell)))
  (assert (= 51 (funcall scoped 9)))
  (assert (= 100 *lexical-forwarding-special*))
  (assert (equal (multiple-value-list (funcall update 0)) '(84)))
  (assert (equal (multiple-value-list (progn (values :old :stale)
                                            (funcall update 0))) '(84)))
  ;; Keep a movable object live in the same captured cell across allocating
  ;; code and a collection; the next access must observe the updated value.
  (funcall set-cell (list :kept :payload))
  (egcl-ext:gc)
  (assert (equal (funcall get-cell) '(:kept :payload)))
  (funcall set-cell 42))
(format t "LEXICAL-FORWARDING-OK~%")
"#;

const READY: &str = r#"
(loop repeat 100 until (and (= 2 (egcl-ext:function-tier update))
                            (= 2 (egcl-ext:function-tier checked)))
      do (forwarding-warm update checked) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier update)))
(assert (= 2 (egcl-ext:function-tier checked)))
"#;

#[test]
fn lexical_available_values_preserve_mutating_calls_errors_and_mv() {
    let mut reference = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let ready = if tier == "t2" { READY } else { "nil" };
        let program = PROGRAM.replace(":READY", ready);
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env_remove("EGCL_T2_NO_QUEUE")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_GC_DISABLE")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_POISON");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run lexical forwarding regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("LEXICAL-FORWARDING-OK"));
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
        } else {
            reference = Some(stdout);
        }
    }
}
