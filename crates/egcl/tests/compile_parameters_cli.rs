// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn compile_and_load(setup: &str, source: &str, before_load: &str, after_load: &str) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("egcl-parameters-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("parameters.lisp");
    let compiled_path = dir.join("parameters.bfasl");
    std::fs::write(&source_path, source).unwrap();
    let program = format!(
        "(progn {setup}
           (multiple-value-bind (output warnings failure)
               (compile-file {:?} :output-file {:?})
             (declare (ignore warnings))
             (assert output) (assert (not failure)))
           {before_load}
           (delete-file {:?})
           (load {:?})
           {after_load}
           (format t \"PARAMETERS-OK~%\"))",
        source_path.to_str().unwrap(),
        compiled_path.to_str().unwrap(),
        source_path.to_str().unwrap(),
        compiled_path.to_str().unwrap(),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PARAMETERS-OK"));
    std::fs::remove_file(compiled_path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn parameter_initialization_waits_until_loading() {
    compile_and_load(
        "(defparameter *initializations* 0)",
        "(defparameter *compiled-parameter* (progn (incf *initializations*) (list 73)))",
        "(assert (zerop *initializations*)) (assert (not (boundp '*compiled-parameter*)))",
        "(assert (= *initializations* 1)) (assert (equal *compiled-parameter* '(73)))",
    );
}

#[test]
fn compilation_preserves_an_existing_parameter_value() {
    compile_and_load(
        "(defparameter *compiled-parameter* :before)",
        "(defparameter *compiled-parameter* :after)",
        "(assert (eq *compiled-parameter* :before))",
        "(assert (eq *compiled-parameter* :after))",
    );
}

#[test]
fn explicit_compile_time_initialization_runs_once() {
    compile_and_load(
        "(defparameter *initializations* 0)",
        "(eval-when (:compile-toplevel :load-toplevel :execute)
           (defparameter *compiled-parameter* (incf *initializations*)))",
        "(assert (= *initializations* 1)) (assert (= *compiled-parameter* 1))",
        "(assert (= *initializations* 2)) (assert (= *compiled-parameter* 2))",
    );
}

#[test]
fn direct_and_macro_generated_parameters_proclaim_special_at_compile_time() {
    compile_and_load(
        "",
        "(progn
           (defparameter direct-parameter :loaded)
           (defmacro define-parameter () '(defparameter expanded-parameter :expanded))
           (define-parameter)
           (defun read-direct-parameter () direct-parameter)
           (defun read-expanded-parameter () expanded-parameter)
           (eval-when (:compile-toplevel)
             (assert (eq (let ((direct-parameter :dynamic)) (symbol-value 'direct-parameter)) :dynamic))
             (assert (eq (let ((expanded-parameter :dynamic)) (symbol-value 'expanded-parameter)) :dynamic))))",
        "(assert (not (boundp 'direct-parameter)))
         (assert (not (boundp 'expanded-parameter)))",
        "(assert (eq (read-direct-parameter) :loaded))
         (assert (eq (read-expanded-parameter) :expanded))",
    );
}

#[test]
fn unrelated_same_named_macros_keep_their_compile_time_effects() {
    compile_and_load(
        "(defparameter *foreign-macro-count* 0)
         (defpackage :parameter-macros (:use :cl) (:shadow :defparameter))
         (defmacro parameter-macros::defparameter (name)
           (declare (ignore name))
           '(eval-when (:compile-toplevel) (incf *foreign-macro-count*)))",
        "(parameter-macros::defparameter ordinary-name)",
        "(assert (= *foreign-macro-count* 1))",
        "(assert (= *foreign-macro-count* 1))",
    );
}
