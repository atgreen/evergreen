// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, path::Path, process::Command};

const SETUP: &str = r#"
(defvar *rename-effects* 0)
(defvar *rename-around* 0)
(defclass rename-parent ()
  ((local :initform (progn (incf *rename-effects*) (list :old)) :accessor rename-local)
   (shared :allocation :class :initform (list :shared) :accessor rename-shared)))
(defclass rename-child (rename-parent) ())
(defparameter *rename-class* (find-class 'rename-parent))
(defparameter *rename-child-class* (find-class 'rename-child))
(defparameter *rename-instance* (make-instance 'rename-parent))
(defparameter *rename-child-instance* (make-instance 'rename-child))
(defparameter *rename-shared* (rename-shared *rename-instance*))
(defparameter *rename-slot* (first (egcl-mop:class-direct-slots *rename-class*)))
(defgeneric rename-message (object))
(defmethod rename-message ((object rename-parent)) :old)
(defparameter *rename-method* (find-method #'rename-message nil (list *rename-class*)))
(declaim (notinline rename-native))
(defun rename-native (class name) (setf (class-name class) name))
(assert (eq (setf (class-name *rename-class*) 'rename-moved) 'rename-moved))
(assert (eq (find-class 'rename-parent) *rename-class*))
(assert (null (find-class 'rename-moved nil)))
(assert (eq (type-of *rename-instance*) *rename-class*))
(setf (find-class 'rename-moved) *rename-class*)
(assert (eq (type-of *rename-instance*) 'rename-moved))
(setf (find-class 'rename-preserved) *rename-class*)
(setf (find-class 'rename-alias) *rename-class*)
(defclass rename-alias () ((fresh :initform :alias :reader rename-alias-fresh)))
(assert (not (eq (find-class 'rename-alias) *rename-class*)))
(assert (eq (rename-alias-fresh (make-instance 'rename-alias)) :alias))
;; The old registration now denotes an improperly named class: DEFCLASS must
;; create a fresh class rather than redefining the retained, renamed object.
(defclass rename-parent () ((fresh :initform :new :reader rename-fresh)))
(defparameter *rename-new-class* (find-class 'rename-parent))
(assert (not (eq *rename-new-class* *rename-class*)))
(assert (eq (rename-fresh (make-instance 'rename-parent)) :new))
(assert (eq (setf (class-name *rename-class*) nil) nil))
(assert (null (class-name *rename-class*)))
(assert (eq (type-of *rename-instance*) *rename-class*))
(assert (eq (find-class 'rename-moved) *rename-class*))
(defclass rename-moved () ((fresh :initform :unnamed :reader rename-unnamed-fresh)))
(defparameter *rename-after-unname* (find-class 'rename-moved))
(assert (not (eq *rename-after-unname* *rename-class*)))
(assert (eq (rename-unnamed-fresh (make-instance 'rename-moved)) :unnamed))
;; A conflicting primary name is allowed; it does not steal that registration.
(setf (class-name *rename-class*) 'rename-parent)
(assert (eq (find-class 'rename-parent) *rename-new-class*))
(assert (eq (type-of *rename-instance*) *rename-class*))
(setf (class-name *rename-class*) nil)
(defstruct rename-structure (value 19))
(defparameter *rename-structure-class* (find-class 'rename-structure))
(defparameter *rename-structure-instance* (make-rename-structure))
(setf (class-name *rename-structure-class*) 'renamed-structure)
(assert (eq (find-class 'rename-structure) *rename-structure-class*))
(assert (= (rename-structure-value (make-rename-structure)) 19))
(assert (eq (class-of *rename-structure-instance*) *rename-structure-class*))
(assert (eq (type-of *rename-structure-instance*) *rename-structure-class*))
(setf (class-name *rename-structure-class*) nil)
(defclass rename-metaclass (standard-class) ())
(defmethod egcl-mop:validate-superclass ((class rename-metaclass) (super standard-class)) t)
(defclass rename-custom () () (:metaclass rename-metaclass))
(defparameter *rename-custom-class* (find-class 'rename-custom))
(defmethod (setf class-name) :around (new (class rename-metaclass))
  (incf *rename-around*) (call-next-method))
(defclass rename-image-target () ())
(defparameter *rename-image-class* (find-class 'rename-image-target))
(defparameter *rename-image-name* (make-symbol "IMAGE-PRIMARY"))
(setf (class-name *rename-image-class*) *rename-image-name*)
(defun rename-type-error (thunk)
  (handler-case (progn (funcall thunk) nil) (type-error () t)))
(defun rename-program-error (thunk)
  (handler-case (progn (funcall thunk) nil) (program-error () t)))
"#;
const CHECKS: &str = r#"
(assert (eq (class-name *rename-image-class*) *rename-image-name*))
(assert (eq (find-class 'rename-image-target) *rename-image-class*))
(assert (null (symbol-package (class-name *rename-image-class*))))
(assert (fboundp '(setf class-name)))
(let ((writer (fdefinition '(setf class-name))))
  (assert (typep writer 'standard-generic-function))
  (assert (find-method writer nil (list (find-class t) (find-class 'class)) nil))
  (assert (eq (funcall writer 'rename-once *rename-class*) 'rename-once))
  (assert (eq (class-name *rename-class*) 'rename-once))
  (assert (eq (funcall writer nil *rename-class*) nil))
  (dolist (args '(() (only-one) (one two three)))
    (assert (rename-program-error (lambda () (apply writer args)))))
  ;; A replacement controls public SETF calls, while the retained GF remains
  ;; callable. Restoration must preserve exact canonical function identity.
  (unwind-protect
    (progn
      (setf (fdefinition '(setf class-name)) #'list)
      (assert (equal (setf (class-name *rename-class*) 'replacement)
                     (list 'replacement *rename-class*)))
      (assert (null (class-name *rename-class*)))
      (assert (eq (funcall writer 'retained *rename-class*) 'retained)))
    (setf (fdefinition '(setf class-name)) writer))
  (assert (eq writer (fdefinition '(setf class-name)))))
(let ((events nil))
  (setf (class-name (progn (push :class events) *rename-class*))
        (progn (push :name events) nil))
  (assert (equal (reverse events) '(:class :name))))
(dolist (bad (list 7 #\x (list :not-a-symbol)))
  (assert (rename-type-error (lambda () (setf (class-name *rename-class*) bad)))))
(assert (null (class-name *rename-class*)))
(dolist (bad (list nil 7 'rename-parent))
  (assert (rename-type-error (lambda () (setf (class-name bad) 'invalid-class)))))
(assert (eq (setf (class-name (find-class 'integer)) 'integer) 'integer))
(assert (rename-program-error
          (lambda () (setf (class-name (find-class 'integer)) 'not-an-integer))))
(assert (eq (class-name (find-class 'integer)) 'integer))
(let ((before *rename-around*))
  (assert (eq (rename-native *rename-custom-class* 'rename-custom-moved) 'rename-custom-moved))
  (assert (= (1+ before) *rename-around*))
  (assert (eq (find-class 'rename-custom) *rename-custom-class*))
  (assert (null (find-class 'rename-custom-moved nil)))
  (setf (class-name *rename-custom-class*) 'rename-custom))
(assert (null (class-name *rename-structure-class*)))
(assert (eq (type-of *rename-structure-instance*) *rename-structure-class*))
(assert (= (rename-structure-value *rename-structure-instance*) 19))
(assert (eq (class-of *rename-instance*) *rename-class*))
(assert (eq (type-of *rename-instance*) *rename-class*))
(assert (eq (find-class 'rename-parent) *rename-new-class*))
(assert (eq (find-class 'rename-moved) *rename-after-unname*))
(assert (eq (find-class 'rename-preserved) *rename-class*))
(assert (eq *rename-method* (find-method #'rename-message nil (list *rename-class*))))
(assert (eq (first (egcl-mop:method-specializers *rename-method*)) *rename-class*))
(assert (eq (rename-message *rename-instance*) :old))
(assert (eq (rename-message *rename-child-instance*) :old))
(assert (member *rename-class* (egcl-mop:class-precedence-list *rename-child-class*)))
(assert (eq *rename-slot* (first (egcl-mop:class-direct-slots *rename-class*))))
(let ((before *rename-effects*) (fresh (make-instance *rename-class*)))
  (assert (= (1+ before) *rename-effects*))
  (assert (equal (rename-local fresh) '(:old)))
  (assert (eq (rename-shared fresh) *rename-shared*)))
(setf (rename-shared *rename-child-instance*) *rename-shared*)
(assert (eq (rename-shared *rename-instance*) *rename-shared*))
(when *rename-required-tier*
  (dotimes (i 100) (rename-native *rename-class* nil))
  (loop repeat 200 until (= *rename-required-tier* (egcl-ext:function-tier 'rename-native))
        do (rename-native *rename-class* nil) (sleep 0.01))
  (assert (= *rename-required-tier* (egcl-ext:function-tier 'rename-native)))
  (assert (eq (rename-native *rename-class* 'native-primary) 'native-primary))
  (assert (eq (class-name *rename-class*) 'native-primary))
  (assert (eq (type-of *rename-instance*) *rename-class*))
  (rename-native *rename-class* nil))
(assert (equal (multiple-value-list (setf (class-name *rename-class*) nil)) '(nil)))
(write-line "CLASS-NAME-WRITER-OK")
"#;

fn run(program: &str, tier: &str, stress: bool, image: Option<&Path>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
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
    let output = command.args(["--eval", program]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn class_names_change_without_retargeting_class_identity_or_registrations() {
    let mut reference = None;
    for (tier, required, stress) in [
        ("interp", "nil", false),
        ("t1", "1", false),
        ("t2", "2", false),
        ("t2", "2", true),
    ] {
        let output = run(
            &format!("(defparameter *rename-required-tier* {required}){SETUP}{CHECKS}"),
            tier,
            stress,
            None,
        );
        assert!(output.contains("CLASS-NAME-WRITER-OK"));
        if let Some(previous) = &reference {
            assert_eq!(&output, previous);
        } else {
            reference = Some(output);
        }
    }
}

#[test]
fn renamed_anonymous_classes_and_methods_survive_source_free_load_and_core() {
    let dir = std::env::temp_dir().join(format!("egcl-class-name-writer-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("classes.lisp");
    let fasl = dir.join("classes.bfasl");
    let image = dir.join("classes.core");
    fs::write(&source, SETUP).unwrap();
    run(
        &format!("(compile-file {source:?} :output-file {fasl:?})"),
        "interp",
        false,
        None,
    );
    let bytes = fs::read(&fasl).unwrap();
    let unit = egcl_rt::bfasl::load(&bytes).unwrap();
    assert!(
        unit.section(egcl_rt::bfasl::section::TOPLEVEL_FORMS)
            .is_none()
    );
    assert!(
        unit.section(egcl_rt::bfasl::section::BYTECODE_UNIT)
            .is_some_and(|body| body.starts_with(b"BBU\0"))
    );
    fs::remove_file(source).unwrap();
    for stress in [false, true] {
        let output = run(
            &format!("(load {fasl:?})(defparameter *rename-required-tier* 2){CHECKS}"),
            "t2",
            stress,
            None,
        );
        assert!(output.contains("CLASS-NAME-WRITER-OK"));
    }
    run(
        &format!("(load {fasl:?})(egcl-ext:save-lisp-and-die {image:?})"),
        "interp",
        false,
        None,
    );
    for stress in [false, true] {
        let output = run(
            &format!("(defparameter *rename-required-tier* 2){CHECKS}"),
            "t2",
            stress,
            Some(&image),
        );
        assert!(output.contains("CLASS-NAME-WRITER-OK"));
    }
    fs::remove_dir_all(dir).unwrap();
}
