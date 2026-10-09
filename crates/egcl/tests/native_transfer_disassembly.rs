// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_disassembly)))]

use std::process::Command;

#[test]
fn disassemble_labels_checked_code() {
    check_listing(false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn disassemble_distinguishes_segment_code_from_checked_code() {
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
    assert!(stdout.contains("Legacy checked ABI"), "{stdout}");
    assert_eq!(stdout.contains("Native segment ABI"), enabled, "{stdout}");
    if enabled {
        let segment = stdout.split("Legacy checked ABI").next().unwrap();
        assert!(
            segment.contains("+0000:"),
            "segment machine code must be shown: {stdout}"
        );
        assert!(
            segment.contains("native call veneer"),
            "helper targets must be identifiable: {stdout}"
        );
    }
}
