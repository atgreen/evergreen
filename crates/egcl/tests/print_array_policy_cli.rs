// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(defun array-prin1-render (value) (prin1-to-string value))
(dotimes (i 80) (array-prin1-render #(1 2)))
(when *printer-required-tier*
  (loop repeat 100
        until (= *printer-required-tier* (egcl-ext:function-tier 'array-prin1-render))
        do (array-prin1-render #(1 2)) (sleep 0.01))
  (assert (= *printer-required-tier* (egcl-ext:function-tier 'array-prin1-render))))
(defun escaped-renderings (value)
  (list (write-to-string value) (array-prin1-render value) (format nil "~S" value)
        (with-output-to-string (s) (write value :stream s))
        (with-output-to-string (s) (prin1 value s))
        (string-trim '(#\Space #\Newline) (with-output-to-string (s) (print value s)))
        (string-trim '(#\Space #\Newline) (with-output-to-string (s) (pprint value s)))
        (with-output-to-string (s) (format s "~S" value))))
(defun unescaped-renderings (value)
  (list (princ-to-string value) (format nil "~A" value)
        (with-output-to-string (s) (princ value s))
        (with-output-to-string (s) (format s "~A" value))))
(defun check-rendering-group (texts)
  (dolist (text (rest texts)) (assert (equal text (first texts)))))
(let ((arrays (list (vector 'g797-hidden 'g797-hidden) #() #*101
                    (make-array 3 :element-type '(unsigned-byte 8) :initial-element 77)
                    (make-array 3 :element-type 'double-float :initial-element 1.25d0)
                    (make-array '(2 2) :initial-element 'g797-hidden)
                    (make-array nil :initial-element 'g797-hidden)
                    (make-array 4 :fill-pointer 2 :initial-element 'g797-hidden)
                    (make-array 2 :displaced-to (vector 0 'g797-hidden 'g797-hidden 9)
                                  :displaced-index-offset 1)
                    (make-array 3 :element-type 'double-float :adjustable t :initial-element 2.5d0)
                    (make-array 4 :element-type 'bit :fill-pointer 2 :initial-element 1))))
  (dolist (pretty '(nil t))
    (dolist (circle '(nil t))
      (let ((*print-array* nil) (*print-pretty* pretty) (*print-circle* circle))
        (dolist (array arrays)
          (let ((escaped (escaped-renderings array)) (plain (unescaped-renderings array)))
            (check-rendering-group escaped)
            (check-rendering-group plain)
            (assert (equal (first escaped) (first plain)))
            (assert (search "#<" (first escaped)))
            (assert (not (search "G797-HIDDEN" (string-upcase (first escaped))))))))
      (let ((*print-array* t) (*print-pretty* pretty) (*print-circle* circle))
        (dolist (array arrays)
          (assert (not (search "#<" (prin1-to-string array))))))
      (let ((*print-array* nil) (*print-readably* t) (*print-pretty* pretty)
            (*print-circle* circle))
        (dolist (array arrays)
          (assert (equalp array (read-from-string (prin1-to-string array)))))))))
;; Strings retain ordinary escaped/unescaped syntax, including non-simple ones.
(dolist (string (list "abc" (make-array 4 :element-type 'character :fill-pointer 2
                            :initial-contents "abcd")
                      (make-array 2 :element-type 'character :displaced-to "abcd"
                            :displaced-index-offset 1)))
  (let ((*print-array* nil))
    (check-rendering-group (escaped-renderings string))
    (check-rendering-group (unescaped-renderings string))
    (assert (char= (char (prin1-to-string string) 0) #\"))
    (assert (equal string (princ-to-string string)))))
(let ((package (find-package "CL-USER")))
  (check-rendering-group (escaped-renderings package))
  (check-rendering-group (unescaped-renderings package))
  (assert (search "PACKAGE" (prin1-to-string package))))
(defclass g797-printable () ())
(defmethod print-object ((object g797-printable) stream)
  (declare (ignore object)) (write-string "USER-PRINTER-797" stream))
(let ((*print-array* nil) (object (make-instance 'g797-printable)))
  (dolist (text (append (escaped-renderings object) (unescaped-renderings object)))
    (assert (equal text "USER-PRINTER-797"))))
(let ((*print-array* nil) (*print-pretty* t)
      (*print-pprint-dispatch* (copy-pprint-dispatch nil)))
  (set-pprint-dispatch 'vector
    (lambda (stream object) (declare (ignore object)) (write-string "PRETTY-797" stream))
    100)
  (dolist (text (append (escaped-renderings #(1 2)) (unescaped-renderings #(1 2))))
    (assert (equal text "PRETTY-797"))))
(let ((*print-array* nil) (*print-circle* t) (*print-level* 0))
  (let ((array #(1 2)))
    (assert (equal (write-to-string array) (prin1-to-string array)))))
(let ((*print-array* nil) (*print-readably* t) (*print-circle* t)
      (array (make-array 1)))
  (setf (aref array 0) array)
  (let* ((text (prin1-to-string array)) (copy (read-from-string text)))
    (assert (search "#1=" text))
    (assert (eq copy (aref copy 0)))))
(write-line "PRINT-ARRAY-POLICY-OK")
"##;

#[test]
fn all_printer_entrypoints_share_array_visibility_and_heap_object_hooks() {
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.env("EGCL_FORCE_TIER", tier);
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
        command.env("EGCL_T2_LAZY", "0").env("EGCL_T2_THREADS", "1");
        let required = if tier == "t2" { "2" } else { "nil" };
        let program = format!("(defparameter *printer-required-tier* {required}){PROGRAM}");
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
                .filter(|line| line.trim() == "PRINT-ARRAY-POLICY-OK")
                .count(),
            1
        );
    }
}
