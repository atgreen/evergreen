// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! GETF and LIST are builtins, and stay correct (bliss-kr8pp).
//!
//! GETF was a DEFUN in boot.lisp — a DO loop plus an &optional — and LIST had
//! no evaluated handler, so COMPILED code reached it through the
//! synthesize-`(name 'arg …)`-and-re-evaluate detour. Measured on x86-64
//! release, 200k iterations:
//!
//!   (getf plist :key)   7.310us -> 0.315us
//!   (list 1 2 3 4)      8.665us -> 0.720us
//!
//! against 0.035us for CAR, which has always had one. They matter because they
//! are how a property list is read and built, which a UI framework does per
//! node per frame.
//!
//! The risk in moving GETF into Rust is not speed but SEMANTICS, and a plist is
//! full of edge cases: a missing key, an explicit default, an ODD-length plist
//! whose final key has no value, SETF of a present key versus an absent one,
//! and reaching it through FUNCALL rather than in operator position — the
//! compiled and tree-walked paths are different dispatches and a builtin can
//! easily be wired into only one. Each is checked here.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (let ((p (list :a 1 :b 2 :width 4)))
    (format t "FOUND ~A~%"    (getf p :width))
    (format t "MISSING ~A~%"  (getf p :zz))
    (format t "DEFAULT ~A~%"  (getf p :zz :fallback))
    (format t "EMPTY ~A~%"    (getf '() :a :d))
    ;; An odd-length plist: :B has no value cell, so GETF must not read past it.
    (format t "ODD ~A~%"      (getf (list :a 1 :b) :b :d))
    (setf (getf p :b) 99)
    (format t "SETF-PRESENT ~A~%" (getf p :b))
    (setf (getf p :new) 7)
    (format t "SETF-ABSENT ~A~%"  (getf p :new))
    (format t "FUNCALL ~A~%"  (funcall #'getf p :width))
    (format t "LIST ~A~%"     (list 1 2 3 4))
    (format t "LIST-EMPTY ~A~%" (list))
    (format t "APPLY-LIST ~A~%" (apply #'list (list 1 2))))
"#;

#[test]
fn getf_and_list_are_correct_builtins() {
    for stress in [false, true] {
        let mut command = Command::new(BIN);
        command.args(["--no-init", "--eval", PROGRAM]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run egcl");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stress={stress}: {stdout}\n{stderr}");
        for expected in [
            "FOUND 4",
            "MISSING NIL",
            "DEFAULT FALLBACK",
            "EMPTY D",
            "ODD D",
            "SETF-PRESENT 99",
            "SETF-ABSENT 7",
            "FUNCALL 4",
            "LIST (1 2 3 4)",
            "LIST-EMPTY NIL",
            "APPLY-LIST (1 2)",
        ] {
            assert!(
                stdout.lines().any(|l| l.trim() == expected),
                "stress={stress}: missing {expected:?} in:\n{stdout}\n{stderr}"
            );
        }
    }
}
