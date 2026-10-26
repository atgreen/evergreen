// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

// This program also runs unchanged on SBCL as a semantic reference.
const PROGRAM: &str = r##"
(progn
  (defun u8-error-p (thunk)
    (handler-case (progn (funcall thunk) nil) (type-error () t)))
  (defun u8-check (array expected)
    (assert (equal (array-element-type array) '(unsigned-byte 8)))
    (assert (typep array '(vector (unsigned-byte 8))))
    (assert (not (typep array 'simple-vector)))
    (assert (equalp array expected)))
  (deftype public-octet () '(unsigned-byte 8))
  (assert (equal (upgraded-array-element-type 'public-octet) '(unsigned-byte 8)))
  (assert (equal (upgraded-array-element-type '(integer 0 255)) '(unsigned-byte 8)))
  (let ((a (make-array '(4) :element-type 'public-octet :initial-contents '(0 1 128 255))))
    (u8-check a #(0 1 128 255))
    (assert (typep a '(simple-array (unsigned-byte 8) (4))))
    (assert (typep a (type-of a)))
    (assert (string= (format nil "~S" a) "#(0 1 128 255)"))
    (let ((*print-base* 16) (*print-radix* t))
      (assert (string= (format nil "~S" a) (format nil "~S" #(0 1 128 255))))
      (assert (string= (write-to-string a) (write-to-string #(0 1 128 255)))))
    (assert (u8-error-p (lambda () (setf (aref a 0) 256))))
    (assert (u8-error-p (lambda () (setf (aref a 0) -1))))
    (assert (u8-error-p (lambda () (setf (aref a 0) '(not-an-octet)))))
    (assert (= (aref a 0) 0))
    (assert (u8-error-p (lambda () (aref a 4))))
    (assert (u8-error-p (lambda () (svref a 0))))
    (assert (u8-error-p (lambda () (setf (svref a 0) 7))))
    (let ((events nil) (general (vector 0)))
      (setf (svref (progn (push 1 events) general) (progn (push 2 events) 0))
            (progn (push 3 events) 7))
      (assert (= (svref general 0) 7))
      (assert (equal events '(3 2 1))))
    (assert (= (aref a 0) 0))
    (u8-check (copy-seq a) #(0 1 128 255))
    (u8-check (subseq a 1 3) #(1 128))
    (u8-check (reverse a) #(255 128 1 0))
    (u8-check (remove 1 a) #(0 128 255))
    (u8-check (substitute 9 1 a) #(0 9 128 255))
    (u8-check (make-sequence '(vector (unsigned-byte 8)) 3 :initial-element 7) #(7 7 7))
    (u8-check (make-sequence '(simple-array (unsigned-byte 8) (0)) 0) #())
    (u8-check (merge '(vector (unsigned-byte 8)) '(1 3) #(2 4) #'<) #(1 2 3 4))
    (u8-check (coerce '(3 4) '(vector (unsigned-byte 8))) #(3 4))
    (u8-check (map '(vector (unsigned-byte 8)) #'1+ #(0 1 2)) #(1 2 3))
    (u8-check (concatenate '(vector (integer 0 255)) a '(4)) #(0 1 128 255 4))
    (u8-check (remove-duplicates (make-array 4 :element-type '(unsigned-byte 8)
                                              :initial-contents '(1 2 1 3))) #(2 1 3))
    (let ((b (reverse a))) (sort b #'<) (u8-check b a))
    (let ((table (make-hash-table :test 'equalp)))
      (setf (gethash a table) :found)
      (assert (eq (gethash #(0 1 128 255) table) :found))))
  (let ((a (make-array 4 :element-type '(unsigned-byte 8) :adjustable t
                         :fill-pointer 2 :initial-element 7)))
    (assert (= (length a) 2))
    (assert (= (array-total-size a) 4))
    (assert (= (aref a 3) 7))
    (assert (u8-error-p (lambda () (elt a 3))))
    (assert (= (vector-push 8 a) 2))
    (assert (= (vector-push 9 a) 3))
    (assert (= (vector-push-extend 255 a 2) 4))
    (assert (= (vector-pop a) 255))
    (assert (eq a (adjust-array a 3 :fill-pointer 2 :initial-element 0)))
    (assert (= (array-total-size a) 3))
    (u8-check a #(7 7))
    (let ((b (make-array 2 :element-type '(unsigned-byte 8) :displaced-to a
                            :displaced-index-offset 1)))
      (assert (not (typep b 'simple-array)))
      (assert (eq (array-displacement b) a))
      (setf (aref b 1) 42)
      (assert (= (aref a 2) 42))
      (u8-check (copy-seq b) #(7 42))))
  (let* ((base (make-array 6 :element-type '(unsigned-byte 8) :initial-element 3))
         (a (make-array '(2 2) :element-type '(unsigned-byte 8)
                              :displaced-to base :displaced-index-offset 1)))
    (assert (equal (array-element-type a) '(unsigned-byte 8)))
    (assert (not (typep a 'simple-array)))
    (assert (eq (array-displacement a) base))
    (setf (aref a 1 1) 255)
    (assert (= (aref base 4) 255)))
  (let ((a (make-array '(2 2) :element-type '(unsigned-byte 8)
                             :initial-contents '((0 1) (128 255)))))
    (assert (typep a '(simple-array (unsigned-byte 8) (2 2))))
    (assert (= (aref a 1 1) 255))
    (let ((b (adjust-array a '(3 2) :initial-element 42)))
      (assert (equal (array-element-type b) '(unsigned-byte 8)))
      (assert (= (aref b 1 1) 255))
      (assert (= (aref b 2 1) 42))))
  (let ((a (make-array nil :element-type '(unsigned-byte 8) :initial-contents 255)))
    (assert (= (aref a) 255))
    (assert (typep a '(simple-array (unsigned-byte 8) ())))
    (assert (equal (array-element-type a) '(unsigned-byte 8))))
  (u8-check (make-array 0 :element-type '(unsigned-byte 8)) #())
  (assert (u8-error-p (lambda () (make-array 2 :element-type '(unsigned-byte 8)
                                             :initial-contents '(0 256)))))
  (format t "PUBLIC-U8-OK~%"))
"##;

// Standard readable output is checked by semantics, not SBCL-specific #A syntax.
// EGCL uses standard #. reconstruction and therefore expects *READ-EVAL* = T.
const PRINTER_PROGRAM: &str = r##"
(progn
  (defun u8-readable-check (a)
    (let ((*print-readably* t) (*print-base* 16) (*print-radix* t)
          (*print-array* nil) (*print-length* 1) (*read-eval* t))
      (dolist (text (list (write-to-string a :readably t)
                         (with-output-to-string (stream) (write a :stream stream :readably t))
                         (format nil "~S" a)))
        (let ((b (read-from-string text)))
          (assert (equal (array-element-type b) '(unsigned-byte 8)))
          (assert (equal (array-dimensions a) (array-dimensions b)))
          (dotimes (i (array-total-size a))
            (assert (= (row-major-aref a i) (row-major-aref b i))))
          ;; Preserving vector flags is an EGCL convenience beyond array
          ;; similarity: SBCL's readable #A copy omits these flags.
          #+egcl
          (progn
            (assert (eq (array-has-fill-pointer-p a) (array-has-fill-pointer-p b)))
            (when (array-has-fill-pointer-p a)
              (assert (= (fill-pointer a) (fill-pointer b))))
            (assert (eq (adjustable-array-p a) (adjustable-array-p b))))))
      (let* ((nested (list a (vector a)))
             (text (format nil "~S" nested))
             (copy (read-from-string text)))
        (assert (equal (array-element-type (car copy)) '(unsigned-byte 8)))
        (assert (equal (array-element-type (aref (cadr copy) 0)) '(unsigned-byte 8)))
        (dolist (b (list (car copy) (aref (cadr copy) 0)))
          (assert (equal (array-dimensions a) (array-dimensions b)))
          (dotimes (i (array-total-size a))
            (assert (= (row-major-aref a i) (row-major-aref b i))))))))
  (u8-readable-check (make-array 4 :element-type '(unsigned-byte 8) :initial-contents '(0 1 128 255)))
  (u8-readable-check (make-array '(2 2) :element-type '(unsigned-byte 8) :initial-contents '((0 1) (128 255))))
  (u8-readable-check (make-array nil :element-type '(unsigned-byte 8) :initial-contents 255))
  (u8-readable-check (make-array '(0 2) :element-type '(unsigned-byte 8)))
  (u8-readable-check (make-array 8 :element-type '(unsigned-byte 8) :fill-pointer 3 :adjustable t :initial-contents '(0 1 2 3 4 5 128 255)))
  (let* ((base (make-array 6 :element-type '(unsigned-byte 8) :initial-contents '(0 1 2 3 128 255)))
         (view (make-array 3 :element-type '(unsigned-byte 8) :displaced-to base :displaced-index-offset 3)))
    (u8-readable-check view))
  (format t "PUBLIC-U8-PRINTER-OK~%"))
"##;

#[test]
fn readable_octet_arrays_roundtrip_through_write_and_format() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", PRINTER_PROGRAM])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PUBLIC-U8-PRINTER-OK"));
}

#[test]
fn public_octet_arrays_preserve_types_options_and_sequence_semantics() {
    for tier in ["t0", "t1", "t2"] {
        run(tier, false);
    }
}

#[test]
fn public_octet_arrays_preserve_results_under_moving_gc_stress() {
    run("t2", true);
}

fn run(tier: &str, stress: bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.env("EGCL_FORCE_TIER", tier);
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tier={tier} stress={stress}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PUBLIC-U8-OK"));
}
