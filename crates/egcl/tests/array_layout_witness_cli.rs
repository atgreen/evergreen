// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
(defparameter *record-element-type* (list 'unsigned-byte 8))
(defvar *record-writer-effects* 0)
(declaim (notinline layout-record-writer layout-write-into))
;; Pack consecutive integer records, truncating the final record to capacity.
(defun layout-record-writer (words size)
  (declare (type fixnum size))
  (incf *record-writer-effects*)
  (let ((output (make-array size :element-type *record-element-type*))
        (start 0) (record-number 0))
    (tagbody
      next-record
      (if (>= start size) (go finished))
      (let ((lane (aref words record-number)) (index start)
            (end (min (+ start 8) size)))
        (tagbody
          next-byte
          (if (>= index end) (go record-finished))
          (setf (aref output index) (ldb (byte 8 0) lane))
          (setq lane (ash lane -8))
          (setq index (1+ index))
          (go next-byte)
          record-finished))
      (setq start (+ start 8))
      (setq record-number (1+ record-number))
      (go next-record)
      finished)
    output))
;; A passed destination also exercises unsupported shapes and zero trips.
(defun layout-write-into (array start end value)
  (let ((index start) (lane value))
    (tagbody
      again
      (if (>= index end) (go done))
      (setf (aref array index) (ldb (byte 8 0) lane))
      (setq lane (ash lane -8))
      (setq index (1+ index))
      (go again)
      done)
    array))
(defun layout-words (mixed)
  (let ((words (make-array 40)))
    (dotimes (i 40)
      (setf (aref words i)
        (if mixed
          (case (mod i 4)
            (0 1311768467463790320)
            (1 -18446744073709551615)
            (2 (+ (ash 1 200) 81985529216486895))
            (t (- (+ (ash 1 200) 81985529216486895))))
          18446744073709551615)))
    words))
(defun layout-check (words size measured general)
  (let ((before (egcl-ext:deopt-count)) (effects *record-writer-effects*))
    (let ((output (layout-record-writer words size)))
      (when measured (assert (= before (egcl-ext:deopt-count))))
      (assert (= *record-writer-effects* (1+ effects)))
      (assert (= (length output) size))
      (if general
          (assert (simple-vector-p output))
          (assert (equal (array-element-type output) '(unsigned-byte 8))))
      ;; Reference uses byte positions in the original records, rather than
      ;; updating a carried lane or reproducing the writer's nested loops.
      (dotimes (i size)
        (assert (= (aref output i)
                   (ldb (byte 8 (* 8 (mod i 8))) (aref words (floor i 8))))))
      (when measured (format t "LAYOUT-RECORD ~D ~S ~S~%" size general (coerce output 'list)))
      output)))
(let ((ordinary (layout-words nil)) (mixed (layout-words t)))
  (dotimes (i 100)
    (layout-check ordinary 136 nil nil)
    (when (= (mod i 16) 0) (layout-check mixed 7 nil nil)))
  (loop repeat 100 until (= 2 (egcl-ext:function-tier 'layout-record-writer))
        do (layout-check ordinary 136 nil nil) (sleep 0.01))
  (assert (= 2 (egcl-ext:function-tier 'layout-record-writer)))
  ;; Train both upgraded layouts and integer-width fallbacks before measuring.
  (let ((stable 0) (ready nil))
    (dotimes (batch 40)
      (unless ready
        (let ((before (egcl-ext:deopt-count)))
          (dolist (width '(8 64 8))
            (setf (car (cdr *record-element-type*)) width)
            (dolist (size '(0 7 9 135 136 137 257))
              (layout-check ordinary size nil (= width 64))
              (layout-check mixed size nil (= width 64))))
          (sleep 0.01)
          (if (and (= before (egcl-ext:deopt-count))
                   (= 2 (egcl-ext:function-tier 'layout-record-writer)))
              (incf stable) (setq stable 0))
          (when (>= stable 3) (setq ready t)))))
    (assert ready))
  (format t "LAYOUT-TIER ~D~%" (egcl-ext:function-tier 'layout-record-writer))
  ;; Mutate the same descriptor conses between hot calls. Width64 upgrades to
  ;; general T storage in EGCL, then width8 must produce packed bytes again.
  (dolist (width '(8 64 8))
    (setf (car (cdr *record-element-type*)) width)
    (dolist (size '(0 7 9 135 136 137 257))
      (layout-check ordinary size t (= width 64))
      (layout-check mixed size t (= width 64)))))
 ;; A changed constructor element type must never borrow the UB8 witness.
(let ((descriptor *record-element-type*) (effects *record-writer-effects*))
  (setq *record-element-type* 'double-float)
  (assert (eq :type-error
    (handler-case
      (progn (layout-record-writer (layout-words nil) 7) :no-error)
      (type-error () :type-error))))
  (assert (= *record-writer-effects* (1+ effects)))
  (setq *record-element-type* descriptor))
(assert (null (layout-write-into nil 3 3 :not-an-integer)))
(let ((general (make-array 10 :initial-element 99)))
  (assert (eq general (layout-write-into general 1 8 -1)))
  (dotimes (i 10) (assert (= (aref general i) (if (and (>= i 1) (< i 8)) 255 99)))))
(let ((bytes (make-array 4 :element-type '(unsigned-byte 8) :initial-element 99))
      (effects 0))
  (assert (eq :error
    (handler-case
      (progn (layout-write-into
               (progn (setq effects (+ (* effects 10) 1)) bytes)
               (progn (setq effects (+ (* effects 10) 2)) 2)
               (progn (setq effects (+ (* effects 10) 3)) 9)
               (progn (setq effects (+ (* effects 10) 4)) -1)) :no-error)
      (error () :error))))
  (assert (= effects 1234))
  (assert (equalp bytes #(99 99 255 255))))
(let ((bytes (make-array 8 :element-type '(unsigned-byte 8) :initial-element 99)))
  (assert (eq :type-error
    (handler-case (progn (layout-write-into bytes 0 8 :bad-integer) :no-error)
      (type-error () :type-error))))
  (assert (every (lambda (x) (= x 99)) bytes)))
(format t "LAYOUT-STEADY-DEOPTS 0~%ARRAY-LAYOUT-WITNESS-OK~%")
"#;

#[test]
fn array_layout_witness_preserves_binary_records_and_mutable_constructor_types() {
    let mut reference: Option<String> = None;
    for (enabled, inline_allocation) in [(false, true), (true, true), (true, false)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_T0_T1_THRESHOLD", "20")
            .env("EGCL_T2_THRESHOLD", "40")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_ARRAY_LAYOUT_LOG", "1")
            .env("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS", "1")
            .env_remove("EGCL_DISABLE_INLINE_U8_ALLOCATION")
            .env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR")
            .env_remove("EGCL_FORCE_TIER")
            .env_remove("EGCL_DISABLE_ARRAY_LAYOUT_FACTS")
            .env_remove("EGCL_DISABLE_OCTET_WORD_STORES")
            .env_remove("EGCL_DISABLE_LOOP_PEELING")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_POISON")
            .args(["--no-init", "--eval", PROGRAM]);
        if !enabled {
            command.env("EGCL_DISABLE_ARRAY_LAYOUT_FACTS", "1");
        }
        if !inline_allocation {
            command.env("EGCL_DISABLE_INLINE_U8_ALLOCATION", "1");
        }
        let output = command
            .output()
            .expect("run binary record layout differential");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("LAYOUT-TIER 2"));
        assert!(stdout.contains("LAYOUT-STEADY-DEOPTS 0"));
        assert!(stdout.contains("ARRAY-LAYOUT-WITNESS-OK"));
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("LAYOUT-RECORD-WRITER: octet word stores: 1")),
            "binary writer must compile its actual fused kernel\n{stderr}"
        );
        assert!(stderr.lines().any(|line| line.contains("LAYOUT-RECORD-WRITER: optimized worker result installed with ordinary x86-64 Lisp T2 ABI")),
            "target must install the intended ordinary native path\n{stderr}");
        let witnessed = stderr.lines().any(|line| {
            line.contains("LAYOUT-RECORD-WRITER: array layout witnesses:")
                && line
                    .rsplit(':')
                    .next()
                    .and_then(|n| n.trim().parse::<usize>().ok())
                    .is_some_and(|n| n > 0)
        });
        assert_eq!(
            witnessed, enabled,
            "layout witness admission must match ablation\n{stderr}"
        );
        let fused = stderr.lines().any(|line| {
            line.contains("LAYOUT-RECORD-WRITER")
                && line.ends_with("inline allocation witness reused")
        });
        assert_eq!(
            fused,
            enabled && inline_allocation,
            "only the actual eligible allocation/probe pair is fused\n{stderr}"
        );
        if !inline_allocation {
            assert!(
                stderr.contains("[native-array-constructor] fast count=1"),
                "collecting constructor fallback must actually execute\n{stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected.as_str());
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}

const RELOCATION_PROGRAM: &str = r#"
(defvar *layout-relocate* nil)
(defvar *layout-moved* 0)
(declaim (notinline layout-moving-writer layout-moving-address layout-moving-collect))
(defun layout-moving-address (object) (egcl-internal::%object-address object))
(defun layout-moving-collect (array)
  (when *layout-relocate*
    (let ((before (layout-moving-address array)))
      (egcl-ext:gc :full t)
      (unless (= before (layout-moving-address array)) (incf *layout-moved*))))
  nil)
(defun layout-moving-writer (words size)
  (declare (type fixnum size))
  (let ((output (make-array size :element-type '(unsigned-byte 8)))
        (start 0) (number 0))
    (layout-moving-collect output)
    (tagbody
      next-record
      (if (>= start size) (go finished))
      (let ((lane (aref words number)) (index start) (end (min (+ start 8) size)))
        (tagbody
          next-byte
          (if (>= index end) (go record-finished))
          (setf (aref output index) (ldb (byte 8 0) lane))
          (setq lane (ash lane -8))
          (setq index (1+ index))
          (go next-byte)
          record-finished))
      (setq start (+ start 8))
      (setq number (1+ number))
      (go next-record)
      finished)
    output))
(let ((words (make-array 18 :initial-element 18446744073709551615)))
  (dotimes (i 100) (layout-moving-writer words 137))
  (loop repeat 100 until (= 2 (egcl-ext:function-tier 'layout-moving-writer))
        do (layout-moving-writer words 137) (sleep 0.01))
  (assert (= 2 (egcl-ext:function-tier 'layout-moving-writer)))
  (setq *layout-relocate* t)
  (dotimes (i 8)
    (let ((output (layout-moving-writer words 137)))
      (assert (= (length output) 137))
      (dotimes (byte 137) (assert (= (aref output byte) 255)))))
  (assert (> *layout-moved* 0))
  (format *error-output* "LAYOUT-RELOCATION-COUNT ~D~%" *layout-moved*))
(format t "LAYOUT-ACTUAL-MOVEMENT-AND-BYTES-OK~%")
"#;

#[test]
fn array_layout_witness_keeps_relocated_output_valid_before_record_stores() {
    let mut reference: Option<String> = None;
    for enabled in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_T0_T1_THRESHOLD", "20")
            .env("EGCL_T2_THRESHOLD", "40")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_ARRAY_LAYOUT_LOG", "1")
            .env("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS", "1")
            .env_remove("EGCL_DISABLE_INLINE_U8_ALLOCATION")
            .env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_FORCE_TIER")
            .env_remove("EGCL_DISABLE_ARRAY_LAYOUT_FACTS")
            .env_remove("EGCL_DISABLE_OCTET_WORD_STORES")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .args(["--no-init", "--eval", RELOCATION_PROGRAM]);
        if !enabled {
            command.env("EGCL_DISABLE_ARRAY_LAYOUT_FACTS", "1");
        }
        let output = command.output().expect("run layout relocation probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("LAYOUT-ACTUAL-MOVEMENT-AND-BYTES-OK"));
        for line in stderr
            .lines()
            .filter(|line| line.starts_with("LAYOUT-RELOCATION-COUNT "))
        {
            eprintln!("enabled={enabled} {line}");
        }
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("LAYOUT-RELOCATION-COUNT ")
                    && line
                        .split_whitespace()
                        .last()
                        .and_then(|n| n.parse::<usize>().ok())
                        .is_some_and(|n| n > 0)),
            "an actual changed address is required\n{stderr}"
        );
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("LAYOUT-MOVING-WRITER: octet word stores: 1"))
        );
        assert!(stderr.lines().any(|line| line.contains("LAYOUT-MOVING-WRITER: optimized worker result installed with ordinary x86-64 Lisp T2 ABI")),
            "target must install the intended ordinary native path\n{stderr}");
        let witnessed = stderr.lines().any(|line| {
            line.contains("LAYOUT-MOVING-WRITER: array layout witnesses:")
                && line
                    .rsplit(':')
                    .next()
                    .and_then(|n| n.trim().parse::<usize>().ok())
                    .is_some_and(|n| n > 0)
        });
        assert_eq!(
            witnessed, enabled,
            "moving target must use the intended witness mode\n{stderr}"
        );
        let fused = stderr.lines().any(|line| {
            line.contains("LAYOUT-MOVING-WRITER")
                && line.ends_with("inline allocation witness reused")
        });
        assert_eq!(
            fused, enabled,
            "moving fixture must exercise the eligible native pair\n{stderr}"
        );
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected.as_str());
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
