// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/slot-unbound.lisp"
);

#[test]
fn slot_readers_dispatch_slot_unbound() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_FORCE_TIER", tier)
            .args(["--no-init", "--load"])
            .arg(FIXTURE)
            .output()
            .expect("run SLOT-UNBOUND checks");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("SLOT-UNBOUND-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}

#[test]
fn slot_unbound_methods_survive_compile_file_and_fresh_load() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-slot-unbound-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let artifact = directory.join("slot-unbound.bfasl");
    let form = format!(
        "(multiple-value-bind (file warnings failure) (compile-file {FIXTURE:?} :output-file {:?}) (declare (ignore warnings)) (assert file) (assert (not failure)))",
        artifact.to_str().unwrap()
    );
    let compiled = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &form])
        .output()
        .expect("compile slot protocol fixture");
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let loaded = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load"])
        .arg(&artifact)
        .output()
        .expect("load slot protocol fixture in a fresh process");
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    let stderr = String::from_utf8_lossy(&loaded.stderr);
    assert!(loaded.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("SLOT-UNBOUND-PASS"), "{stdout}\n{stderr}");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn interpreted_slot_unbound_callbacks_obey_the_stack_limit() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "interp")
        .env("EGCL_MAX_CALL_DEPTH", "20")
        .args(["--no-init", "--load"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/slot-unbound-recursion.lisp"
        ))
        .output()
        .expect("run bounded SLOT-UNBOUND recursion");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("SLOT-UNBOUND-RECURSION-PASS"),
        "{stdout}\n{stderr}"
    );
}
