// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! TYPE-OF a pathname is PATHNAME, and of a logical pathname LOGICAL-PATHNAME.
//! Both answered T, the type of every object.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(setf (logical-pathname-translations "TYPEOFLP") (list (list "**;*.*.*" "/tmp/x/**/*.*")))
(defun probe ()
  (list (type-of (pathname "/a/b.c"))
        (type-of (make-pathname :name "x"))
        (type-of (logical-pathname "TYPEOFLP:A;B.C"))
        (subtypep (type-of (pathname "/a")) 'pathname)))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe)))"#;

#[test]
fn type_of_a_pathname_names_its_class() {
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
            stdout.contains("RESULT (PATHNAME PATHNAME LOGICAL-PATHNAME T)"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
