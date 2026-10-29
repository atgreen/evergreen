// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

fn run(args: &[&str], stress: bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init").args(args);
    for name in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
    ] {
        command.env_remove(name);
    }
    if stress {
        command.envs([
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ]);
    }
    let output = command.output().expect("run constant slot regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stress={stress}: {}\n{stdout}\n{stderr}",
        output.status
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.trim() == "DEFSTRUCT-CONSTANT-SLOTS-OK")
            .count(),
        1,
        "{stdout}\n{stderr}"
    );
}

#[test]
fn defstruct_copiers_preserve_t_and_nil_slot_identity_after_caught_conditions() {
    let source = include_str!("fixtures/defstruct-constant-slots.lisp");
    let dir = std::env::temp_dir().join(format!("egcl-constant-slots-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("constant-slots.lisp");
    let compiled = dir.join("constant-slots.bfasl");
    fs::write(&path, source).unwrap();
    for stress in [false, true] {
        run(&["--load", path.to_str().unwrap()], stress);
    }
    // The quoted EVAL form is read as one object, so establish the control
    // package before reading its qualified symbol tokens.
    run(
        &[
            "--eval",
            r#"(defpackage "CONSTANT-SHADOW" (:use "CL") (:shadow "T" "NIL"))"#,
            "--eval",
            &format!("(eval '(progn {source}))"),
        ],
        false,
    );
    run(
        &[
            "--eval",
            &format!(
                "(progn (multiple-value-bind (file warnings failure) (compile-file {path:?} :output-file {compiled:?}) (declare (ignore file warnings)) (assert (not failure))) (write-line \"DEFSTRUCT-CONSTANT-SLOTS-OK\"))"
            ),
        ],
        false,
    );
    for stress in [false, true] {
        run(&["--load", compiled.to_str().unwrap()], stress);
    }
    fs::remove_dir_all(dir).unwrap();
}
