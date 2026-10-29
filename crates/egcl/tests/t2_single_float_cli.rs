// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Single-float arithmetic and comparisons must give the same answer at every
//! tier. The ppc64le
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
    if out.contains("FA: background T2 result published")
        || out.contains("FA: optimized worker result installed with ")
    {
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

/// The five comparisons, where a backend selecting on the wrong condition is
/// invisible until an operand pair exercises it. Ties separate `<` from `<=`
/// and `>` from `>=`, and `0.0` against `-0.0` must compare equal. The ppc64le
/// emitter builds `<=`/`>=` from LT-or-EQ and GT-or-EQ rather than from the
/// integer "not GT"/"not LT" encodings, which an unordered compare would make
/// true.
#[test]
fn single_float_comparisons_match_t0_once_t2_installed() {
    let out = run(r#"
      (defun fc (a b) (list (< a b) (<= a b) (> a b) (>= a b) (= a b)))
      (dotimes (k 3000) (fc 1.5 2.25))
      (print (fc 1.5 2.25))
      (print (fc 2.25 1.5))
      (print (fc 2.25 2.25))
      (print (fc 0.0 -0.0))
      (print (fc -1.5 1.5))
      (print (eval '(fc 2.25 2.25)))"#);
    for expected in [
        "(T T NIL NIL NIL)", // 1.5 < 2.25, and again across zero
        "(NIL NIL T T NIL)", // 2.25 > 1.5
        "(NIL T NIL T T)",   // a tie, and 0.0 against -0.0: <=, >= and = only
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
}
