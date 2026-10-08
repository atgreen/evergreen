// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Single-float arithmetic must give the same answer at every tier. The ppc64le
//! T2 emitter once reinterpreted single-precision bits as a double in the
//! register file, so `(+ 1.5 2.25)` returned 2.2578125 once a function reached
//! T2 (bliss-pjq99). This pins the answer through a full T0 → T1 → T2 run on
//! whatever architecture the test executes on.
#![cfg(all(
    target_pointer_width = "64",
    any(
        all(unix, target_arch = "x86_64"),
        all(
            target_os = "linux",
            target_env = "gnu",
            any(
                target_arch = "aarch64",
                all(target_arch = "powerpc64", target_endian = "little"),
                target_arch = "s390x"
            )
        )
    )
))]
use std::process::Command;

/// Run `program` with promotion thresholds low enough that a function called a
/// few thousand times is T2 before the final, checked call.
fn run(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T2_THRESHOLD", "20")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LOG", "stderr")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout + "\n" + &stderr
}

#[test]
fn single_float_add_matches_t0_once_t2_installed() {
    let out = run(r#"
      (defun fa (a b) (+ a b))
      (dotimes (k 3000) (fa 1.5 2.25))
      (print (list (fa 1.5 2.25) (fa 10.0 5.0) (fa -0.125 0.125)))
      (print (eval '(fa 1.5 2.25)))"#);
    assert!(out.contains("(3.75 15.0 0.0)"), "{out}");
    if out.contains("FA: T2 INSTALLED") {
        // Only meaningful where the backend installed T2; everywhere it did, the
        // result above came from native single-float code.
        assert!(!out.contains("2.2578125"), "{out}");
    }
}

#[test]
fn single_float_accumulator_loop_matches_t0_once_t2_installed() {
    let out = run(r#"
      (defun fl (n) (let ((s 0.0)) (dotimes (i n) (setq s (+ s 1.5))) s))
      (dotimes (k 3000) (fl 100))
      (print (list (fl 1) (fl 2) (fl 10)))"#);
    assert!(out.contains("(1.5 3.0 15.0)"), "{out}");
}
