// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn decoded_time_arguments_are_checked_at_each_execution_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_FORCE_TIER", tier)
            .args(["--no-init", "--load"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/encode-universal-time.lisp"
            ))
            .output()
            .expect("run decoded-time checks");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("ENCODE-TIME-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
