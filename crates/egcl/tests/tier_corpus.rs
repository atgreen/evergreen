// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The tier-differential half of R4.68 runs in ordinary Cargo/CI tests, beside
//! the forced-guard-failure and frame reconstruction tests in tier_observability.
//! Use the same checked-in corpus as scripts/tier-diff.sh, including its output
//! checksum and completion marker; matching truncated output must never pass.

use std::process::Command;

fn run_corpus(tier: &str, transitions: bool) -> (String, String) {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/differential/tier-corpus.lisp");
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    let probe = if transitions && tier == "t2" {
        "(assert (> (egcl-ext:function-osr-count 'fib-iter) 0))"
    } else {
        ""
    };
    let program = format!("(load {corpus:?}) {probe} (values)");
    command.args(["--no-init", "--eval", &program]);
    for key in [
        "EGCL_LOG",
        "EGCL_T2_LOG",
        "EGCL_BACKEND",
        "EGCL_DISABLE_T2",
        "EGCL_T2",
        "EGCL_LAZY_COMPILE",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_THRESHOLD",
        "EGCL_T2_THRESHOLD",
        "EGCL_T1_T2_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_LOOP_HEAT_THRESHOLD",
        "EGCL_OSR_THRESHOLD",
        "EGCL_ANON_OSR_THRESHOLD",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_PROFILING_DISABLED",
    ] {
        command.env_remove(key);
    }
    command.env("EGCL_FORCE_TIER", tier);
    if transitions && tier != "interp" {
        command
            .env("EGCL_T1_T2_BACKEDGE_THRESHOLD", "5")
            .env("EGCL_OSR_THRESHOLD", "10")
            .env("EGCL_ANON_OSR_THRESHOLD", "10");
    }
    let output = command.output().expect("run tier corpus");
    assert!(
        output.status.success(),
        "tier={tier}, transitions={transitions}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Match the shell harness: only semicolon progress/banner lines are noise.
    let observable = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec())
            .expect("UTF-8 corpus output")
            .lines()
            .filter(|line| !line.starts_with("; "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let stdout = observable(&output.stdout);
    let stderr = observable(&output.stderr);
    assert_eq!(
        stdout.lines().filter(|line| *line == "CORPUS-DONE").count(),
        1,
        "tier={tier}: missing or repeated completion marker: {stdout}\n{stderr}"
    );
    let checksums: Vec<_> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(":CHECKSUM => "))
        .collect();
    assert_eq!(
        checksums.len(),
        1,
        "tier={tier}: missing or repeated checksum: {stdout}"
    );
    checksums[0]
        .parse::<u32>()
        .expect("corpus checksum must be numeric");
    (stdout, stderr)
}

fn compare_tiers(transitions: bool) {
    let oracle = run_corpus("interp", transitions);
    for tier in ["t0", "t1", "t2"] {
        assert_eq!(
            run_corpus(tier, transitions),
            oracle,
            "tier={tier}, transitions={transitions}: observable output differs from interpreter"
        );
    }
}

#[test]
fn checked_in_corpus_agrees_across_all_tiers() {
    compare_tiers(false);
}

// This observed transition is an x86 OSR gate, like the live-handoff tests
// in tier_observability; the ordinary corpus comparison runs on every target.
#[cfg(target_arch = "x86_64")]
#[test]
fn checked_in_corpus_agrees_with_forced_tier_transitions() {
    compare_tiers(true);
}
