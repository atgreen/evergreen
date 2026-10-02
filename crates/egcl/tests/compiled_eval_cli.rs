// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn run(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_T1_THRESHOLD", "10")
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

#[test]
fn eval_call_compiles_and_preserves_its_environment_and_values() {
    let stdout = run(r#"(progn
          (defparameter *eval-dynamic* :global)
          (defun eval-wrapper (form)
            (dotimes (i 1))
            (let ((eval-local 42))
              (flet ((eval-local-function () :local))
                (eval form))))
          (disassemble #'eval-wrapper)
          (format t "DYNAMIC:~S~%"
            (let ((*eval-dynamic* :bound)) (eval-wrapper '*eval-dynamic*)))
          (format t "VALUES:~S~%"
            (multiple-value-list (eval-wrapper '(values 3 4))))
          (format t "ZERO:~S~%"
            (multiple-value-list (eval-wrapper '(values))))
          (format t "LEXICAL:~S~%"
            (handler-case (eval-wrapper 'eval-local)
              (unbound-variable () :unbound)))
          (format t "FUNCTION:~S~%"
            (handler-case (eval-wrapper '(eval-local-function))
              (undefined-function () :undefined)))
          (dotimes (i 600)
            (unless (equal (multiple-value-list (eval-wrapper '(values 3 4))) '(3 4))
              (error "EVAL values changed during promotion")))
          (format t "TIER:~A~%" (egcl-ext:function-tier 'eval-wrapper))
          (format t "HOT:~S~%"
            (let ((*eval-dynamic* :hot)) (eval-wrapper '*eval-dynamic*))))"#);
    for expected in [
        "DYNAMIC::BOUND",
        "VALUES:(3 4)",
        "ZERO:NIL",
        "LEXICAL::UNBOUND",
        "FUNCTION::UNDEFINED",
        "HOT::HOT",
    ] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
    // Inspect before the first invocation so a hot-call retry cannot hide
    // a whole-function fallback caused by the EVAL call.
    assert!(stdout.contains("; EVAL-WRAPPER —"), "{stdout}");
    assert!(
        stdout.contains("TIER:1") || stdout.contains("TIER:2"),
        "{stdout}"
    );
}

#[test]
fn eval_is_a_function_with_exactly_one_argument() {
    let stdout = run(r#"(progn
          (format t "FUNCTION:~S~%" (not (null (fboundp 'eval))))
          (format t "CALL:~S~%" (funcall #'eval '(+ 2 3)))
          (format t "DIRECT:~S~%"
            (handler-case (eval 1 2) (program-error () :arity)))
          (format t "INDIRECT:~S~%"
            (handler-case (funcall 'eval) (program-error () :arity))))"#);
    for expected in ["FUNCTION:T", "CALL:5", "DIRECT::ARITY", "INDIRECT::ARITY"] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
}

#[test]
fn eval_call_survives_compilation_and_fresh_process_loading() {
    let dir = std::env::temp_dir().join(format!("egcl-compiled-eval-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("eval.lisp");
    let fasl = dir.join("eval.fasl");
    std::fs::write(&source, "(defun saved-eval (form) (eval form))").unwrap();
    run(&format!("(compile-file {source:?} :output-file {fasl:?})"));
    // A fresh process has no interpreted definition left over from compilation.
    let stdout = run(&format!(
        "(progn (load {fasl:?}) (disassemble #'saved-eval) \
         (format t \"RESULT:~S~%\" \
           (multiple-value-list (saved-eval '(values 7 8)))))"
    ));
    std::fs::remove_dir_all(dir).unwrap();
    assert!(stdout.contains("RESULT:(7 8)"), "{stdout}");
    assert!(stdout.contains("; SAVED-EVAL —"), "{stdout}");
}
