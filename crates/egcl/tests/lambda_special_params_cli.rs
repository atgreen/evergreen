// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Two eval-and-compile conformance fixes:
//! - A lambda parameter declared or proclaimed special is bound dynamically as
//!   soon as it is bound, so later &optional/&key/&aux init forms, and the
//!   functions they call, see the new value (ansi-test LAMBDA.64). A free
//!   SPECIAL declaration still applies only to the body.
//! - DEFINE-COMPILER-MACRO and SETF COMPILER-MACRO-FUNCTION accept the
//!   function name (setf NAME), and COMPILER-MACRO-FUNCTION returns it (ansi
//!   DEFINE-COMPILER-MACRO.4). Calls to NAME itself are not expanded with it.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(define-compiler-macro (setf cm-place) (newval x)
  (return-from cm-place `(setf (car ,x) ,newval)))
(defun cm-place (x) (car x))
(defun probe ()
  (list
   (let ((x :bad)) (declare (special x))
     (flet ((%f () x))
       (list ((lambda (x &aux (y (%f))) (declare (special x)) y) :good)
             ((lambda (x &optional (y (%f))) (declare (special x)) y) :good)
             (funcall (lambda (x &key (y (%f))) (declare (special x)) y) :good)
             (funcall (lambda (&aux (y (%f))) (declare (special x)) y))
             (funcall (lambda (x) (declare (special x)) (%f)) :inner)
             (%f))))
   (let ((x :lex)) (funcall (lambda (&aux (y x)) (declare (special x)) y)))
   (functionp (compiler-macro-function '(setf cm-place)))
   (compiler-macro-function 'cm-place)
   (funcall (compiler-macro-function '(setf cm-place)) '(funcall #'(setf cm-place) 1 c) nil)
   (cm-place (list 7))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe))
(setf (compiler-macro-function '(setf cm-place)) nil)
(format t "~&REMOVED ~s~%" (compiler-macro-function '(setf cm-place))))"#;

#[test]
fn special_parameters_and_setf_compiler_macros() {
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
                "RESULT ((:GOOD :GOOD :GOOD :BAD :INNER :BAD) :LEX T NIL (SETF (CAR C) 1) 7)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
        assert!(stdout.contains("REMOVED NIL"), "{tier}: {stdout}\n{stderr}");
    }
}
