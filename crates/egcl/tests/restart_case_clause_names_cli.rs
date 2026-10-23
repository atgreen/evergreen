// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A RESTART-CASE clause is not a form. A restart named `:LOOP` was expanded as
//! a call to the LOOP macro when a DEFUN body was macroexpanded at definition
//! time, which signalled `Unsupported LOOP action "NIL"` into the user's
//! handlers and failed COMPILE-FILE (GitHub #528). A HANDLER-CASE clause type
//! such as `(OR ERROR STORAGE-CONDITION)` was expanded as an OR form the same way.
use std::path::PathBuf;
use std::process::{Command, Output};

fn egcl(program: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap()
}

fn run(program: &str) -> String {
    let output = egcl(program);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

#[test]
fn defining_a_function_does_not_expand_restart_clauses_as_forms() {
    let stdout = run(r#"(progn
         (defvar *signalled* '())
         (handler-bind ((error (lambda (c) (push (princ-to-string c) *signalled*))))
           (eval '(defun named-loop ()
                    (restart-case (invoke-restart :loop)
                      (:loop () :restarted))))
           (eval '(defun named-when (x)
                    (restart-case (if x (invoke-restart 'when 5) 0)
                      (when (n) :report "Retry." (when n (list :when n))))))
           (eval '(defun compound-clause ()
                    (handler-case (error "z")
                      ((or error storage-condition) (e) (list :caught (type-of e))))))
           (eval '(defun handled-loop ()
                    (block b
                      (handler-bind ((error (lambda (c) (when c (return-from b :handled)))))
                        (error "y"))))))
         (format t "~&SIGNALLED ~s~%" *signalled*)
         (format t "~&RESULT ~s~%"
                 (list (named-loop) (named-when t) (compound-clause) (handled-loop)
                       (macro-function :loop) (and (macro-function 'loop) t))))"#);
    assert!(stdout.contains("SIGNALLED NIL"), "{stdout}");
    assert!(
        stdout.contains("RESULT (:RESTARTED (:WHEN 5) (:CAUGHT SIMPLE-ERROR) :HANDLED NIL T)"),
        "{stdout}"
    );
}

#[test]
fn compile_file_accepts_a_restart_named_loop() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("restart-case-loop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("keyword-loop.lisp");
    std::fs::write(
        &source,
        "(defun restart-named-loop ()\n  \
           (restart-case (invoke-restart :loop)\n    \
             (:loop () :restarted)))\n",
    )
    .unwrap();
    let stdout = run(&format!(
        r#"(multiple-value-bind (out warnings-p failure-p) (compile-file "{}")
             (load out)
             (format t "~&RESULT ~s ~s ~s~%" warnings-p failure-p (restart-named-loop)))"#,
        source.display()
    ));
    assert!(stdout.contains("RESULT NIL NIL :RESTARTED"), "{stdout}");
}
