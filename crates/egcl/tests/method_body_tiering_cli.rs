// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A compiled method body tiers up by loop heat, as a DEFUN body does.
//!
//! A method's compiled callable was never installed in its uninterned
//! symbol's function cell, where the tiering bookkeeping looks for it, so loop
//! heat never promoted it past T1. A method whose body loops stayed there until
//! its 4096th call, roughly 80x slower than the same body in a DEFUN.

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn a_hot_loop_in_a_method_body_reaches_t2() {
    // 200 calls is far below the 4096-call invocation threshold, so only loop
    // heat (1000 back-edges per call) can promote the body.
    let source = r#"
        (progn
          (defclass heat-box () ((n :initarg :n :reader heat-n)))
          (defgeneric heat-sum (box))
          (defmethod heat-sum ((box heat-box))
            (let ((s 0))
              (dotimes (i (heat-n box)) (setf s (logand (+ s i) #xffff)))
              s))
          (let ((box (make-instance 'heat-box :n 1000)))
            (dotimes (k 200) (assert (= (heat-sum box) 40748))))
          (format t "METHOD-HEAT-OK~%"))
    "#;
    let output = Command::new("timeout")
        .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", source])
        .env("EGCL_T2_LOG", "1")
        .output()
        .expect("run method tiering regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("METHOD-HEAT-OK"), "{stdout}");
    // The callable is named after its generic function: (METHOD ...HEAT-SUM).
    let method_line = |line: &&str| line.contains("[T2] (METHOD ") && line.contains("HEAT-SUM");
    assert!(
        stderr
            .lines()
            .filter(method_line)
            .any(|line| line.contains("optimized worker result installed")),
        "the method body must reach T2 by loop heat: {}",
        stderr
            .lines()
            .filter(method_line)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// A method's callable lives in an uninterned symbol named "(METHOD ...)",
/// which does not read back as a symbol. The T1 compiler looked its identity
/// up by that name, so every method whose body needed the optimizing frontend
/// was declined -- logged only as "native emission declined" -- and, with no
/// installed T1, never queued for T2 (bliss-6tf0e). It now uses the installed
/// symbol, so invocation heat alone takes a method to T2.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn a_method_hot_by_invocation_reaches_t2_with_unchanged_results() {
    let source = r#"
        (progn
          (defclass inv-box () ((n :initarg :n :reader inv-n)))
          (defvar *inv-log* 0)
          (defgeneric inv-sum (box x))
          (defmethod inv-sum ((box inv-box) x) (+ (inv-n box) x))
          (defmethod inv-sum :after ((box inv-box) x) (setf *inv-log* (logxor *inv-log* x)))
          (let ((box (make-instance 'inv-box :n 3)) (s 0))
            (dotimes (i 500) (incf s (inv-sum box i)))
            (format t "SUM=~D LOG=~D~%" s *inv-log*)))
    "#;
    let run = |threshold: &str| {
        Command::new("timeout")
            .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
            .args(["--no-init", "--eval", source])
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_T0_T1_THRESHOLD", threshold)
            .env("EGCL_T1_T2_THRESHOLD", "20")
            .output()
            .expect("run method invocation tiering regression")
    };
    let tiered = run("5");
    let interpreted = run("1000000000");
    let stdout = String::from_utf8_lossy(&tiered.stdout);
    let stderr = String::from_utf8_lossy(&tiered.stderr);
    assert!(tiered.status.success(), "{stdout}\n{stderr}");
    assert!(
        interpreted.status.success(),
        "{}",
        String::from_utf8_lossy(&interpreted.stderr)
    );
    assert!(stdout.contains("SUM=126250 LOG="), "{stdout}");
    assert_eq!(
        stdout,
        String::from_utf8_lossy(&interpreted.stdout),
        "tiered and interpreted results differ"
    );
    let method_lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.contains("[T2] (METHOD ") && line.contains("INV-SUM"))
        .collect();
    assert!(
        !method_lines.iter().any(|line| line.contains("T1 declined")),
        "a plain method must not be declined at T1:\n{}",
        method_lines.join("\n")
    );
    for qualifier in ["INV-SUM)", "INV-SUM :AFTER)"] {
        assert!(
            method_lines.iter().any(|line| line.contains(qualifier)
                && line.contains("optimized worker result installed")),
            "{qualifier} must reach T2 by invocation heat:\n{}",
            method_lines.join("\n")
        );
    }
}
