// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A macro whose expander signals during COMPILE-FILE is a compile-time error
//! (CLHS 3.2.2.2, 3.2.5), as on SBCL and CCL: it is reported, COMPILE-FILE
//! returns WARNINGS-P and FAILURE-P true, and the form compiles to code that
//! signals a PROGRAM-ERROR when reached. EGCL used to say nothing, return NIL
//! for both, and expand the macro again at run time, in whatever dynamic
//! environment it found then (GitHub #514).
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn egcl(program: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap()
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Compile `source` in its own process and return its three values as printed.
fn compile(source: &Path) -> (String, String) {
    let output = egcl(&format!(
        r#"(multiple-value-bind (out warnings-p failure-p) (compile-file "{}")
             (format t "~&RESULT ~s ~s ~s~%" (and out t) warnings-p failure-p))"#,
        source.display()
    ));
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

const SIGNALLING_MACRO: &str = "(defmacro errs-at-compile-time ()\n\
       (if *compile-file-pathname*\n\
           (error \"expanded at compile time, as it should be\")\n\
           ''(:expanded-at-run-time)))\n";

#[test]
fn macroexpansion_error_fails_the_compile_and_signals_at_run_time() {
    let dir = scratch("compile-file-macroexpansion-error");
    let source = dir.join("lazy-src2.lisp");
    std::fs::write(
        &source,
        format!(
            "{SIGNALLING_MACRO}\
             (defun probe-fallback () (errs-at-compile-time))\n\
             (defun probe-closure () (mapcar (lambda (x) (list x (errs-at-compile-time))) '(1)))\n\
             (defun probe-neighbour () :neighbour)\n"
        ),
    )
    .unwrap();
    let (stdout, stderr) = compile(&source);
    assert!(stdout.contains("RESULT T T T"), "{stdout}\n{stderr}");
    assert!(
        stderr.contains("caught ERROR")
            && stderr.contains("DEFUN PROBE-FALLBACK")
            && stderr.contains("DEFUN PROBE-CLOSURE")
            && stderr.contains("during macroexpansion of (ERRS-AT-COMPILE-TIME)")
            && stderr.contains("expanded at compile time, as it should be"),
        "{stdout}\n{stderr}"
    );

    // A fresh process: nothing from the compilation is around to help.
    let fasl = dir.join("lazy-src2.fasl");
    let output = egcl(&format!(
        r#"(progn
             (load "{}")
             (dolist (name '(probe-fallback probe-closure))
               (handler-case (progn (funcall name)
                                    (format t "~&~a EXPANDED-AT-RUN-TIME~%" name))
                 (program-error (e)
                   (format t "~&~a PROGRAM-ERROR ~a~%" name
                           (search "Execution of a form compiled with errors"
                                   (princ-to-string e))))))
             (format t "~&NEIGHBOUR ~s~%" (probe-neighbour)))"#,
        fasl.display()
    ));
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("PROBE-FALLBACK PROGRAM-ERROR 0")
            && stdout.contains("PROBE-CLOSURE PROGRAM-ERROR 0")
            && stdout.contains("NEIGHBOUR :NEIGHBOUR")
            && !stdout.contains("EXPANDED-AT-RUN-TIME"),
        "{stdout}\n{stderr}"
    );
}

/// A top-level macro call whose expander signals is compiled into a load-time
/// action that signals, not one that expands the macro while loading.
#[test]
fn toplevel_macroexpansion_error_signals_at_load_time() {
    let dir = scratch("compile-file-toplevel-macroexpansion-error");
    let source = dir.join("toplevel.lisp");
    std::fs::write(
        &source,
        format!("{SIGNALLING_MACRO}(defvar *seen* (errs-at-compile-time))\n"),
    )
    .unwrap();
    let (stdout, stderr) = compile(&source);
    assert!(stdout.contains("RESULT T T T"), "{stdout}\n{stderr}");
    let output = egcl(&format!(
        r#"(handler-case (progn (load "{}") (format t "~&LOADED ~s~%" *seen*))
             (program-error () (format t "~&LOAD PROGRAM-ERROR~%")))"#,
        dir.join("toplevel.fasl").display()
    ));
    let (stdout, stderr) = text(&output);
    assert!(stdout.contains("LOAD PROGRAM-ERROR"), "{stdout}\n{stderr}");
}

/// Macros that expand at compile time keep compiling cleanly, including one
/// defined by a later EVAL-WHEN and one whose expander calls a function made
/// available earlier in the compilation environment.
#[test]
fn successful_expansions_still_compile_cleanly() {
    let dir = scratch("compile-file-macroexpansion-clean");
    let source = dir.join("clean.lisp");
    std::fs::write(
        &source,
        "(eval-when (:compile-toplevel :load-toplevel :execute)\n\
           (defun helper () 42))\n\
         (defmacro uses-helper () (helper))\n\
         (defun calls-helper-macro () (uses-helper))\n\
         (eval-when (:compile-toplevel :load-toplevel :execute)\n\
           (defmacro late-mac () ''late))\n\
         (defun after-late () (late-mac))\n\
         (defun with-local () (macrolet ((m () ''local)) (m)))\n",
    )
    .unwrap();
    let (stdout, stderr) = compile(&source);
    assert!(stdout.contains("RESULT T NIL NIL"), "{stdout}\n{stderr}");
    assert!(!stderr.contains("caught ERROR"), "{stdout}\n{stderr}");
    let output = egcl(&format!(
        r#"(progn (load "{}")
             (format t "~&VALUES ~s ~s ~s~%" (calls-helper-macro) (after-late) (with-local)))"#,
        dir.join("clean.fasl").display()
    ));
    let (stdout, stderr) = text(&output);
    assert!(
        stdout.contains("VALUES 42 LATE LOCAL"),
        "{stdout}\n{stderr}"
    );
}

/// A COMPILE-FILE nested in another one's compile-time evaluation keeps its
/// failure to itself.
#[test]
fn nested_compile_file_failure_does_not_fail_the_outer_compile() {
    let dir = scratch("compile-file-macroexpansion-nested");
    let inner = dir.join("inner.lisp");
    std::fs::write(
        &inner,
        format!("{SIGNALLING_MACRO}(defun inner-probe () (errs-at-compile-time))\n"),
    )
    .unwrap();
    let outer = dir.join("outer.lisp");
    std::fs::write(
        &outer,
        format!(
            "(eval-when (:compile-toplevel)\n\
               (defparameter cl-user::*inner-failure*\n\
                 (nth-value 2 (compile-file \"{}\"))))\n\
             (defun outer-probe () :outer)\n",
            inner.display()
        ),
    )
    .unwrap();
    let output = egcl(&format!(
        r#"(multiple-value-bind (out warnings-p failure-p) (compile-file "{}")
             (format t "~&RESULT ~s ~s ~s INNER ~s~%" (and out t) warnings-p failure-p
                     (symbol-value (find-symbol "*INNER-FAILURE*" "CL-USER"))))"#,
        outer.display()
    ));
    let (stdout, stderr) = text(&output);
    assert!(
        stdout.contains("RESULT T NIL NIL INNER T"),
        "{stdout}\n{stderr}"
    );
}

/// A handler that answers the expander's error with a non-local exit leaves
/// COMPILE-FILE: the compile-time passes that tolerate a failed expansion must
/// not swallow the transfer.
#[test]
fn handler_exit_from_a_failed_expansion_leaves_compile_file() {
    let dir = scratch("compile-file-macroexpansion-handler-exit");
    let source = dir.join("exit.lisp");
    std::fs::write(
        &source,
        format!("{SIGNALLING_MACRO}(errs-at-compile-time)\n"),
    )
    .unwrap();
    let output = egcl(&format!(
        r#"(format t "~&EXIT ~s~%"
             (block out
               (handler-bind ((error (lambda (c) (return-from out (princ-to-string c)))))
                 (compile-file "{}")
                 :completed)))"#,
        source.display()
    ));
    let (stdout, stderr) = text(&output);
    assert!(
        stdout.contains("EXIT \"expanded at compile time, as it should be\""),
        "{stdout}\n{stderr}"
    );
}
