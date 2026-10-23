// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Errors CLHS requires, and one it forbids, in four operators:
//! - APPLY's last argument must be a proper list; a non-list or dotted list
//!   was spread silently (bliss-0qbf).
//! - Unary /= accepts any number; a complex signalled TYPE-ERROR (bliss-cbjz).
//! - DEFPACKAGE :IMPORT-FROM a missing package is a PACKAGE-ERROR, before the
//!   package is made; it interned fresh symbols (bliss-g9i8).
//! - SETF SUBSEQ rejects non-integer and out-of-range bounds; they were
//!   coerced or clamped (bliss-9zfj).
//!
//! The expected line is SBCL 2.6.9's output for the same program.
use std::process::Command;

const PROGRAM: &str = r##"(progn
(defmacro try (form) `(handler-case ,form (error (e) (list :error (if (typep e 'type-error) :type-error (if (typep e 'package-error) :package-error :error))))))
(defun probe ()
  (list
   :apply (try (apply #'list 1 2))
   :apply2 (try (apply #'+ 1 '(2 . 3)))
   :apply-ok (apply #'list 1 '(2 3))
   :apply-funcall (try (funcall #'apply #'list 1 2))
   :neq (try (/= #c(1 2)))
   :neq2 (try (/= #c(1 2) #c(1 2)))
   :neq-bad (try (/= 'x))
   :import (try (eval '(defpackage :g9i8-test (:use :cl) (:import-from :no-such-package-zz #:foo))))
   :import-left (find-package :g9i8-test)
   :import-ok (try (package-name (eval '(defpackage :g9i8-ok (:use) (:import-from :cl #:car)))))
   :subseq (try (let ((s (list 1 2 3))) (setf (subseq s 2 1) '(9)) s))
   :subseq2 (try (let ((s (vector 1 2 3))) (setf (subseq s 0 5) #(7 7)) s))
   :subseq3 (try (let ((s (vector 1 2 3))) (setf (subseq s 'a) #(7)) s))
   :subseq-ok (let ((s (vector 1 2 3 4))) (setf (subseq s 1 3) #(8 9 10)) s)
   :subseq-ok2 (let ((s (list 1 2 3 4))) (setf (subseq s 2) '(5)) s)))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe)))"##;

#[test]
fn operators_signal_the_errors_clhs_requires() {
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
                "RESULT (:APPLY (:ERROR :TYPE-ERROR) :APPLY2 (:ERROR :TYPE-ERROR) :APPLY-OK (1 2 3) \
                 :APPLY-FUNCALL (:ERROR :TYPE-ERROR) :NEQ T :NEQ2 NIL :NEQ-BAD (:ERROR :TYPE-ERROR) \
                 :IMPORT (:ERROR :PACKAGE-ERROR) :IMPORT-LEFT NIL :IMPORT-OK \"G9I8-OK\" \
                 :SUBSEQ (:ERROR :TYPE-ERROR) :SUBSEQ2 (:ERROR :TYPE-ERROR) :SUBSEQ3 (:ERROR :TYPE-ERROR) \
                 :SUBSEQ-OK #(1 8 9 4) :SUBSEQ-OK2 (1 2 5 4))"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
