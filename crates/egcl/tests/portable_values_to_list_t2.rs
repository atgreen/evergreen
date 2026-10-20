// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `MULTIPLE-VALUE-LIST` on a target that has no mapped native entry.
//!
//! x86-64 Linux services `AuxData::ValuesToList` through its mapped transfer
//! protocol (`TRANSFER_VALUES_TO_LIST_REQUEST`). A checked-ABI target used to
//! see an `AuxData` its `Call` arm did not know and decline the whole function,
//! so a hot `(multiple-value-list (floor x 3))` stopped one tier below the
//! optimizing one. ppc64le now calls the plain `c2i_values_to_list` adapter
//! from its framed emitter instead.
//!
//! What must hold on EVERY target is the values: the collected list is the
//! form's only value, at every tier. The tier assertion is made only where the
//! adapter is wired, so a target that still declines keeps passing honestly.

use std::process::Command;

const PROGRAM: &str = r#"
  (defun mvl-floor (x) (multiple-value-list (floor x 3)))
  (defun mvl-three (x) (multiple-value-list (values x (1+ x) (+ x 2))))
  (defun mvl-allocating (x) (multiple-value-list (values (list x x) (cons x x))))
  (defun mvl-none () (multiple-value-list (values)))
  (dotimes (i 300) (mvl-floor 7) (mvl-three 1) (mvl-allocating 2) (mvl-none))
  (format t "VALUES=~S~%"
          (list (mvl-floor 7) (mvl-three 1) (mvl-allocating 2) (mvl-none)))
  ;; The list is the ONLY value: a leaked secondary would appear here.
  (format t "SINGLE=~S~%" (multiple-value-list (mvl-floor 7)))
  (format t "TIERS=~S~%"
          (mapcar #'egcl-ext:function-tier
                  '(mvl-floor mvl-three mvl-allocating mvl-none)))
"#;

const VALUES: &str = "VALUES=((2 1) (1 2 3) ((2 2) (2 . 2)) NIL)";

fn run(envs: &[(&str, &str)]) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.env_remove("EGCL_FORCE_TIER");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.args(["--no-init", "--eval", PROGRAM]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

#[test]
fn every_tier_collects_the_same_single_value() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = run(&[("EGCL_FORCE_TIER", tier)]);
        assert!(out.contains(VALUES), "{tier}: {out}");
        assert!(out.contains("SINGLE=((2 1))"), "{tier}: {out}");
    }
}

/// Reaching T2 is the point of the portable adapter, so assert it where the
/// adapter exists. Without it each of these declined with `UnsupportedOp(0x9C)`
/// and stayed at T1.
#[test]
#[cfg(all(target_arch = "powerpc64", target_endian = "little"))]
fn the_portable_adapter_lets_a_hot_collector_reach_t2() {
    let out = run(&[
        ("EGCL_T0_T1_THRESHOLD", "5"),
        ("EGCL_T2_THRESHOLD", "20"),
        ("EGCL_T2_THREADS", "1"),
    ]);
    assert!(out.contains(VALUES), "{out}");
    assert!(out.contains("TIERS=(2 2 2 2)"), "{out}");
}
