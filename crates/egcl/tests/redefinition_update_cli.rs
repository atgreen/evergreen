// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A class redefinition updates its existing instances through the CLHS
//! protocol, not just by copying the slots that survived (bliss-oi5a6).
//!
//! `update_if_obsolete` migrated an instance to the new layout and left every
//! slot the redefinition ADDED unbound. CLHS 4.3.6 says those slots are
//! initialized by applying their initforms, which is what the default method on
//! `UPDATE-INSTANCE-FOR-REDEFINED-CLASS` does — and that generic function
//! existed in `lib/boot.lisp`, correctly written, with nothing ever calling it.
//! Measured before this: `(defclass c () ())`, an instance, then
//! `(defclass c () ((s :initform 42)))` left S unbound where SBCL answers 42.
//!
//! The trigger is the first access, which is where CLHS requires the update to
//! have happened ("before the next access to the instance"). SHARED-INITIALIZE
//! fills only UNBOUND slots, so arriving late cannot clobber a value the
//! program has already stored — which is what makes a lazy trigger sound.

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

/// The reported behaviour, plus the two things a lazy trigger could get wrong:
/// clobbering a stored value, and running the protocol more than once.
#[test]
fn a_slot_added_by_a_redefinition_takes_its_initform() {
    let stdout = run(r#"
      (defclass c () ((a :initform 1)))
      (defvar *i* (make-instance 'c))
      (eval '(defclass c () ((a :initform 1) (s :initform 42))))
      (format t "ADDED-SLOT ~a~%" (slot-value *i* 's))
      (format t "KEPT-SLOT ~a~%" (slot-value *i* 'a))

      ;; A value already stored in an added slot must survive: SHARED-INITIALIZE
      ;; fills only unbound slots.
      (defclass d () ((a :initform 1)))
      (defvar *j* (make-instance 'd))
      (eval '(defclass d () ((a :initform 1) (s :initform 42))))
      (setf (slot-value *j* 's) 7)
      (format t "WRITE-WINS ~a~%" (slot-value *j* 's))

      ;; An added slot with no initform stays unbound (CLHS 4.3.6).
      (defclass r () ((a :initform 1)))
      (defvar *r* (make-instance 'r))
      (eval '(defclass r () ((a :initform 1) (noinit))))
      (format t "NO-INITFORM ~a~%" (slot-boundp *r* 'noinit))

      ;; Exactly once per instance, however many accesses follow.
      (defvar *n* 0)
      (defclass t1 () ((a :initform 1)))
      (defvar *k* (make-instance 't1))
      (defmethod update-instance-for-redefined-class :after
          ((i t1) added discarded plist &rest initargs)
        (declare (ignore added discarded plist initargs))
        (incf *n*))
      (eval '(defclass t1 () ((a :initform 1) (s :initform 5))))
      (slot-value *k* 'a) (slot-value *k* 's) (slot-boundp *k* 's) (slot-value *k* 'a)
      (format t "PROTOCOL-RUNS ~a~%" *n*)
      (format t "REDEFINITION-UPDATE-OK~%")
    "#);
    expect(&stdout, &[
        "ADDED-SLOT 42",
        "KEPT-SLOT 1",
        "WRITE-WINS 7",
        "NO-INITFORM NIL",
        "PROTOCOL-RUNS 1",
        "REDEFINITION-UPDATE-OK",
    ]);
}

/// The migrated address must not escape into Lisp. `EQ` compares tagged words
/// directly — only the slot accessors chase forwarding — so handing a caller
/// the migration's replacement breaks identity against every other reference
/// the program holds, which all name the forwarded original.
///
/// This is here because I broke it: running the update protocol in
/// SLOT-MAKUNBOUND and returning what it answered made
/// `(eq (slot-makunbound i 'a) i)` NIL, where it is T on the commit before.
/// CLHS says SLOT-MAKUNBOUND returns the instance, and callers compare it.
#[test]
fn the_migrations_replacement_address_does_not_escape_into_lisp() {
    let stdout = run(r#"
      (defclass c () ((a :initform 1)))
      (defvar *i* (make-instance 'c))
      (defvar *alias* *i*)
      (eval '(defclass c () ((a :initform 1) (b :initform 2))))
      ;; SLOT-MAKUNBOUND returns the instance, and it runs the update protocol.
      (format t "MAKUNBOUND-EQ ~a~%" (eq (slot-makunbound *i* 'a) *i*))
      ;; Two references to one instance stay EQ across the update.
      (format t "ALIAS-EQ ~a~%" (eq *i* *alias*))
      ;; And after a read, which is the other path that migrates.
      (defclass d () ((a :initform 1)))
      (defvar *k* (make-instance 'd))
      (defvar *kalias* *k*)
      (eval '(defclass d () ((a :initform 1) (b :initform 2))))
      (slot-value *k* 'a)
      (format t "AFTER-READ-EQ ~a~%" (eq *k* *kalias*))
      ;; An EQ hash table keyed by the instance still finds it.
      (defvar *h* (make-hash-table :test 'eq))
      (setf (gethash *k* *h*) :found)
      (format t "EQ-HASH ~a~%" (gethash *k* *h*))
      (format t "IDENTITY-OK~%")
    "#);
    expect(&stdout, &[
        "MAKUNBOUND-EQ T",
        "ALIAS-EQ T",
        "AFTER-READ-EQ T",
        "EQ-HASH FOUND",
        "IDENTITY-OK",
    ]);
}

/// A user method receives the arguments CLHS specifies, including the values of
/// the slots the redefinition discarded.
#[test]
fn a_user_method_sees_the_added_discarded_and_property_list() {
    let stdout = run(r#"
      (defclass e () ((keep :initform 1) (drop :initform 2)))
      (defvar *k* (make-instance 'e))
      (defvar *seen* nil)
      (defmethod update-instance-for-redefined-class :before
          ((i e) added discarded plist &rest initargs)
        (declare (ignore initargs))
        (setf *seen* (list :added added :discarded discarded :plist plist)))
      (eval '(defclass e () ((keep :initform 1) (fresh :initform 3))))
      (slot-value *k* 'keep)
      (format t "PROTOCOL-ARGS ~a~%" *seen*)
      (format t "FRESH-INITFORM ~a~%" (slot-value *k* 'fresh))
      (format t "DISCARDED-GONE ~a~%" (slot-exists-p *k* 'drop))
      (format t "KEPT ~a~%" (slot-value *k* 'keep))
      (format t "PROTOCOL-ARGS-OK~%")
    "#);
    expect(&stdout, &[
        "PROTOCOL-ARGS (ADDED (FRESH) DISCARDED (DROP) PLIST (DROP 2))",
        "FRESH-INITFORM 3",
        "DISCARDED-GONE NIL",
        "KEPT 1",
        "PROTOCOL-ARGS-OK",
    ]);
}

/// Every first-touch path triggers it, in both tiers. SLOT-BOUNDP answered NIL
/// and SLOT-MAKUNBOUND let a later read resurrect the slot before this.
#[test]
fn every_first_touch_runs_the_update_in_both_tiers() {
    let stdout = run(r#"
      (defmacro obsolete (name)
        `(progn (defclass ,name () ((a :initform 1)))
                (defvar ,(intern (format nil "*~a*" name)) (make-instance ',name))
                (eval '(defclass ,name () ((a :initform 1) (s :initform 42))))))
      (obsolete p1) (obsolete p2) (obsolete p3) (obsolete p4) (obsolete p5)

      ;; SLOT-BOUNDP as the very first touch.
      (format t "BOUNDP-FIRST ~a~%" (slot-boundp *p1* 's))
      ;; A reader accessor as the first touch.
      (defclass p2 () ((a :initform 1) (s :initform 42 :accessor p2-s)))
      (format t "ACCESSOR-FIRST ~a~%" (p2-s *p2*))
      ;; SLOT-MAKUNBOUND first: the protocol has run, so the slot stays unbound
      ;; rather than being re-initialized by the next read.
      (slot-makunbound *p3* 's)
      (format t "MAKUNBOUND-STICKS ~a~%" (slot-boundp *p3* 's))
      (format t "MAKUNBOUND-READ-SIGNALS ~a~%"
               (not (null (nth-value 1 (ignore-errors (slot-value *p3* 's))))))

      ;; The compiled tier, for a read and for SLOT-BOUNDP.
      (defun compiled-read () (slot-value *p4* 's))
      (defun compiled-boundp () (slot-boundp *p5* 's))
      (format t "COMPILED-READ ~a~%" (compiled-read))
      (format t "COMPILED-BOUNDP ~a~%" (compiled-boundp))
      (format t "FIRST-TOUCH-OK~%")
    "#);
    expect(&stdout, &[
        "BOUNDP-FIRST T",
        "ACCESSOR-FIRST 42",
        "MAKUNBOUND-STICKS NIL",
        "MAKUNBOUND-READ-SIGNALS T",
        "COMPILED-READ 42",
        "COMPILED-BOUNDP T",
        "FIRST-TOUCH-OK",
    ]);
}
