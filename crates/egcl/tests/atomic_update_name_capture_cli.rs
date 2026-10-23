// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A macro named ATOMIC-INCF or ATOMIC-DECF in another package runs its own
//! expansion. EGCL matched the operator by name and ran
//! EGCL-EXT:ATOMIC-INCF/DECF instead, while MACROEXPAND-1 returned the user's
//! expansion (GH #576). EGCL-EXT's own operators still work.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defpackage :ain (:use :cl) (:export #:atomic-incf #:atomic-decf))
(defmacro ain:atomic-incf (place &optional (delta 1)) `(list :user-macro ',place ,delta))
(defmacro ain:atomic-decf (place &optional (delta 1)) `(list :user-macro ',place ,delta))
(defvar *n* 10)
(defvar *v* (vector 7))
(defun via-function () (ain:atomic-incf *n* 5))
(defun via-function-decf () (ain:atomic-decf *n* 2))
(defun ext () (list (egcl-ext:atomic-incf *n* 1) (egcl-ext:atomic-decf (svref *v* 0) 2)))
(defun probe ()
  (list (macroexpand-1 '(ain:atomic-incf *n* 5))
        (eval '(ain:atomic-incf *n* 5))
        (via-function)
        (via-function-decf)
        (funcall (compile nil '(lambda () (ain:atomic-incf *n* 4))))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s ~s ~s ~s~%" (probe) *n* (ext) (list *n* (svref *v* 0))))"#;

#[test]
fn a_user_atomic_incf_macro_runs_its_own_expansion() {
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
                "RESULT ((LIST :USER-MACRO (QUOTE *N*) 5) (:USER-MACRO *N* 5) (:USER-MACRO *N* 5) \
                 (:USER-MACRO *N* 2) (:USER-MACRO *N* 4)) 10 (10 7) (11 5)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
