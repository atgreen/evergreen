// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
(declaim (notinline octet-word-target octet-word-opaque))
(defun octet-word-target (record start end value)
  (let ((array (svref record 0)) (index start) (lane value))
    (tagbody
      again
      (if (>= index end) (go done))
      (setf (aref array index) (ldb (byte 8 0) lane))
      (setq lane (ash lane -8))
      (setq index (1+ index))
      (go again)
      done)
    (setf (svref record 1) lane
          (svref record 2) index
          (svref record 3) value)
    array))
(defun octet-word-opaque (record start end value)
  (declare (notinline ash >=))
  (let ((array (svref record 0)) (index start) (lane value))
    (tagbody
      again
      (if (>= index end) (go done))
      (setf (aref array index) (ldb (byte 8 0) lane))
      (setq lane (ash lane -8))
      (setq index (1+ index))
      (go again)
      done)
    (setf (svref record 1) lane
          (svref record 2) index
          (svref record 3) value)
    array))
(defun octet-word-check (count value general)
  (let* ((array (if general (make-array 16 :initial-element 99)
                           (make-array 16 :element-type '(unsigned-byte 8) :initial-element 99)))
         (record (vector array nil nil nil))
         (expected (make-array 16 :initial-element 99))
         (remaining value)
         (expected-lane (if (zerop count) value (ash value (* -8 count)))))
    (dotimes (i count)
      (setf (aref expected (+ 2 i)) (ldb (byte 8 0) remaining))
      (setq remaining (ash remaining -8)))
    (let ((before (egcl-ext:deopt-count)))
      (let ((result (octet-word-target record 2 (+ 2 count) value)))
        (let ((after (egcl-ext:deopt-count))) (assert (= before after)))
        (let ((lane (svref record 1)) (index (svref record 2)) (original (svref record 3)))
          (assert (eq result array))
          (assert (eq original value))
          (assert (= index (+ 2 count)))
          (assert (eql lane expected-lane))
          (assert (equalp array expected))
          (format t "OCTET-CASE ~D ~S ~S ~S ~D~%" count value general lane index))))))
(defun octet-word-train (count value general)
  (let* ((array (if general (make-array 16 :initial-element 99)
                           (make-array 16 :element-type '(unsigned-byte 8) :initial-element 99)))
         (record (vector array nil nil nil))
         (expected (make-array 16 :initial-element 99))
         (remaining value)
         (expected-lane (if (zerop count) value (ash value (* -8 count)))))
    (dotimes (i count)
      (setf (aref expected (+ 2 i)) (ldb (byte 8 0) remaining))
      (setq remaining (ash remaining -8)))
    (let ((unused nil))
      (let ((result (octet-word-target record 2 (+ 2 count) value)))
        (let ((lane (svref record 1)) (index (svref record 2)) (original (svref record 3)))
          (assert (eq result array))
          (assert (eq original value))
          (assert (= index (+ 2 count)))
          (assert (eql lane expected-lane))
          (assert (equalp array expected))
          t)))))
(defvar *octet-argument-effects* 0)
(defun octet-word-error (name array start end value)
  (let ((record (vector array nil nil nil)))
    (setq *octet-argument-effects* 0)
    (let ((condition
           (handler-case
               (progn
                 (octet-word-target
                   (progn (incf *octet-argument-effects*) record)
                   (progn (incf *octet-argument-effects*) start)
                   (progn (incf *octet-argument-effects*) end)
                   (progn (incf *octet-argument-effects*) value))
                 :no-error)
             (type-error () :type-error)
             (error () :error))))
      (assert (not (eq condition :no-error)))
      (assert (= *octet-argument-effects* 4))
      (assert (null (svref record 1)))
      (assert (null (svref record 2)))
      (assert (null (svref record 3)))
      (format t "OCTET-ERROR ~S ~S ~D~%" name condition *octet-argument-effects*)
      (when (arrayp array)
        (format t "OCTET-AFTER ~S ~S~%" name (coerce array 'list)))
      condition)))
(let* ((warm (make-array 16 :element-type '(unsigned-byte 8) :initial-element 99))
       (record (vector warm nil nil nil)))
  (dotimes (i 60)
    (octet-word-target record 2 10 18446744073709551615)
    (octet-word-opaque record 2 10 18446744073709551615))
  (loop repeat 50
        until (= 2 (egcl-ext:function-tier 'octet-word-target))
        do (octet-word-target record 2 10 18446744073709551615) (sleep 0.01))
  (assert (= 2 (egcl-ext:function-tier 'octet-word-target)))
  (format t "OCTET-TIER ~D~%" (egcl-ext:function-tier 'octet-word-target)))
 ;; Train the retained scalar fallback as well as the fused path. Cold numeric
;; speculation may legitimately deopt during warmup; measured cases below
;; still require zero deopts and retain every semantic assertion.
(let ((stable 0) (ready nil))
  (dotimes (batch 40)
    (unless ready
      (let ((before (egcl-ext:deopt-count)))
        (dolist (value (list 1 most-negative-fixnum
                            9223372036854775808 -18446744073709551615
                            (+ (ash 1 200) 1311768467463790320)
                            (- (+ (ash 1 200) 1311768467463790320))))
          (dolist (general '(nil t))
            (dolist (count '(0 1 3 7 8 9))
              (octet-word-train count value general))))
        (sleep 0.01)
        (if (and (= 2 (egcl-ext:function-tier 'octet-word-target))
                 (= before (egcl-ext:deopt-count)))
            (incf stable)
            (setq stable 0))
        (when (>= stable 3) (setq ready t)))))
  (assert ready))
;; All supported integer widths and both array layouts are valid. Unsupported
;; full-word probes must use the scalar loop, retaining its exact live-outs.
(dolist (value (list 0 1 most-positive-fixnum most-negative-fixnum
                     9223372036854775808 18446744073709551615
                     -9223372036854775808 -18446744073709551615
                     (+ (ash 1 200) 1311768467463790320)
                     (- (+ (ash 1 200) 1311768467463790320))))
  (octet-word-check 8 value nil)
  (octet-word-check 8 value t))
(dolist (count '(0 1 2 3 4 5 6 7 9))
  (octet-word-check count 18446744073709551615 nil))
;; A zero-trip loop does not inspect its destination or integer operand.
(let* ((record (vector nil nil nil nil)) (before (egcl-ext:deopt-count)))
  (let ((array (octet-word-target record 2 2 :not-an-integer)))
    (let ((after (egcl-ext:deopt-count))) (assert (= before after)))
    (assert (null array)) (assert (eq (svref record 1) :not-an-integer))
    (assert (= (svref record 2) 2)) (assert (eq (svref record 3) :not-an-integer))))
(format t "OCTET-VALID-DEOPTS 0~%")
;; NOTINLINE must keep the same result while leaving the calls opaque to this
;; optimization. Standard CL arithmetic function redefinition is unsupported.
(let* ((array (make-array 12 :element-type '(unsigned-byte 8) :initial-element 99))
       (record (vector array nil nil nil)))
  (let ((result (octet-word-opaque record 2 10 -1)))
    (assert (eq result array)) (assert (= (svref record 1) -1))
    (assert (= (svref record 2) 10)) (assert (= (svref record 3) -1))
    (dotimes (i 12) (assert (= (aref array i) (if (and (>= i 2) (< i 10)) 255 99))))))
(format t "OCTET-NOTINLINE-OK~%")
(assert (eq (octet-word-error :destination :bad-array 0 8 255) :type-error))
(let ((array (make-array 12 :element-type '(unsigned-byte 8) :initial-element 99)))
  (assert (eq (octet-word-error :value array 2 10 :bad-value) :type-error))
  (dotimes (i 12) (assert (= (aref array i) 99))))
(let ((array (make-array 8 :element-type '(unsigned-byte 8) :initial-element 99)))
  (octet-word-error :negative-index array -1 7 255)
  (dotimes (i 8) (assert (= (aref array i) 99))))
(let ((array (make-array 8 :element-type '(unsigned-byte 8) :initial-element 99)))
  (octet-word-error :partial-bounds array 3 11 18446744073709551615)
  (dotimes (i 8) (assert (= (aref array i) (if (< i 3) 99 255)))))
(format t "OCTET-WORD-FUSION-OK~%")
"#;

#[test]
fn whole_word_octet_stores_preserve_scalar_results_liveouts_and_error_effects() {
    let mut expected = None;
    for (enabled, stress) in [(false, false), (true, false), (true, true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_POISON")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .args(["--no-init", "--eval", PROGRAM]);
        if enabled {
            command.env_remove("EGCL_DISABLE_OCTET_WORD_STORES");
        } else {
            command.env("EGCL_DISABLE_OCTET_WORD_STORES", "1");
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run octet loop differential");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("OCTET-WORD-FUSION-OK"),
            "{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("OCTET-VALID-DEOPTS 0"),
            "{stdout}\n{stderr}"
        );
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("OCTET-WORD-TARGET:")
                    && line.contains("installed with ordinary x86-64 Lisp T2 ABI")),
            "target must execute its installed ordinary native entry: {stderr}"
        );
        let fused = stderr
            .lines()
            .any(|line| line.contains("OCTET-WORD-TARGET: octet word stores: 1"));
        assert_eq!(
            fused, enabled,
            "fusion admission evidence mismatch: {stderr}"
        );
        assert!(
            !stderr
                .lines()
                .any(|line| line.contains("OCTET-WORD-OPAQUE: octet word stores:")),
            "NOTINLINE operator calls must stay opaque: {stderr}"
        );
        if stress {
            assert!(
                stderr.contains("[gc-stress] EGCL_GC_STRESS_AFTER_INIT: stressing from allocation"),
                "stress must actually be enabled: {stderr}"
            );
        }
        let result: Vec<_> = stdout
            .lines()
            .filter(|line| line.starts_with("OCTET-"))
            .map(str::to_owned)
            .collect();
        if let Some(expected) = &expected {
            assert_eq!(&result, expected, "enabled={enabled} stress={stress}");
        } else {
            expected = Some(result);
        }
    }
}
