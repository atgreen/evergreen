// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
(declaim (notinline feedback-scoped-overflow feedback-declared-overflow))
(defun feedback-scoped-overflow (x arithmetic)
  (if (typep x 'fixnum) (if arithmetic (+ x 1) x) :outside))
(defun feedback-declared-overflow (x arithmetic)
  (declare (type fixnum x))
  (if arithmetic (+ x 1) x))
(defun feedback-warm ()
  (feedback-scoped-overflow 7 nil)
  (feedback-declared-overflow 7 nil))
(dotimes (i 60) (feedback-warm))
(loop repeat 100
      until (and (= 2 (egcl-ext:function-tier 'feedback-scoped-overflow))
                 (= 2 (egcl-ext:function-tier 'feedback-declared-overflow)))
      do (feedback-warm) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'feedback-scoped-overflow)))
(assert (= 2 (egcl-ext:function-tier 'feedback-declared-overflow)))
;; Each source proof allows fixnum operands, but cannot prove result fit.
;; First fail the actual installed fixnum result guard, not a lower-tier guess.
(let* ((before (egcl-ext:deopt-count))
       (answer (feedback-scoped-overflow 1152921504606846975 t))
       (after (egcl-ext:deopt-count)))
  (assert (> after before))
  (assert (= answer 1152921504606846976)))
(let* ((before (egcl-ext:deopt-count))
       (answer (feedback-declared-overflow 1152921504606846975 t))
       (after (egcl-ext:deopt-count)))
  (assert (> after before))
  (assert (= answer 1152921504606846976)))
;; Operand samples remain fixnums forever. The failed result family must stay
;; retired anyway, including for an explicitly declared fixnum parameter.
(let ((stable 0) (ready nil))
  (loop repeat 100 until ready do
    (let* ((before (egcl-ext:deopt-count))
           (a (feedback-scoped-overflow 1152921504606846975 t))
           (b (feedback-declared-overflow 1152921504606846975 t))
           (after (egcl-ext:deopt-count)))
      (assert (= a 1152921504606846976))
      (assert (= b 1152921504606846976))
      (if (and (= 2 (egcl-ext:function-tier 'feedback-scoped-overflow))
               (= 2 (egcl-ext:function-tier 'feedback-declared-overflow))
               (= before after))
          (incf stable) (setq stable 0))
      (when (>= stable 3) (setq ready t)))
    (unless ready (sleep 0.01)))
  (assert ready))
(assert (eq (feedback-scoped-overflow :not-numeric t) :outside))
(assert (= (feedback-scoped-overflow 7 t) 8))
(assert (= (feedback-declared-overflow 7 t) 8))
(format t "NUMERIC-FAILURE-FEEDBACK-OK~%")
"#;

#[test]
fn overflow_feedback_retires_failed_family_without_erasing_operand_type_proofs() {
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_T2_NO_QUEUE")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_POISON")
            .args(["--no-init", "--eval", PROGRAM]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command
            .output()
            .expect("run persistent overflow feedback regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("NUMERIC-FAILURE-FEEDBACK-OK"));
        for name in ["FEEDBACK-SCOPED-OVERFLOW", "FEEDBACK-DECLARED-OVERFLOW"] {
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains(&format!("{name}: speculated "))),
                "initial version must contain real guarded math\n{stderr}"
            );
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains(&format!("{name}: 0 speculatable sites"))),
                "replacement must keep failed family generic\n{stderr}"
            );
            assert!(
                stderr.lines().any(|line| line
                    .contains(&format!("{name}: optimized worker result installed with "))
                    && (line.contains("ordinary x86-64 Lisp T2 ABI")
                        || line.contains("mapped T2 ABI"))),
                "actual native T2 is required\n{stderr}"
            );
        }
    }
}
