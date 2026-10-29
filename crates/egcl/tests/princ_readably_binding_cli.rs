// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(defvar *princ-observations* nil)
(defvar *princ-nested-write* nil)
(declaim (notinline native-princ-string))
(defun native-princ-string (value) (princ-to-string value))
(defun stream-princ-string (value) (with-output-to-string (s) (princ value s)))
(defun format-a-string (value) (format nil "~A" value))
(defun format-a-stream (value) (with-output-to-string (s) (format s "~A" value)))
(defun format-d-fallback (value) (format nil "~D" value))
(defun format-f-fallback (value) (format nil "~F" value))
(defclass princ-observer () ())
(defmethod print-object ((object princ-observer) stream)
  (declare (ignore object))
  (push (list *print-escape* *print-readably*) *princ-observations*)
  (when *princ-nested-write*
    (let* ((array (make-array 2 :element-type '(unsigned-byte 8) :initial-contents '(1 2)))
           (copy (read-from-string (write-to-string array :escape nil :readably t))))
      (assert (equal (array-element-type copy) '(unsigned-byte 8)))
      (assert (null *print-readably*))))
  (write-string (if *print-readably* "#.(make-instance 'princ-observer)" "OBSERVED") stream))
(defclass princ-failure () ((mode :initarg :mode)))
(defmethod print-object ((object princ-failure) stream)
  (declare (ignore stream))
  (assert (null *print-readably*))
  (if (eq (slot-value object 'mode) :throw)
      (throw 'princ-test-exit :escaped)
      (error "expected printer failure")))
(let ((array (make-array 2 :element-type '(unsigned-byte 8) :initial-contents '(1 2))))
  (dotimes (i 80) (native-princ-string array))
  (when *princ-required-tier*
    (loop repeat 100 until (= *princ-required-tier* (egcl-ext:function-tier 'native-princ-string))
          do (native-princ-string array) (sleep 0.01))
    (assert (= *princ-required-tier* (egcl-ext:function-tier 'native-princ-string)))))
(let* ((outer (list :outer-generalized-boolean)) (*print-readably* outer)
       (*print-escape* t) (*print-array* t) (object (make-instance 'princ-observer))
       (operations (list #'native-princ-string #'stream-princ-string #'format-a-string #'format-a-stream
                         #'format-d-fallback #'format-f-fallback)))
  (dolist (operation operations)
    (setq *princ-observations* nil)
    (let ((*princ-nested-write* t)) (assert (equal (funcall operation object) "OBSERVED")))
    (assert (equal (first *princ-observations*) '(nil nil)))
    (assert (eq *print-readably* outer))
    (assert *print-escape*)
    (let ((array (make-array 2 :element-type '(unsigned-byte 8) :initial-contents '(1 2))))
      (assert (equal (funcall operation array) "#(1 2)")))
    (assert (eq (handler-case (funcall operation (make-instance 'princ-failure :mode :error))
                  (error () :caught)) :caught))
    (assert (eq *print-readably* outer))
    (assert (eq (catch 'princ-test-exit
                  (funcall operation (make-instance 'princ-failure :mode :throw)) :not-thrown)
                :escaped))
    (assert (eq *print-readably* outer)))
  ;; WRITE with escape false and readably true remains a readable operation.
  (let* ((array (make-array 2 :element-type '(unsigned-byte 8) :initial-contents '(3 4)))
         (copy (read-from-string (write-to-string array :escape nil :readably t))))
    (assert (equal (array-element-type copy) '(unsigned-byte 8)))
    (assert (equalp array copy)))
  (setq *princ-observations* nil)
  (write-to-string object :escape nil :readably t)
  (assert (eq (second (first *princ-observations*)) t))
  (assert (eq *print-readably* outer))
  (setq *princ-observations* nil)
  (format nil "~S" object)
  (assert (eq (second (first *princ-observations*)) outer))
  (let ((*print-pretty* t) (*print-pprint-dispatch* (copy-pprint-dispatch nil)))
    (set-pprint-dispatch 'princ-observer
      (lambda (stream value) (declare (ignore value))
        (push (list *print-escape* *print-readably*) *princ-observations*)
        (write-string "PRETTY" stream)) 100)
    (dolist (operation operations)
      (setq *princ-observations* nil)
      (assert (equal (funcall operation object) "PRETTY"))
      (assert (equal (first *princ-observations*) '(nil nil)))
      (assert (eq *print-readably* outer)))
    (set-pprint-dispatch 'princ-observer
      (lambda (stream value) (declare (ignore stream value))
        (assert (null *print-readably*)) (error "expected pretty failure")) 100)
    (dolist (operation operations)
      (assert (eq (handler-case (funcall operation object) (error () :caught)) :caught))
      (assert (eq *print-readably* outer))))
  (assert *print-escape*))
(write-line "PRINC-BINDINGS-OK")
"##;

#[test]
fn princ_and_aesthetic_format_bind_visible_specials_and_restore_on_exits() {
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
        let program = format!("(defparameter *princ-required-tier* {required}){PROGRAM}");
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
                .filter(|line| line.trim() == "PRINC-BINDINGS-OK")
                .count(),
            1
        );
    }
}
