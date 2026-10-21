// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A function that came with a restored image reports the tier its installed
//! native code runs at (GitHub #466).
//!
//! The image's bytecode registry used to carry the body owned by each named
//! function object twice: once as the named definition and once under the
//! object's private definition symbol. Restoring decoded the two as distinct
//! copies, so the code compiled and published for the named body never matched
//! the body recorded for the object. The native code ran, but
//! `function-tier` answered 0 and `function-invoke-count` kept counting.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::path::Path;
use std::process::Command;

const DEFS: &str = "\
(defun tier-probe-fn (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (mod i 7))))))
(defun tier-caller (k) (let ((acc 0)) (dotimes (j k acc) (setq acc (+ acc (tier-probe-fn 50))))))
";

fn egcl(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut command = Command::new("timeout");
    command
        .current_dir(dir)
        .args([
            "--kill-after=5",
            "300",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
        ])
        .args(args);
    for name in [
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_T2_THRESHOLD",
        "EGCL_OSR_THRESHOLD",
        "EGCL_T2",
        "EGCL_DISABLE_T2",
        "EGCL_PROFILING_DISABLED",
        "EGCL_FORCE_TIER",
        "EGCL_T2_DISCARD",
    ] {
        command.env_remove(name);
    }
    command.envs(envs.iter().copied());
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "egcl {args:?} failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// Compile the definitions to a fasl, warm them briefly, and save an image.
fn save_image(dir: &Path) {
    std::fs::write(dir.join("defs.lisp"), DEFS).unwrap();
    let out = egcl(
        dir,
        &[
            "--eval",
            r#"(progn
                 (compile-file "defs.lisp" :output-file "defs.fasl")
                 (load "defs.fasl")
                 (tier-caller 20)
                 (format t "SAVING~%")
                 (egcl-ext:save-lisp-and-die "t.core"))"#,
        ],
        &[],
    );
    assert!(out.contains("SAVING"), "{out}");
}

/// Warm the restored function until it reaches `tier`, then check that the
/// tier is visible by name and by object.
fn restored_program(tier: u8) -> String {
    format!(
        r#"(progn
             (dotimes (i 400)
               (when (= (egcl-ext:function-tier 'tier-probe-fn) {tier}) (return))
               (tier-caller 20))
             (let ((by-name (egcl-ext:function-tier 'tier-probe-fn))
                   (by-object (egcl-ext:function-tier (fdefinition 'tier-probe-fn)))
                   (before (egcl-ext:function-invoke-count 'tier-probe-fn))
                   (result (tier-caller 2000))
                   (after (egcl-ext:function-invoke-count 'tier-probe-fn)))
               (format t "RESTORED tier=~D object=~D before=~D after=~D result=~D~%"
                       by-name by-object before after result)))"#
    )
}

fn restored_line(out: &str) -> &str {
    out.lines()
        .find(|line| line.starts_with("RESTORED "))
        .unwrap_or_else(|| panic!("no RESTORED line: {out}"))
}

fn assert_restored_tier(line: &str, tier: u8) {
    let fields: std::collections::HashMap<&str, &str> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.split_once('='))
        .collect();
    assert_eq!(fields["tier"], tier.to_string(), "tier by name: {line}");
    assert_eq!(fields["object"], tier.to_string(), "tier by object: {line}");
    // T1 code still counts invocations toward T2; installed T2 code does not.
    if tier == 2 {
        assert_eq!(
            fields["before"], fields["after"],
            "invoke count kept advancing: {line}"
        );
    }
    assert_eq!(fields["result"], "294000", "result: {line}");
}

fn scratch(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "egcl-restored-tier-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn restored_function_reports_installed_tiers() {
    let dir = scratch("tiers");
    save_image(&dir);

    // T2: the reporter's case.
    let out = egcl(
        &dir,
        &["--image", "t.core", "--eval", &restored_program(2)],
        &[],
    );
    assert_restored_tier(restored_line(&out), 2);

    // T1 publishes through the same path.
    let out = egcl(
        &dir,
        &["--image", "t.core", "--eval", &restored_program(1)],
        &[("EGCL_DISABLE_T2", "1")],
    );
    assert_restored_tier(restored_line(&out), 1);

    std::fs::remove_dir_all(&dir).unwrap();
}
