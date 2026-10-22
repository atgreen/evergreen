// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A built-in generic function falls back to its built-in behaviour when no
//! user method applies, and calls made while that built-in runs dispatch as
//! they would anywhere else (GitHub #519).

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval(program: &str) -> String {
    let out = Command::new("timeout")
        .args(["--kill-after=5", "120", BIN, "--no-init", "--eval", program])
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// GitHub #519: an INITIALIZE-INSTANCE :after method makes another instance.
/// Every pairing of a direct call, APPLY and FUNCALL for the outer and the
/// inner MAKE-INSTANCE must build both; the four through #'make-instance on
/// both levels signalled no-applicable-method.
#[test]
fn make_instance_nests_inside_an_after_method_through_every_call_form() {
    let out = eval(
        "(progn \
           (defclass part () ((n :initarg :n :reader part-n))) \
           (defclass outer () ((part :accessor outer-part))) \
           (defvar *inner*) \
           (defmethod initialize-instance :after ((o outer) &rest initargs) \
             (declare (ignore initargs)) \
             (setf (outer-part o) \
                   (ecase *inner* \
                     (:direct (make-instance 'part :n 1)) \
                     (:apply (apply #'make-instance 'part :n 2 '())) \
                     (:funcall (funcall #'make-instance 'part :n 3))))) \
           (loop for inner in '(:direct :apply :funcall) \
                 append (progn (setq *inner* inner) \
                          (list (part-n (outer-part (make-instance 'outer))) \
                                (part-n (outer-part (apply #'make-instance 'outer '()))) \
                                (part-n (outer-part (funcall #'make-instance 'outer)))))))",
    );
    assert_eq!(out, "(1 1 1 2 2 2 3 3 3)");
}

/// The configuration the fallback exists for: a user method on MAKE-INSTANCE
/// for one class makes the name a user generic, and every other class is still
/// built by the built-in, including one made inside another's :after method.
#[test]
fn a_user_make_instance_method_leaves_other_classes_and_nested_calls_working() {
    let out = eval(
        "(progn \
           (defclass special-thing () ((tag :initarg :tag :reader tag))) \
           (defclass plain () ((tag :initarg :tag :reader tag))) \
           (defclass holder () ((inside :accessor inside))) \
           (defmethod make-instance ((name (eql 'special-thing)) &rest initargs) \
             (declare (ignore initargs)) \
             (apply #'make-instance 'plain '(:tag :special))) \
           (defmethod initialize-instance :after ((h holder) &rest initargs) \
             (declare (ignore initargs)) \
             (setf (inside h) \
                   (list (tag (apply #'make-instance 'plain :tag :nested '())) \
                         (tag (funcall #'make-instance 'special-thing))))) \
           (list (tag (make-instance 'plain :tag :direct)) \
                 (tag (apply #'make-instance 'plain '(:tag :applied))) \
                 (inside (apply #'make-instance 'holder '()))))",
    );
    assert_eq!(out, "(:DIRECT :APPLIED (:NESTED :SPECIAL))");
}

/// CLASS-NAME takes the same fallback: a user method for one class leaves the
/// built-in reader for the rest, also when it is reached during another
/// built-in fallback.
#[test]
fn class_name_falls_back_inside_a_nested_built_in_call() {
    let out = eval(
        "(progn \
           (defclass renamed () ()) \
           (defclass watched () ((seen :accessor seen))) \
           (defmethod class-name ((c (eql (find-class 'renamed)))) 'other-name) \
           (defmethod initialize-instance :after ((w watched) &rest initargs) \
             (declare (ignore initargs)) \
             (setf (seen w) (list (funcall #'class-name (find-class 'watched)) \
                                  (funcall #'class-name (find-class 'renamed))))) \
           (seen (apply #'make-instance 'watched '())))",
    );
    assert_eq!(out, "(WATCHED OTHER-NAME)");
}
