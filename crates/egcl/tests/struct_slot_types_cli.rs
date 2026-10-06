// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::{Command, Output};

const PROGRAM: &str = include_str!("fixtures/struct-slot-types.lisp");

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn constructors_enforce_declared_slot_types() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCT-SLOT-TYPES-OK"));
}

#[test]
fn constructor_slot_checks_survive_compiled_file_loading() {
    let source = std::env::temp_dir().join(format!(
        "egcl-struct-slot-types-{}.lisp",
        std::process::id()
    ));
    let fasl = source.with_extension("bfasl");
    std::fs::write(&source, PROGRAM).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            &format!(
                "(compile-file {:?} :output-file {:?})",
                source.to_str().unwrap(),
                fasl.to_str().unwrap()
            ),
        ])
        .output()
        .unwrap();
    assert_success(&output);
    std::fs::remove_file(source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load", fasl.to_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCT-SLOT-TYPES-OK"));
    std::fs::remove_file(fasl).unwrap();
}

#[test]
fn included_slot_types_survive_image_restore() {
    let image = std::env::temp_dir().join(format!(
        "egcl-struct-slot-types-{}.core",
        std::process::id()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            &format!(
                "(defstruct saved-parent (x 2 :type integer))
                 (defstruct (saved-child (:include saved-parent (x 3 :type (integer 0 5)))) )
                 (egcl-ext:save-lisp-and-die {:?})",
                image.to_str().unwrap()
            ),
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--image",
            image.to_str().unwrap(),
            "--eval",
            "(defstruct (restored-child (:include saved-child)))
             (assert (= 3 (restored-child-x (make-restored-child))))
             (assert (handler-case (progn (make-restored-child :x 6) nil)
                       (type-error () t)))
             (assert (handler-case (progn (make-restored-child :x :bad) nil)
                       (type-error () t)))
             (format t \"RESTORED-SLOT-TYPES-OK~%\")",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("RESTORED-SLOT-TYPES-OK"));
    std::fs::remove_file(image).unwrap();
}
