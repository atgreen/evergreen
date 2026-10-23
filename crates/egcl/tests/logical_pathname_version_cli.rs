// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A logical namestring's version field is parsed and kept: NEWEST is
//! :NEWEST, * is :WILD, digits are an integer, and NAMESTRING renders it. The
//! field was dropped, so "cltest:temp.dat.newest" had version NIL and the
//! namestring "CLTEST:TEMP.DAT" (bliss-3kcwz; ansi PARSE-NAMESTRING.5).
use std::process::Command;

const PROGRAM: &str = r#"(progn
(setf (logical-pathname-translations "LPVER") (list (list "**;*.*.*" "/tmp/lpver/**/*.*")))
(defun probe ()
  (let ((p (parse-namestring "lpver:temp.dat.newest")))
    (list (namestring p) (pathname-version p) (typep p 'logical-pathname)
          (pathname-version (parse-namestring "lpver:a;b.c.3"))
          (namestring (parse-namestring "lpver:a;b.c.3"))
          (pathname-version (parse-namestring "lpver:a;b.c.*"))
          (namestring (parse-namestring "lpver:b.c"))
          (pathname-version (parse-namestring "lpver:b.c"))
          (namestring (make-pathname :host "LPVER" :name "X" :type "Y" :version :newest))
          (equal (parse-namestring "lpver:b.c.1") (parse-namestring "lpver:b.c.2"))
          (namestring (translate-logical-pathname "lpver:a;b.c.newest"))
          (pathname-version (parse-namestring "/tmp/a.b")))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe)))"#;

#[test]
fn a_logical_namestring_keeps_its_version() {
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
                "RESULT (\"LPVER:TEMP.DAT.NEWEST\" :NEWEST T 3 \"LPVER:A;B.C.3\" :WILD \"LPVER:B.C\" NIL \
                 \"LPVER:X.Y.NEWEST\" NIL \"/tmp/lpver/a/b.c\" NIL)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
