// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A failing lint gate must not suppress later checks or turn the job green.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

#[test]
fn lint_driver_runs_all_checks_and_propagates_each_failure() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = std::env::temp_dir().join(format!("egcl-lint-gates-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let trace = directory.join("trace");
    for name in ["python3", "make", "bash", "cargo"] {
        let stub = directory.join(name);
        fs::write(
            &stub,
            r#"#!/bin/sh
name=${0##*/}
printf '%s %s\n' "$name" "$*" >> "$GATE_TRACE"
if [ "$name" = "$FAIL_GATE" ] || [ "$FAIL_GATE" = all ]; then
    echo "injected failure: $name" >&2
    exit 7
fi
"#,
        )
        .unwrap();
        fs::set_permissions(stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths(std::iter::once(directory.clone()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    for failing in ["none", "python3", "make", "bash", "cargo", "all"] {
        fs::write(&trace, "").unwrap();
        let output = Command::new("/bin/bash")
            .arg(root.join("scripts/ci-lint.sh"))
            .current_dir(&directory)
            .env("PATH", &path)
            .env("GATE_TRACE", &trace)
            .env("FAIL_GATE", failing)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.success(),
            failing == "none",
            "case={failing}\n{stdout}\n{stderr}"
        );
        assert_eq!(fs::read_to_string(&trace).unwrap(), concat!(
            "python3 scripts/spec-coverage.py --gate\n",
            "make clippy\n",
            "bash scripts/gc-root-lint.sh\n",
            "cargo test -p egcl --test spec_validation_infra ansi_expected_failures_and_ci_contracts\n"
        ), "case={failing}: every check must execute");
        if failing != "none" {
            assert!(stderr.contains("injected failure:"), "{stderr}");
            assert!(stdout.contains("FAIL"), "{stdout}");
        }
    }
    fs::remove_dir_all(directory).unwrap();
}
