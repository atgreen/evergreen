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
//! The expected lines of the first five programs were taken from the binary
//! before that change. The later ones check the initialization protocol's
//! generic functions (bliss-uchht, bliss-agqzs) against SBCL 2.6.9.

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

/// Every kind of method on MAKE-INSTANCE, ALLOCATE-INSTANCE,
/// INITIALIZE-INSTANCE and SHARED-INITIALIZE, added after warm-up, runs in the
/// standard combination's order, and CALL-NEXT-METHOD reaches the standard
/// methods (bliss-uchht). The expected line is SBCL 2.6.9's, except for the
/// first entry: SBCL's constructor for a constant class name does not run the
/// :around method specialized on the SYMBOL, but MAKE-INSTANCE's standard
/// method on a symbol calls MAKE-INSTANCE again (CLHS), so the method applies.
#[test]
fn initialization_generic_functions_method_order() {
    check_all_tiers(
        r#"
(progn
  (defvar *mio-log* nil)
  (defun mio-note (x) (push x *mio-log*) x)
  (defclass mio-a ()
    ((x :initarg :x :initform (mio-note :x-initform))
     (y :initarg :y)
     (z :initform 'z)))
  (defun mio-make (i) (make-instance 'mio-a :y i))
  (defun mio-make-by (class &rest initargs) (apply #'make-instance class initargs))
  (dotimes (i 3000) (mio-make i))
  (defmethod make-instance :around ((c (eql 'mio-a)) &rest initargs)
    (mio-note (list :mi-around (length initargs)))
    (call-next-method))
  (defmethod make-instance :before ((c (eql (find-class 'mio-a))) &rest initargs)
    (mio-note (list :mi-before (length initargs))))
  (defmethod make-instance :after ((c (eql (find-class 'mio-a))) &rest initargs)
    (mio-note (list :mi-after (length initargs))))
  (defmethod allocate-instance :around ((c (eql (find-class 'mio-a))) &rest initargs)
    (mio-note (list :ai-around (length initargs)))
    (call-next-method))
  (defmethod allocate-instance :before ((c (eql (find-class 'mio-a))) &rest initargs)
    (declare (ignore initargs))
    (mio-note :ai-before))
  (defmethod allocate-instance ((c (eql (find-class 'mio-a))) &rest initargs)
    (declare (ignore initargs))
    (mio-note :ai-primary)
    (let ((o (call-next-method)))
      (mio-note (list :ai-fresh (slot-boundp o 'x) (slot-boundp o 'y) (slot-boundp o 'z)))
      o))
  (defmethod allocate-instance :after ((c (eql (find-class 'mio-a))) &rest initargs)
    (declare (ignore initargs))
    (mio-note :ai-after))
  (defmethod initialize-instance :around ((o mio-a) &rest initargs)
    (mio-note (list :ii-around (length initargs)))
    (let ((r (call-next-method)))
      (mio-note :ii-around-done)
      r))
  (defmethod initialize-instance :before ((o mio-a) &key)
    (mio-note (list :ii-before (slot-boundp o 'y))))
  (defmethod initialize-instance ((o mio-a) &key y)
    (mio-note (list :ii-primary y (next-method-p)))
    (call-next-method)
    (mio-note (list :ii-primary-done (slot-value o 'x) (slot-value o 'y))))
  (defmethod initialize-instance :after ((o mio-a) &key)
    (mio-note :ii-after))
  (defmethod shared-initialize :around ((o mio-a) slots &rest initargs)
    (mio-note (list :si-around slots (length initargs)))
    (call-next-method))
  (defmethod shared-initialize :before ((o mio-a) slots &key)
    (mio-note (list :si-before slots)))
  (defmethod shared-initialize ((o mio-a) slots &key y)
    (mio-note (list :si-primary slots y))
    (call-next-method))
  (defmethod shared-initialize :after ((o mio-a) slots &key)
    (mio-note (list :si-after slots (slot-value o 'z))))
  (dotimes (i 3000) (mio-make i))
  (setq *mio-log* nil)
  (let* ((a (mio-make 7))
         (l1 (reverse *mio-log*))
         (b (progn (setq *mio-log* nil) (mio-make-by (find-class 'mio-a) :x 1 :y 2)))
         (l2 (reverse *mio-log*)))
    (let ((*print-pretty* nil)) (format t "RESULT ~S ~S ~S ~S~%"
            l1 (list (slot-value a 'x) (slot-value a 'y) (slot-value a 'z))
            l2 (list (slot-value b (quote x)) (slot-value b (quote y)))))))
"#,
        "RESULT ((:MI-AROUND 2) (:MI-BEFORE 2) (:AI-AROUND 2) :AI-BEFORE :AI-PRIMARY (:AI-FRESH NIL NIL NIL) :AI-AFTER (:II-AROUND 2) (:II-BEFORE NIL) (:II-PRIMARY 7 T) (:SI-AROUND T 2) (:SI-BEFORE T) (:SI-PRIMARY T 7) :X-INITFORM (:SI-AFTER T Z) (:II-PRIMARY-DONE :X-INITFORM 7) :II-AFTER :II-AROUND-DONE (:MI-AFTER 2)) (:X-INITFORM 7 Z) ((:MI-BEFORE 4) (:AI-AROUND 4) :AI-BEFORE :AI-PRIMARY (:AI-FRESH NIL NIL NIL) :AI-AFTER (:II-AROUND 4) (:II-BEFORE NIL) (:II-PRIMARY 2 T) (:SI-AROUND T 4) (:SI-BEFORE T) (:SI-PRIMARY T 2) (:SI-AFTER T Z) (:II-PRIMARY-DONE 1 2) :II-AFTER :II-AROUND-DONE (:MI-AFTER 4)) (1 2)",
    );
}

/// A user primary method on INITIALIZE-INSTANCE or SHARED-INITIALIZE that does
/// not call the next method leaves the slots uninitialized, an ALLOCATE-INSTANCE
/// method makes the instance, and removing the methods brings the standard
/// behaviour back. Expected output from SBCL 2.6.9.
#[test]
fn primary_methods_that_do_not_call_the_next_method() {
    check_all_tiers(
        r#"
(progn
  (defvar *mio-log* nil)
  (defun mio-note (x) (push x *mio-log*) x)
  (defclass mio-n () ((a :initarg :a :initform (mio-note :a-initform)) (b :initform 2)))
  (defclass mio-s () ((a :initarg :a :initform (mio-note :a-initform)) (b :initform 2)))
  (defclass mio-p () ((a :initarg :a :initform 1)))
  (defun mio-make-n (i) (make-instance 'mio-n :a i))
  (defun mio-make-s (i) (make-instance 'mio-s :a i))
  (defun mio-make-p (i) (make-instance 'mio-p :a i))
  (defun mio-bound (o) (list (slot-boundp o 'a) (and (slot-exists-p o 'b) (slot-boundp o 'b))))
  (defun mio-warm () (dotimes (i 3000) (mio-make-n i) (mio-make-s i) (mio-make-p i)))
  (mio-warm)
  ;; A primary that does not call the next method: the standard method, and
  ;; with it every slot's initialization, never runs.
  (defmethod initialize-instance ((o mio-n) &key a)
    (mio-note (list :ii-n a)))
  (defmethod shared-initialize ((o mio-s) slots &key a)
    (mio-note (list :si-s slots a)))
  ;; An ALLOCATE-INSTANCE primary that returns its own instance.
  (defmethod allocate-instance ((c (eql (find-class 'mio-p))) &rest initargs)
    (mio-note (list :ai-p (length initargs)))
    (let ((o (call-next-method)))
      (setf (slot-value o 'a) :from-alloc)
      o))
  (mio-warm)
  (setq *mio-log* nil)
  (let* ((n (mio-make-n 5))
         (s (mio-make-s 6))
         (p (mio-make-p 7))
         (p2 (make-instance 'mio-p))
         (log (reverse *mio-log*)))
    (setq *mio-log* nil)
    ;; REINITIALIZE-INSTANCE goes through SHARED-INITIALIZE too.
    (reinitialize-instance s :a 8)
    (let ((relog (reverse *mio-log*)))
      ;; Removing the methods brings the standard behaviour back.
      (remove-method #'initialize-instance
                     (find-method #'initialize-instance '() (list (find-class 'mio-n))))
      (remove-method #'allocate-instance
                     (find-method #'allocate-instance '()
                                  (list (list 'eql (find-class 'mio-p)))))
      (mio-warm)
      (setq *mio-log* nil)
      (let ((n2 (mio-make-n 9)) (p3 (mio-make-p 10)))
        (let ((*print-pretty* nil))
          (format t "RESULT ~S ~S ~S ~S ~S ~S ~S ~S ~S~%"
                  log (mio-bound n) (mio-bound s)
                  (slot-value p 'a) (slot-value p2 'a)
                  relog (mio-bound n2) (slot-value p3 'a) (reverse *mio-log*)))))))
"#,
        "RESULT ((:II-N 5) (:SI-S T 6) (:AI-P 2) (:AI-P 0)) (NIL NIL) (NIL NIL) 7 :FROM-ALLOC ((:SI-S NIL 8)) (T T) 10 NIL",
    );
}

/// The &KEY parameters of ALLOCATE-INSTANCE, INITIALIZE-INSTANCE and
/// SHARED-INITIALIZE methods make initargs valid, those of MAKE-INSTANCE
/// methods do not, and each method receives its keyword (CLHS 7.1.2). A
/// MAKE-INSTANCE method with &KEY used to make every other initarg an invalid
/// keyword argument. Expected output from SBCL 2.6.9.
#[test]
fn initarg_validity_from_method_keys() {
    check_all_tiers(
        r#"
(progn
  (defvar *mio-log* nil)
  (defun mio-note (x) (push x *mio-log*) x)
  (defclass mio-k () ((a :initarg :a)))
  (defun mio-make-k (&rest initargs) (apply #'make-instance 'mio-k initargs))
  (defun mio-try (&rest initargs)
    (handler-case (progn (apply #'mio-make-k initargs) :ok)
      (program-error () :program-error)))
  (dotimes (i 3000) (mio-make-k :a i))
  (let ((results nil))
    (push (mio-try :alloc-key 1) results)
    (defmethod allocate-instance :before ((c (eql (find-class 'mio-k))) &key alloc-key)
      (mio-note (list :alloc-key alloc-key)))
    (push (mio-try :alloc-key 1) results)
    (push (mio-try :ii-key 2) results)
    (defmethod initialize-instance :around ((o mio-k) &key ii-key)
      (mio-note (list :ii-key ii-key))
      (call-next-method))
    (push (mio-try :ii-key 2) results)
    (push (mio-try :si-key 3) results)
    (defmethod shared-initialize ((o mio-k) slots &key si-key)
      (mio-note (list :si-key slots si-key))
      (call-next-method))
    (push (mio-try :si-key 3) results)
    (push (mio-try :mi-key 4) results)
    (defmethod make-instance :before ((c (eql (find-class 'mio-k))) &key mi-key)
      (mio-note (list :mi-key mi-key)))
    (push (mio-try :mi-key 4) results)
    (push (mio-try :bogus 5) results)
    (push (mio-try :bogus 5 :allow-other-keys t) results)
    (dotimes (i 3000) (mio-make-k :a i :alloc-key i :ii-key i :si-key i))
    (setq *mio-log* nil)
    (let ((k (mio-make-k :si-key 30 :ii-key 20 :alloc-key 10 :a 0)))
      (let ((*print-pretty* nil))
        (format t "RESULT ~S ~S ~S~%" (reverse results) (reverse *mio-log*) (slot-value k 'a))))))
"#,
        "RESULT (:PROGRAM-ERROR :OK :PROGRAM-ERROR :OK :PROGRAM-ERROR :OK :PROGRAM-ERROR :PROGRAM-ERROR :PROGRAM-ERROR :OK) ((:MI-KEY NIL) (:ALLOC-KEY 10) (:II-KEY 20) (:SI-KEY T 30)) 0",
    );
}

/// A MAKE-INSTANCE :around method alone, specialized on the class object or on
/// STANDARD-CLASS, reaches the standard method with CALL-NEXT-METHOD
/// (bliss-agqzs), and CHANGE-CLASS calls SHARED-INITIALIZE through
/// UPDATE-INSTANCE-FOR-DIFFERENT-CLASS. Expected output from SBCL 2.6.9.
#[test]
fn make_instance_around_methods_and_change_class() {
    check_all_tiers(
        r#"
(progn
  (defvar *mio-log* nil)
  (defun mio-note (x) (push x *mio-log*) x)
  (defclass mio-w () ((x :initarg :x :initform 0)))
  (defclass mio-v () ((y :initarg :y :initform (mio-note :y-initform)) (x :initarg :x)))
  (defun mio-make-w (i) (make-instance 'mio-w :x i))
  (defun mio-make-w-class (i) (make-instance (find-class 'mio-w) :x i))
  (dotimes (i 3000) (mio-make-w i) (mio-make-w-class i))
  ;; Only :around methods apply: the standard method is the primary.
  (defmethod make-instance :around ((c (eql (find-class 'mio-w))) &rest initargs)
    (mio-note (list :mi-around-class initargs))
    (let ((o (call-next-method)))
      (mio-note (list :mi-around-class-done (slot-value o 'x)))
      o))
  (dotimes (i 3000) (mio-make-w i) (mio-make-w-class i))
  (setq *mio-log* nil)
  (let ((r1 (list (slot-value (mio-make-w 3) 'x) (slot-value (mio-make-w-class 4) 'x)
                  (reverse *mio-log*))))
    (defmethod make-instance :around ((c standard-class) &rest initargs)
      (when (eq c (find-class 'mio-w))
        (mio-note (list :mi-around-standard-class (length initargs))))
      (call-next-method))
    (setq *mio-log* nil)
    (let ((r2 (list (slot-value (mio-make-w 5) 'x) (slot-value (mio-make-w-class 6) 'x)
                    (reverse *mio-log*))))
      ;; CHANGE-CLASS reaches SHARED-INITIALIZE through
      ;; UPDATE-INSTANCE-FOR-DIFFERENT-CLASS.
      (defmethod update-instance-for-different-class :before ((p mio-w) (c mio-v) &key y)
        (mio-note (list :uifdc-before (slot-value p 'x) y)))
      (defmethod shared-initialize :around ((o mio-v) slots &key y)
        (mio-note (list :si-around slots y))
        (call-next-method))
      (setq *mio-log* nil)
      (let ((o (mio-make-w 8)))
        (setq *mio-log* nil)
        (change-class o 'mio-v)
        (let ((r3 (list (slot-value o 'x) (slot-value o 'y) (reverse *mio-log*))))
          (setq *mio-log* nil)
          (let ((o2 (mio-make-w 9)))
            (setq *mio-log* nil)
            (change-class o2 'mio-v :y 1)
            (let ((r4 (list (slot-value o2 'y) (reverse *mio-log*))))
              (let ((*print-pretty* nil))
                (format t "RESULT ~S ~S ~S ~S~%" r1 r2 r3 r4)))))))))
"#,
        "RESULT (3 4 ((:MI-AROUND-CLASS (:X 3)) (:MI-AROUND-CLASS-DONE 3) (:MI-AROUND-CLASS (:X 4)) (:MI-AROUND-CLASS-DONE 4))) (5 6 ((:MI-AROUND-CLASS (:X 5)) (:MI-AROUND-STANDARD-CLASS 2) (:MI-AROUND-CLASS-DONE 5) (:MI-AROUND-CLASS (:X 6)) (:MI-AROUND-STANDARD-CLASS 2) (:MI-AROUND-CLASS-DONE 6))) (8 :Y-INITFORM ((:UIFDC-BEFORE 8 NIL) (:SI-AROUND (Y) NIL) :Y-INITFORM)) (1 ((:UIFDC-BEFORE 9 1) (:SI-AROUND (Y) 1)))",
    );
}
