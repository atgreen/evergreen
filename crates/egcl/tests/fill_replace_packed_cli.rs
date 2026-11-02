// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FILL and REPLACE on bit-vectors, octet and double-float vectors, strings,
//! simple vectors and fill-pointer vectors (bliss-gg3i1.7.19). Native bodies
//! fill and copy whole words or bytes at a time; every result is checked
//! against an element-by-element reference over ranges that start and end on
//! and off byte and word boundaries, including overlapping copies within one
//! vector, and the element type errors are those (SETF ELT) signals.

use std::process::Command;

const PROGRAM: &str = r#"
(defun ref-fill (v item start end)
  (loop for i from start below end do (setf (aref v i) item))
  v)
(defun ref-replace (a b s1 s2 n)
  (let ((src (loop for i from s2 below (+ s2 n) collect (aref b i))))
    (loop for i from s1 for x in src do (setf (aref a i) x))
    a))
(defun pattern (type n)
  (let ((v (make-array n :element-type type)))
    (dotimes (i n v)
      (setf (aref v i)
            (cond ((eq type 'bit) (if (zerop (mod (* i 7) 3)) 1 0))
                  ((equal type '(unsigned-byte 8)) (mod (* i 37) 256))
                  ((eq type 'double-float) (* i 1.5d0))
                  ((eq type 'character) (code-char (+ 65 (mod i 26))))
                  (t (list i)))))))
(defvar *checks* 0)
(defun check (label got want)
  (incf *checks*)
  (unless (equalp got want)
    (format t "MISMATCH ~A~%  got  ~S~%  want ~S~%" label got want)
    (error "mismatch ~A" label)))
(dolist (spec '((bit 0 1) (bit 1 0) ((unsigned-byte 8) 200 7)
                (double-float 2.5d0 -1d0) (character #\z #\b) (t :x :y)))
  (destructuring-bind (type item item2) spec
    (dolist (n '(0 1 7 8 9 63 64 65 130))
      (dolist (start (remove-duplicates (list 0 1 3 7 8 9 (floor n 2) n)))
        (dolist (end (remove-duplicates (list start (min n (1+ start)) (min n (+ start 9))
                                              (min n (+ start 64)) n)))
          (when (<= start end n)
            (let ((a (pattern type n)) (b (pattern type n)))
              (check (list :fill type n start end item)
                     (fill a item :start start :end end)
                     (ref-fill b item start end))
              (check (list :fill-all type n item2) (fill (pattern type n) item2)
                     (ref-fill (pattern type n) item2 0 n)))
            ;; REPLACE from another vector and within the same one, both ways.
            (dolist (s2 (remove-duplicates (list 0 1 5 (max 0 (- n 9)))))
              (when (<= s2 n)
                (let ((count (min (- end start) (- n s2))))
                  (let ((a (pattern type n)) (b (pattern type n)) (src (fill (pattern type n) item)))
                    (check (list :replace type n start s2 count)
                           (replace a src :start1 start :end1 (+ start count) :start2 s2)
                           (ref-replace b (fill (pattern type n) item) start s2 count)))
                  (let ((a (pattern type n)) (b (pattern type n)))
                    (check (list :replace-self type n start s2 count)
                           (replace a a :start1 start :end1 (+ start count) :start2 s2)
                           (ref-replace b (copy-seq b) start s2 count))))))))))))
;; A base string and a fill-pointer vector take the same native path.
(let ((s (make-string 10 :element-type 'base-char :initial-element #\a)))
  (check :base-string (fill s #\q :start 2 :end 5) "aaqqqaaaaa"))
(let ((v (make-array 10 :fill-pointer 6 :initial-element 0)))
  (check :fill-pointer (coerce (fill v 9 :start 1) 'list) '(0 9 9 9 9 9))
  (check :fill-pointer-tail (aref v 7) 0))
(let ((v (make-array 20 :element-type 'bit :fill-pointer 12 :initial-element 0)))
  (fill v 1 :start 3)
  (check :bit-fill-pointer (list (count 1 v) (aref v 13)) '(9 0)))
;; Element type errors are (SETF ELT)'s, and only for a non-empty range.
(macrolet ((type-error-p (form)
             `(handler-case (progn ,form nil) (type-error () t))))
  (check :bit-2 (type-error-p (fill (make-array 9 :element-type 'bit) 2)) t)
  (check :octet-256 (type-error-p (fill (make-array 9 :element-type '(unsigned-byte 8)) 256)) t)
  (check :double-1 (type-error-p (fill (make-array 9 :element-type 'double-float) 1)) t)
  (check :base-lambda (type-error-p (fill (make-string 3 :element-type 'base-char)
                                          (code-char 955))) t)
  (check :string-1 (type-error-p (fill (make-string 3) 1)) t)
  (check :empty-range (fill (make-array 9 :element-type 'bit) 2 :start 4 :end 4)
         (make-array 9 :element-type 'bit)))
(format t "FILL-REPLACE-OK ~D~%" *checks*)
"#;

fn run(stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", PROGRAM]);
    if stress {
        command
            .env("EGCL_GC_STRESS", "7")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success() && stdout.contains("FILL-REPLACE-OK"),
        "stress={stress}\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn fill_and_replace_match_an_element_by_element_reference() {
    run(false);
}

#[test]
fn fill_and_replace_are_gc_safe_under_stress() {
    run(true);
}
