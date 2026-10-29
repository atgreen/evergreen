// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const SOURCE: &str = r#"
;; FASL reloading a DEFSTRUCT reaches DEFCLASS with an existing structure
;; metaclass, exactly like Gendl's %COMPUTING-MARKER% definition.
(defstruct protocol-structure-marker (datum 17))
(defparameter *protocol-structure-class* (find-class 'protocol-structure-marker))
(defstruct protocol-structure-marker (datum 18))
(assert (eq *protocol-structure-class* (find-class 'protocol-structure-marker)))
(assert (typep *protocol-structure-class* 'structure-class))
(assert (eq (egcl-mop::%finalize-defined-class *protocol-structure-class*)
            *protocol-structure-class*))
(assert (= (protocol-structure-marker-datum (make-protocol-structure-marker)) 18))
(assert (eq (egcl-mop::%finalize-defined-class (find-class 'integer)) (find-class 'integer)))
(defclass protocol-funcallable () ((datum :initform 23))
  (:metaclass funcallable-standard-class))
(assert (egcl-mop:class-finalized-p (find-class 'protocol-funcallable)))
(assert (= (slot-value (make-instance 'protocol-funcallable) 'datum) 23))
(defvar *protocol-init-count* 0)
(defvar *protocol-cpl-count* 0)
(defvar *protocol-slots-count* 0)
(defvar *protocol-default-count* 0)
(defclass protocol-meta (standard-class)
  ((label :initarg :label :initform :unlabelled :accessor protocol-label)))
(defmethod egcl-mop:validate-superclass ((class protocol-meta) (super standard-class)) t)
(defclass protocol-direct (egcl-mop:standard-direct-slot-definition)
  ((unit :initarg :unit :initform :none :reader protocol-unit)))
(defclass protocol-effective (egcl-mop:standard-effective-slot-definition)
  ((unit :initarg :unit :initform :none :reader protocol-unit)))
(defmethod egcl-mop:direct-slot-definition-class ((class protocol-meta) &rest args)
  (declare (ignore args)) (find-class 'protocol-direct))
(defmethod egcl-mop:effective-slot-definition-class ((class protocol-meta) &rest args)
  (declare (ignore args)) (find-class 'protocol-effective))
(defmethod egcl-mop:compute-class-precedence-list ((class protocol-meta))
  (incf *protocol-cpl-count*) (call-next-method))
(defmethod egcl-mop:compute-slots ((class protocol-meta))
  (incf *protocol-slots-count*) (call-next-method))
(defmethod egcl-mop:compute-default-initargs ((class protocol-meta))
  (incf *protocol-default-count*) (call-next-method))
(defmethod egcl-mop:compute-effective-slot-definition ((class protocol-meta) name direct)
  (let ((slot (call-next-method)))
    (setf (slot-value slot 'unit) (protocol-unit (first direct)))
    (when (eq name 'overridden)
      (setf (slot-value slot 'egcl-mop::initfunction) (lambda () 99)))
    slot))
(let ((lexical 41))
  (defclass protocol-example ()
    ((value :initarg :value :type integer :initform (progn (incf *protocol-init-count*) lexical) :unit :meters)
     (overridden :initform 2)
     (defaulted :initarg :defaulted)
     (empty :initform nil))
    (:metaclass protocol-meta)
    (:default-initargs :defaulted (+ lexical 1))))
(assert (eq (egcl-mop:finalize-inheritance (find-class 'protocol-example))
            (find-class 'protocol-example)))
(defparameter *protocol-direct-slot* (first (egcl-mop:class-direct-slots (find-class 'protocol-example))))
(defparameter *protocol-effective-slot* (first (egcl-mop:class-slots (find-class 'protocol-example))))
(defun protocol-check ()
  (let* ((class (find-class 'protocol-example))
         (direct (egcl-mop:class-direct-slots class))
         (effective (egcl-mop:class-slots class))
         (object (make-instance class)))
    (assert (eq *protocol-direct-slot* (first direct)))
    (assert (eq *protocol-effective-slot* (first effective)))
    (assert (typep (first direct) 'protocol-direct))
    (assert (typep (first effective) 'protocol-effective))
    (assert (typep (first direct) 'egcl-mop:direct-slot-definition))
    (assert (typep (first effective) 'egcl-mop:effective-slot-definition))
    (assert (eq (protocol-unit (first direct)) :meters))
    (assert (eq (protocol-unit (first effective)) :meters))
    (assert (eq (egcl-mop:slot-definition-type (first direct)) 'integer))
    (assert (= (funcall (egcl-mop:slot-definition-initfunction (first direct))) 41))
    (assert (= (slot-value object 'value) 41))
    (assert (= (slot-value object 'overridden) 99))
    (assert (= (slot-value object 'defaulted) 42))
    (assert (slot-boundp object 'empty))
    (assert (null (slot-value object 'empty)))
    (assert (= (length effective) 4))
    (assert (egcl-mop:class-finalized-p class))
    (assert (plusp *protocol-cpl-count*))
    (assert (plusp *protocol-slots-count*))
    (assert (plusp *protocol-default-count*))
    t))
(protocol-check)
(let* ((calls 0)
       (function (lambda () (incf calls) 73))
       (class (egcl-mop:ensure-class 'protocol-ensured
                :direct-superclasses (list (find-class 'standard-object))
                :direct-slots (list (list :name 'ensured :initargs '(:ensured)
                                         :initform :source-form :initfunction function
                                         :type 'integer)))))
  (assert (eq class (find-class 'protocol-ensured)))
  (assert (eq function (egcl-mop:slot-definition-initfunction
                       (first (egcl-mop:class-direct-slots class)))) )
  (assert (eq (egcl-mop:slot-definition-initform
               (first (egcl-mop:class-direct-slots class))) :source-form))
  (assert (= (slot-value (make-instance class) 'ensured) 73))
  (assert (= calls 1))
  (assert (= (slot-value (make-instance class :ensured 9) 'ensured) 9))
  (assert (= calls 1))
  (assert (eq class (egcl-mop:ensure-class 'protocol-ensured)))
  (assert (= (slot-value (make-instance class) 'ensured) 73))
  (assert (= calls 2)))
(let ((class (egcl-mop:ensure-class 'protocol-labelled :metaclass 'protocol-meta
               :direct-slots (list (list :name 'datum :initform 6 :initfunction (lambda () 6) :unit :seconds))
               :label :first)))
  (assert (eq (protocol-label class) :first))
  (assert (= (slot-value (make-instance class) 'datum) 6))
  (assert (eq class (egcl-mop:ensure-class 'protocol-labelled :label :second)))
  (assert (eq (protocol-label class) :second)))
(defclass protocol-cpl-meta (standard-class) ())
(defmethod egcl-mop:validate-superclass ((class protocol-cpl-meta) (super standard-class)) t)
(defmethod egcl-mop:compute-class-precedence-list ((class protocol-cpl-meta))
  (let ((ordinary (call-next-method)))
    (if (eq (class-name class) 'protocol-cpl-result)
        (list* (first ordinary) (third ordinary) (second ordinary) (cdddr ordinary))
        ordinary)))
(defclass protocol-left () ((choice :initform :left)))
(defclass protocol-right () ((choice :initform :right)))
(defgeneric protocol-dispatch (object))
(defmethod protocol-dispatch ((object protocol-left)) :left)
(defmethod protocol-dispatch ((object protocol-right)) :right)
(defclass protocol-cpl-result (protocol-left protocol-right) () (:metaclass protocol-cpl-meta))
(assert (eq (protocol-dispatch (make-instance 'protocol-cpl-result)) :right))
(assert (eq (slot-value (make-instance 'protocol-cpl-result) 'choice) :right))
(defclass protocol-default-meta (standard-class) ())
(defmethod egcl-mop:validate-superclass ((class protocol-default-meta) (super standard-class)) t)
(defmethod egcl-mop:compute-default-initargs ((class protocol-default-meta))
  (list (list :datum :computed-default (lambda () 85))))
(defclass protocol-default-result () ((datum :initarg :datum)) (:metaclass protocol-default-meta))
(assert (= (slot-value (make-instance 'protocol-default-result) 'datum) 85))
(assert (eq (second (first (egcl-mop:class-default-initargs (find-class 'protocol-default-result))))
            :computed-default))
(defclass protocol-parent () ((inherited :initform 5)))
(defclass protocol-child (protocol-parent) ((own :initform 6)))
(defparameter *protocol-old-object* (make-instance 'protocol-child))
(defclass protocol-parent () ((inherited :initform 7) (added :initform 8)))
(assert (equal (mapcar #'egcl-mop:slot-definition-name
                       (egcl-mop:class-slots (find-class 'protocol-child)))
               '(own inherited added)))
(assert (= (slot-value (make-instance 'protocol-child) 'added) 8))
(assert (= (slot-value *protocol-old-object* 'inherited) 5))
(assert (handler-case (progn (defclass protocol-forward-child (protocol-not-yet) ())
                             (egcl-mop:finalize-inheritance (find-class 'protocol-forward-child)) nil)
          (error () t)))
(defclass protocol-not-yet () ((ready :initform t)))
(assert (egcl-mop:class-finalized-p (find-class 'protocol-forward-child)))
(assert (slot-value (make-instance 'protocol-forward-child) 'ready))
(defvar *protocol-reject-layout* nil)
(defclass protocol-failing-meta (standard-class) ())
(defmethod egcl-mop:validate-superclass ((class protocol-failing-meta) (super standard-class)) t)
(defmethod egcl-mop:compute-slots ((class protocol-failing-meta))
  (if *protocol-reject-layout* (error "rejected test layout") (call-next-method)))
(defclass protocol-retry () ((preserved :initform 1)) (:metaclass protocol-failing-meta))
(defparameter *protocol-retry-instance* (make-instance 'protocol-retry))
(setf (slot-value *protocol-retry-instance* 'preserved) 777)
(setq *protocol-reject-layout* t)
(assert (handler-case
            (progn (defclass protocol-retry () ((temporary :initform 2))
                     (:metaclass protocol-failing-meta)) nil)
          (error () t)))
(assert (not (egcl-mop:class-finalized-p (find-class 'protocol-retry))))
;; The failed direct definition remains recorded, but its provisional layout
;; must never migrate this object and silently discard PRESERVED.
(assert (= (egcl-internal::%raw-slot-value *protocol-retry-instance* 'preserved) 777))
(setq *protocol-reject-layout* nil)
(defclass protocol-retry () ((added :initform 3) (preserved :initform 4))
  (:metaclass protocol-failing-meta))
(assert (= (slot-value *protocol-retry-instance* 'preserved) 777))
(assert (= (slot-value *protocol-retry-instance* 'added) 3))
(defvar *protocol-parent-reject* nil)
(defvar *protocol-parent-attempts* 0)
(defclass protocol-parent-meta (standard-class) ())
(defmethod egcl-mop:validate-superclass ((class protocol-parent-meta) (super standard-class)) t)
(defmethod egcl-mop:compute-slots ((class protocol-parent-meta))
  (when (and *protocol-parent-reject* (eq (class-name class) 'protocol-failed-parent))
    (incf *protocol-parent-attempts*) (error "parent computation rejected"))
  (call-next-method))
(defmethod egcl-mop:validate-superclass ((class standard-class) (super protocol-parent-meta)) t)
(defclass protocol-failed-parent () ((kept :initform 1 :accessor protocol-ancestor-kept)) (:metaclass protocol-parent-meta))
(defclass protocol-failed-child (protocol-failed-parent) ((own :initform 2)))
(defparameter *protocol-failed-child-object* (make-instance 'protocol-failed-child))
(setf (slot-value *protocol-failed-child-object* 'kept) 888)
(dotimes (i 20) (assert (= (protocol-ancestor-kept *protocol-failed-child-object*) 888)))
(setq *protocol-parent-reject* t)
(assert (handler-case (progn (defclass protocol-failed-parent () ((temporary :initform 3))
                               (:metaclass protocol-parent-meta)) nil) (error () t)))
(dolist (form '((slot-value *protocol-failed-child-object* 'kept)
                (protocol-ancestor-kept *protocol-failed-child-object*)
                (make-instance 'protocol-failed-child)
                (egcl-mop:finalize-inheritance (find-class 'protocol-failed-child))))
  (let ((before *protocol-parent-attempts*))
    (assert (handler-case (progn (eval form) nil) (error () t)))
    (assert (= *protocol-parent-attempts* (1+ before))))
  (assert (not (egcl-mop:class-finalized-p (find-class 'protocol-failed-child))))
  (assert (= (egcl-internal::%raw-slot-value *protocol-failed-child-object* 'kept) 888)))
(setq *protocol-parent-reject* nil)
(defclass protocol-failed-parent () ((added :initform 3) (kept :initform 4))
  (:metaclass protocol-parent-meta))
(assert (egcl-mop:class-finalized-p (find-class 'protocol-failed-child)))
(assert (= (slot-value *protocol-failed-child-object* 'kept) 888))
(assert (= (slot-value *protocol-failed-child-object* 'added) 3))
;; Runtime finalization consults the physical public binding, including a
;; replacement closure and an unbound GF whose method metadata still exists.
(defclass protocol-binding-result () ((kept :initform 19)))
(let* ((class (find-class 'protocol-binding-result))
       (saved (fdefinition 'egcl-mop:finalize-inheritance))
       (calls 0))
  (unwind-protect
      (progn
        (setf (fdefinition 'egcl-mop:finalize-inheritance)
              (lambda (target) (incf calls) (funcall saved target)))
        (egcl-internal::%mop-class-protocol :begin class)
        (assert (= (slot-value (make-instance class) 'kept) 19))
        (assert (= calls 1))
        (fmakunbound 'egcl-mop:finalize-inheritance)
        (egcl-internal::%mop-class-protocol :begin class)
        (assert (handler-case (progn (make-instance class) nil)
                  (undefined-function () t)))
        (assert (not (egcl-mop:class-finalized-p class)))
        (assert (= calls 1)))
    (setf (fdefinition 'egcl-mop:finalize-inheritance) saved))
  (assert (= (slot-value (make-instance class) 'kept) 19))
  (assert (egcl-mop:class-finalized-p class)))
(format t "MOP-SLOT-PROTOCOL-OK~%")
"#;

#[test]
fn custom_slot_classes_drive_real_layout_initialization_and_finalization() {
    for (tier, stress) in [("t0", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        for key in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_DISABLE_T2",
        ] {
            command.env_remove(key);
        }
        command.env("EGCL_FORCE_TIER", tier);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command
            .args(["--no-init", "--eval", SOURCE])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("MOP-SLOT-PROTOCOL-OK"),
            "{stdout}\n{stderr}"
        );
    }
}

#[test]
fn slot_metaobjects_and_lexical_initfunctions_survive_a_core_image() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-mop-protocol-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let image = directory.join("protocol.core");
    let save = format!("(egcl-ext:save-lisp-and-die {:?})", image);
    let built = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", SOURCE, "--eval", &save])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let restored = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_GC_STRESS", "17").env("EGCL_GC_STRESS_AFTER_INIT", "1").env("EGCL_GC_POISON", "1")
        .env_remove("EGCL_GC_STRESS_SKIP").env_remove("EGCL_GC_STRESS_AT")
        .args(["--no-init", "--image"]).arg(&image)
        .args(["--eval", "(progn (protocol-check) (assert (= (slot-value *protocol-retry-instance* 'preserved) 777)) (format t \"MOP-SLOT-IMAGE-OK~%\"))"])
        .output().unwrap();
    assert!(
        restored.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&restored.stdout),
        String::from_utf8_lossy(&restored.stderr)
    );
    assert!(String::from_utf8_lossy(&restored.stdout).contains("MOP-SLOT-IMAGE-OK"));
    std::fs::remove_dir_all(directory).unwrap();
}
