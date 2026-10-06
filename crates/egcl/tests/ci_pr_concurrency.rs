// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::path::Path;

fn check_pr_only_policy(workflow: &str) {
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
    // Pin the expression, not just the presence of a concurrency key. A
    // shared non-PR group can drop pending main runs even when cancellation
    // of an already-running job is disabled. Unique run/attempt IDs avoid it.
    assert_eq!(
        policy,
        concat!(
            "  group: pr-revision-v1-${{ github.workflow }}-${{ github.event_name == 'pull_request' && format('pr-{0}', github.event.pull_request.number) || format('run-{0}-{1}', github.run_id, github.run_attempt) }}\n",
            "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}"
        ),
        "{workflow}: only revisions of the same PR and workflow may supersede each other"
    );
    assert_eq!(
        source.matches("concurrency:").count(),
        1,
        "{workflow}: a job-level group could override the workflow's isolation"
    );
}

#[test]
fn ci_deduplicates_only_future_pr_revisions() {
    check_pr_only_policy("ci.yml");
}

#[test]
fn gc_deduplicates_only_future_pr_revisions() {
    check_pr_only_policy("gc.yml");
}
