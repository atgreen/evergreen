// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! MAKE-PATHNAME :DIRECTORY '(:RELATIVE) is a specified, empty relative
//! directory, distinct from an unspecified one. It was stored as NIL, so as a
//! PATHNAME-MATCH-P wildcard it matched every directory (bliss-p5p5b; ansi
//! PATHNAME-MATCH-P.4).
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defun probe ()
  (let ((rel (make-pathname :directory '(:relative))))
    (list (pathname-directory rel)
          (pathname-match-p (make-pathname :directory '(:relative :wild)) rel)
          (pathname-match-p rel rel)
          (pathname-match-p (make-pathname :directory '(:relative "a"))
                            (make-pathname :directory '(:relative :wild)))
          (namestring (make-pathname :directory '(:relative) :name "x"))
          (namestring (merge-pathnames (make-pathname :directory '(:relative) :name "x") #p"/tmp/"))
          (pathname-directory (make-pathname :directory nil)))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe)))"#;

#[test]
fn an_empty_relative_directory_is_specified() {
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
            stdout.contains("RESULT ((:RELATIVE) NIL T T \"x\" \"/tmp/x\" NIL)"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
