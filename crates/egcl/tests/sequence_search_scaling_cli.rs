// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
#[cfg(unix)]
fn search_large_inputs_without_restarting_each_list_walk() {
    check(
        include_str!("fixtures/search-large-input.lisp"),
        "LARGE-SEARCH-PASS",
    );
}

#[test]
#[cfg(unix)]
fn repeated_vector_searches_do_not_copy_the_entire_target() {
    check(
        include_str!("fixtures/search-repeated-vector.lisp"),
        "REPEATED-VECTOR-SEARCH-PASS",
    );
}

#[cfg(unix)]
#[test]
fn search_vector_kinds_preserve_element_and_range_semantics() {
    check(
        include_str!("fixtures/search-vector-kinds.lisp"),
        "VECTOR-SEARCH-KINDS-PASS",
    );
}

#[cfg(unix)]
#[test]
fn search_rejects_non_sequences_even_without_candidates() {
    check(
        include_str!("fixtures/search-invalid-target.lisp"),
        "SEARCH-INVALID-TARGET-PASS",
    );
}

#[cfg(unix)]
fn check(program: &str, marker: &str) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "30",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}
