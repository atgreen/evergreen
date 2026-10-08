// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check(tier: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", tier)
        .args(["--no-init", "--load"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/lexical-handler-bind.lisp"
        ))
        .output()
        .expect("run lexical handler bindings");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("LEXICAL-HANDLERS-PASS"),
        "{tier}: {stdout}\n{stderr}"
    );
}

#[test]
fn interpreted_lexical_handlers() {
    check("interp");
}

#[test]
fn bytecode_lexical_handlers() {
    check("t0");
}

#[test]
fn baseline_native_lexical_handlers() {
    check("t1");
}

#[test]
fn optimizing_native_lexical_handlers() {
    check("t2");
}
