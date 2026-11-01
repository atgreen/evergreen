// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! A fresh instance of a closure template whose code has reached T2 runs that
//! code at full speed from the first call of a long-running caller loop
//! (Backpack stage 2, bliss-gg3i1.7.15.3).
//!
//! Instances share their template's code identity and tiering record, so a new
//! instance must not restart tiering or take a slower route than the instances
//! made before it: the first fresh instance's loop must run about as fast as
//! the next one's, and the template must be at T2.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

const PROGRAM: &str = r#"
(defun fresh-callex (f n) (let ((s 0)) (dotimes (i n) (setq s (funcall f s))) s))
(defun fresh-mk (n) (lambda (x) (+ x n)))
(dotimes (i 20) (fresh-callex (fresh-mk 1) 1000))
;; Let background compilation settle: each short call publishes what has
;; finished and requests the next tier.
(dotimes (i 4) (sleep 0.5) (fresh-callex (fresh-mk 1) 10))
(egcl-ext:gc :full t)
(defvar *fresh-first*
  (let ((start (get-internal-real-time)))
    (assert (= (fresh-callex (fresh-mk 1) 400000) 400000))
    (- (get-internal-real-time) start)))
(defvar *fresh-second*
  (let ((start (get-internal-real-time)))
    (assert (= (fresh-callex (fresh-mk 1) 400000) 400000))
    (- (get-internal-real-time) start)))
(format t "FRESH-TIERS ~D ~D ~D ~D~%" *fresh-first* *fresh-second*
        (egcl-ext:function-tier (fresh-mk 1))
        (egcl-ext:function-tier 'fresh-callex))
"#;

#[test]
fn fresh_instance_runs_its_templates_optimized_code_in_a_long_caller_loop() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_LAZY_COMPILE",
        "EGCL_PROFILING_DISABLED",
        "EGCL_GC_STRESS",
        "EGCL_T2_LOG",
    ] {
        command.env_remove(key);
    }
    let output = command
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix("FRESH-TIERS "))
        .unwrap_or_else(|| panic!("no result line\n{stdout}\n{stderr}"));
    let fields: Vec<u64> = line
        .split_whitespace()
        .map(|field| field.parse().unwrap())
        .collect();
    let (first, second, template_tier, caller_tier) = (fields[0], fields[1], fields[2], fields[3]);
    assert_eq!(template_tier, 2, "the template must be optimized: {stdout}");
    assert_eq!(caller_tier, 2, "the caller must be optimized: {stdout}");
    // A fresh instance takes its template's optimized route at once. Allow
    // scheduling noise, far below the ~6x of a loop stuck in a lower tier.
    assert!(
        first <= 3 * second.max(20),
        "first fresh instance ran {first} ms against {second} ms for the next one\n{stdout}"
    );
}
