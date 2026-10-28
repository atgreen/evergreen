// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const SOURCE: &str = r#"
(declaim (notinline scoped-original-double scoped-bounded-increment
                    scoped-overflow-increment scoped-false-double scoped-prior-double))
;; X is the original TOP-typed parameter, never a numeric IF-join result.
;; OTHER is also unconstrained: no literal float chooses the arithmetic family.
(defun scoped-original-double (x other arithmetic)
  (if (typep x 'double-float)
      (if arithmetic (+ x other) x)
      :not-double))
(defun scoped-bounded-increment (x arithmetic)
  (if (typep x 'fixnum)
      (if (>= x -20)
          (if (<= x 20) (if arithmetic (+ x 1) x) :outside)
          :outside)
      :outside))
;; A type proof alone cannot discharge possible arithmetic overflow.
(defun scoped-overflow-increment (x arithmetic)
  (if (typep x 'fixnum) (if arithmetic (+ x 1) x) :outside))
(defun scoped-false-double (x)
  (if (typep x 'double-float) x (+ x 1)))
(defun scoped-prior-double (x)
  (let ((answer (+ x 1))) (cons (typep x 'double-float) answer)))
(defun scoped-numeric-warm ()
  (scoped-original-double 1.25d0 1 nil)
  (scoped-original-double 2.5d0 -2 nil)
  (scoped-original-double :not-a-number :unused nil)
  (scoped-bounded-increment -20 nil)
  (scoped-bounded-increment 20 nil)
  (scoped-overflow-increment 1 nil)
  ;; These negative targets must also be native when CHECK exercises them.
  ;; Their arithmetic can sample normally; the original-input positive target
  ;; above still receives no arithmetic samples before its isolated first use.
  (dolist (x '(2.5d0 2.5f0 2))
    (scoped-false-double x)
    (scoped-prior-double x)))
;; The three positive/overflow target sites receive no arithmetic samples
;; before native installation; negative helper sites warm representatively.
(dotimes (i 40) (scoped-numeric-warm))
"#;

const READY: &str = r#"
(loop repeat 100
      until (and (= 2 (egcl-ext:function-tier 'scoped-original-double))
                 (= 2 (egcl-ext:function-tier 'scoped-bounded-increment))
                 (= 2 (egcl-ext:function-tier 'scoped-overflow-increment))
                 (= 2 (egcl-ext:function-tier 'scoped-false-double))
                 (= 2 (egcl-ext:function-tier 'scoped-prior-double)))
      do (scoped-numeric-warm) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'scoped-original-double)))
(assert (= 2 (egcl-ext:function-tier 'scoped-bounded-increment)))
(assert (= 2 (egcl-ext:function-tier 'scoped-overflow-increment)))
(assert (= 2 (egcl-ext:function-tier 'scoped-false-double)))
(assert (= 2 (egcl-ext:function-tier 'scoped-prior-double)))
;; Count only the target call, before any assertion/reference arithmetic.
(let* ((before (egcl-ext:deopt-count))
       (answer (scoped-original-double 1.25d0 1 t))
       (after (egcl-ext:deopt-count)))
  (assert (= before after))
  (assert (typep answer 'double-float))
  (assert (eql answer 2.25d0)))
(let* ((before (egcl-ext:deopt-count))
       (answer (scoped-bounded-increment -20 t))
       (after (egcl-ext:deopt-count)))
  (assert (= before after)) (assert (eql answer -19)))
;; The same inferred FIXNUM family has an unsafe result interval here. Its
;; first overflowing call must recover, and record explicit declined feedback.
(let* ((before (egcl-ext:deopt-count))
       (answer (scoped-overflow-increment 1152921504606846975 t))
       (after (egcl-ext:deopt-count)))
  (assert (> after before)) (assert (= answer 1152921504606846976)))
;; Give background replacement time, then require actual stable T2 execution.
;; Repeated overflow must not reinstate the same rejected fixnum assumption.
(let ((stable 0) (ready nil))
  (loop repeat 80 until ready do
    (let ((before (egcl-ext:deopt-count)))
      (dotimes (i 3)
        (assert (= (scoped-overflow-increment 1152921504606846975 t)
                   1152921504606846976)))
      (if (and (= 2 (egcl-ext:function-tier 'scoped-overflow-increment))
               (= before (egcl-ext:deopt-count)))
          (incf stable) (setf stable 0))
      (when (>= stable 3) (setf ready t)))
    (unless ready (sleep 0.01)))
  (assert ready))
"#;

const CHECK: &str = r#"
(dolist (pair '((1.25d0 2.25d0) (2.5d0 3.5d0) (-4d0 -3d0)))
  (let ((answer (scoped-original-double (car pair) 1 t)))
    (assert (typep answer 'double-float)) (assert (eql answer (cadr pair)))))
(dolist (x (list nil :absent "not-a-number" 7 1.25f0 (ash 1 80)))
  (assert (eq (scoped-original-double x :invalid-partner t) :not-double)))
(assert (eq (handler-case (scoped-original-double 1.25d0 :invalid-partner t)
              (type-error () :type-error)) :type-error))
(assert (eql (scoped-false-double 2.5d0) 2.5d0))
(assert (eql (scoped-false-double 2.5f0) 3.5f0))
(assert (eql (scoped-false-double 2) 3))
(assert (= (scoped-false-double (ash 1 80)) (1+ (ash 1 80))))
(assert (eq (handler-case (scoped-false-double "bad") (type-error () :type-error))
            :type-error))
(let ((double (scoped-prior-double 2.5d0)) (single (scoped-prior-double 2.5f0))
      (integer (scoped-prior-double 2)))
  (assert (eq (car double) t)) (assert (eql (cdr double) 3.5d0))
  (assert (null (car single))) (assert (eql (cdr single) 3.5f0))
  (assert (null (car integer))) (assert (eql (cdr integer) 3)))
(assert (eq (handler-case (scoped-prior-double "bad") (type-error () :type-error))
            :type-error))
(dolist (x '(-20 0 20)) (assert (= (scoped-bounded-increment x t) (1+ x))))
(dolist (x '(-21 21 1152921504606846975 nil :bad))
  (assert (eq (scoped-bounded-increment x t) :outside)))
(assert (= (scoped-overflow-increment 1152921504606846975 t) 1152921504606846976))
(format t "SCOPED-NUMERIC-ORIGINAL-INPUT-OK~%")
"#;

#[test]
fn original_input_typep_proofs_select_raw_double_and_preserve_failed_feedback() {
    let mut reference: Option<String> = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let ready = if tier == "t2" { READY } else { "" };
        let program = format!("{SOURCE}\n{ready}\n{CHECK}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_T2_NO_QUEUE")
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
        let output = command
            .output()
            .expect("run scoped original-input numeric regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("SCOPED-NUMERIC-ORIGINAL-INPUT-OK"));
        if tier == "t2" {
            for name in [
                "SCOPED-ORIGINAL-DOUBLE",
                "SCOPED-BOUNDED-INCREMENT",
                "SCOPED-OVERFLOW-INCREMENT",
                "SCOPED-FALSE-DOUBLE",
                "SCOPED-PRIOR-DOUBLE",
            ] {
                assert!(
                    stderr.lines().any(|line| line.contains(&format!("{name}:"))
                        && line.contains("optimized worker result installed with ")
                        && (line.contains("ordinary x86-64 Lisp T2 ABI")
                            || line.contains("mapped T2 ABI"))),
                    "{name} must actually install native T2\n{stderr}"
                );
            }
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("SCOPED-ORIGINAL-DOUBLE:")
                        && line.contains("T2 emitted;")
                        && line
                            .split("raw_f64_values=")
                            .nth(1)
                            .and_then(|tail| tail.split(',').next())
                            .and_then(|value| value.parse::<usize>().ok())
                            .is_some_and(|count| count > 0)),
                "the original input's true branch must reach raw double native emission\n{stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected.as_str());
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
