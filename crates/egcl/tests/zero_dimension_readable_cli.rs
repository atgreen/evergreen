// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline empty-array-readable-text))
(defun empty-array-readable-text (array) (write-to-string array :readably t))
(let ((probe (make-array '(0 2))))
  (dotimes (i 80) (empty-array-readable-text probe))
  (when *empty-array-required-tier*
    (loop repeat 100
          until (= *empty-array-required-tier* (egcl-ext:function-tier 'empty-array-readable-text))
          do (empty-array-readable-text probe) (sleep 0.01))
    (assert (= *empty-array-required-tier* (egcl-ext:function-tier 'empty-array-readable-text)))))
(defun empty-array-fill (type)
  (cond ((member type '(base-char character)) #\a)
        ((eq type 'double-float) 1.0d0) ((eq type t) nil) (t 0)))
(defun make-empty-case (dims type adjustable displaced)
  (if displaced
      (make-array dims :element-type type :adjustable adjustable
        :displaced-to (make-array 8 :element-type type :initial-element (empty-array-fill type))
        :displaced-index-offset 1)
      (make-array dims :element-type type :adjustable adjustable)))
(defun empty-array-renderings (array)
  (list (empty-array-readable-text array) (prin1-to-string array)
        (format nil "~S" array)
        (with-output-to-string (stream) (write array :stream stream :readably t))
        (with-output-to-string (stream) (prin1 array stream))))
(defun check-empty-readback (array)
  (dolist (text (empty-array-renderings array))
    (let ((copy (read-from-string text)))
      (assert (equal (array-dimensions copy) (array-dimensions array)))
      (assert (= (array-rank copy) (array-rank array)))
      (assert (equal (array-element-type copy) (array-element-type array)))
      (assert (eq (adjustable-array-p copy) (adjustable-array-p array)))
      (assert (= (array-total-size copy) 0)))))
(let ((*print-readably* t) (*print-array* nil))
  (dolist (dims '((0 2) (2 0 3) (2 0) (0 0) (0 2 0) (2 0 0)))
    (dolist (type '(t bit base-char character double-float (unsigned-byte 8)))
      (dolist (adjustable '(nil t))
        (dolist (displaced '(nil t))
          (check-empty-readback (make-empty-case dims type adjustable displaced))))))
  (let ((*print-pretty* t) (*print-circle* t))
    (check-empty-readback (make-empty-case '(2 0 3) 'double-float t t)))
  (dolist (base '(2 16))
    (let ((*print-base* base) (*read-base* base) (*print-case* :downcase))
      (check-empty-readback (make-empty-case '(0 3) '(unsigned-byte 8) t nil)))))
;; Ordinary display retains standard #nA syntax. General rank-zero arrays and
;; string printing are unchanged; specialized nonempty rank-zero metadata is
;; tracked separately rather than hidden by a weaker round-trip assertion.
(let ((*print-readably* nil) (*print-array* t) (*print-pretty* nil) (*print-circle* nil))
  (assert (equal (write-to-string (make-array '(0 2))) "#2A()"))
  (assert (equal (prin1-to-string (make-array '(2 0))) "#2A(() ())"))
  (let ((array (make-array '(0 2))))
    (assert (equal (princ-to-string array) "#2A()"))
    (assert (equal (format nil "~A" array) "#2A()"))))
(let* ((array (make-array nil :initial-element 'rank-zero-control))
       (copy (read-from-string (empty-array-readable-text array))))
  (assert (null (array-dimensions copy)))
  (assert (eq (array-element-type copy) (array-element-type array)))
  (assert (eq (adjustable-array-p copy) (adjustable-array-p array)))
  (assert (eq (row-major-aref copy 0) 'rank-zero-control)))
(let ((*print-readably* t) (*print-array* nil))
  (assert (equal (read-from-string (prin1-to-string "text")) "text")))

;; Read-time constructor syntax is unavailable when READ-EVAL is NIL.
(dolist (pretty '(nil t))
  (let ((*print-readably* t) (*read-eval* nil) (*print-pretty* pretty))
    (dolist (array (list (make-array '(0 2))
                        (make-array '(2 0 3) :element-type 'double-float)
                        (make-array '(0 2) :element-type '(unsigned-byte 8))
                        (make-array 3 :element-type '(unsigned-byte 8) :initial-element 7)))
      (dolist (renderer (list #'empty-array-readable-text #'prin1-to-string
                             (lambda (x) (format nil "~S" x))
                             (lambda (x) (with-output-to-string (s) (write x :stream s)))
                             (lambda (x) (write-to-string x :escape nil :readably t))
                             (lambda (x) (write-to-string (list x x) :circle t))))
        (let ((seen nil))
          (assert
            (handler-case
                (handler-bind ((print-not-readable
                                 (lambda (condition)
                                   (assert (eq (print-not-readable-object condition) array))
                                   (setq seen t))))
                  (funcall renderer array) nil)
              (print-not-readable (condition)
                (assert (eq (print-not-readable-object condition) array)) t)))
          (assert seen)))
      (assert (= (read-from-string (prin1-to-string 42)) 42)))))
;; Ordinary display does not require read-time evaluation.
(let ((*print-readably* nil) (*read-eval* nil))
  (assert (not (search "#." (write-to-string (make-array '(0 2))))))
  (assert (not (search "#." (prin1-to-string (make-array 3 :element-type '(unsigned-byte 8) :initial-element 7))))))
;; A user pretty-printer is consulted before the built-in constructor policy.
(let ((*print-readably* t) (*read-eval* nil) (*print-pretty* t)
      (*print-pprint-dispatch* (copy-pprint-dispatch nil))
      (called nil) (array (make-array '(0 2))))
  (set-pprint-dispatch 'array
    (lambda (stream object)
      (declare (ignore stream))
      (setq called t)
      (error 'print-not-readable :object object)))
  (assert (handler-case (progn (prin1-to-string array) nil)
            (print-not-readable (condition)
              (assert (eq (print-not-readable-object condition) array)) t)))
  (assert called))

(write-line "EMPTY-ARRAY-READABLE-OK")
"##;

#[test]
fn readable_empty_arrays_preserve_shape_storage_type_and_adjustability() {
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let required = if tier == "t2" { "2" } else { "nil" };
        let program = format!("(defparameter *empty-array-required-tier* {required}){PROGRAM}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier} stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "EMPTY-ARRAY-READABLE-OK")
                .count(),
            1
        );
    }
}
