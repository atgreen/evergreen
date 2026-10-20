// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Fixnum arithmetic against a constant wider than an imm32 must reach T2 on
//! x86-64 and give the interpreter's answer there (bliss-mghob). The emitter
//! used to refuse `(* a 1000000000000)`, `(ash a 40)` and `(< a 1000000000000)`,
//! leaving the whole function at T1, and it compiled `(ash (logand a
//! #xFFFFFFFFFF) 30)` to a plain `shl` that wrapped instead of making a bignum.
#![cfg(all(unix, target_arch = "x86_64"))]
use std::process::Command;

/// Run `program` with promotion thresholds low enough that each warmed function
/// is T2 before the final, checked calls.
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

const FUNCTIONS: [&str; 6] = [
    "S-BIGNUM",
    "S-MULLEFT",
    "S-ASH40",
    "S-CMPBIG",
    "S-CMPVAL",
    "S-SH",
];

#[test]
fn wide_constant_arithmetic_installs_at_t2_with_interpreter_results() {
    let out = run(r#"
      (defun s-bignum (a) (* a 1000000000000))
      (defun s-mulleft (a) (* 1000000000000 a))
      (defun s-ash40 (a) (ash a 40))
      (defun s-cmpbig (a) (if (< a 1000000000000) :small :big))
      (defun s-cmpval (a) (> 1000000000000 a))
      (defun s-sh (a) (ash (logand a #xFFFFFFFFFF) 30))
      (defun warm ()
        (dotimes (i 300)
          (s-bignum i) (s-mulleft i) (s-ash40 i) (s-cmpbig i) (s-cmpval i) (s-sh i)))
      (warm) (sleep 1) (warm)
      (print (list (s-bignum 7) (s-bignum (expt 2 50)) (s-mulleft -3)
                   (s-ash40 3) (s-ash40 (expt 2 25))
                   (s-cmpbig 3) (s-cmpbig 2000000000000)
                   (s-cmpval 3) (s-cmpval 2000000000000)
                   (s-sh 3) (s-sh #xFFFFFFFFFF)))"#);
    for name in FUNCTIONS {
        assert!(
            out.contains(&format!("{name}: optimized worker result installed")),
            "{name} must reach T2\n{out}"
        );
        assert!(
            !out.contains(&format!("{name}: mapped T2 emission declined")),
            "{name} must not be declined by the emitter\n{out}"
        );
    }
    assert!(
        out.contains(
            "(7000000000000 1125899906842624000000000000 -3000000000000 3298534883328 \
             36893488147419103232 :SMALL :BIG T NIL 3221225472 1180591620716337561600)"
        ),
        "{out}"
    );
}
