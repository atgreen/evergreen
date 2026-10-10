// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn cold_recursive_source_counts_each_logical_call_once() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"(defun cold-count-fib (n)
                  (if (< n 2) n
                      (+ (cold-count-fib (- n 1)) (cold-count-fib (- n 2)))))
               (format t "BEFORE ~D~%"
                 (egcl-ext:function-invoke-count 'cold-count-fib))
               (assert (= (cold-count-fib 20) 6765))
               (format t "AFTER ~D~%"
                 (egcl-ext:function-invoke-count 'cold-count-fib))
               (assert (= (cold-count-fib 0) 0))
               (format t "FOLLOWUP ~D~%"
                 (egcl-ext:function-invoke-count 'cold-count-fib))"#,
        ])
        .env("EGCL_LAZY_COMPILE", "1")
        .env("EGCL_LAZY_THRESHOLD", "8")
        .env("EGCL_PROFILING_DISABLED", "0")
        .env_remove("EGCL_FORCE_TIER")
        .output()
        .expect("run cold recursive source");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "cold recursive source failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let counts: Vec<_> = stdout
        .lines()
        .filter(|line| {
            line.starts_with("BEFORE ")
                || line.starts_with("AFTER ")
                || line.starts_with("FOLLOWUP ")
        })
        .collect();
    assert_eq!(counts, ["BEFORE 0", "AFTER 21891", "FOLLOWUP 21892"]);
}

#[test]
fn bytecode_arity_fallback_counts_one_failed_invocation() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"(defun count-arity-leaf (x) x)
               (defun count-arity-caller ()
                 (handler-case (count-arity-leaf) (error () :caught)))
               (format t "RESULT ~S~%" (count-arity-caller))
               (format t "CALLS ~D~%"
                 (egcl-ext:function-invoke-count 'count-arity-leaf))"#,
        ])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "1000000")
        .env("EGCL_PROFILING_DISABLED", "0")
        .env_remove("EGCL_FORCE_TIER")
        .output()
        .expect("run bytecode arity fallback");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "arity fallback failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.lines().any(|line| line == "RESULT :CAUGHT"),
        "{stdout}"
    );
    assert!(stdout.lines().any(|line| line == "CALLS 1"), "{stdout}");
}
