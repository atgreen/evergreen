// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! (setf (symbol-function x) f) on a non-symbol signals TYPE-ERROR, as CLHS
//! requires and ansi-test SYMBOL-FUNCTION.ERROR.4 checks; it was a
//! SIMPLE-ERROR. SETF FDEFINITION of a (setf name) still works.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defun sf (x) (setf (symbol-function x) (function identity)))
(defun probe ()
  (list (handler-case (sf 1)
          (type-error (e) (list :type-error (type-error-datum e) (type-error-expected-type e)))
          (error (e) (list :other e)))
        (handler-case (sf "x") (type-error () :type-error))
        (progn (sf 'sfok) (funcall 'sfok 5))
        (progn (setf (fdefinition '(setf sfp)) (lambda (v x) (list v x)))
               (funcall (fdefinition '(setf sfp)) 1 2))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe)))"#;

#[test]
fn setf_symbol_function_of_a_non_symbol_is_a_type_error() {
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
            stdout.contains("RESULT ((:TYPE-ERROR 1 SYMBOL) :TYPE-ERROR 5 (1 2))"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
