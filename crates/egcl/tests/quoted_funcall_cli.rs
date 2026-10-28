// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const SOURCE: &str = r#"
(declaim (notinline qf-target qf-mv-target qf-builtin-loop qf-quoted-change
                    qf-object-change qf-flet-target qf-mv-caller))
(defun qf-target (k) (1+ k))
(defun qf-mv-target (x) (values x :secondary))
(defun qf-builtin-loop (xs)
  (let ((n 0)) (loop while xs do (setq xs (funcall 'cdr xs)) (incf n)) n))
(defun qf-quoted-change (change)
  (funcall 'qf-target
    (progn (when change (setf (symbol-function 'qf-target) (lambda (k) (+ k 100)))) 2)))
(defun qf-object-change (change)
  (funcall #'qf-target
    (progn (when change (setf (symbol-function 'qf-target) (lambda (k) (+ k 100)))) 2)))
(setq *qf-self-before-redefine* -1)
(defun qf-self-change (change)
  (if change
      (funcall 'qf-self-change
        (progn
          (setq *qf-self-before-redefine* (egcl-ext:deopt-count))
          (setf (symbol-function 'qf-self-change) (lambda (ignored) (declare (ignore ignored)) :new))
          nil))
      :old))
(defun qf-flet-target (x)
  (flet ((qf-target (k) (+ k 1000)))
    (values (funcall 'qf-target x) (funcall #'qf-target x))))
(defun qf-mv-caller (x) (funcall 'qf-mv-target x))
(defun qf-tail (n) (if (zerop n) (values 0 :tail) (funcall 'qf-tail (1- n))))
(defun qf-warm ()
  (assert (= 3 (qf-builtin-loop '(1 2 3))))
  (assert (= 3 (qf-quoted-change nil)))
  (assert (= 3 (qf-object-change nil)))
  (assert (eq :old (qf-self-change nil)))
  (assert (equal '(3 1002) (multiple-value-list (qf-flet-target 2))))
  (assert (equal '(7 :secondary) (multiple-value-list (qf-mv-caller 7))))
  (assert (equal '(0 :tail) (multiple-value-list (qf-tail 4)))))
(dotimes (i 40) (qf-warm))
"#;

const READY: &str = r#"
(loop repeat 100 until
  (every (lambda (name) (= 2 (egcl-ext:function-tier name)))
    '(qf-builtin-loop qf-quoted-change qf-object-change qf-self-change qf-flet-target qf-mv-caller))
  do (qf-warm) (sleep 0.01))
(dolist (name '(qf-builtin-loop qf-quoted-change qf-object-change qf-self-change qf-flet-target qf-mv-caller))
  (assert (= 2 (egcl-ext:function-tier name))))
"#;

const CHECK: &str = r#"
(qf-warm)
;; Prove the actual argument still runs natively BEFORE it replaces this caller.
;; A later generation-guard deopt is valid, but an unseen-input entry deopt
;; would already increment the count observed inside this argument.
(let ((before (egcl-ext:deopt-count)))
  (assert (eq :new (qf-self-change t)))
  (assert (= before *qf-self-before-redefine*)))
(assert (= 102 (qf-quoted-change t)))
(defun qf-target (k) (1+ k))
(assert (= 3 (qf-object-change t)))
(assert (equal '(0 :tail) (multiple-value-list (qf-tail 20))))
(assert (eq :arity (handler-case (progn (funcall 'qf-mv-target) :missing) (program-error () :arity))))
(assert (eq :type (handler-case (progn (funcall 42 1) :missing) (type-error () :type))))
(assert (= 1002 (flet ((funcall (f k) (declare (ignore f)) (+ k 1000)))
                   (funcall 'qf-target 2))))
(defpackage :qf-shadow (:use :cl) (:shadow :funcall) (:export :funcall))
(defun qf-shadow:funcall (f k) (declare (ignore f)) (+ k 2000))
(assert (= 2002 (qf-shadow:funcall 'qf-target 2)))
(format t "QUOTED-FUNCALL-OK~%")
"#;

#[test]
fn quoted_global_calls_preserve_lookup_timing_namespaces_and_native_recovery() {
    let mut reference = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        for key in [
            "EGCL_PROFILING_DISABLED",
            "EGCL_T2_NO_QUEUE",
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(key);
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
            .expect("run quoted FUNCALL regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("QUOTED-FUNCALL-OK"));
        if tier == "t2" {
            for name in [
                "QF-BUILTIN-LOOP",
                "QF-QUOTED-CHANGE",
                "QF-OBJECT-CHANGE",
                "QF-SELF-CHANGE",
                "QF-FLET-TARGET",
                "QF-MV-CALLER",
            ] {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{name}: optimized worker result installed with "))
                        && (line.contains("ordinary x86-64 Lisp T2 ABI")
                            || line.contains("mapped T2 ABI"))),
                    "actual T2 for {name}\n{stderr}"
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
