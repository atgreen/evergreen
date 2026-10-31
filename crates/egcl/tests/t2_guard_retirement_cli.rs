// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A T2 version retired because a non-numeric guard keeps failing is not
//! recompiled without limit (bliss-gg3i1.7.6). AREF on an adjustable vector
//! fails T2's simple-vector guard; each replacement was compiled from the same
//! profile and failed the same way, so the function was recompiled at T1 and
//! T2 every few dozen calls for the rest of the run. SBCL's cross-compiler hit
//! this in FIND-SOURCE-ROOT, NOTE-CONFLICTS and others thousands of times.

use std::process::Command;

const PROGRAM: &str = r#"
(defvar *adjustable* (make-array 10 :adjustable t :fill-pointer 10 :initial-element 7))
(defun guard-ref (vector index) (aref vector index))
(defun guard-drive (n)
  (let ((sum 0))
    (dotimes (i n sum) (incf sum (guard-ref *adjustable* (mod i 10))))))
(dotimes (round 40) (guard-drive 5000))
(format t "RESULT ~S~%" (guard-drive 10))
"#;

#[test]
fn repeated_guard_failures_stop_recompiling_t2() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {PROGRAM})")])
        .env("EGCL_T2_LOG", "1")
        .env_remove("EGCL_FORCE_TIER")
        .env_remove("EGCL_DISABLE_T2")
        .env_remove("EGCL_GC_STRESS")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("RESULT 70"), "{stdout}");
    let installs = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|line| line.contains("GUARD-REF: optimized worker result installed"))
        .count();
    assert!(
        installs <= 2,
        "GUARD-REF was installed at T2 {installs} times"
    );
}
