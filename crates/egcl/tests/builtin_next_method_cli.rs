// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A user method on a built-in generic such as MAKE-INSTANCE has the built-in
//! behaviour as its next method, and MAKE-INSTANCE of a class NAME dispatches
//! again on the class object, as the standard method on SYMBOL does (CLHS
//! MAKE-INSTANCE). EGCL signalled "The function CALL-NEXT-METHOD is
//! undefined", and skipped methods specialized on the class object when the
//! call named the class (bliss-y3tk7). A single compiled primary method that
//! uses NEXT-METHOD-P must also see its own (empty) next-method chain, not an
//! enclosing method's. Expected values are SBCL 2.6.9's.
use std::process::Command;

const PROGRAM: &str = r#"(progn
  (defvar *nmp* :unset)
  (defclass st () ((tag :initarg :tag :reader tag)))
  (defclass st2 () ((tag :initarg :tag :initform :none :reader tag)))
  (defclass plain () ((a :initarg :a :reader a)))
  (defmethod make-instance ((class (eql (find-class 'st))) &rest initargs)
    (declare (ignore initargs))
    (call-next-method class :tag :x))
  (defmethod make-instance :around ((class (eql (find-class 'st2))) &rest initargs)
    (list :around (tag (apply #'call-next-method class initargs))))
  (defmethod make-instance ((class (eql (find-class 'st2))) &rest initargs)
    (declare (ignore initargs))
    (setq *nmp* (next-method-p))
    (call-next-method))
  (defgeneric solo (x))
  (defmethod solo ((x integer)) (list :nmp (next-method-p)))
  (defgeneric outer (x))
  (defmethod outer ((x integer)) (list :inner (solo x) (call-next-method)))
  (defmethod outer ((x number)) :number)
  (dotimes (i 20) (solo i) (outer i))
  (format t "~&RESULT ~s~%"
          (list (tag (make-instance (find-class 'st)))
                (make-instance 'st2 :tag 5)
                *nmp*
                (tag (make-instance 'st :tag 1))
                (a (make-instance 'plain :a 7))
                (solo 3)
                (outer 4))))"#;

#[test]
fn methods_on_a_builtin_generic_reach_the_builtin_as_next_method() {
    for tier in ["interp", "t0", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains(
                "RESULT (:X (:AROUND 5) T :X 7 (:NMP NIL) (:INNER (:NMP NIL) :NUMBER))"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
