// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! STRING, VECTOR, FIND-PACKAGE, GENSYM and the compiled SETF store
//! primitives (EGCL::SET-AREF/SET-ELT/SET-CAR/SET-CDR) have evaluated-argument
//! entries, so compiled code calls them directly instead of synthesizing and
//! re-evaluating a form (bliss-qoy0k). Their results must not change; the
//! expected line is what main printed before the change.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defun probe ()
  (let ((v (vector 1 2 3)) (s (make-string 3 :initial-element #\a)) (l (list 1 2 3))
        (m (make-array '(2 2) :initial-element 0)) (c (cons 1 2)) (str "abc"))
    (setf (aref v 1) 9) (setf (svref v 2) 8) (setf (char s 0) #\z) (setf (elt l 2) 7)
    (setf (aref m 1 1) 5) (setf (car c) 'x) (setf (cdr c) 'y)
    (list v s l m c
          (eq (string str) str) (string 'foo) (string #\q)
          (handler-case (funcall #'string) (program-error () :pe))
          (handler-case (string 12) (type-error () :te) (error () :err))
          (vector) (vector 1 'a "b") (funcall #'vector 1 2)
          (package-name (find-package "CL")) (find-package "NO-SUCH-PKG-QOY")
          (package-name (find-package :keyword)) (eq (find-package (find-package "CL-USER")) (find-package "CL-USER"))
          (let ((*gensym-counter* 40)) (list (symbol-name (gensym)) (symbol-name (gensym "P")) (symbol-name (gensym 7)) *gensym-counter*))
          (symbol-package (gensym))
          (handler-case (gensym -1) (type-error () :te))
          (handler-case (setf (elt l 9) 0) (type-error () :te))
          (handler-case (let ((x (vector 1))) (setf (aref x 5) 0)) (error () :err)))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe))
)"#;

#[test]
fn builtins_called_from_compiled_code_keep_their_results() {
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
                "RESULT (#(1 9 8) \"zaa\" (1 2 7) #2A((0 0) (0 5)) (X . Y) T \"FOO\" \"q\" :PE \"12\" \
                 #() #(1 A \"b\") #(1 2) \"COMMON-LISP\" NIL \"KEYWORD\" T (\"G40\" \"P41\" \"G7\" 42) \
                 NIL :TE :TE :ERR)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
