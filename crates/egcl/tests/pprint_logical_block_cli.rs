// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn checked(command: &mut Command) {
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("PPRINT-LOGICAL-BLOCK-OK"), "{stdout}");
    assert!(stdout.contains("QUICKLISP-PPRINT-OK"), "{stdout}");
}

#[test]
fn logical_blocks_execute_in_both_evaluators() {
    let source = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/pprint-logical-block.lisp"
    );
    for tier in ["interp", "t0", "t1", "t2"] {
        checked(
            Command::new(env!("CARGO_BIN_EXE_egcl"))
                .env("EGCL_FORCE_TIER", tier)
                .args(["--no-init", "--load", source]),
        );
    }
}

#[test]
fn logical_block_layout_and_circular_output() {
    for (fixture, marker) in [
        ("pprint-layout", "PPRINT-LAYOUT-OK"),
        ("pprint-circle", "PPRINT-CIRCLE-OK"),
        ("pprint-gray", "PPRINT-GRAY-OK"),
    ] {
        let path = format!(
            "{}/tests/fixtures/{fixture}.lisp",
            env!("CARGO_MANIFEST_DIR")
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--load", &path])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{fixture}: {stdout}\n{stderr}");
        assert!(stdout.contains(marker), "{fixture}: {stdout}");
    }
}

#[test]
fn logical_blocks_survive_compile_file() {
    let source = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/pprint-logical-block.lisp"
    );
    let compiled = std::env::temp_dir().join(format!("egcl-pprint-{}.bfasl", std::process::id()));
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args([
        "--no-init",
        "--eval",
        &format!(
            "(load (compile-file {source:?} :output-file {:?}))",
            compiled.to_str().unwrap()
        ),
    ]));
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args([
        "--no-init",
        "--load",
        compiled.to_str().unwrap(),
    ]));
    std::fs::remove_file(compiled).unwrap();
}
