// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const SOURCE: &str = r#"
(declaim (notinline hint-default-caller hint-double-caller hint-sf-caller hint-df-caller
                    hint-increment-caller hint-nested-caller hint-unknown-caller
                    hint-mutable-caller))
(defun hint-default (x) (float x))
(defun hint-double (x) (float x))
(defun hint-sf (x prototype) (float x prototype))
(defun hint-df (x prototype) (float x prototype))
(defun hint-default-caller (arithmetic other)
  (let ((n (hint-default 5))) (if arithmetic (+ n other) n)))
(defun hint-double-caller (arithmetic other)
  (let ((n (hint-double 5d0))) (if arithmetic (+ n other) n)))
(defun hint-sf-caller (arithmetic other)
  (let ((n (hint-sf 5 0f0))) (if arithmetic (+ n other) n)))
(defun hint-df-caller (arithmetic other)
  (let ((n (hint-df 5 0d0))) (if arithmetic (+ n other) n)))
(defun hint-increment-caller (arithmetic)
  (let ((n (hint-default 5))) (if arithmetic (incf n) n)))
(defun hint-identity (x) x)
(defun hint-nested (x) (hint-default (hint-identity x)))
(defun hint-nested-caller (arithmetic)
  (let ((n (hint-nested 5))) (if arithmetic (+ n 1) n)))
(defun hint-unknown-caller (x arithmetic)
  (let ((n (hint-default x))) (if arithmetic (+ n 1) n)))
(setq *hint-ticks* 0 *hint-source-ticks* 0 *hint-switch* nil)
(defun hint-source-effect () (incf *hint-source-ticks*))
(defun hint-mutable () (hint-source-effect) (float 5))
(defun hint-before-call ()
  (when *hint-switch*
    (setq *hint-switch* nil)
    (setf (symbol-function 'hint-mutable)
      (lambda () (hint-source-effect) "not numeric"))))
(defun hint-mutable-caller (arithmetic)
  (incf *hint-ticks*)
  (hint-before-call)
  (let ((n (hint-mutable)))
    (if arithmetic (+ n 1) (values n :kept))))
(defun hint-warm ()
  (hint-default-caller nil 1) (hint-double-caller nil 1)
  (hint-sf-caller nil 1) (hint-df-caller nil 1)
  (hint-increment-caller nil) (hint-nested-caller nil)
  (hint-unknown-caller 5 nil) (hint-unknown-caller 5d0 nil)
  (hint-mutable-caller nil))
(dotimes (i 40) (hint-warm))
"#;

const READY: &str = r#"
(loop repeat 100
 until (every (lambda (name) (= 2 (egcl-ext:function-tier name)))
   '(hint-default-caller hint-double-caller hint-sf-caller hint-df-caller
     hint-increment-caller hint-nested-caller hint-unknown-caller hint-mutable-caller))
 do (hint-warm) (sleep 0.01))
(dolist (name '(hint-default-caller hint-double-caller hint-sf-caller hint-df-caller
               hint-increment-caller hint-nested-caller hint-unknown-caller hint-mutable-caller))
  (assert (= 2 (egcl-ext:function-tier name))))
;; Arithmetic has NEVER executed. Measure only successful stable arithmetic;
;; the mutation/error checks below deliberately make no zero-deopt claim.
(let ((before (egcl-ext:deopt-count)))
  (assert (eql (hint-default-caller t 1) 6f0))
  (assert (eql (hint-double-caller t 1) 6d0))
  (assert (eql (hint-sf-caller t 1) 6f0))
  (assert (eql (hint-df-caller t 1) 6d0))
  (assert (eql (hint-increment-caller t) 6f0))
  (assert (eql (hint-nested-caller t) 6f0))
  ;; The unknown argument makes the result SF|DF. Keep cold arithmetic generic
  ;; rather than selecting a fixnum family incompatible with either format.
  (assert (eql (hint-unknown-caller 5 t) 6f0))
  (assert (eql (hint-unknown-caller 5d0 t) 6d0))
  (assert (= before (egcl-ext:deopt-count))))
"#;

const CHECK: &str = r#"
(dolist (other '(1 -2))
  (assert (eql (hint-default-caller t other) (if (= other 1) 6f0 3f0)))
  (assert (eql (hint-double-caller t other) (if (= other 1) 6d0 3d0)))
  (assert (eql (hint-sf-caller t other) (if (= other 1) 6f0 3f0)))
  (assert (eql (hint-df-caller t other) (if (= other 1) 6d0 3d0))))
(setq *hint-ticks* 0 *hint-source-ticks* 0)
(assert (equal (multiple-value-list (hint-mutable-caller nil)) '(5f0 :kept)))
(assert (= *hint-ticks* 1))
(assert (= *hint-source-ticks* 1))
;; Caller is still the native warmed version here: its helper replaces the
;; subsequent callee while this caller's activation is suspended.
(setq *hint-switch* t)
(assert (eq :type-error
  (handler-case (progn (hint-mutable-caller t) :missing-error) (type-error () :type-error))))
(assert (= *hint-ticks* 2))
(assert (= *hint-source-ticks* 2))
(defun hint-mutable () (hint-source-effect) "changed by defun")
(assert (eq :type-error
  (handler-case (progn (hint-mutable-caller t) :missing-error) (type-error () :type-error))))
(assert (= *hint-ticks* 3))
(assert (= *hint-source-ticks* 3))
;; Captured callable replacement is not identified by its mutable public name.
(setf (symbol-function 'hint-mutable)
  (let ((v 7d0)) (lambda () (hint-source-effect) v)))
(assert (equal (multiple-value-list (hint-mutable-caller t)) '(8d0)))
(assert (= *hint-ticks* 4))
(assert (= *hint-source-ticks* 4))
(assert (equal (multiple-value-list (hint-mutable-caller nil)) '(7d0 :kept)))
(assert (= *hint-ticks* 5))
(assert (= *hint-source-ticks* 5))
;; NOTINLINE keeps named dispatch and correct effects/MV, without a hint promise.
(defun hint-opaque () (declare (notinline hint-mutable)) (hint-mutable))
(assert (eql (hint-opaque) 7d0))
(format t "CALLEE-RETURN-FACTS-OK ~D~%" *hint-ticks*)
"#;

#[test]
fn callee_return_expectations_choose_cold_numeric_families_but_guard_redefinitions() {
    let mut reference = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        for name in [
            "EGCL_PROFILING_DISABLED",
            "EGCL_T2_NO_QUEUE",
            "EGCL_DISABLE_RETURN_TYPE_HINTS",
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let ready = if tier == "t2" { READY } else { "" };
        let program = format!("{SOURCE}\n{ready}\n{CHECK}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .expect("run return hint regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("CALLEE-RETURN-FACTS-OK 5"));
        if tier == "t2" {
            for name in [
                "HINT-DEFAULT-CALLER",
                "HINT-DOUBLE-CALLER",
                "HINT-SF-CALLER",
                "HINT-DF-CALLER",
                "HINT-INCREMENT-CALLER",
                "HINT-NESTED-CALLER",
                "HINT-UNKNOWN-CALLER",
                "HINT-MUTABLE-CALLER",
            ] {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{name}: optimized worker result installed with "))
                        && (line.contains("ordinary x86-64 Lisp T2 ABI")
                            || line.contains("mapped T2 ABI"))),
                    "actual native install for {name}\n{stderr}"
                );
            }
            for name in [
                "HINT-DEFAULT-CALLER",
                "HINT-DOUBLE-CALLER",
                "HINT-SF-CALLER",
                "HINT-DF-CALLER",
                "HINT-INCREMENT-CALLER",
                "HINT-NESTED-CALLER",
                "HINT-UNKNOWN-CALLER",
                "HINT-MUTABLE-CALLER",
            ] {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{name}: guarded return type hints: "))
                        && !line.contains("hints: 0 value(s)")),
                    "actual summary consumer for {name}\n{stderr}"
                );
            }
            for name in ["HINT-DOUBLE-CALLER", "HINT-DF-CALLER"] {
                assert!(
                    stderr.lines().any(|line| line.contains(&format!("{name}:"))
                        && line.contains("T2 emitted;")
                        && line
                            .split("raw_f64_values=")
                            .nth(1)
                            .and_then(|part| part.split(',').next())
                            .and_then(|n| n.parse::<usize>().ok())
                            .is_some_and(|n| n > 0)),
                    "actual raw double math for {name}\n{stderr}"
                );
            }
        }
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected);
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
