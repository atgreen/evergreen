// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::path::Path;

fn check_ci_revision_policy(workflow: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/workflows")
        .join(workflow);
    let source = fs::read_to_string(path).unwrap();
    let policy = source
        .split_once("\nconcurrency:\n")
        .unwrap_or_else(|| panic!("{workflow}: missing workflow-level concurrency"))
        .1
        .split("\n\n")
        .next()
        .unwrap();
    // PR revisions and pushes to one ref supersede stale automatic runs.
    // Explicit validations use unique IDs, including reusable scheduled calls.
    assert_eq!(
        policy,
        concat!(
            "  group: ci-tiered-v1-${{ !inputs.preserve_run && github.event_name == 'pull_request' && format('pr-{0}', github.event.pull_request.number) || !inputs.preserve_run && github.event_name == 'push' && format('push-{0}', github.ref) || format('run-{0}-{1}', github.run_id, github.run_attempt) }}\n",
            "  cancel-in-progress: ${{ !inputs.preserve_run && (github.event_name == 'pull_request' || github.event_name == 'push') }}"
        ),
        "{workflow}: only automatic revisions of the same PR or push ref may supersede each other"
    );
    assert_eq!(
        source.matches("concurrency:").count(),
        1,
        "{workflow}: a job-level group could override the workflow's isolation"
    );
}

#[test]
fn ci_cancels_stale_pr_and_push_runs_but_preserves_explicit_validation() {
    check_ci_revision_policy("ci.yml");
}

#[test]
fn intensive_matrices_run_only_on_schedule_or_explicit_request() {
    for workflow in ["gc.yml", "cross.yml"] {
        let source = workflow_source(workflow);
        let triggers = source
            .split_once("\non:\n")
            .unwrap()
            .1
            .split_once("\npermissions:\n")
            .unwrap()
            .0;
        for event in ["schedule", "workflow_dispatch", "workflow_call"] {
            assert!(
                triggers.contains(&format!("  {event}:\n")),
                "{workflow}: missing {event}"
            );
        }
        for event in ["push", "pull_request", "pull_request_target"] {
            assert!(
                !triggers.contains(&format!("  {event}:")),
                "{workflow}: expensive automatic {event}"
            );
        }
        let callable = triggers.split_once("  workflow_call:\n").unwrap().1;
        assert!(callable.contains("      preserve_run:\n"));
        assert!(callable.contains("        type: boolean\n        default: true"));
    }
}

fn workflow_source(workflow: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/workflows")
            .join(workflow),
    )
    .unwrap()
}

#[test]
fn intensive_matrices_cancel_only_automatic_same_ref_nightlies() {
    for (workflow, prefix) in [
        ("gc.yml", "gc-intensive-v1"),
        ("cross.yml", "cross-intensive-v1"),
    ] {
        let source = workflow_source(workflow);
        let policy = source
            .split_once("\nconcurrency:\n")
            .unwrap()
            .1
            .split("\n\n")
            .next()
            .unwrap();
        // The explicit-call default protects a scheduled release caller too:
        // reusable workflows inherit the caller's github.event_name.
        let expected = concat!(
            "  group: PREFIX-${{ github.workflow }}-${{ github.event_name == 'schedule' && !inputs.preserve_run && format('scheduled-{0}', github.ref) || format('run-{0}-{1}', github.run_id, github.run_attempt) }}\n",
            "  cancel-in-progress: ${{ github.event_name == 'schedule' && !inputs.preserve_run }}"
        ).replace("PREFIX", prefix);
        assert_eq!(
            policy, expected,
            "{workflow}: explicit runs must not evict each other"
        );
        assert_eq!(source.matches("concurrency:").count(), 1);
    }
}

#[test]
fn gc_full_and_subset_are_mutually_exclusive_and_default_to_full() {
    let source = workflow_source("gc.yml");
    let subset = source
        .split_once("  stress-subset:\n")
        .unwrap()
        .1
        .split_once("  stress-full:\n")
        .unwrap()
        .0;
    let full = source.split_once("  stress-full:\n").unwrap().1;
    assert!(subset.starts_with("    if: inputs.scope == 'subset'\n"));
    assert!(full.starts_with("    if: inputs.scope != 'subset'\n"));
    assert_eq!(
        source.matches("        default: full\n").count(),
        2,
        "manual and reusable calls must both default to the full suite"
    );
    assert!(
        subset.contains("exit $status"),
        "a subset failure must fail the job"
    );
    assert!(full.contains("cargo test --release --workspace --no-fail-fast"));
    assert!(!source.contains("continue-on-error:"));
    assert!(!source.contains("|| true"));
}
