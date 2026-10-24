// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! STRING, VECTOR, FIND-PACKAGE, GENSYM and the compiled SETF store
//! primitives (EGCL::SET-AREF/SET-ELT/SET-CAR/SET-CDR) have evaluated-argument
//! entries, so compiled code calls them directly instead of synthesizing and
//! re-evaluating a form (bliss-qoy0k). Their results must not change; the
//! expected line is what main printed before the change.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defun probe ()
  (let ((v (vector 1 2 3)) (s (make-string 3 :initial-element #\a)) (l (list 1 2 3))
        (m (make-array '(2 2) :initial-element 0)) (c (cons 1 2)) (str "abc"))
    (setf (aref v 1) 9) (setf (svref v 2) 8) (setf (char s 0) #\z) (setf (elt l 2) 7)
    (setf (aref m 1 1) 5) (setf (car c) 'x) (setf (cdr c) 'y)
    (list v s l m c
          (eq (string str) str) (string 'foo) (string #\q)
          (handler-case (funcall #'string) (program-error () :pe))
          (handler-case (string 12) (type-error () :te) (error () :err))
          (vector) (vector 1 'a "b") (funcall #'vector 1 2)
          (package-name (find-package "CL")) (find-package "NO-SUCH-PKG-QOY")
          (package-name (find-package :keyword)) (eq (find-package (find-package "CL-USER")) (find-package "CL-USER"))
          (let ((*gensym-counter* 40)) (list (symbol-name (gensym)) (symbol-name (gensym "P")) (symbol-name (gensym 7)) *gensym-counter*))
          (symbol-package (gensym))
          (handler-case (gensym -1) (type-error () :te))
          (handler-case (setf (elt l 9) 0) (type-error () :te))
          (handler-case (let ((x (vector 1))) (setf (aref x 5) 0)) (error () :err)))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe))
)"#;

#[test]
fn builtins_called_from_compiled_code_keep_their_results() {
    for tier in ["interp", "t0", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains(
                "RESULT (#(1 9 8) \"zaa\" (1 2 7) #2A((0 0) (0 5)) (X . Y) T \"FOO\" \"q\" :PE \"12\" \
                 #() #(1 A \"b\") #(1 2) \"COMMON-LISP\" NIL \"KEYWORD\" T (\"G40\" \"P41\" \"G7\" 42) \
                 NIL :TE :TE :ERR)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}

const PROGRAM_2: &str = r##"(progn
(defun probe2 ()
  (let ((l (list (list 1 'a) (list 2 'b) (list 3 'c)))
        (v (make-array 5 :adjustable t :fill-pointer 2))
        (d (make-array 3 :displaced-to (make-array 6) :displaced-index-offset 2)))
    (list (member 2 l :key #'car) (member '(2 b) l :test #'equal) (member 2 l :key #'car :test-not #'=)
          (member 9 l :key (lambda (e) (car e)) :test (lambda (a b) (eql a b)))
          (handler-case (member 1 '(1) :bogus 3) (program-error () :pe))
          (member 1 '(1 2) :bogus 3 :allow-other-keys t)
          (handler-case (member 1 '(1) :key) (program-error () :pe))
          (handler-case (member 5 '(1 . 2) :test #'eql) (type-error (c) (type-error-datum c)))
          (assoc 2 l :test #'=) (assoc 'b '((a . 1) nil (b . 2)) :test #'eq)
          (assoc 2 '((1 . a) (2 . b)) :key #'1+)
          (handler-case (assoc 1 '(5) :test #'eql) (type-error (c) (type-error-datum c)))
          (mapcar #'car l) (mapcar #'+ '(1 2 3) '(10 20)) (mapcar #'list '(1) '(2) '(3))
          (handler-case (mapcar #'car) (program-error () :pe))
          (handler-case (mapcar #'1+ '(1 . 2)) (type-error (c) (type-error-datum c)))
          (map 'list #'1+ '(1 2)) (map 'vector #'cons '(1 2) #(a b c)) (map 'string #'char-upcase "ab")
          (map nil #'1+ '(1)) (handler-case (map 'list #'1+) (program-error () :pe))
          (evenp 4) (oddp (1+ (expt 2 70))) (handler-case (oddp 1.5) (type-error (c) (type-error-datum c)))
          (handler-case (funcall #'evenp 1 2) (program-error () :pe))
          (type-of 1) (type-of 7) (type-of "ab") (type-of #*10) (type-of :k) (type-of (make-array '(2 2)))
          (handler-case (funcall #'type-of) (program-error () :pe))
          (boundp '*print-base*) (boundp 'no-such-var-qoy) (handler-case (boundp 3) (type-error () :te))
          (bit-vector-p #*1) (simple-bit-vector-p "1")
          (string= "xabcy" "abc" :start1 1 :end1 4) (string< "abc" "abd") (string> "abc" "abd")
          (handler-case (string= "a" "a" :start3 1) (program-error () :pe))
          (hash-table-test (make-hash-table :test #'equal)) (hash-table-test (make-hash-table))
          (format nil "~a/~s" 1 "x") (format nil (formatter "<~a>") 2)
          (adjustable-array-p v) (array-has-fill-pointer-p v) (multiple-value-list (array-displacement d))
          (array-rank (make-array '(2 2))) (array-dimension (make-array '(2 3)) 1)
          (handler-case (array-dimension v 1) (program-error () :pe))
          (array-element-type "a") (namestring "/tmp/qoy.lisp") (probe-file "/nonexistent-qoy/x")
          (class-name (find-class 'standard-object)) (find-class 'no-such-class-qoy nil)
          (handler-case (find-class 'no-such-class-qoy) (error () :err))
          (symbol-value '*print-base*) (handler-case (symbol-value 'no-such-var-qoy) (unbound-variable () :ub))
          (/ 1 3) (/ 4) (/ 3.0d0 2) (/ #c(2 4) 2) (handler-case (/ 1 0) (division-by-zero () :dz))
          (handler-case (/) (program-error () :pe))
          (float 1/2 1d0) (complex 1 0) (complex 1d0 2) (realpart #c(3 4)) (imagpart 2d0)
          (numerator 6/4) (denominator 6/4) (rational 0.5d0) (sqrt -4d0) (sin 0d0) (exp 0d0)
          (multiple-value-list (decode-float 1.5d0)) (handler-case (funcall #'sqrt 1 2) (program-error () :pe))
          (random 1) (handler-case (random -1) (type-error () :te)) (handler-case (funcall #'random) (program-error () :pe))
          (concatenate 'string "ab" (list #\c)) (concatenate 'list #(1) '(2))
          (let ((s (make-string-output-stream))) (write-string "hi" s) (get-output-stream-string s))
          (make-array 3 :element-type 'bit :initial-contents '(1 0 1)))))
(dotimes (i 20) (probe2))
(format t "~&RESULT ~s~%" (probe2))
)"##;

/// The second batch (bliss-qoy0k): MEMBER/ASSOC keywords, MAPCAR, MAP, the
/// numeric predicates and functions, TYPE-OF, BOUNDP, STRING=, FORMAT, the
/// array and pathname builtins and their errors. The expected line is what
/// main printed before the change, in every tier.
#[test]
fn second_batch_of_builtins_keep_their_results() {
    for tier in ["interp", "t0", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM_2])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains(r##"RESULT (((2 B) (3 C)) ((2 B) (3 C)) ((1 A) (2 B) (3 C)) NIL :PE (1 2) :PE 2 (2 B) (B . 2) (1 . A) 5 (1 2 3) (11 22) ((1 2 3)) :PE 2 (2 3) #((1 . A) (2 . B)) "AB" NIL :PE T T 1.5 :PE BIT (INTEGER 0 1152921504606846975) (SIMPLE-ARRAY CHARACTER (2)) (SIMPLE-BIT-VECTOR 2) KEYWORD (SIMPLE-ARRAY T (2 2)) :PE T NIL :TE T NIL T 2 NIL :PE EQUAL EQL "1/\"x\"" "<2>" T T (#(NIL NIL NIL NIL NIL NIL) 2) 2 3 :PE CHARACTER "/tmp/qoy.lisp" NIL STANDARD-OBJECT NIL :ERR 10 :UB 1/3 1/4 1.5d0 #C(1 2) :DZ :PE 0.5d0 1 #C(1.0d0 2.0d0) 3 0.0d0 3 2 1/2 #C(0.0d0 2.0d0) 0.0d0 1.0d0 (0.75d0 1 1.0d0) :PE 0 :TE :PE "abc" (1 2) "hi" #*101)"##),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
