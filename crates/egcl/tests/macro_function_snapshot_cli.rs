// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn checked(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn held_macro_functions_keep_their_definition_and_call_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            include_str!("fixtures/macro-function-snapshot.lisp"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("MACRO-SNAPSHOTS-OK"));
}

#[test]
fn source_and_compiled_macro_snapshots_survive_image_restore() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "egcl-macro-snapshots-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("macro.lisp");
    let fasl = dir.join("macro.bfasl");
    let image = dir.join("macro.core");
    std::fs::write(
        &source,
        "(defmacro compiled-target (x) (list 'list :compiled-old x))",
    )
    .unwrap();
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args([
        "--no-init",
        "--eval",
        &format!(
            "(compile-file {:?} :output-file {:?})",
            source.to_str().unwrap(),
            fasl.to_str().unwrap()
        ),
    ]));
    std::fs::remove_file(&source).unwrap();
    let save = format!(
        "(load {:?})
         (defparameter *compiled-held* (macro-function 'compiled-target))
         (defmacro compiled-target (x) (list 'list :compiled-new x))
         (assert (equal '(list :compiled-old 3)
                        (funcall *compiled-held* '(compiled-target 3) nil)))
         {}
         (egcl-ext:save-lisp-and-die {:?})",
        fasl.to_str().unwrap(),
        include_str!("fixtures/macro-function-snapshot.lisp"),
        image.to_str().unwrap()
    );
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args(["--no-init", "--eval", &save]));
    std::fs::remove_file(&fasl).unwrap();
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args([
        "--no-init",
        "--image",
        image.to_str().unwrap(),
        "--eval",
        "(assert (equal '(list :compiled-old 4)
                        (funcall *compiled-held* '(compiled-target 4) nil)))
         (assert (equal '(list :compiled-new 4)
                        (funcall (macro-function 'compiled-target) '(compiled-target 4) nil)))
         (assert (equal '(list :old 4) (funcall *held-expander* '(snapshot-target 4) nil)))
         (assert (equal '(+ 31 4) (funcall *captured-expander* '(captured-target 4) nil)))
         (assert (equal '(list :old 4) (macroexpand-1 '(copied-target 4))))",
    ]));
    std::fs::remove_file(image).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
