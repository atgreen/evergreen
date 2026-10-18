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

/// A class NAMED AS A SUPERCLASS BEFORE IT IS DEFINED and then defined with a
/// `(:metaclass M)` is an instance of M too (GitHub #384, second half).
///
/// This is the case the first fix could not reach. A class metaobject carries
/// its metaclass's slots inline, allocated with it, so the placeholder minted
/// for a forward reference — before anything has named M — is always a cell
/// short, and pointing it at M's layout would write past the allocation.
/// `CLASS-OF` answered M while TYPEP, SLOT-VALUE and dispatch all still saw a
/// plain STANDARD-CLASS. DEFCLASS now replaces the placeholder with a
/// metaobject sized for the definition, as SBCL CHANGE-CLASSes a
/// FORWARD-REFERENCED-CLASS.
///
/// Gendl's kernel is the reporter: every DEFINE-OBJECT appends VANILLA-MIXIN,
/// which is forward-referenced until its own file loads and is then defined
/// `(:metaclass gdl-class)` — the one class of the seven that failed to
/// dispatch CLEAR-CACHES.
///
/// Both tiers, and the two shapes that must NOT be replaced: a metaclass with
/// no slots fits the placeholder as it stands, and a forward reference with no
/// `:metaclass` at all is untouched. The subclass that caused the forward
/// reference is the thing replacement can break — its recorded superclass is
/// the placeholder — so its inheritance is checked on both.
#[test]
fn a_forward_referenced_class_with_a_user_metaclass_is_an_instance_of_it() {
    let stdout = run(r#"
      (defclass fmeta (standard-class) ((x :initform 1 :accessor fmeta-x)))
      (defmethod egcl-mop:validate-superclass ((c fmeta) (s standard-class)) t)
      (defgeneric fcc (c))
      (defmethod fcc ((c fmeta)) :meta)
      (defmethod fcc ((c standard-class)) :std)

      ;; LATE is only a forward reference here, and gets its metaclass after.
      (defclass late-sub (late) () (:metaclass fmeta))
      (defclass late () () (:metaclass fmeta))

      (format t "CLASS-OF-IS-META ~a~%" (eq (class-of (find-class 'late)) (find-class 'fmeta)))
      (format t "TYPEP-META ~a~%" (typep (find-class 'late) 'fmeta))
      (format t "TYPE-OF ~a~%" (type-of (find-class 'late)))
      (format t "SLOT-EXISTS ~a~%" (slot-exists-p (find-class 'late) 'x))
      (format t "INITFORM ~a~%" (fmeta-x (find-class 'late)))
      (format t "DISPATCH-PREFERS-META ~a~%" (fcc (find-class 'late)))
      (setf (fmeta-x (find-class 'late)) 7)
      (format t "ACCESSOR-ROUNDTRIP ~a~%" (fmeta-x (find-class 'late)))
      ;; Replacing the placeholder must not orphan the subclass that named it:
      ;; its recorded superclass, its precedence list and its instances all
      ;; have to name the class the registry now answers.
      (format t "SUPER-IS-REGISTERED ~a~%"
               (eq (car (egcl-mop:class-direct-superclasses (find-class 'late-sub)))
                   (find-class 'late)))
      (format t "SUB-IS-RECORDED ~a~%"
               (eq (car (egcl-mop:class-direct-subclasses (find-class 'late)))
                   (find-class 'late-sub)))
      (format t "SUB-INHERITS ~a~%" (typep (make-instance 'late-sub) 'late))
      (format t "CLASS-NAME ~a~%" (class-name (find-class 'late)))

      ;; The same questions from inside a compiled function (memory
      ;; `acceptance-eval-wrapper-hides-compiled-path`: a check in the
      ;; tree-walker does not cover cli/bytecode.rs).
      (defun compiled-probe ()
        (list (typep (find-class 'late) 'fmeta)
              (slot-exists-p (find-class 'late) 'x)
              (slot-value (find-class 'late) 'x)
              (fcc (find-class 'late))))
      (format t "COMPILED ~a~%" (compiled-probe))

      ;; Replacement never has to find the placeholder's instances, because a
      ;; forward reference cannot have any -- instantiating one is refused
      ;; (bliss-jv7tj, pinned by its own test below).
      (defclass inst-sub (inst-late) ())
      (format t "INSTANCE-REFUSED ~a~%"
               (not (null (nth-value 1 (ignore-errors (make-instance 'inst-late))))))
      (defclass inst-late () ((s :initform 3)) (:metaclass fmeta))
      (format t "INSTANCE-AFTER-DEFINITION ~a~%"
               (slot-value (make-instance 'inst-sub) 's))

      ;; A metaclass with no slots: the placeholder already fits its layout, so
      ;; the object is reused rather than replaced.
      (defclass bare-meta (standard-class) ())
      (defmethod egcl-mop:validate-superclass ((c bare-meta) (s standard-class)) t)
      (defclass bare-sub (bare-late) ())
      (defclass bare-late () () (:metaclass bare-meta))
      (format t "BARE-TYPEP ~a~%" (typep (find-class 'bare-late) 'bare-meta))
      (format t "BARE-SUB-INHERITS ~a~%" (typep (make-instance 'bare-sub) 'bare-late))

      ;; No :metaclass at all: nothing is replaced, and the inherited slot the
      ;; late definition introduced still reaches the subclass's instances.
      (defclass plain-sub (plain-late) ())
      (defclass plain-late () ((s :initform 7)))
      (format t "PLAIN-SUPER-EQ ~a~%"
               (eq (car (egcl-mop:class-direct-superclasses (find-class 'plain-sub)))
                   (find-class 'plain-late)))
      (format t "PLAIN-INHERITED-SLOT ~a~%" (slot-value (make-instance 'plain-sub) 's))
      (format t "FORWARD-METACLASS-OK~%")
    "#);
    expect(&stdout, &[
        "CLASS-OF-IS-META T",
        "TYPEP-META T",
        "TYPE-OF FMETA",
        "SLOT-EXISTS T",
        "INITFORM 1",
        "DISPATCH-PREFERS-META META",
        "ACCESSOR-ROUNDTRIP 7",
        "SUPER-IS-REGISTERED T",
        "SUB-IS-RECORDED T",
        "SUB-INHERITS T",
        "CLASS-NAME LATE",
        "COMPILED (T T 7 META)",
        "INSTANCE-REFUSED T",
        "INSTANCE-AFTER-DEFINITION 3",
        "BARE-TYPEP T",
        "BARE-SUB-INHERITS T",
        "PLAIN-SUPER-EQ T",
        "PLAIN-INHERITED-SLOT 7",
        "FORWARD-METACLASS-OK",
    ]);
}

/// A class that is only a forward reference cannot be instantiated, and does not
/// claim to be finalized (bliss-jv7tj; spec R9.39).
///
/// `MAKE-INSTANCE`, `ALLOCATE-INSTANCE` and `CHANGE-CLASS` each used to hand
/// back a standard object whose class had no slots yet, because nothing had
/// said what its slots were -- a layout the eventual DEFCLASS then contradicts.
/// SBCL signals ("cannot allocate an instance of a forward referenced class"),
/// and the refusal is also what lets `replace_forward_referenced_class` move a
/// placeholder's registrations without hunting for its instances.
///
/// `CLASS-FINALIZED-P` answered T for everything and `FINALIZE-INHERITANCE` was
/// a no-op, which AMOP permits for a class that finalizes eagerly -- but not for
/// one whose superclass is still undefined, since its precedence list and
/// layout cannot be settled. A class is unfinalized while ANY class it inherits
/// from is a forward reference, which is why `SUB` reports NIL too.
#[test]
fn a_forward_referenced_class_cannot_be_instantiated_or_finalized() {
    let stdout = run(r#"
      (defclass fr-sub2 (fr-undef) ())
      (defun erred (thunk) (not (null (nth-value 1 (ignore-errors (funcall thunk))))))

      ;; Direct forms.
      (format t "MAKE-INSTANCE ~a~%" (erred (lambda () (make-instance 'fr-undef))))
      (format t "ALLOCATE-INSTANCE ~a~%"
               (erred (lambda () (allocate-instance (find-class 'fr-undef)))))
      ;; Through FUNCALL, which takes a different arm of the evaluator.
      (format t "ALLOCATE-VIA-FUNCALL ~a~%"
               (erred (lambda () (funcall #'allocate-instance (find-class 'fr-undef)))))
      ;; CHANGE-CLASS reaches neither: a class with no more slots than the
      ;; instance has re-points the wrapper in place and never allocates.
      (format t "CHANGE-CLASS ~a~%"
               (erred (lambda () (change-class (make-instance 'fr-sub2) 'fr-undef))))
      ;; A defined class is unaffected.
      (format t "DEFINED-CLASS-OK ~a~%" (typep (make-instance 'fr-sub2) 'fr-sub2))

      ;; Finalization tells the truth, for the forward reference AND for the
      ;; subclass that inherits from it.
      (format t "FINALIZED-UNDEF ~a~%" (egcl-mop:class-finalized-p (find-class 'fr-undef)))
      (format t "FINALIZED-SUB ~a~%" (egcl-mop:class-finalized-p (find-class 'fr-sub2)))
      (format t "FINALIZED-STANDARD-OBJECT ~a~%"
               (egcl-mop:class-finalized-p (find-class 'standard-object)))
      (format t "FINALIZE-SIGNALS ~a~%"
               (erred (lambda () (egcl-mop:finalize-inheritance (find-class 'fr-sub2)))))

      ;; The same questions from compiled code (memory
      ;; `acceptance-eval-wrapper-hides-compiled-path`).
      (defun compiled-refusals ()
        (list (erred (lambda () (make-instance 'fr-undef)))
              (erred (lambda () (allocate-instance (find-class 'fr-undef))))
              (egcl-mop:class-finalized-p (find-class 'fr-undef))
              (erred (lambda () (egcl-mop:finalize-inheritance (find-class 'fr-sub2))))))
      (format t "COMPILED ~a~%" (compiled-refusals))

      ;; Every one of them becomes legal the moment the class is defined.
      (defclass fr-undef () ((a :initform 1)))
      (format t "AFTER-MAKE-INSTANCE ~a~%" (slot-value (make-instance 'fr-undef) 'a))
      (format t "AFTER-ALLOCATE ~a~%"
               (typep (allocate-instance (find-class 'fr-undef)) 'fr-undef))
      (format t "AFTER-CHANGE-CLASS ~a~%"
               (typep (change-class (make-instance 'fr-sub2) 'fr-undef) 'fr-undef))
      (format t "AFTER-FINALIZED ~a~%"
               (list (egcl-mop:class-finalized-p (find-class 'fr-undef))
                     (egcl-mop:class-finalized-p (find-class 'fr-sub2))))
      (format t "AFTER-FINALIZE ~a~%"
               (eq (egcl-mop:finalize-inheritance (find-class 'fr-sub2))
                   (find-class 'fr-sub2)))
      (format t "AFTER-INHERITED-SLOT ~a~%" (slot-value (make-instance 'fr-sub2) 'a))
      (format t "FORWARD-REFUSAL-OK~%")
    "#);
    expect(&stdout, &[
        "MAKE-INSTANCE T",
        "ALLOCATE-INSTANCE T",
        "ALLOCATE-VIA-FUNCALL T",
        "CHANGE-CLASS T",
        "DEFINED-CLASS-OK T",
        "FINALIZED-UNDEF NIL",
        "FINALIZED-SUB NIL",
        "FINALIZED-STANDARD-OBJECT T",
        "FINALIZE-SIGNALS T",
        "COMPILED (T T NIL T)",
        "AFTER-MAKE-INSTANCE 1",
        "AFTER-ALLOCATE T",
        "AFTER-CHANGE-CLASS T",
        "AFTER-FINALIZED (T T)",
        "AFTER-FINALIZE T",
        "AFTER-INHERITED-SLOT 1",
        "FORWARD-REFUSAL-OK",
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
