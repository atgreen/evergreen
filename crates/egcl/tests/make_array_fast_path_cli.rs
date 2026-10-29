// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

/// MAKE-ARRAY builds a plain vector natively when its only options are
/// :ELEMENT-TYPE and :INITIAL-ELEMENT and the type upgrades to T or packed U8; filled BIT storage is also native. Every result
/// here is what the full option parsing gave before: the defaults (NIL for T,
/// 0 for integer types), option order and duplicates, and the requests that
/// keep the full path (character, double-float, unsupported :INITIAL-CONTENTS,
/// :FILL-POINTER, higher-rank dimensions, a negative length, an unknown option).
/// :INITIAL-CONTENTS from a list or vector is copied natively too; the wrong
/// length, an improper list, or contents with an element take the full path.
#[test]
fn make_array_fast_path_matches_the_full_option_parsing() {
    let program = r#"
(defun describe-array (a)
  (list (type-of a) (length a) (coerce a 'list)))
(format t "~&RESULT:~S~%"
  (list (describe-array (make-array 3))
        (describe-array (make-array 3 :element-type t))
        (describe-array (make-array 3 :element-type '(unsigned-byte 8)))
        (describe-array (make-array 3 :element-type '(signed-byte 32) :initial-element -1))
        (describe-array (make-array 3 :element-type 'fixnum))
        (describe-array (make-array 3 :element-type 'integer))
        (describe-array (make-array 3 :initial-element :x :element-type '(unsigned-byte 16)))
        (describe-array (make-array 3 :initial-element 1 :initial-element 2))
        (describe-array (make-array 3 :element-type '(unsigned-byte 1)))
        (describe-array (make-array 3 :element-type 'bit))
        (describe-array (make-array 3 :element-type 'character :initial-element #\a))
        (describe-array (make-array 3 :element-type 'double-float))
        (describe-array (make-array '(3) :initial-element 7))
        (describe-array (make-array 3 :initial-contents '(a b c)))
        (array-has-fill-pointer-p (make-array 3 :fill-pointer 1))
        (array-dimensions (make-array '(2 3) :initial-element 0))
        (describe-array (make-array 0))
        (handler-case (make-array -1) (error () :negative))
        (handler-case (make-array 3 :bogus 1) (error () :bad-key))))

(defun d2 (a) (list (type-of a) (length a) (coerce a 'list)))
(format t "~&CONTENTS:~S~%"
  (list (d2 (make-array 3 :initial-contents '(a b c)))
        (d2 (make-array 3 :element-type '(unsigned-byte 8) :initial-contents #(1 2 3)))
        (d2 (make-array 2 :initial-contents "xy"))
        (d2 (make-array 0 :initial-contents nil))
        (d2 (make-array 2 :initial-contents (make-array 2 :element-type 'double-float :initial-element 1d0)))
        (handler-case (make-array 3 :initial-contents '(a b)) (error () :short))
        (handler-case (make-array 2 :initial-contents '(a b . c)) (error () :improper))
        (handler-case (d2 (make-array 2 :initial-contents '(1 2) :initial-element 0)) (error () :both))
        (d2 (make-array 3 :element-type 'character :initial-contents "abc"))
        (d2 (make-array 4 :element-type 'bit :initial-contents '(1 0 1 1)))))    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {program})")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(
            r#"RESULT:(((SIMPLE-VECTOR 3) 3 (NIL NIL NIL)) ((SIMPLE-VECTOR 3) 3 (NIL NIL NIL)) ((SIMPLE-ARRAY (UNSIGNED-BYTE 8) (3)) 3 (0 0 0)) ((SIMPLE-VECTOR 3) 3 (-1 -1 -1)) ((SIMPLE-VECTOR 3) 3 (0 0 0)) ((SIMPLE-VECTOR 3) 3 (0 0 0)) ((SIMPLE-VECTOR 3) 3 (:X :X :X)) ((SIMPLE-VECTOR 3) 3 (1 1 1)) ((SIMPLE-BIT-VECTOR 3) 3 (0 0 0)) ((SIMPLE-BIT-VECTOR 3) 3 (0 0 0)) ((SIMPLE-ARRAY CHARACTER (3)) 3 (#\a #\a #\a)) ((SIMPLE-ARRAY DOUBLE-FLOAT (3)) 3 (0.0d0 0.0d0 0.0d0)) ((SIMPLE-VECTOR 3) 3 (7 7 7)) ((SIMPLE-VECTOR 3) 3 (A B C)) T (2 3) ((SIMPLE-VECTOR 0) 0 NIL) :NEGATIVE :BAD-KEY)"#
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            r#"CONTENTS:(((SIMPLE-VECTOR 3) 3 (A B C)) ((SIMPLE-ARRAY (UNSIGNED-BYTE 8) (3)) 3 (1 2 3)) ((SIMPLE-VECTOR 2) 2 (#\x #\y)) ((SIMPLE-VECTOR 0) 0 NIL) ((SIMPLE-VECTOR 2) 2 (1.0d0 1.0d0)) :SHORT :IMPROPER :BOTH ((SIMPLE-ARRAY CHARACTER (3)) 3 (#\a #\b #\c)) ((SIMPLE-BIT-VECTOR 4) 4 (1 0 1 1)))"#
        ),
        "{stdout}"
    );
}

#[test]
fn rank_one_dimension_lists_match_the_full_make_array_path() {
    let program = r#"
        (defun rank-one-description (array)
          (list (array-rank array) (array-dimensions array) (array-element-type array)
                (loop for i below (array-total-size array) collect (row-major-aref array i))))
        ;; Assert helper admission independently of public result equivalence.
        (let ((fast (egcl::%make-simple-array '(3) '(:element-type (unsigned-byte 8)))))
          __CHECK_ADMISSION__)
        (assert (egcl::%make-simple-array 3 '(:element-type (unsigned-byte 8))))
        (dolist (dimensions '(nil (2 3) (2 . 3) (-1) (1.5) ((2))))
          (assert (null (egcl::%make-simple-array dimensions nil))))
        (dolist (options '((:fill-pointer 0) (:adjustable t) (:displaced-to #(1 2 3))
                           (:element-type bit) (:element-type character)
                           (:element-type double-float) (:unsupported-option t)))
          (assert (null (egcl::%make-simple-array '(3) options))))
        (assert (null (egcl::%make-simple-array '(3) '(:initial-contents (1 2)))))
        (assert (null (egcl::%make-simple-array '(3) '(:initial-contents (1 2 3 . 4)))))
        (assert (null (egcl::%make-simple-array '(3)
                        '(:initial-contents (1 2 3) :initial-element 0))))
        (format t "RANK-ONE:~S~%"
          (list
            (rank-one-description (make-array '(0) :element-type '(unsigned-byte 8)))
            (rank-one-description (make-array '(3)))
            (rank-one-description (make-array '(3) :element-type '(unsigned-byte 8)))
            (rank-one-description (make-array '(3) :element-type '(signed-byte 32) :initial-element -1))
            (rank-one-description (make-array '(3) :element-type 'integer :initial-element :x))
            (rank-one-description (make-array '(3) :initial-element 7 :initial-element 9))
            (rank-one-description (make-array '(3) :element-type t :element-type 'character))
            (rank-one-description (make-array '(3) :element-type '(unsigned-byte 8) :initial-contents '(1 2 3)))
            (rank-one-description (make-array '(3) :initial-contents #(4 5 6)))
            (rank-one-description (make-array '(3) :initial-contents "xyz"))
            (rank-one-description (make-array '(3) :initial-contents
                                    (make-array 3 :element-type 'double-float :initial-element 2d0)))
            (rank-one-description (make-array '(3) :element-type 'bit))
            (rank-one-description (make-array '(3) :element-type 'character :initial-element #\x))
            (rank-one-description (make-array '(3) :element-type 'double-float :initial-element 1d0))
            (rank-one-description (make-array '(3) :fill-pointer 2 :initial-element 8))
            (rank-one-description (make-array '(3) :adjustable t :initial-element 6))
            (rank-one-description (make-array '(2) :displaced-to #(7 8 9) :displaced-index-offset 1))
            (rank-one-description (make-array nil :initial-contents 42))
            (rank-one-description (make-array '(2 2) :initial-element 4))
            (handler-case (make-array '(-1)) (type-error () :negative))
            (handler-case (make-array '(1.5)) (type-error () :noninteger))
            (handler-case (make-array '(3) :initial-contents '(1 2)) (error () :short))
            (handler-case (make-array '(2) :initial-element 0 :initial-contents '(1 2)) (error () :both))))
    "#;
    let mut results = Vec::new();
    for enabled in [false, true] {
        let admission = if enabled {
            "(assert fast) (assert (equal (array-element-type fast) '(unsigned-byte 8))) (assert (equal (coerce fast 'list) '(0 0 0)))"
        } else {
            "(assert (null fast))"
        };
        let case = program.replace("__CHECK_ADMISSION__", admission);
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", &case]);
        if enabled {
            command.env_remove("EGCL_DISABLE_RANK_ONE_ARRAY_FASTPATH");
        } else {
            command.env("EGCL_DISABLE_RANK_ONE_ARRAY_FASTPATH", "1");
        }
        let output = command.output().expect("run rank-one array case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("RANK-ONE:"), "{stdout}");
        results.push(stdout);
    }
    assert_eq!(
        results[0], results[1],
        "rank-one admission changes public array semantics"
    );
}

#[test]
fn rank_one_array_heap_fill_and_contents_survive_collecting_allocation() {
    let program = r#"
        (dotimes (i 30)
          (let* ((fill (list i "rank-one-fill"))
                 (array (make-array (list 12) :initial-element fill))
                 (contents (list (list i) (list (+ i 1)) (list (+ i 2)))))
            (assert (= (length array) 12))
            (dotimes (j 12) (assert (eq (aref array j) fill)))
            (let ((copy (make-array (list 3) :initial-contents contents)))
              (assert (equal (coerce copy 'list) contents))
              (dotimes (j 3) (assert (eq (aref copy j) (nth j contents)))))))
        ;; Reading packed doubles allocates boxed values. A failed octet
        ;; initialization must retain both the source and the error datum.
        (let ((caught 0))
          (dotimes (i 8)
            (handler-case
                (make-array (list 3) :element-type '(unsigned-byte 8)
                  :initial-contents
                    (make-array 3 :element-type 'double-float :initial-element 1d0))
              (type-error (condition)
                (assert (= (type-error-datum condition) 1d0))
                (incf caught))))
          (assert (= caught 8)))
        (format t "RANK-ONE-HEAP-OK~%")
    "#;
    let mut outputs = Vec::new();
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env_remove("EGCL_DISABLE_RANK_ONE_ARRAY_FASTPATH");
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command
            .output()
            .expect("run collecting rank-one array case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("RANK-ONE-HEAP-OK"), "{stdout}");
        outputs.push(stdout);
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn filled_bit_vectors_preserve_options_errors_and_storage_under_gc() {
    let program = r#"
      (dolist (dimension '(0 1 7 8 9 64))
        (dolist (fill '(0 1))
          (let ((array (make-array dimension :element-type 'bit :initial-element fill)))
            (assert (typep array 'simple-bit-vector))
            (assert (= (length array) dimension))
            (assert (every (lambda (value) (= value fill)) array)))))
      (let ((array (make-array '(9) :element-type 'bit :initial-element 1)))
        (setf (aref array 8) 0)
        (assert (= (aref array 8) 0))
        (assert (= (aref array 7) 1)))
      #+egcl (assert (every #'zerop (make-array 64 :element-type 'bit)))
      #+egcl (assert (every #'zerop (make-array 64 :element-type '(unsigned-byte 1))))
      (assert (every (lambda (value) (= value 1))
                     (make-array 9 :initial-element 1 :initial-element 2 :element-type 'bit)))
      (assert (typep (make-array 9 :element-type 'bit :element-type t) 'simple-bit-vector))
      (assert (every #'zerop
                     (make-array 9 :element-type 'bit :unknown 42 :allow-other-keys t)))
      (assert (handler-case
                (progn (make-array 9 :element-type 'bit :unknown 42
                                   :allow-other-keys nil :allow-other-keys t) nil)
                (program-error () t)))
      (assert (handler-case (progn (apply (symbol-function 'make-array) (list 1 :element-type 'bit :initial-element 2)) nil)
                (type-error () t)))
      (assert (handler-case (progn (make-array 1 :element-type 'bit :initial-element) nil)
                (program-error () t)))
      (assert (handler-case (progn (make-array -1 :element-type 'bit) nil)
                (type-error () t)))
      (assert (equal (coerce (make-array 3 :element-type 'bit :initial-contents '(1 0 1)) 'list)
                     '(1 0 1)))
      (assert (equal (array-dimensions (make-array '(2 3) :element-type 'bit :initial-element 1))
                     '(2 3)))
      (let ((array (make-array 4 :element-type 'bit :adjustable t :fill-pointer 2
                              :initial-element 1)))
        (assert (adjustable-array-p array))
        (assert (= (fill-pointer array) 2)))
      (let* ((base (make-array 4 :element-type 'bit :initial-element 1))
             (view (make-array 2 :element-type 'bit :displaced-to base :displaced-index-offset 1)))
        (setf (aref view 0) 0)
        (assert (= (aref base 1) 0)))
      (defpackage :foreign-bit-type (:use :cl) (:shadow :bit))
      (let ((foreign (intern "BIT" :foreign-bit-type)))
        (eval `(deftype ,foreign () '(unsigned-byte 8)))
        (assert (not (typep (make-array 2 :element-type foreign) 'bit-vector)))
        #+egcl (assert (null (egcl::%make-simple-array 3 (list :element-type foreign)))))
      #+egcl
      (progn
        (assert (typep (egcl::%make-simple-array 64 '(:element-type bit :initial-element 0))
                       'simple-bit-vector))
        (assert (typep (egcl::%make-simple-array '(9) '(:element-type bit :initial-element 1))
                       'simple-bit-vector))
        (assert (null (egcl::%make-simple-array 3 '(:element-type bit :initial-contents (1 0 1)))))
)
      (write-line "FILLED-BIT-ARRAYS-OK")
    "#;
    let mut outputs = Vec::new();
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", &format!("(progn {program})")]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("FILLED-BIT-ARRAYS-OK"), "{stdout}");
        outputs.push(stdout);
    }
    assert_eq!(outputs[0], outputs[1]);
}
