// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
(defvar *arity-effects* 0)
(defun arity-program-error-p (thunk)
  (handler-case (progn (funcall thunk) nil)
    (program-error (condition)
      (assert (search "SETF" (princ-to-string condition))) t)))
(defclass arity-object () ((slot :accessor arity-slot :initform :old)))
(defgeneric (setf arity-write) (new object))
(defmethod (setf arity-write) (new (object arity-object))
  (incf *arity-effects*) (values new :secondary))
(defgeneric arity-ordinary (new object))
(defmethod arity-ordinary (new (object arity-object)) (list new object))
(defparameter *arity-object* (make-instance 'arity-object))
(assert (arity-program-error-p (lambda () (setf (arity-slot) :new))))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-slot) :new))))
(assert (arity-program-error-p (lambda () (funcall (fdefinition '(setf arity-write)) :new))))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-write)))))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-write) :new *arity-object* :extra))))
(assert (handler-case (progn (arity-ordinary :new) nil) (program-error () t)))
(assert (zerop *arity-effects*))
(assert (equal (multiple-value-list (funcall #'(setf arity-write) :new *arity-object*))
               '(:new :secondary)))
(assert (= *arity-effects* 1))
;; Correct arity with an inapplicable receiver must still reach the original
;; no-applicable-method condition, rather than being changed to PROGRAM-ERROR.
(assert (handler-case (progn (funcall #'(setf arity-write) :new 123) nil)
          (program-error () (error "correct arity incorrectly rejected"))
          (error (condition) (search "no applicable method" (princ-to-string condition)))))
(assert (= *arity-effects* 1))
(defgeneric (setf arity-optional) (new object &optional extra))
(defmethod (setf arity-optional) (new (object arity-object) &optional (extra :default))
  (list new extra))
(assert (equal (funcall #'(setf arity-optional) :new *arity-object*) '(:new :default)))
(assert (equal (funcall #'(setf arity-optional) :new *arity-object* :given) '(:new :given)))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-optional) :new))))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-optional) :new *arity-object* 1 2))))
(defgeneric (setf arity-rest) (new object &rest rest))
(defmethod (setf arity-rest) (new (object arity-object) &rest rest) (cons new rest))
(assert (equal (funcall #'(setf arity-rest) :new *arity-object* 1 2 3) '(:new 1 2 3)))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-rest) :new))))
(defgeneric (setf arity-key) (new object &key alpha))
(defmethod (setf arity-key) (new (object arity-object) &key alpha beta)
  (list new alpha beta))
(assert (equal (funcall #'(setf arity-key) :new *arity-object* :alpha 1 :beta 2) '(:new 1 2)))
(assert (equal (funcall #'(setf arity-key) :new *arity-object* :other 9 :allow-other-keys t)
               '(:new nil nil)))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-key) :new))))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-key) :new *arity-object* :other 1))))
(defgeneric (setf arity-open-key) (new object &key &allow-other-keys))
(defmethod (setf arity-open-key) (new (object arity-object) &key &allow-other-keys) new)
(assert (eq (funcall #'(setf arity-open-key) :new *arity-object* :arbitrary nil) :new))
;; SETF names must now enforce the same method congruence checks as symbols.
(assert (handler-case
          (progn (eval '(defmethod (setf arity-write) (new object &optional extra)
                          (declare (ignore extra)) new)) nil)
          (program-error () t)))
(assert (equal (multiple-value-list (funcall #'(setf arity-write) :still *arity-object*))
               '(:still :secondary)))
;; A fresh definition uses the new lambda list rather than stale metadata.
(defgeneric (setf arity-redefined) (new object))
(fmakunbound '(setf arity-redefined))
(defgeneric (setf arity-redefined) (new))
(defmethod (setf arity-redefined) (new) new)
(assert (eq (funcall #'(setf arity-redefined) :replacement) :replacement))
(assert (arity-program-error-p (lambda () (funcall #'(setf arity-redefined)))))
(declaim (notinline arity-native-call))
(defun arity-native-call (function object mode)
  (if (zerop mode) (funcall function :native object) (funcall function :native)))
(dotimes (i 120) (arity-native-call #'(setf arity-write) *arity-object* 0))
(when *arity-required-tier*
  (loop repeat 100 until (= (egcl-ext:function-tier 'arity-native-call) *arity-required-tier*)
        do (arity-native-call #'(setf arity-write) *arity-object* 0) (sleep 0.01))
  (assert (= (egcl-ext:function-tier 'arity-native-call) *arity-required-tier*)))
(let ((before *arity-effects*))
  (assert (arity-program-error-p
            (lambda () (arity-native-call #'(setf arity-write) *arity-object* 1))))
  (assert (= before *arity-effects*))
  (assert (equal (multiple-value-list
                   (arity-native-call (fdefinition '(setf arity-write)) *arity-object* 0))
                 '(:native :secondary)))
  (assert (= *arity-effects* (1+ before))))
;; Argument effects occur once before the entry check, even on failure.
(let ((count 0))
  (assert (arity-program-error-p
            (lambda () (funcall #'(setf arity-write) (progn (incf count) :new)))))
  (assert (= count 1)))
(write-line "SETF-GENERIC-ARITY-OK")
"#;

#[test]
fn setf_generic_entry_checks_arity_before_dispatch_in_all_tiers() {
    let mut baseline = None;
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let source = format!("(defparameter *arity-required-tier* {required})\n{PROGRAM}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", &source]);
        for key in [
            "EGCL_FORCE_TIER",
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
        ] {
            command.env_remove(key);
        }
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("SETF-GENERIC-ARITY-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}
