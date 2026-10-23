// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A LET of an earmuffed name that nobody proclaimed special is lexical, as
//! CLHS requires: earmuffs are a naming convention, not a declaration. EGCL
//! made every `*…*` binding dynamic, so a local SPECIAL declaration seemed to
//! reach nested bindings and callees (bliss-c3gyu; ansi-test SET.2). DEFVAR'd
//! and standard special variables stay dynamic.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defun probe1 ()
  (let ((*y* nil)) (declare (special *y*))
    (let ((f (lambda () *y*))) (let ((*y* t)) (funcall f)))))
(defun probe2 () (let ((*z* 7)) (declare (special *z*)) *z*))
(defun probe4 () (if (boundp '*z*) (symbol-value '*z*) :unbound))
(defun probe3 () (let ((*z* 9)) (list :inner *z* :leaked (probe4))))
(defun probe5 ()
  (multiple-value-list
   (let ((*xx* 'a) (var '*xx*)) (declare (special *xx*))
     (values (let ((*xx* 'c)) (list (set var 'b) *xx* (symbol-value var))) *xx*))))
(defvar *dv* 1)
(defvar *unset*)
(defun dvread () *dv*)
(defun unset-read () (if (boundp '*unset*) *unset* :unbound))
(defun probe6 ()
  (list (let ((*dv* 2)) (dvread))
        (let ((*unset* 3)) (unset-read))
        (let ((*print-base* 16)) (format nil "~a" 255))
        (let ((*package* (find-package :keyword))) (prin1-to-string :a))))
(defun probe () (list (probe1) (probe2) (probe3) (probe5) (probe6)))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s ~s~%" (probe) (funcall (compile nil '(lambda () (probe1))))))"#;

#[test]
fn an_unproclaimed_earmuffed_let_binding_is_lexical() {
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
                "RESULT (NIL 7 (:INNER 9 :LEAKED :UNBOUND) ((B C B) B) (2 3 \"FF\" \":A\")) NIL"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
