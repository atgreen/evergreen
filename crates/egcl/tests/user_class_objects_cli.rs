// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A DEFCLASS class is an object, not its name symbol (bliss-vsjn1, bliss-rj5o).
//!
//! `(find-class 'foo)` used to BE the symbol FOO. Several behaviours were
//! therefore indistinguishable from the correct ones and passed by accident:
//! DEFCLASS "returning the class" when it returned the name, TYPE-OF returning
//! a name where CLHS wants the class, and a forward-referenced superclass
//! recording a bare symbol that later became the class for free. Each is
//! implemented for real now, and each has a row below.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "180", BIN, "--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

fn expect(text: &str, rows: &[&str]) {
    for row in rows {
        assert!(text.contains(row), "missing {row:?} in:\n{text}");
    }
}

#[test]
fn a_user_class_is_an_object_and_defclass_returns_it() {
    let stdout = run(r#"
      (defclass uc () ((a :initform 1)))
      (defstruct st x)
      (define-condition cc (error) ())
      (format t "SYMBOLP ~a~%" (symbolp (find-class 'uc)))
      (format t "META-UC ~a~%" (class-name (class-of (find-class 'uc))))
      (format t "META-ST ~a~%" (class-name (class-of (find-class 'st))))
      (format t "META-CC ~a~%" (class-name (class-of (find-class 'cc))))
      (format t "NAME ~a~%" (class-name (find-class 'uc)))
      (format t "IS-CLASS ~a~%" (typep (find-class 'uc) 'class))
      ;; CLHS DEFCLASS returns the class. This was the name before, which was
      ;; the same object, so ansi-test DEFCLASS-n-DEFCLASS-RETURNS-CLASS,
      ;; FIND-CLASS.15/16/18-21 and CLASS-REDEFINITION.1-3 all passed anyway.
      (format t "RETURNS-CLASS ~a~%" (eq (eval '(defclass dr () ())) (find-class 'dr)))
      ;; A redefinition must hand back the SAME metaobject, or instances of the
      ;; old class and every table keyed by the class value are orphaned.
      (format t "REDEF-SAME ~a~%"
              (eq (eval '(defclass rd () ((a)))) (eval '(defclass rd () ((b))))))
      (format t "INSTANCE ~a~%" (slot-value (make-instance 'uc) 'a))
      (format t "TYPEP-INSTANCE ~a~%" (typep (make-instance 'uc) 'uc))
      (format t "STRUCT ~a~%" (st-x (make-st :x 5)))
      (format t "USER-CLASS-OK~%")
    "#);
    expect(&stdout, &[
        "SYMBOLP NIL",
        "META-UC STANDARD-CLASS",
        "META-ST STRUCTURE-CLASS",
        "META-CC STANDARD-CLASS",
        "NAME UC",
        "IS-CLASS T",
        "RETURNS-CLASS T",
        "REDEF-SAME T",
        "INSTANCE 1",
        "TYPEP-INSTANCE T",
        "STRUCT 5",
        "USER-CLASS-OK",
    ]);
}

/// CLHS TYPE-OF answers the class's PROPER name, and the class itself when it
/// has none. A name is proper only while FIND-CLASS maps it back.
#[test]
fn type_of_answers_the_class_when_it_has_no_proper_name() {
    let stdout = run(r#"
      (let ((c (eval '(defclass tn () ((a))))))
        (format t "WITH-NAME ~a~%" (type-of (make-instance c)))
        (setf (find-class 'tn) nil)
        (format t "WITHOUT-NAME ~a~%" (eq (type-of (make-instance c)) c))
        ;; and the orphaned class is still instantiable, which is what
        ;; ansi-test TYPE-OF.9 needs. SLOT-BOUNDP, not SLOT-VALUE: the slot has
        ;; no :initform, so reading it correctly signals UNBOUND-SLOT.
        (format t "STILL-INSTANTIABLE ~a~%"
                (and (make-instance c) (not (slot-boundp (make-instance c) 'a)))))
      (format t "TYPE-OF-OK~%")
    "#);
    expect(&stdout, &[
        "WITH-NAME TN",
        "WITHOUT-NAME T",
        "STILL-INSTANTIABLE T",
        "TYPE-OF-OK",
    ]);
}

/// A superclass that is not defined yet gets a placeholder metaobject the
/// eventual DEFCLASS reuses, which is what SBCL does. Needed nothing while a
/// class was its name symbol.
#[test]
fn a_forward_referenced_superclass_resolves_when_it_is_defined() {
    let stdout = run(r#"
      (let* ((sub (eval '(defclass fr-sub (fr-super) ((b :initform 2)))))
             (super (eval '(defclass fr-super () ((a :initform 1))))))
        (format t "SUB-IS-CLASS ~a~%" (typep sub 'class))
        (format t "SUPER-IS-CLASS ~a~%" (typep super 'class))
        (format t "SUPER-REUSED ~a~%" (eq super (find-class 'fr-super)))
        (format t "INHERITS ~a~%" (typep (make-instance 'fr-sub) 'fr-super))
        (format t "INHERITED-SLOT ~a~%" (slot-value (make-instance 'fr-sub) 'a)))
      (format t "FORWARD-REF-OK~%")
    "#);
    expect(&stdout, &[
        "SUB-IS-CLASS T",
        "SUPER-IS-CLASS T",
        "SUPER-REUSED T",
        "INHERITS T",
        "INHERITED-SLOT 1",
        "FORWARD-REF-OK",
    ]);
}

/// The metaclass half of GitHub #384: a class with a user `:metaclass` IS an
/// instance of it, so TYPEP holds, a method specialized on the metaclass
/// applies and wins over one on STANDARD-CLASS, and the class carries the
/// metaclass's slots.
///
/// The metaclass's slot `:initform`s run on the class metaobject too, through
/// the same path MAKE-INSTANCE uses, so a read before any write answers the
/// initform rather than signalling UNBOUND-SLOT. Only for a freshly allocated
/// metaobject: re-running them on a redefinition would clobber whatever the
/// program has stored in those slots, which for a metaclass protocol is its
/// entire per-class state.
#[test]
fn a_class_with_a_user_metaclass_is_an_instance_of_it() {
    let stdout = run(r#"
      (defclass meta (standard-class) ((x :initform 1 :accessor meta-x)))
      (defmethod egcl-mop:validate-superclass ((c meta) (s standard-class)) t)
      (defclass foo () () (:metaclass meta))
      (format t "CLASS-OF-IS-META ~a~%" (eq (class-of (find-class 'foo)) (find-class 'meta)))
      (format t "TYPEP-META ~a~%" (typep (find-class 'foo) 'meta))
      (format t "TYPEP-STD ~a~%" (typep (find-class 'foo) 'standard-class))
      (format t "SLOT-EXISTS ~a~%" (slot-exists-p (find-class 'foo) 'x))
      ;; A method on the metaclass applies, and beats one on STANDARD-CLASS.
      (defgeneric cc2 (c))
      (defmethod cc2 ((c meta)) :meta)
      (format t "DISPATCH-META ~a~%" (cc2 (find-class 'foo)))
      (defmethod cc2 ((c standard-class)) :std)
      (format t "DISPATCH-PREFERS-META ~a~%" (cc2 (find-class 'foo)))
      ;; The metaclass's :initform ran on the class metaobject.
      (format t "INITFORM ~a~%" (slot-value (find-class 'foo) 'x))
      (format t "INITFORM-VIA-ACCESSOR ~a~%" (meta-x (find-class 'foo)))
      ;; A redefinition must NOT re-run it over a stored value.
      (setf (meta-x (find-class 'foo)) 99)
      (eval '(defclass foo () () (:metaclass meta)))
      (format t "REDEF-KEEPS-SLOT ~a~%" (meta-x (find-class 'foo)))
      ;; The class carries the metaclass's slot: writing then reading works.
      (setf (slot-value (find-class 'foo) 'x) 42)
      (format t "SLOT-ROUNDTRIP ~a~%" (slot-value (find-class 'foo) 'x))
      (setf (meta-x (find-class 'foo)) 7)
      (format t "ACCESSOR-ROUNDTRIP ~a~%" (meta-x (find-class 'foo)))
      ;; A direct instance of the metaclass is unaffected.
      (let ((m (make-instance 'meta)))
        (format t "DIRECT-INSTANCE ~a ~a~%" (typep m 'meta) (meta-x m)))
      (format t "METACLASS-OK~%")
    "#);
    expect(&stdout, &[
        "CLASS-OF-IS-META T",
        "TYPEP-META T",
        "TYPEP-STD T",
        "SLOT-EXISTS T",
        "INITFORM 1",
        "INITFORM-VIA-ACCESSOR 1",
        "REDEF-KEEPS-SLOT 99",
        "DISPATCH-META META",
        "DISPATCH-PREFERS-META META",
        "SLOT-ROUNDTRIP 42",
        "ACCESSOR-ROUNDTRIP 7",
        "DIRECT-INSTANCE T 1",
        "METACLASS-OK",
    ]);
}

/// The condition hierarchy must survive, and a built-in or condition class
/// must still refuse redefinition.
///
/// This is here because I broke it: minting a forward-reference placeholder
/// during bootstrap registered one under ERROR, shadowing the lazily
/// registered real condition class, and ERROR silently lost SERIOUS-CONDITION
/// and CONDITION from its precedence list — which in turn stopped
/// `(defclass program-error ...)` from being refused. Two steps removed from
/// the change, and only one ansi-test row went red.
#[test]
fn the_condition_hierarchy_and_the_redefinition_refusals_are_intact() {
    let stdout = run(r#"
      (format t "ERROR-CPL ~a~%"
              (mapcar #'class-name (class-precedence-list (find-class 'error))))
      (format t "PROGRAM-ERROR-CPL ~a~%"
              (mapcar #'class-name (class-precedence-list (find-class 'program-error))))
      (format t "REDEF-BUILTIN ~a~%"
              (handler-case (progn (eval '(defclass integer () ())) :accepted)
                (error () :refused)))
      (format t "REDEF-CONDITION ~a~%"
              (handler-case (progn (eval '(defclass program-error () ())) :accepted)
                (error () :refused)))
      (format t "BUILTIN-SUPERCLASS ~a~%"
              (handler-case (progn (eval '(defclass bad (hash-table) ())) :accepted)
                (error () :refused)))
      (format t "CONDITION-OK~%")
    "#);
    expect(&stdout, &[
        "ERROR-CPL (ERROR SERIOUS-CONDITION CONDITION STANDARD-OBJECT T)",
        "PROGRAM-ERROR-CPL (PROGRAM-ERROR ERROR SERIOUS-CONDITION CONDITION STANDARD-OBJECT T)",
        "REDEF-BUILTIN REFUSED",
        "REDEF-CONDITION REFUSED",
        "BUILTIN-SUPERCLASS REFUSED",
        "CONDITION-OK",
    ]);
}
