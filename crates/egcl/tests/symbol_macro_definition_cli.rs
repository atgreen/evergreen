// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::process::Command;

fn run(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run symbol macro definition regression");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("SYMBOL-MACRO-DEFINITION-OK"));
}

#[test]
fn recursive_expansion_preserves_symbol_macro_definition_syntax() {
    run(r#"(progn
      (define-symbol-macro definition-name 42)
      (defmacro expansion-syntax () 99)
      (assert (equal (egcl-ext:macroexpand-all
                      '(define-symbol-macro definition-name (expansion-syntax)))
                     '(define-symbol-macro definition-name (expansion-syntax))))
      (write-line "SYMBOL-MACRO-DEFINITION-OK"))"#);
}

#[test]
fn compile_file_loads_symbol_macros_emitted_by_macrolet() {
    let dir = std::env::temp_dir().join(format!(
        "egcl-symbol-macro-definition-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("definitions.lisp");
    let artifact = dir.join("definitions.bfasl");
    fs::write(
        &source,
        r#"(macrolet ((def-it (sym name) `(define-symbol-macro ,sym ,(find-package name))))
              (def-it definition-cl-package "COMMON-LISP")
              (def-it definition-keyword-package "KEYWORD"))
            (defun defined-package-macros ()
              (and (eq definition-cl-package (find-package "COMMON-LISP"))
                   (eq definition-keyword-package (find-package "KEYWORD"))))"#,
    )
    .unwrap();
    // COMPILE-FILE makes these global symbol macros visible immediately. Its
    // MACROLET load-form expansion must preserve their definition names while
    // walking the emitted load actions a second time.
    run(&format!(
        "(progn (load (compile-file {source:?} :output-file {artifact:?}))
                (assert (defined-package-macros))
                (write-line \"SYMBOL-MACRO-DEFINITION-OK\"))"
    ));
    fs::remove_file(&source).unwrap();
    // Verify the load actions independently of the compilation environment and
    // without the source file, as well as the same-process host-build shape.
    run(&format!(
        "(progn (load {artifact:?}) (assert (defined-package-macros))
                (write-line \"SYMBOL-MACRO-DEFINITION-OK\"))"
    ));
    fs::remove_dir_all(&dir).unwrap();
}
