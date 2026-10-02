// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn saved_macro_expanders_preserve_the_callers_handlers() {
    let directory = std::env::temp_dir().join(format!("egcl-macro-condition-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("macros.lisp");
    let fasl = directory.join("macros.fasl");
    std::fs::write(&source, "(defmacro saved-condition-probe () (error 'program-error))").unwrap();
    let compiled = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!(
            "(multiple-value-bind (path warnings failure) (compile-file {source:?} :output-file {fasl:?}) (declare (ignore warnings)) (assert path) (assert (not failure)))"
        )])
        .output().unwrap();
    assert!(compiled.status.success(), "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout), String::from_utf8_lossy(&compiled.stderr));
    assert!(!String::from_utf8_lossy(&compiled.stderr).contains("source-only"));
    std::fs::remove_file(&source).unwrap();
    let loaded = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!(
            "(load {fasl:?}) (assert (handler-case (progn (macroexpand-1 '(saved-condition-probe)) nil) (program-error () t))) (format t \"SAVED-MACRO-CONDITION-OK~%\")"
        )])
        .output().unwrap();
    assert!(loaded.status.success(), "{}\n{}",
        String::from_utf8_lossy(&loaded.stdout), String::from_utf8_lossy(&loaded.stderr));
    assert!(String::from_utf8_lossy(&loaded.stdout).contains("SAVED-MACRO-CONDITION-OK"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn macroexpand_preserves_the_callers_condition_context() {
    let source = r#"
      (defmacro condition-context-probe () (error 'program-error))
      (assert (handler-case
                  (progn (macroexpand-1 '(condition-context-probe)) nil)
                (program-error () t)))
      (assert (handler-case
                  (progn (macroexpand '(condition-context-probe)) nil)
                (program-error () t)))
      (defmacro recoverable-context-probe ()
        (restart-case (error 'program-error)
          (supply-expansion () 42)))
      (assert (= (handler-bind ((program-error
                                 (lambda (condition)
                                   (declare (ignore condition))
                                   (invoke-restart 'supply-expansion))))
                   (macroexpand-1 '(recoverable-context-probe)))
                 42))
      (assert (eq (restart-case
                      (handler-bind ((program-error
                                       (lambda (condition)
                                         (declare (ignore condition))
                                         (invoke-restart 'recover-caller))))
                        (macroexpand-1 '(condition-context-probe)))
                    (recover-caller () :recovered))
                  :recovered))
      (format t "MACROEXPAND-CONDITION-CONTEXT-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("MACROEXPAND-CONDITION-CONTEXT-OK"));
}
