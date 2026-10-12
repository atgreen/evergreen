// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn internal_packed_vectors_preserve_values_and_storage_under_gc_stress() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_GC_STRESS", "1")
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .args([
            "--no-init",
            "--eval",
            include_str!("../../../tests/packed-f64/check.lisp"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("[gc-stress] EGCL_GC_STRESS_AFTER_INIT: stressing from allocation"));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Packed double-float vector checks passed.")
    );
}

#[test]
fn public_double_float_array_representations_agree_under_gc_stress() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_GC_STRESS", "1")
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .args([
            "--no-init",
            "--eval",
            include_str!("../../../tests/packed-f64/public-check.lisp"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("[gc-stress] EGCL_GC_STRESS_AFTER_INIT: stressing from allocation"));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Public packed double-float array checks passed.")
    );
}
