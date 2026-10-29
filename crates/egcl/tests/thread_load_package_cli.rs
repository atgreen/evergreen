// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LOAD binds *PACKAGE* dynamically, so other threads cannot move a loading
//! thread's package.
//!
//! LOAD and COMPILE-FILE snapshotted the caller's package and restored it at
//! the end, but during the load the file's package lived in the GLOBAL value,
//! which every thread shares. A file that SETQs *PACKAGE* -- as Gendl's
//! GDL:INITIALIZE does -- and then starts a thread lost its package as soon as
//! the thread started (the new thread's view is the global value) or any
//! thread SETQed *PACKAGE*: the loading thread read its next form in the
//! other package, and "undefined function" followed for functions the same
//! file had just defined.
//!
//! Every symbol in the loaded sources is package-qualified, so a regression
//! shows as a wrong answer rather than an unreadable form.

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

const SOURCE: &str = r#"
(cl:defpackage :tlp (:use :cl))
(cl:defun cl-user::pkg () (cl:package-name cl:*package*))
(cl:defun cl-user::run (fn) (egcl-thread:join-thread (egcl-thread:make-thread fn) :timeout 30))
(cl:setq cl:*package* (cl:find-package :tlp))
(cl:format cl:t "~&AFTER-SETQ ~a~%" (cl-user::pkg))
(cl:format cl:t "~&THREAD-SEES ~a~%" (cl-user::run (cl:lambda () (cl-user::pkg))))
(cl:format cl:t "~&AFTER-THREAD ~a~%" (cl-user::pkg))
(cl-user::run (cl:lambda () (cl:setq cl:*package* (cl:find-package :keyword)) :ok))
(cl:format cl:t "~&AFTER-THREAD-SETQ ~a~%" (cl-user::pkg))
;; A bare symbol read now must land in TLP.
(cl:format cl:t "~&BARE-SYMBOL ~a~%" (cl:package-name (cl:symbol-package 'bare-sym)))
"#;

#[test]
fn a_thread_cannot_move_the_package_of_a_file_being_loaded() {
    let path = std::env::temp_dir().join(format!("egcl-tlp-{}.lisp", std::process::id()));
    std::fs::write(&path, SOURCE).expect("write source");
    let stdout = egcl(&format!(
        "(cl:progn (cl:load {:?}) (cl:format cl:t \"~&CALLER ~a~%\" (cl:package-name cl:*package*)))",
        path.to_string_lossy()
    ));
    let _ = std::fs::remove_file(&path);
    for expected in [
        "AFTER-SETQ TLP",
        // A new thread sees the global value, as in SBCL.
        "THREAD-SEES COMMON-LISP-USER",
        "AFTER-THREAD TLP",
        "AFTER-THREAD-SETQ TLP",
        "BARE-SYMBOL TLP",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}

#[test]
fn a_stream_load_binds_the_package_too() {
    let path = std::env::temp_dir().join(format!("egcl-tlp-stream-{}.lisp", std::process::id()));
    std::fs::write(&path, SOURCE).expect("write source");
    let program = format!(
        "(cl:progn (cl:with-open-file (s {:?}) (cl:load s)) (cl:format cl:t \"~&DONE~%\"))",
        path.to_string_lossy()
    );
    let stdout = egcl(&program);
    let _ = std::fs::remove_file(&path);
    for expected in [
        "AFTER-SETQ TLP",
        "AFTER-THREAD TLP",
        "AFTER-THREAD-SETQ TLP",
        "BARE-SYMBOL TLP",
        "DONE",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}
