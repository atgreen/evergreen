// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A call to another package's LENGTH is not a call to CL:LENGTH.
//!
//! The direct-builtin shortcut matched a callee by its bare name. A generic
//! function named by a symbol of another package -- `(defgeneric other::length
//! (x))`, in a package that does not use COMMON-LISP -- leaves the symbol's
//! function cell empty, so nothing refused the match, and COMPILE-FILE'd code
//! called CL:LENGTH instead: "The value 42 is not of type SEQUENCE." Every Gendl
//! message named like a CL function (LENGTH, FIRST, POSITION, ...) is such a
//! generic function.

use std::process::Command;

fn egcl(program: &str) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

const DEFS: &str = r#"
(cl:defpackage :dbp (:use))
(cl:defgeneric dbp::length (x))
(cl:defmethod dbp::length ((x cl:t)) (cl:list :dbp-length x))
(cl:defgeneric dbp::first (x))
(cl:defmethod dbp::first ((x cl:t)) (cl:list :dbp-first x))
"#;

const CALLERS: &str = r#"
(cl:defun cl-user::call-dbp-length (x) (dbp::length x))
(cl:defun cl-user::call-dbp-first (x) (dbp::first x))
(cl:defun cl-user::call-cl-length (x) (cl:length x))
(cl:defun cl-user::call-cl-first (x) (cl:first x))
"#;

#[test]
fn compiled_calls_reach_another_packages_generic_length_and_first() {
    let dir = std::env::temp_dir().join(format!("egcl-dbp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let defs = dir.join("defs.lisp");
    let callers = dir.join("callers.lisp");
    let fasl = dir.join("callers.fasl");
    std::fs::write(&defs, DEFS).expect("write defs");
    std::fs::write(&callers, CALLERS).expect("write callers");
    let stdout = egcl(&format!(
        "(cl:progn (cl:load {defs:?}) (cl:compile-file {callers:?} :output-file {fasl:?}) (cl:load {fasl:?}) \
          (cl:dolist (call (cl:list (cl:list \"DBP-LENGTH\" 'call-dbp-length 42) (cl:list \"DBP-FIRST\" 'call-dbp-first 42) \
                                    (cl:list \"CL-LENGTH\" 'call-cl-length (cl:list 1 2 3)) (cl:list \"CL-FIRST\" 'call-cl-first (cl:list 1 2 3)))) \
            (cl:format cl:t \"~&~a ~s~%\" (cl:first call) \
              (cl:handler-case (cl:funcall (cl:second call) (cl:third call)) (cl:error (e) (cl:list :error (cl:type-of e)))))))",
        defs = defs.to_string_lossy(),
        callers = callers.to_string_lossy(),
        fasl = fasl.to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for expected in [
        "DBP-LENGTH (:DBP-LENGTH 42)",
        "DBP-FIRST (:DBP-FIRST 42)",
        "CL-LENGTH 3",
        "CL-FIRST 1",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
}
