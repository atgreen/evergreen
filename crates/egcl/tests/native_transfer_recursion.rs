// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_disassembly)))]

use std::process::Command;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn fibonacci_segment_uses_native_self_calls() {
    assert!(egcl_rt::native_transfer::is_supported());
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
            (defun segment-fib (n)
              (if (< n 2) n
                  (+ (segment-fib (- n 1)) (segment-fib (- n 2)))))
            (dotimes (i 30) (assert (= 55 (segment-fib 10))))
            (assert (= 6765 (segment-fib 20)))
            (disassemble 'segment-fib)
        "#,
        ])
        .env("EGCL_NATIVE_TRANSFER", "1")
        .env("EGCL_T0_T1_THRESHOLD", "1")
        .env("EGCL_T2_THRESHOLD", "3")
        .output()
        .expect("run native Fibonacci");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let segment = stdout
        .split("Native segment ABI")
        .nth(1)
        .expect("Fibonacci must have a native segment entry")
        .split("Legacy checked ABI")
        .next()
        .unwrap();
    // Relative CALL instructions decode with an immediate address. Compatibility
    // veneers are called through registers; they cannot satisfy this assertion.
    let direct_calls = segment
        .lines()
        .filter(|line| {
            let Some((_, operand)) = line.split_once("call ") else {
                return false;
            };
            let operand = operand.split_whitespace().next().unwrap_or("");
            operand.strip_suffix('h').is_some_and(|address| {
                !address.is_empty() && address.chars().all(|c| c.is_ascii_hexdigit())
            })
        })
        .count();
    assert_eq!(
        direct_calls, 2,
        "both recursive edges must stay native:\n{segment}"
    );
}
