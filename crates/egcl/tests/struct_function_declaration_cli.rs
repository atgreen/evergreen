// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const SOURCE: &str = r#"
(defpackage :slot-function-probe (:use :cl))
(in-package :slot-function-probe)
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar *expansion-calls* 0)
  (defvar *expansion-hook* (constantly nil)))
(deftype callback (arguments &optional result)
  (incf *expansion-calls*)
  (funcall *expansion-hook*)
  ;; Allocate fresh syntax every time the declaration is checked.
  (let ((scratch (make-list 16 :initial-element arguments)))
    (declare (ignore scratch))
    (list 'function arguments result)))
(deftype maybe-callback () '(or null (callback (t) t)))
(defvar *invalid-number* 42)
(defvar *invalid-text* "bad")
(defun allowed-callback-p (value) (eq value #'identity))
(defstruct holder (callback #'identity :type (callback (t) t)))
(defstruct maybe-holder (callback nil :type maybe-callback))
(defstruct restricted-holder
  (callback #'identity :type (and (callback (t) t) (satisfies allowed-callback-p))))
(defstruct complement-holder
  (value 42 :type (and integer (not (callback (t) t)))))
(defstruct number-holder (value 0 :type (integer 0 9)))
(defun rejected-p (thunk)
  (handler-case (progn (funcall thunk) nil) (type-error () t)))
(defun dynamic-typep (value spec)
  (declare (notinline typep))
  (typep value spec))
(defun verify ()
  (assert (functionp (holder-callback (make-holder))))
  (assert (null (maybe-holder-callback (make-maybe-holder))))
  (assert (eq (maybe-holder-callback (make-maybe-holder :callback #'identity)) #'identity))
  (assert (rejected-p (lambda () (make-holder :callback *invalid-number*))))
  (assert (rejected-p (lambda () (make-maybe-holder :callback *invalid-number*))))
  (assert (functionp (restricted-holder-callback (make-restricted-holder))))
  (assert (rejected-p (lambda () (make-restricted-holder :callback #'car))))
  (assert (rejected-p (lambda () (make-restricted-holder :callback *invalid-number*))))
  ;; NOT keeps the checkable integer restriction; unknown signatures are not
  ;; discriminated. These cases have identical outcomes on the SBCL oracle.
  (assert (= (complement-holder-value (make-complement-holder)) 42))
  (assert (rejected-p (lambda () (make-complement-holder :value *invalid-text*))))
  (assert (rejected-p (lambda () (make-number-holder :value *invalid-number*))))
  (let ((offset 37))
    (assert (= (funcall (holder-callback
                         (make-holder :callback (lambda (value) (+ value offset))))
                       5)
               42)))
  (let ((object (make-number-holder)))
    (assert (rejected-p (lambda () (setf (number-holder-value object) *invalid-number*))))
    (setf (number-holder-value object) 7)
    (assert (= (number-holder-value object) 7)))
  (let ((object (make-holder)))
    (setf (holder-callback object) #'car)
    (assert (eq (holder-callback object) #'car))
    (assert (rejected-p (lambda () (setf (holder-callback object) *invalid-number*))))
    #+egcl
    (let ((before *expansion-calls*))
      (setf (holder-callback object) #'identity)
      (assert (= *expansion-calls* (1+ before))))
    #+egcl
    (let ((before *expansion-calls*)
          (*expansion-hook* (lambda () (error "declaration hook failed"))))
      (assert (handler-case (progn (setf (holder-callback object) #'car) nil)
                (error () t)))
      (assert (= *expansion-calls* (1+ before))))
    #+egcl
    (progn
    (handler-bind ((type-error
                     (lambda (condition)
                       (assert (equal (type-error-expected-type condition)
                                      '(callback (t) t)))
                       (assert (eql (type-error-datum condition) 42))
                       (invoke-restart 'store-value #'identity))))
      (setf (holder-callback object) *invalid-number*))
    (assert (eq (holder-callback object) #'identity))))
  #+egcl
  (let ((object
          (handler-bind ((type-error
                           (lambda (condition)
                             (assert (equal (type-error-expected-type condition)
                                            '(callback (t) t)))
                             (invoke-restart 'store-value #'identity))))
            (make-holder :callback *invalid-number*))))
    (assert (eq (holder-callback object) #'identity)))
  ;; Ordinary CHECK-TYPE continues using its original predicate and restart.
  (let ((value *invalid-text*))
    (handler-bind ((type-error (lambda (condition)
                                (assert (eq (type-error-expected-type condition) 'integer))
                                (invoke-restart 'store-value 7))))
      (check-type value integer))
    (assert (= value 7)))
  (assert (handler-case (progn (dynamic-typep #'identity '(function (t) t)) nil)
            (error () t)))
  (dotimes (i 500)
    (assert (= (funcall (holder-callback (make-holder)) i) i)))
  (write-line "STRUCT-FUNCTION-DECLARATION-OK"))
"#;

fn run(program: &str, stress: bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
    command
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_POISON")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = command
        .output()
        .expect("run structure function declaration regression");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCT-FUNCTION-DECLARATION-OK"));
}

#[test]
fn function_slot_declarations_accept_callbacks_and_preserve_checks() {
    let source = std::env::temp_dir().join(format!(
        "egcl-function-slots-source-{}.lisp",
        std::process::id()
    ));
    fs::write(&source, SOURCE).unwrap();
    let program = format!(
        "(progn (load {source:?}) (funcall (find-symbol \"VERIFY\" \"SLOT-FUNCTION-PROBE\")))"
    );
    run(&program, false);
    run(&program, true);
    fs::remove_file(source).unwrap();
}

#[test]
fn function_slot_declarations_persist_without_source() {
    let directory =
        std::env::temp_dir().join(format!("egcl-function-slots-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("functions.lisp");
    let artifact = directory.join("functions.bfasl");
    fs::write(&source, SOURCE).unwrap();
    run(
        &format!(
            "(progn (multiple-value-bind (path warnings failure)
                (compile-file {source:?} :output-file {artifact:?})
                (declare (ignore warnings)) (assert path) (assert (not failure)) (load path))
                (funcall (find-symbol \"VERIFY\" \"SLOT-FUNCTION-PROBE\")))"
        ),
        false,
    );
    fs::remove_file(&source).unwrap();
    run(
        &format!(
            "(progn (load {artifact:?}) (funcall (find-symbol \"VERIFY\" \"SLOT-FUNCTION-PROBE\")))"
        ),
        true,
    );
    fs::remove_dir_all(directory).unwrap();
}
