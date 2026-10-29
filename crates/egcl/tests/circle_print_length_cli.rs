// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline truncated-write truncated-format))
(defun truncated-write (object length)
  (write-to-string object :length length :circle t :pretty nil))
(defun truncated-format (object length)
  (let ((*print-length* length) (*print-circle* t) (*print-pretty* nil))
    (format nil "~S" object)))
(defun logical-write (object length)
  (let ((*print-pretty* t) (*print-length* length) (*print-circle* t)
        (*print-right-margin* 100))
    (with-output-to-string (stream)
      (pprint-logical-block (stream object :prefix "[" :suffix "]")
        (pprint-exit-if-list-exhausted)
        (write (pprint-pop) :stream stream)
        (loop (pprint-exit-if-list-exhausted)
              (write-char #\Space stream)
              (write (pprint-pop) :stream stream))))))
(defun ordinary-check (object length expected)
  (let ((*print-readably* nil) (*print-length* length) (*print-circle* t))
    (assert (equal (truncated-write object length) expected))
    (assert (equal (truncated-format object length) expected))
    (dolist (pretty '(nil t))
      (let ((*print-pretty* pretty))
        (dolist (text (list (prin1-to-string object)
                           (with-output-to-string (s) (write object :stream s))))
          (assert (equal text expected)))))))
(let ((object (list 1 2 3)))
  (dotimes (i 80)
    (truncated-write object (if (oddp i) 2 nil))
    (truncated-format object (if (oddp i) 2 nil)))
  (when *circle-length-required-tier*
    (assert (= (egcl-ext:function-tier 'truncated-write) *circle-length-required-tier*))
    (assert (= (egcl-ext:function-tier 'truncated-format) *circle-length-required-tier*))))
(let* ((tail (list 2)) (object (cons 1 tail)))
  (setf (cdr tail) tail)
  (dolist (case '((0 "(...)" "[...]")
                  (1 "(1 ...)" "[1 ...]")
                  (2 "(1 2 ...)" "[1 2 ...]")
                  (3 "(1 . #1=(2 . #1#))" "[1 . #1=(2 . #1#)]")
                  (nil "(1 . #1=(2 . #1#))" "[1 . #1=(2 . #1#)]")))
    (ordinary-check object (first case) (second case))
    (let ((*print-readably* nil))
      (assert (equal (logical-write object (first case)) (third case))))))
(let* ((tail (list 1)) (object (list* tail 2 tail)))
  (dolist (case '((0 "(...)" "[...]")
                  (1 "((1) ...)" "[(1) ...]")
                  (2 "((1) 2 ...)" "[(1) 2 ...]")
                  (3 "(#1=(1) 2 . #1#)" "[#1=(1) 2 . #1#]")
                  (nil "(#1=(1) 2 . #1#)" "[#1=(1) 2 . #1#]")))
    (ordinary-check object (first case) (second case))
    (let ((*print-readably* nil))
      (assert (equal (logical-write object (first case)) (third case))))))
;; A shared tail acquires a fresh list budget when its definition becomes visible.
(let* ((v (vector 7)) (tail (list v v)) (object (list (list (cons 0 tail)) tail)))
  (ordinary-check object 2 "(((0 . #1=(#2=#(7) #2#))) #1#)"))
;; Re-expanding a labelled tail must not recount its already visible car.
(let* ((tail (list (list 9) (list 10))) (object (list (list (cons 0 tail)) tail)))
  (ordinary-check object 2 "(((0 . #1=((9) (10)))) #1#)"))
(let ((symbol (make-symbol "X")))
  (ordinary-check (cons symbol symbol) 1 "(#1=#:X . #1#)"))
(let ((object (list 1)))
  (setf (cdr object) object)
  (ordinary-check object 1 "(1 ...)")
  (ordinary-check object 2 "#1=(1 . #1#)")
  ;; READABLY overrides the finite length, using the original unbounded graph scan.
  (let ((*print-readably* t) (*print-length* 0) (*print-circle* t))
    (dolist (text (list (truncated-write object 0) (truncated-format object 0)))
      (let ((copy (read-from-string text)))
        (assert (= (car copy) 1))
        (assert (eq (cdr copy) copy))))))
(let ((v (vector 7))) (ordinary-check (vector v v) 1 "#(#(7) ...)"))
(let ((v (vector 1 nil)))
  (setf (aref v 1) v)
  (ordinary-check v 1 "#(1 ...)"))
(let ((v (vector nil 2)))
  (setf (aref v 0) v)
  (ordinary-check v 1 "#1=#(#1# ...)"))
(let* ((v (vector 7))
       (a (make-array '(2 3) :initial-contents (list (list v 101 v) (list v 202 v)))))
  (ordinary-check a 2 "#2A((#1=#(7) 101 ...) (#1# 202 ...))")
  (ordinary-check a 1 "#2A((#(7) ...) ...)"))
(let ((a (make-array nil)))
  (setf (row-major-aref a 0) a)
  (ordinary-check a 0 "#1=#0A#1#"))
;; Finite-length scans retain the opaque leaf identities accepted by the
;; unlimited scanner. READABLY NIL is essential: readable printing ignores length.
(let ((*print-readably* nil))
  (dolist (leaf (list (make-random-state nil) (make-array 0 :element-type nil)))
    (let* ((object (list leaf leaf))
           (complete (truncated-write object nil))
           (single (write-to-string leaf :circle t :pretty nil :readably nil)))
      (assert (search "#1=" complete))
      (assert (search "#1#" complete))
      (ordinary-check object 2 complete)
      (ordinary-check object 1 (concatenate 'string "(" single " ...)")))))
;; The scan never invokes user printers or exposes a truncated user object.
(defclass visible-printer () ())
(defparameter *visible-printer-calls* 0)
(defmethod print-object ((object visible-printer) stream)
  (incf *visible-printer-calls*)
  (make-list 20 :initial-element object)
  (write-string "VISIBLE" stream)
  object)
(let ((object (make-instance 'visible-printer)))
  (dolist (renderer (list #'truncated-write #'truncated-format))
    (setq *visible-printer-calls* 0)
    (let ((*print-readably* nil))
      (assert (equal (funcall renderer (list object object) 1) "(VISIBLE ...)")))
    (assert (= *visible-printer-calls* 1))))
(write-line "CIRCLE-PRINT-LENGTH-OK")
"##;

#[test]
fn truncated_graphs_label_only_visible_references_and_preserve_cycles() {
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_egcl"));
        let program = format!(
            "(defparameter *circle-length-required-tier* {}){PROGRAM}",
            if tier == "t2" { "2" } else { "nil" }
        );
        child.args(["--no-init", "--eval", &program]);
        child
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for key in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
        ] {
            child.env_remove(key);
        }
        if stress {
            child
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{tier}/{stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "CIRCLE-PRINT-LENGTH-OK")
                .count(),
            1
        );
    }
}
