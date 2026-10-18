// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A compiled method body tiers up by loop heat, as a DEFUN body does.
//!
//! A method's compiled callable was never installed in its uninterned
//! symbol's function cell, where the tiering bookkeeping looks for it, so loop
//! heat never promoted it past T1. A method whose body loops stayed there until
//! its 4096th call, roughly 80x slower than the same body in a DEFUN.

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
