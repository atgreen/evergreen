// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn leading_fresh_line_directive_sees_the_destination_column_at_each_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_FORCE_TIER", tier)
            .args(["--no-init", "--load"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/format-fresh-line.lisp"
            ))
            .output()
            .expect("run FORMAT ~& checks");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("FORMAT-FRESH-LINE-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
