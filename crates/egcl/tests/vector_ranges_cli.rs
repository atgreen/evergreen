// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
  (defun fill-native-range (vector start end)
    (declare (type fixnum start end))
    (let ((result (do ((i start (1+ i))) ((>= i end) vector)
                    (setf (aref vector i) i)))) result))
  (defun fill-native-value (vector end value)
    (declare (type fixnum end))
    (let ((result (do ((i 0 (1+ i))) ((>= i end) vector)
                    (setf (aref vector i) value)))) result))
  (dotimes (i 200)
    (fill-native-range (make-array 8 :initial-element -1) 0 8)
    (fill-native-value (make-array 8 :initial-element -1) 8 42))
  (dolist (name '(fill-native-range fill-native-value))
    (loop repeat 60 until (= (egcl-ext:function-tier name) 2)
          do (fill-native-range (make-array 8) 0 8)
             (fill-native-value (make-array 8) 8 42) (sleep 0.01))
    (assert (= (egcl-ext:function-tier name) 2)))
  (dolist (limits '((0 0) (0 1) (0 7) (2 5) (7 7) (6 3) (0 -1)))
    (let* ((array (make-array 7 :initial-element -1))
           (start (first limits)) (end (second limits))
           (before (egcl-ext:deopt-count)))
      (assert (eq (fill-native-range array start end) array))
      (assert (= before (egcl-ext:deopt-count)))
      (dotimes (i 7)
        (assert (= (aref array i) (if (and (>= i start) (< i end)) i -1))))
      (format t "RANGE ~S ~S~%" limits (coerce array 'list))))
  ;; Invalid vectors are irrelevant for zero-trip loops.
  (assert (= (fill-native-range 42 4 2) 42))
  ;; Failed predicates must use the original checked loop: preserve all valid
  ;; prefix writes, then signal at the original first invalid access.
  (dolist (limits '((-1 2) (0 5)))
    (let ((array (make-array 3 :initial-element -1)) (caught nil))
      (handler-case (fill-native-range array (first limits) (second limits))
        (error (condition) (setf caught condition)))
      (assert caught)
      (assert (equal (coerce array 'list)
                     (if (< (first limits) 0) '(-1 -1 -1) '(0 1 2))))
      (format t "RANGE-ERROR ~S ~S~%" limits (type-of caught))))
  ;; Packed arrays take the checked route and retain their element semantics.
  (let* ((array (make-array 7 :element-type 'double-float :initial-element 0d0))
         (before (egcl-ext:deopt-count)))
    (fill-native-value array 7 2.5d0)
    (assert (= before (egcl-ext:deopt-count)))
    (assert (every (lambda (x) (= x 2.5d0)) array)))
  ;; Heap values exercise the same old-reference and remembered-set barriers
  ;; as the checked store, in both directions, with no guessed value type.
  (let* ((marker (list (copy-seq "retained") (list 7 11)))
         (array (make-array 7 :initial-element marker))
         (before (egcl-ext:deopt-count)))
    (fill-native-range array 0 7)
    (assert (= before (egcl-ext:deopt-count)))
    (setf before (egcl-ext:deopt-count))
    (fill-native-value array 7 marker)
    (assert (= before (egcl-ext:deopt-count)))
    (dotimes (i 7) (assert (eq (aref array i) marker)))
    (assert (equal marker '("retained" (7 11)))))
  (format t "VECTOR-RANGES-OK~%")
"#;

#[test]
fn native_counted_ranges_keep_checked_fallback_effects_and_gc_roots() {
    let mut baseline = None;
    for (enabled, stress) in [(false, false), (true, false), (true, true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .args(["--no-init", "--eval", PROGRAM]);
        if enabled {
            command.env_remove("EGCL_DISABLE_VECTOR_RANGES");
        } else {
            command.env("EGCL_DISABLE_VECTOR_RANGES", "1");
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run native vector ranges");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("VECTOR-RANGES-OK"), "{stdout}");
        if enabled {
            for name in ["FILL-NATIVE-RANGE", "FILL-NATIVE-VALUE"] {
                assert!(
                    stderr.lines().any(|line| line.contains(name)
                        && line.contains("vector range versions: 1 stores=1")),
                    "{name} did not version a counted loop:\n{stderr}"
                );
                assert!(
                    stderr.lines().any(|line| line.contains(name)
                        && line.contains(
                            "optimized worker result installed with ordinary x86-64 Lisp T2 ABI"
                        )),
                    "{name} did not install ordinary native code:\n{stderr}"
                );
            }
        }
        if let Some(expected) = &baseline {
            assert_eq!(&stdout, expected, "range versioning changes output");
        } else {
            baseline = Some(stdout);
        }
    }
}
