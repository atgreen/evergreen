// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const HEADER: &str = r#"
(defparameter *extent-element-type* (list 'unsigned-byte 8))
(defvar *extent-effects* 0)
(defvar *extent-last-array* nil)
(defvar *extent-relocate* nil)
(defvar *extent-moved* 0)
(declaim (notinline extent-direct-writer extent-list-writer extent-before-store extent-address))
(defun extent-address (object) (egcl-internal::%object-address object))
(defun extent-before-store (array)
  (incf *extent-effects*)
  (setq *extent-last-array* array)
  (when *extent-relocate*
    (let ((before (extent-address array)))
      (egcl-ext:gc :full t)
      (unless (= before (extent-address array)) (incf *extent-moved*))))
  nil)
"#;

// Two actual source producers: an immutable direct fixnum dimension, and a
// mutable proper singleton list. Their shared record loop is intentionally
// identical; only the first may obtain a static capacity waiver.
const WRITER: &str = r#"
(defun $NAME $PARAMETERS
  (declare (type fixnum size))
  (let ((output (make-array $DIMENSION :element-type *extent-element-type*))
        (start 0) (number 0))
    (extent-before-store output)
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
"#;

const CHECK: &str = r#"
(defun extent-check (words dimensions size listed general measured)
  (let ((effects *extent-effects*) (before (egcl-ext:deopt-count)))
    (let ((array (if listed (extent-list-writer words dimensions size)
                           (extent-direct-writer words size))))
      (assert (= *extent-effects* (1+ effects)))
      (when measured (assert (= before (egcl-ext:deopt-count))))
      (assert (= (length array) size))
      (if general (assert (simple-vector-p array))
          (assert (equal (array-element-type array) '(unsigned-byte 8))))
      (dotimes (i size)
        (assert (= (aref array i)
                   (ldb (byte 8 (* 8 (mod i 8))) (aref words (floor i 8)))))))))
(let ((words (make-array 40 :initial-element 18446744073709551615))
      (dimensions (list 136)))
  (dotimes (i 120)
    (extent-check words dimensions 136 nil nil nil)
    (extent-check words dimensions 136 t nil nil))
  (loop repeat 100 until (and (= 2 (egcl-ext:function-tier 'extent-direct-writer))
                              (= 2 (egcl-ext:function-tier 'extent-list-writer)))
        do (extent-check words dimensions 136 nil nil nil)
           (extent-check words dimensions 136 t nil nil) (sleep 0.01))
  (assert (= 2 (egcl-ext:function-tier 'extent-direct-writer)))
  (assert (= 2 (egcl-ext:function-tier 'extent-list-writer)))
  ;; Train both actual layouts and all supported scalar integer fallbacks.
  (setf (aref words 1) -18446744073709551615)
  (setf (aref words 2) (+ (ash 1 200) 81985529216486895))
  (setf (aref words 3) (- (+ (ash 1 200) 81985529216486895)))
  (let ((stable 0) (ready nil))
    (dotimes (batch 40)
      (unless ready
        (let ((before (egcl-ext:deopt-count)))
          (dolist (width '(8 64 8))
            (setf (car (cdr *extent-element-type*)) width)
            (dolist (size '(0 1 7 8 9 135 136 137 257))
              (setf (car dimensions) size)
              (extent-check words dimensions size nil (= width 64) nil)
              (extent-check words dimensions size t (= width 64) nil)))
          (sleep 0.01)
          (if (and (= before (egcl-ext:deopt-count))
                   (= 2 (egcl-ext:function-tier 'extent-direct-writer))
                   (= 2 (egcl-ext:function-tier 'extent-list-writer)))
              (incf stable) (setq stable 0))
          (when (>= stable 3) (setq ready t)))))
    (assert ready))
  (dolist (width '(8 64 8))
    (setf (car (cdr *extent-element-type*)) width)
    (dolist (size '(0 1 7 8 9 135 136 137 257))
      (setf (car dimensions) size)
      (extent-check words dimensions size nil (= width 64) t)
      (extent-check words dimensions size t (= width 64) t)))
  (format t "EXTENT-TIERS ~D ~D~%EXTENT-STEADY-DEOPTS 0~%"
          (egcl-ext:function-tier 'extent-direct-writer)
          (egcl-ext:function-tier 'extent-list-writer))
  ;; Allocation witness and scalar extent must survive a real collection
  ;; between their production and the first kernel; the helper alone owns the
  ;; address/counter arithmetic, preserving ordinary admission of the writer.
  (setq *extent-relocate* t)
  (dotimes (i 8) (extent-check words dimensions 137 nil nil t))
  (setq *extent-relocate* nil)
  (assert (> *extent-moved* 0))
  (format *error-output* "EXTENT-ACTUAL-RELOCATIONS ~D~%" *extent-moved*)
  ;; Mutating the dimensions list cannot inherit the unrelated loop's SIZE.
  ;; The checked fallback writes its legal prefix and then signals at index 2.
  (setf (car dimensions) 2)
  (let ((effects *extent-effects*))
    (assert (eq :error (handler-case
      (progn (extent-list-writer words dimensions 7) :no-error) (error () :error))))
    (assert (= *extent-effects* (1+ effects)))
    (assert (= (length *extent-last-array*) 2))
    (assert (= (aref *extent-last-array* 0) 255))
    (assert (= (aref *extent-last-array* 1) 255)))
  ;; A packed F64 result from the same private constructor is generic, even
  ;; when its dimension matches a previous successful U8 allocation.
  (let ((descriptor *extent-element-type*) (effects *extent-effects*))
    (setq *extent-element-type* 'double-float)
    (assert (eq :type-error (handler-case
      (progn (extent-direct-writer words 7) :no-error) (type-error () :type-error))))
    (assert (= *extent-effects* (1+ effects)))
    (assert (equal (array-element-type *extent-last-array*) 'double-float))
    (assert (every #'zerop *extent-last-array*))
    (setq *extent-element-type* descriptor)))
(format t "ALLOCATION-EXTENT-NATIVE-OUTPUT-OK~%")
"#;

fn program() -> String {
    let writer = |name: &str, parameters: &str, dimension: &str| {
        WRITER
            .replace("$NAME", name)
            .replace("$PARAMETERS", parameters)
            .replace("$DIMENSION", dimension)
    };
    HEADER.to_owned()
        + &writer("extent-direct-writer", "(words size)", "size")
        + &writer(
            "extent-list-writer",
            "(words dimensions size)",
            "dimensions",
        )
        + CHECK
}

fn installed_count(stderr: &str, owner: &str, field: &str) -> usize {
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.contains(owner) && line.contains("ordinary x86-64 Lisp T2 emitted;"))
        .unwrap_or_else(|| panic!("missing actual ordinary installation for {owner}\n{stderr}"));
    let prefix = format!("{field}=");
    line.split_once(prefix.as_str())
        .and_then(|(_, tail)| tail.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("missing installed field {field} in {line}"))
}

#[test]
fn allocation_extents_are_installed_proofs_with_moving_and_mutable_fallbacks() {
    let mut reference = None;
    let source = program();
    for enabled in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        for (key, _) in std::env::vars() {
            if key.starts_with("EGCL_") {
                command.env_remove(key);
            }
        }
        command
            .env("EGCL_T0_T1_THRESHOLD", "20")
            .env("EGCL_T2_THRESHOLD", "40")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_ALLOCATION_FACTS_LOG", "1")
            .env("EGCL_GC_POISON", "1");
        if !enabled {
            command.env("EGCL_DISABLE_ALLOCATION_EXTENTS", "1");
        }
        let output = command
            .args(["--no-init", "--eval", &source])
            .output()
            .expect("run extent facts regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("ALLOCATION-EXTENT-NATIVE-OUTPUT-OK"));
        assert!(stdout.contains("EXTENT-TIERS 2 2"));
        assert!(stdout.contains("EXTENT-STEADY-DEOPTS 0"));
        for owner in ["EXTENT-DIRECT-WRITER", "EXTENT-LIST-WRITER"] {
            assert!(
                stderr.lines().any(|line| line.contains(owner)
                    && line.contains(
                        "optimized worker result installed with ordinary x86-64 Lisp T2 ABI"
                    )),
                "{stderr}"
            );
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains(owner) && line.contains("octet word stores: 1")),
                "{stderr}"
            );
            let extents = installed_count(&stderr, owner, "allocation_extent_values");
            assert_eq!(extents > 0, enabled, "installed producer {owner}\n{stderr}");
            let proofs = installed_count(&stderr, owner, "extent_bounds_proofs");
            assert_eq!(
                proofs > 0,
                enabled && owner == "EXTENT-DIRECT-WRITER",
                "only the direct immutable dimension may waive bounds\n{stderr}"
            );
        }
        let moves = stderr
            .lines()
            .find_map(|line| {
                line.strip_prefix("EXTENT-ACTUAL-RELOCATIONS ")
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .unwrap_or(0);
        assert!(moves > 0, "must observe changed array addresses\n{stderr}");
        eprintln!("enabled={enabled} actual_relocations={moves}");
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected);
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
