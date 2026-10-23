// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `(compile 'name)` of an interpreted global function compiles it, keeping
//! the same function object (bliss-h5htg). It returned (values NAME NIL NIL)
//! and left the function tree-walked. Generic functions, macros and functions
//! closing over a LET keep working as before.
use std::process::Command;

#[test]
fn compile_compiles_an_existing_interpreted_function() {
    let program = r#"(progn
      (defun sq (x) (* x x))
      (defun rec (n) (if (< n 2) n (+ (rec (- n 1)) (rec (- n 2)))))
      (let ((y 10)) (defun addy (x) (+ x y)))
      (defgeneric gg (x))
      (defmethod gg ((x integer)) (1+ x))
      (defmacro mm (x) x)
      (let ((before (symbol-function 'sq)))
        (format t "~&RESULT ~s~%"
                (list (multiple-value-list (compile 'sq))
                      (eq before (symbol-function 'sq))
                      (sq 7)
                      (progn (compile 'rec) (rec 15))
                      (progn (compile 'addy) (addy 1))
                      (multiple-value-list (compile 'gg)) (gg 1)
                      (multiple-value-list (compile 'mm)) (mm 5))))
      (disassemble 'sq))"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("RESULT ((SQ NIL NIL) T 49 610 11 (GG NIL NIL) 2 (MM NIL NIL) 5)"),
        "{stdout}\n{stderr}"
    );
    assert!(stdout.contains("SQ —"), "{stdout}");
    assert!(!stdout.contains("not a compiled"), "{stdout}");
}

/// `(compile nil '(lambda ...))` expands the lambda's macros during COMPILE
/// (CLHS 3.2.2.2), not at its first call (bliss-bd6r2). The expected list is
/// SBCL 2.6.9's.
#[test]
fn compile_expands_the_macros_of_a_lambda_expression() {
    let program = r#"(progn
      (defvar *flag* nil)
      (defmacro setflag () (setq *flag* t) 1)
      (let ((f (compile nil '(lambda () (setflag)))))
        (format t "~&RESULT ~s~%"
                (list *flag* (funcall f)
                      (progn (setq *flag* nil) (funcall f) *flag*)
                      (functionp f) (compiled-function-p f)
                      (funcall (compile nil '(lambda (x &optional (y 2) &key (z 3))
                                               "doc" (declare (ignorable x)) (list x y z)))
                               1 5 :z 9)
                      (progn (compile 'named-c '(lambda (n) (loop for i below n collect (setflag))))
                             (named-c 3))
                      (funcall (compile nil '(lambda (&rest r) (apply #'+ r))) 1 2 3)))))"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("RESULT (T 1 NIL T T (1 5 9) (1 1 1) 6)"),
        "{stdout}\n{stderr}"
    );
}
