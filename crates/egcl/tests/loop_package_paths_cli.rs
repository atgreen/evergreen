// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LOOP and WITH-PACKAGE-ITERATOR over a package that does not exist signal
//! PACKAGE-ERROR instead of iterating nothing (ansi-test LOOP.7.18-20).
//!
//! The arithmetic cases pin the value an arithmetic FOR variable has in
//! FINALLY. CLHS leaves it unspecified; EGCL matches SBCL, which steps the
//! variable past the limit, and the ANSI gate disables the ansi-test note that
//! assumes otherwise (:LOOP-ITERATION-VALUES-IN-FINALLY, LOOP.1.39-43).
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defun probe ()
  (list (loop for x from 1 to 5 do (progn) finally (return x))
        (loop for x from 1 below 5 do (progn) finally (return x))
        (loop for x from 10 downto 0 do (progn) finally (return x))
        (loop for x from 10 above 0 do (progn) finally (return x))
        (loop for x from 1 to 10 by 3 finally (return x))
        (loop for x from 1 to 0 finally (return x))
        (loop for x from 1 to 3 collect x)
        (loop for x from 1 to 3 and y = 10 then x collect (list x y))
        (loop for i from 0 for x in '(a b c) collect (cons i x))
        (loop for x of-type (integer 1 5) from 1 to 5 collect x)
        (handler-case (loop for s being each symbol of "NO.SUCH.LOOP.PKG" collect s)
          (package-error () :package-error))
        (handler-case (with-package-iterator (it (list "NO.SUCH.LOOP.PKG") :internal) (it))
          (package-error () :package-error))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe)))"#;

#[test]
fn loop_package_paths_and_arithmetic_finally_values() {
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
                "RESULT (6 5 -1 0 13 1 (1 2 3) ((1 10) (2 1) (3 2)) ((0 . A) (1 . B) (2 . C)) \
                 (1 2 3 4 5) :PACKAGE-ERROR :PACKAGE-ERROR)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
