// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn native_threads_share_package_registry_and_symbol_membership() {
    for tier in ["interp", "t0", "t1"] {
        let output = Command::new("timeout")
            .args([
                "--kill-after=5",
                "90",
                env!("CARGO_BIN_EXE_egcl"),
                "--no-init",
                "--eval",
                include_str!("fixtures/thread-package-registry.lisp"),
            ])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "tier={tier} status={:?}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("THREAD-PACKAGE-REGISTRY-PASS"));
    }
}
