// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! MAKE-INSTANCE from warmed, compiled callers (bliss-ih7pa).
//!
//! A class that makes instances repeatedly gets a cached initialization plan,
//! and a compiled `(make-instance ...)` reaches it without rebuilding a form.
//! These programs call MAKE-INSTANCE well past the tiering thresholds and
//! then check that the protocol still behaves as the tree-walker's: initarg
//! validation and its errors, default initargs and initforms (which run, in
//! which order, and how often), methods added after warm-up, class
//! redefinition and obsolete instances, subclasses, shared slots, closures over
//! a DEFCLASS's lexical environment, funcallable instances and conditions.
//!
//! The expected lines were taken from the binary before this change.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [&str; 3] = ["interp", "t0", "t2"];

fn run(tier: &str, program: &str) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", program]);
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("EGCL_FORCE_TIER", tier);
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tier {tier}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn check_all_tiers(program: &str, expected: &str) {
    for tier in TIERS {
        let out = run(tier, program);
        let line = out
            .lines()
            .find(|line| line.starts_with("RESULT "))
            .unwrap_or_else(|| panic!("tier {tier}: no RESULT line in:\n{out}"));
        assert_eq!(line, expected, "tier {tier}:\n{out}");
    }
}

#[test]
fn initarg_validation_after_warm_up() {
    check_all_tiers(
        r#"
(progn
  (defclass mip-pt () ((x :initarg :x) (y :initarg :y :initarg :why)))
  (defun mip-make (i) (make-instance 'mip-pt :x i :y (* 2 i)))
  (defun mip-make-bad (i) (make-instance 'mip-pt :x i :z 1))
  (defun mip-make-aok (i) (make-instance 'mip-pt :x i :z 1 :allow-other-keys t))
  (defun mip-make-dup (i) (make-instance 'mip-pt :y i :why 2 :y 3))
  (defun mip-make-by (class &rest initargs) (apply #'make-instance class initargs))
  (let ((sum 0) (errors 0))
    (dotimes (i 3000)
      (let ((p (mip-make i)))
        (incf sum (+ (slot-value p 'x) (slot-value p 'y))))
      (incf sum (slot-value (mip-make-dup i) 'y))
      (incf sum (slot-value (mip-make-aok i) 'x))
      (when (zerop (mod i 100))
        (handler-case (mip-make-bad i) (program-error () (incf errors)))))
    (format t "RESULT ~S ~S ~S ~S ~S ~S ~S~%"
            sum errors
            (handler-case (mip-make-bad 1) (program-error () :program-error))
            (handler-case (mip-make-by 'mip-pt :x) (program-error () :program-error))
            (handler-case (mip-make-by 'mip-pt 3 4) (program-error () :program-error))
            (slot-value (mip-make-by (find-class 'mip-pt) :why 7) 'y)
            (slot-boundp (mip-make-by 'mip-pt) 'x))))
"#,
        "RESULT 22492500 30 :PROGRAM-ERROR :PROGRAM-ERROR :PROGRAM-ERROR 7 NIL",
    );
}

#[test]
fn default_initargs_and_initforms_order() {
    check_all_tiers(
        r#"
(progn
  (defvar *mip-log* nil)
  (defun mip-note (x) (push x *mip-log*) x)
  (defclass mip-base ()
    ((a :initarg :a :initform (mip-note :a-initform))
     (b :initarg :b)
     (d :initform (mip-note :d-initform)))
    (:default-initargs :b (mip-note :b-default)))
  (defclass mip-sub (mip-base)
    ((c :initarg :c :initform (mip-note :c-initform))
     (e :initarg :e :initform 'e-const))
    (:default-initargs :a (mip-note :a-default) :c (mip-note :c-default)))
  (defun mip-make-sub () (make-instance 'mip-sub))
  (defun mip-make-sub-a (v) (make-instance 'mip-sub :a v))
  (defun mip-make-sub-c (v) (make-instance 'mip-sub :c v :e v))
  (dotimes (i 3000) (mip-make-sub) (mip-make-sub-a i) (mip-make-sub-c i))
  (setq *mip-log* nil)
  (let* ((s1 (mip-make-sub))
         (l1 (reverse *mip-log*))
         (s2 (progn (setq *mip-log* nil) (mip-make-sub-a 42)))
         (l2 (reverse *mip-log*))
         (s3 (progn (setq *mip-log* nil) (mip-make-sub-c 9)))
         (l3 (reverse *mip-log*)))
    (format t "RESULT ~S ~S ~S ~S ~S ~S ~S~%"
            l1 l2 l3
            (list (slot-value s1 'a) (slot-value s1 'b) (slot-value s1 'c)
                  (slot-value s1 'd) (slot-value s1 'e))
            (slot-value s2 'a)
            (list (slot-value s3 'c) (slot-value s3 'e))
            (eq (slot-value (mip-make-sub) 'e) (slot-value (mip-make-sub) 'e)))))
"#,
        "RESULT (:A-DEFAULT :C-DEFAULT :B-DEFAULT :D-INITFORM) (:C-DEFAULT :B-DEFAULT :D-INITFORM) (:A-DEFAULT :B-DEFAULT :D-INITFORM) (:A-DEFAULT :B-DEFAULT :C-DEFAULT :D-INITFORM E-CONST) 42 (9 9) T",
    );
}

#[test]
fn methods_added_after_warm_up() {
    check_all_tiers(
        r#"
(progn
  (defclass mip-w () ((x :initarg :x) (log :initform nil :accessor mip-w-log)))
  (defclass mip-w2 (mip-w) ())
  (defun mip-make-w (i) (make-instance 'mip-w :x i))
  (defun mip-make-w2 (i) (make-instance 'mip-w2 :x i))
  (defun mip-warm () (dotimes (i 3000) (mip-make-w i) (mip-make-w2 i)))
  (mip-warm)
  (let ((results nil))
    (defmethod initialize-instance :after ((w mip-w) &key x)
      (push (list :ii-after x) (mip-w-log w)))
    (push (mip-w-log (mip-make-w 1)) results)
    (mip-warm)
    (defmethod shared-initialize :after ((w mip-w2) slots &key)
      (declare (ignore slots))
      (push :si-after (mip-w-log w)))
    (push (mip-w-log (mip-make-w 2)) results)
    (push (mip-w-log (mip-make-w2 3)) results)
    (mip-warm)
    (defmethod initialize-instance :after ((w mip-w2) &key)
      (push :ii-after-2 (mip-w-log w)))
    (push (mip-w-log (mip-make-w2 4)) results)
    (defmethod make-instance ((class (eql (find-class 'mip-w))) &rest initargs)
      (let ((instance (call-next-method)))
        (push (list :mi-primary (length initargs)) (mip-w-log instance))
        instance))
    (push (mip-w-log (mip-make-w 5)) results)
    (push (mip-w-log (mip-make-w2 6)) results)
    (remove-method #'make-instance
                   (find-method #'make-instance '()
                                (list (list 'eql (find-class 'mip-w)))))
    (mip-warm)
    (push (mip-w-log (mip-make-w 7)) results)
    (remove-method #'initialize-instance
                   (find-method #'initialize-instance '(:after) (list (find-class 'mip-w))))
    (push (mip-w-log (mip-make-w 8)) results)
    (push (mip-w-log (mip-make-w2 9)) results)
    (format t "RESULT ~S~%" (reverse results))))
"#,
        "RESULT (((:II-AFTER 1)) ((:II-AFTER 2)) ((:II-AFTER 3) :SI-AFTER) (:II-AFTER-2 (:II-AFTER 4) :SI-AFTER) ((:MI-PRIMARY 2) (:II-AFTER 5)) (:II-AFTER-2 (:II-AFTER 6) :SI-AFTER) ((:II-AFTER 7)) NIL (:II-AFTER-2 :SI-AFTER))",
    );
}

#[test]
fn redefinition_obsolete_instances_and_change_class() {
    check_all_tiers(
        r#"
(progn
  (defclass mip-r () ((x :initarg :x :initform 1)))
  (defclass mip-r-sub (mip-r) ((y :initarg :y :initform 2)))
  (defclass mip-other () ((z :initarg :z :initform 'z)))
  (defun mip-make-r (&rest args) (apply #'make-instance 'mip-r args))
  (defun mip-make-r-sub () (make-instance 'mip-r-sub))
  (defun mip-warm () (dotimes (i 3000) (mip-make-r :x i) (mip-make-r) (mip-make-r-sub)))
  (mip-warm)
  (let ((old (mip-make-r :x 10)) (results nil))
    (defclass mip-r () ((x :initarg :x :initform 100) (w :initarg :w :initform 'w))
      (:default-initargs :w 'dw))
    (push (list (slot-value (mip-make-r) 'x) (slot-value (mip-make-r) 'w)
                (slot-value (mip-make-r :w 3) 'w)
                (slot-value (mip-make-r-sub) 'x) (slot-value (mip-make-r-sub) 'w)
                (slot-value old 'x) (slot-value old 'w))
          results)
    (mip-warm)
    (push (handler-case (mip-make-r :y 1) (program-error () :program-error)) results)
    (make-instances-obsolete 'mip-r)
    (push (list (slot-value (mip-make-r) 'x) (slot-value old 'x)) results)
    (mip-warm)
    (let ((c (mip-make-r :x 5)))
      (change-class c 'mip-other)
      (push (list (class-name (class-of c)) (slot-value c 'z)) results))
    (defclass mip-r () ((x :initarg :x :initform 7)))
    (push (list (slot-value (mip-make-r) 'x)
                (handler-case (mip-make-r :w 1) (program-error () :program-error)))
          results)
    (format t "RESULT ~S~%" (reverse results))))
"#,
        "RESULT ((100 DW 3 100 DW 10 W) :PROGRAM-ERROR (100 10) (MIP-OTHER Z) (7 :PROGRAM-ERROR))",
    );
}

#[test]
fn shared_slots_closures_funcallable_and_conditions() {
    check_all_tiers(
        r#"
(progn
  (defclass mip-shared () ((count :allocation :class :initform 0 :accessor mip-count)
                           (k :initarg :k :initform :k)))
  (defun mip-make-shared (i) (let ((s (make-instance 'mip-shared :k i))) (incf (mip-count s)) s))
  (let ((n 0))
    (defclass mip-closed () ((serial :initform (incf n) :reader mip-serial)
                             (tag :initarg :tag))
      (:default-initargs :tag (list :tag n))))
  (defun mip-make-closed () (make-instance 'mip-closed))
  (defclass mip-fn () ((a :initarg :a :initform 1)) (:metaclass funcallable-standard-class))
  (defun mip-make-fn (i) (make-instance 'mip-fn :a i))
  (define-condition mip-cond (error) ((k :initarg :k :initform 3 :reader mip-cond-k)))
  (defun mip-make-cond (i) (make-instance 'mip-cond :k i))
  (let ((last-shared nil) (last-closed nil) (fsum 0) (csum 0))
    (dotimes (i 3000)
      (setq last-shared (mip-make-shared i))
      (setq last-closed (mip-make-closed))
      (incf fsum (slot-value (mip-make-fn i) 'a))
      (incf csum (mip-cond-k (mip-make-cond i))))
    (let ((f (mip-make-fn 2)))
      (egcl-ext:set-funcallable-instance-function f (lambda (x) (* x 10)))
      (format t "RESULT ~S ~S ~S ~S ~S ~S ~S ~S ~S~%"
              (mip-count last-shared) (slot-value last-shared 'k)
              (mip-serial last-closed) (slot-value last-closed 'tag)
              fsum (funcall f 4) csum
              (mip-cond-k (make-condition 'mip-cond))
              (typep (mip-make-cond 1) 'error)))))
"#,
        "RESULT 3000 2999 3000 (:TAG 2999) 4498500 40 4498500 3 T",
    );
}
