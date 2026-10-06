// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = include_str!("fixtures/struct-slot-writers.lisp");

#[test]
fn structure_writers_enforce_effective_slot_types() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCT-SLOT-WRITERS-OK"));
}

#[test]
fn structure_writer_checks_survive_source_free_compiled_loading() {
    let dir = std::env::temp_dir().join(format!("egcl-writers-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("writers.lisp");
    let fasl = dir.join("writers.bfasl");
    std::fs::write(&source, PROGRAM).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            &format!("(compile-file {:?} :output-file {:?})", source, fasl),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_file(source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load", fasl.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCT-SLOT-WRITERS-OK"));
    std::fs::remove_file(fasl).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn structure_writer_checks_survive_image_restore() {
    let image = std::env::temp_dir().join(format!("egcl-writers-{}.core", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            &format!(
                "(defstruct saved-writer (x 1 :type integer))
             (egcl-ext:save-lisp-and-die {:?})",
                image
            ),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--image",
            image.to_str().unwrap(),
            "--eval",
            "(let ((object (make-saved-writer)))
               (assert (= 3 (setf (saved-writer-x object) 3)))
               (assert (handler-case (progn (setf (saved-writer-x object) :bad) nil)
                         (type-error () t)))
               (assert (= 3 (saved-writer-x object))))
             (format t \"RESTORED-WRITERS-OK~%\")",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("RESTORED-WRITERS-OK"));
    std::fs::remove_file(image).unwrap();
}
