// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_disassembly)))]

use std::process::Command;

#[test]
fn disassemble_labels_installed_mapped_code_with_cache_disabled() {
    check_listing(false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn disassemble_labels_installed_mapped_code_with_cache_enabled() {
    assert!(
        egcl_rt::native_transfer::is_supported(),
        "segment execution gate"
    );
    check_listing(true);
}

fn check_listing(enabled: bool) {
    let program = r#"
        (defun segment-listing (x) (+ x 1))
        (dotimes (i 30) (assert (= 42 (segment-listing 41))))
        (disassemble 'segment-listing)
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_NATIVE_TRANSFER", if enabled { "1" } else { "0" })
        .env("EGCL_T0_T1_THRESHOLD", "1")
        .output()
        .expect("run disassembly probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Mapped native transfer ABI"), "{stdout}");
    assert!(!stdout.contains("Legacy checked ABI"), "{stdout}");
    assert!(
        stdout.contains("+0000:"),
        "installed machine code must be shown: {stdout}"
    );
    assert!(
        stdout.contains("native precise deopt veneer"),
        "guard continuation targets must be identifiable: {stdout}"
    );
    assert!(
        !stdout.contains("native call veneer"),
        "the arithmetic must compile rather than call the generic helper: {stdout}"
    );
}
