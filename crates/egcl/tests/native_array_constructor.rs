// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
    ;; Constant binding does not make the contents of its cons immutable.
    (defconstant +native-array-descriptor+ (list 'unsigned-byte 8))
    (defvar *native-array-effects* 0)
    (declaim (notinline native-constructor-array native-constructor-bare))
    (defun native-constructor-array (n)
      (let ((result (make-array (list n) :element-type +native-array-descriptor+))) result))
    (defun native-constructor-bare (n)
      (let ((result (make-array n :element-type +native-array-descriptor+))) result))
    (defun native-constructor-effect (n)
      (let ((result (make-array (progn (incf *native-array-effects*) (list n))
                               :element-type +native-array-descriptor+))) result))
    (defun opaque-constructor-array (n)
      (declare (notinline make-array))
      (let ((result (make-array (list n) :element-type +native-array-descriptor+))) result))
    (defun describe-native-array (array)
      (list (array-dimensions array) (array-element-type array)
            (loop for i below (array-total-size array) collect (row-major-aref array i))))
    (format t "OPAQUE-BEGIN~%")
    (disassemble #'opaque-constructor-array)
    (format t "OPAQUE-END~%")
    (dotimes (i 150)
      (let ((array (native-constructor-array 3)))
        (assert (equal (coerce array 'list) '(0 0 0)))
        (assert (typep array '(simple-array (unsigned-byte 8) (*))))
        (assert (not (simple-vector-p array))))
      (assert (= (length (native-constructor-bare 3)) 3))
      (assert (= (length (native-constructor-effect 3)) 3))
      (assert (= (length (opaque-constructor-array 3)) 3)))
    (assert (= (egcl-ext:function-tier 'native-constructor-array) 2))
    (assert (= (egcl-ext:function-tier 'native-constructor-bare) 2))
    (let ((before (egcl-ext:deopt-count)))
      (dotimes (i 40) (native-constructor-array 3) (native-constructor-bare 3))
      (assert (= (egcl-ext:deopt-count) before)))
    (assert (= (length (multiple-value-list (native-constructor-array 0))) 1))
    (assert (= (length (multiple-value-list
                       (progn (values 1 2 3) (native-constructor-bare 8)))) 1))
    (format t "NATIVE-ARRAY:~S~%"
      (list (describe-native-array (native-constructor-array 0))
            (describe-native-array (native-constructor-array 3))
            (describe-native-array (native-constructor-bare 3))))
    ;; Above the bounded inline cap, exercise the collecting private adapter.
    (let ((array (native-constructor-bare 257)))
      (assert (= (length array) 257))
      (assert (every #'zerop array)))
    ;; Width mutation must be observed by the runtime validation, rather than
    ;; trusting an earlier compiler snapshot of the heap constant descriptor.
    (rplaca +native-array-descriptor+ 'double-float)
    (rplacd +native-array-descriptor+ nil)
    (let ((array (native-constructor-array 3))
          (bare (native-constructor-bare 3)))
      (assert (eq (array-element-type bare) 'double-float))
      (assert (every #'zerop bare))
      (assert (eq (array-element-type array) 'double-float))
      (assert (every #'zerop array))
      (format t "MUTATED-ARRAY:~S~%" (describe-native-array array)))
    (rplaca +native-array-descriptor+ 'unsigned-byte)
    (rplacd +native-array-descriptor+ (list 8))
    (assert (typep (native-constructor-array 3) '(simple-array (unsigned-byte 8) (3))))
    ;; Non-rank-one dimensions and errors run the original public semantics.
    (assert (equal (array-dimensions (native-constructor-bare '(2 2))) '(2 2)))
    (assert (= (array-rank (native-constructor-bare nil)) 0))
    (setf *native-array-effects* 0)
    (let ((caught nil))
      (handler-case (native-constructor-effect -1)
        (type-error () (setf caught t)))
      (assert caught)
      (assert (= *native-array-effects* 1)))
    (let ((caught nil))
      (handler-case (native-constructor-array 1.5)
        (type-error () (setf caught t)))
      (assert caught))
    (dolist (dimension (list -1 1.5))
      (let ((caught nil))
        (handler-case (native-constructor-bare dimension)
          (type-error () (setf caught t)))
        (assert caught)))
    (assert (eq (flet ((make-array (dims &rest options)
                       (declare (ignore dims options)) :lexical))
                  (make-array '(3) :element-type '(unsigned-byte 8))) :lexical))
    (format t "NATIVE-ARRAY-CONSTRUCTOR-OK~%")
"#;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn native_array_callsite_adapter_preserves_mutable_types_fallbacks_errors_and_effects() {
    let mut results = Vec::new();
    for enabled in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS", "1")
            .args(["--no-init", "--eval", PROGRAM]);
        if enabled {
            command.env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR");
        } else {
            command.env("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR", "1");
        }
        let output = command.output().expect("run native constructor");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("NATIVE-ARRAY-CONSTRUCTOR-OK"), "{stdout}");
        let opaque = stdout
            .split("OPAQUE-BEGIN")
            .nth(1)
            .unwrap()
            .split("OPAQUE-END")
            .next()
            .unwrap();
        assert!(
            opaque.contains("LoadFunction(") && opaque.contains("FUNCALL"),
            "NOTINLINE constructor did not retain its opaque function call:\n{opaque}"
        );
        if enabled {
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("lowered ")
                        && line.contains("NATIVE-CONSTRUCTOR-ARRAY")),
                "constructor adapter was not emitted:\n{stderr}"
            );
            assert!(stderr.lines().any(|line| line.contains("NATIVE-CONSTRUCTOR-ARRAY: optimized worker result installed with ordinary x86-64 Lisp T2 ABI")),
                "constructor caller did not install ordinary native code:\n{stderr}");
            assert!(
                stderr.contains("[native-array-constructor] fast count=1"),
                "no actual fast adapter execution:\n{stderr}"
            );
            assert!(
                stderr.contains("[native-array-constructor] fallback count=1"),
                "no actual fallback execution:\n{stderr}"
            );
            assert!(
                !stderr
                    .lines()
                    .any(|line| line.contains("lowered ")
                        && line.contains("OPAQUE-CONSTRUCTOR-ARRAY")),
                "NOTINLINE constructor was expanded:\n{stderr}"
            );
        } else {
            assert!(
                !stderr.contains("[native-array-constructor]"),
                "disabled adapter ran:\n{stderr}"
            );
        }
        results.push(stdout);
    }
    assert_eq!(
        results[0], results[1],
        "native adapter changes public semantics"
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn native_constructor_mixed_live_heap_values_survive_gc_stress_and_fallback() {
    let program = r#"
        (defconstant +stress-array-type+ (list 'unsigned-byte 8))
        (defun stress-native-constructor (n marker)
          (let ((array (make-array (list n) :element-type +stress-array-type+)))
            (let ((result (list array marker))) result)))
        (dotimes (i 120) (stress-native-constructor 3 (list "retained" (list 7 11))))
        (assert (= (egcl-ext:function-tier 'stress-native-constructor) 2))
        (dotimes (i 12)
          (let ((marker (list (copy-seq "retained") (list 7 11))))
            (let* ((result (stress-native-constructor 5 marker))
                   (array (first result)) (retained (second result)))
              (assert (eq retained marker))
              (assert (equal retained '("retained" (7 11))))
              (assert (equal (coerce array 'list) '(0 0 0 0 0))))))
        ;; The bounded path must retain the collecting adapter above its cap.
        (let ((result (stress-native-constructor 257 (list 7 11))))
          (assert (= (length (first result)) 257)))
        (rplaca +stress-array-type+ 'double-float)
        (rplacd +stress-array-type+ nil)
        (dotimes (i 6)
          (let ((marker (list (copy-seq "retained") (list 7 11))))
            (let* ((result (stress-native-constructor 5 marker))
                   (array (first result)) (retained (second result)))
              (assert (eq retained marker))
              (assert (equal retained '("retained" (7 11))))
              (assert (eq (array-element-type array) 'double-float)))))
        (format t "NATIVE-CONSTRUCTOR-HEAP-OK~%")
    "#;
    let mut results = Vec::new();
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS", "1")
            .args(["--no-init", "--eval", program]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run native constructor roots");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("NATIVE-CONSTRUCTOR-HEAP-OK"));
        assert!(stderr.lines().any(|line| line.contains("lowered ") && line.contains("STRESS-NATIVE-CONSTRUCTOR")),
            "stress did not lower the intended constructor:\n{stderr}");
        assert!(stderr.lines().any(|line| line.contains("STRESS-NATIVE-CONSTRUCTOR: optimized worker result installed with ordinary x86-64 Lisp T2 ABI")),
            "stress constructor did not install ordinary native code:\n{stderr}");
        assert!(
            stderr.contains("[native-array-constructor] fast count=1"),
            "no stress fast adapter execution:\n{stderr}"
        );
        results.push(stdout);
    }
    assert_eq!(results[0], results[1]);
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn native_filled_bit_constructor_reaches_public_fast_storage_under_gc() {
    let program = r#"(progn
      (declaim (notinline native-filled-bit-array))
      (defun native-filled-bit-array (n)
        (let ((array (make-array n :element-type 'bit))) array))
      (dotimes (i 150)
        (let ((array (native-filled-bit-array 64)))
          (assert (typep array 'simple-bit-vector))
          (assert (= (length array) 64))
          (assert (every #'zerop array))))
      (assert (egcl::%make-simple-array 64 '(:element-type bit :initial-element 0)))
      (write-line "NATIVE-FILLED-BIT-OK"))"#;
    let mut results = Vec::new();
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS", "1")
            .env_remove("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR")
            .args(["--no-init", "--eval", program]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("NATIVE-FILLED-BIT-OK"), "{stdout}");
        assert!(stderr.lines().any(|line| line.contains("NATIVE-FILLED-BIT-ARRAY: optimized worker result installed with ordinary x86-64 Lisp T2 ABI")), "{stderr}");
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("lowered NATIVE-FILLED-BIT-ARRAY")),
            "native BIT constructor site was not emitted: {stderr}"
        );
        assert!(
            stderr.contains("[native-array-constructor] fallback count=1"),
            "the actual native adapter must reach the public BIT constructor: {stderr}"
        );
        results.push(stdout);
    }
    assert_eq!(results[0], results[1]);
}
