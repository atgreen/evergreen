// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
(declaim (notinline bounded-octet-target))
(defun bounded-octet-target (record start limit value)
  (declare (type fixnum start))
  (incf (svref record 4))
  (let ((array (svref record 0)) (index start) (lane value)
        (end (min (+ start 8) limit)))
    (tagbody
      again
      (if (>= index end) (go done))
      (setf (aref array index) (ldb (byte 8 0) lane))
      (setq lane (ash lane -8))
      (setq index (1+ index))
      (go again)
      done)
    (setf (svref record 1) lane (svref record 2) index (svref record 3) value)
    array))
(defun bounded-octet-case (count value general measured)
  (let* ((array (if general (make-array 12 :initial-element 99)
                           (make-array 12 :element-type '(unsigned-byte 8) :initial-element 99)))
         (record (vector array nil nil nil 0))
         (expected (make-array 12 :initial-element 99))
         (before (egcl-ext:deopt-count)))
    (dotimes (i count)
      (setf (aref expected (+ 2 i)) (ldb (byte 8 (* i 8)) value)))
    (let ((result (bounded-octet-target record 2 (+ 2 count) value)))
      (when measured (assert (= before (egcl-ext:deopt-count))))
      (assert (eq result array))
      (assert (equalp array expected))
      (assert (= (svref record 1) (ash value (* -8 count))))
      (assert (= (svref record 2) (+ 2 count)))
      (assert (eql (svref record 3) value))
      (assert (= (svref record 4) 1))
      (when measured (format t "BOUNDED-CASE ~D ~S ~S~%" count value general)))))
;; Most scalar iterations see fixnums after the first unsigned-64 shift.
;; Occasional larger inputs also train the retained generic fallback.
(dotimes (i 100)
  (bounded-octet-case 8 18446744073709551615 nil nil)
  (when (= (mod i 16) 0)
    (bounded-octet-case 7 (+ (ash 1 200) 1311768467463790320) t nil)))
(loop repeat 100 until (= 2 (egcl-ext:function-tier 'bounded-octet-target))
      do (bounded-octet-case 8 18446744073709551615 nil nil) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'bounded-octet-target)))
(let ((stable 0) (ready nil))
  (dotimes (batch 40)
    (unless ready
      (let ((before (egcl-ext:deopt-count)))
        (dolist (count '(0 1 2 7 8))
          (dolist (value (list 18446744073709551615 -1 most-negative-fixnum
                              (+ (ash 1 200) 1311768467463790320)
                              (- (+ (ash 1 200) 1311768467463790320))))
            (dolist (general '(nil t)) (bounded-octet-case count value general nil))))
        (sleep 0.01)
        (if (and (= before (egcl-ext:deopt-count))
                 (= 2 (egcl-ext:function-tier 'bounded-octet-target)))
            (incf stable) (setq stable 0))
        (when (>= stable 3) (setq ready t)))))
  (assert ready))
(format t "BOUNDED-TIER ~D~%" (egcl-ext:function-tier 'bounded-octet-target))
(dolist (count '(0 1 2 7 8))
  (dolist (value (list 18446744073709551615 -1 most-negative-fixnum
                      (+ (ash 1 200) 1311768467463790320)
                      (- (+ (ash 1 200) 1311768467463790320))))
    (dolist (general '(nil t)) (bounded-octet-case count value general t))))
;; Zero trips neither inspect the destination nor perform integer extraction.
(let ((record (vector nil nil nil nil 0)))
  (assert (null (bounded-octet-target record 2 2 :bad-integer)))
  (assert (eq (svref record 1) :bad-integer))
  (assert (= (svref record 2) 2))
  (assert (= (svref record 4) 1)))
;; Arguments and the committed prefix execute once, including on failure.
(let* ((array (make-array 4 :element-type '(unsigned-byte 8) :initial-element 99))
       (record (vector array nil nil nil 0)) (effects 0))
  (assert (eq :error
    (handler-case
      (progn (bounded-octet-target (progn (setq effects (+ (* effects 10) 1)) record)
                                  (progn (setq effects (+ (* effects 10) 2)) 2)
                                  (progn (setq effects (+ (* effects 10) 3)) 10)
                                  (progn (setq effects (+ (* effects 10) 4)) 18446744073709551615)) :no-error)
      (error () :error))))
  (assert (= effects 1234)) (assert (= (svref record 4) 1))
  (assert (equalp array #(99 99 255 255)))
  (assert (null (svref record 1))) (assert (null (svref record 2))))
(let* ((array (make-array 12 :element-type '(unsigned-byte 8) :initial-element 99))
       (record (vector array nil nil nil 0)))
  (assert (eq :type-error
    (handler-case (progn (bounded-octet-target record 2 10 :bad-integer) :no-error)
      (type-error () :type-error))))
  (assert (= (svref record 4) 1))
  (assert (every (lambda (x) (= x 99)) array)))
(format t "BOUNDED-STEADY-DEOPTS 0~%BOUNDED-OCTET-FALLBACK-OK~%")
"#;

#[test]
fn bounded_octet_fallback_preserves_partial_writes_liveouts_and_errors() {
    let mut reference: Option<String> = None;
    for enabled in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_T0_T1_THRESHOLD", "20")
            .env("EGCL_T2_THRESHOLD", "40")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env_remove("EGCL_FORCE_TIER")
            .env_remove("EGCL_DISABLE_OCTET_WORD_STORES")
            .env_remove("EGCL_DISABLE_LOOP_PEELING")
            .env_remove("EGCL_DISABLE_BOUNDED_FALLBACK_PRUNING")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_POISON")
            .args(["--no-init", "--eval", PROGRAM]);
        if !enabled {
            command.env("EGCL_DISABLE_BOUNDED_FALLBACK_PRUNING", "1");
        }
        let output = command
            .output()
            .expect("run bounded scalar fallback differential");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("BOUNDED-TIER 2"));
        assert!(stdout.contains("BOUNDED-STEADY-DEOPTS 0"));
        assert!(stdout.contains("BOUNDED-OCTET-FALLBACK-OK"));
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("BOUNDED-OCTET-TARGET: octet word stores: 1")),
            "actual target must install the fused kernel\n{stderr}"
        );
        let skipped = stderr.lines().any(|line| {
            line.contains("BOUNDED-OCTET-TARGET: type-driven loop peeling:")
                && line.contains("bounded octet scalar fallback")
        });
        assert_eq!(
            skipped, enabled,
            "bounded exclusion must match ablation\n{stderr}"
        );
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected.as_str());
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
