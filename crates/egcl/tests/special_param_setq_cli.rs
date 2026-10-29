// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! SETQ of a special variable that is bound as a parameter reaches the dynamic
//! binding a callee reads.
//!
//! A parameter whose name is special is bound in the frame and then made
//! dynamic by `enter_body_special_decls` (bliss-pw4d), which leaves the frame
//! binding behind. `lookup_var_symbol` reads a special from the value cell,
//! but `assign_var_symbol` tried the frame first, so SETQ and SETF wrote the
//! stale frame copy: the function's callees -- and even its own later
//! `(symbol-value ...)` -- kept the value the parameter was bound to.
//!
//! cl-who's TREE-TO-COMMANDS binds `((:indent *indent*) *indent*)`, SETQs a
//! T to 0, and its callees then add to *INDENT*: "T is not of type NUMBER"
//! for any `:indent t` caller (Gendl's web pages pass it).
//!
//! The bytecode tier declines a function with a special-named parameter
//! (bytecode.rs, "lambda-list:special-parameter-name"), so every case here
//! runs the tree-walker whichever tier calls it; the DEFUN shapes and a
//! COMPILE-FILE'd one are covered anyway.

use std::process::Command;

const PROGRAM: &str = r#"
  (defvar *ind* nil)
  (defun reader () *ind*)
  (defun key-alias (&key ((:indent *ind*) *ind*))
    (when (and *ind* (not (integerp *ind*))) (setq *ind* 0))
    (reader))
  (defun key-plain (&key (*ind* *ind*)) (setq *ind* 0) (reader))
  (defun opt (&optional (*ind* *ind*)) (setq *ind* 0) (reader))
  (defun req (*ind*) (setq *ind* 0) (reader))
  (defun req-setf (*ind*) (setf *ind* 1) (reader))
  (defun req-noset (*ind*) (reader))
  (defun req-own-read (*ind*) (setq *ind* 2) (list *ind* (symbol-value '*ind*) (reader)))
  ;; A name declared special in the body, not earmuffed.
  (defun declared (v) (declare (special v)) (setq v 3) (declared-reader))
  (defun declared-reader () (declare (special v)) v)
  ;; LET was already right; keep it so.
  (defun via-let () (let ((*ind* t)) (setq *ind* 0) (reader)))
  (defmacro report (label form)
    `(format t "~a ~s~%" ,label (handler-case ,form (error (e) (list :error (princ-to-string e))))))
  (report "KEY-ALIAS" (key-alias :indent t))
  (report "KEY-PLAIN" (key-plain :*ind* t))
  (report "OPTIONAL" (opt t))
  (report "REQUIRED" (req t))
  (report "REQUIRED-SETF" (req-setf t))
  (report "REQUIRED-NOSET" (req-noset 7))
  (report "OWN-READ" (req-own-read t))
  (report "DECLARED" (declared t))
  (report "VIA-LET" (via-let))
  ;; The binding is still undone on exit.
  (report "GLOBAL-AFTER" *ind*)
  (format t "SPECIAL-PARAM-OK~%")
"#;

#[test]
fn setq_of_a_special_parameter_reaches_the_dynamic_binding() {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for expected in [
        "KEY-ALIAS 0",
        "KEY-PLAIN 0",
        "OPTIONAL 0",
        "REQUIRED 0",
        "REQUIRED-SETF 1",
        "REQUIRED-NOSET 7",
        "OWN-READ (2 2 2)",
        "DECLARED 3",
        "VIA-LET 0",
        "GLOBAL-AFTER NIL",
        "SPECIAL-PARAM-OK",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}
