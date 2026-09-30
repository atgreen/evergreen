// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Regression for bliss-jql: evaluated primitive calls must not allocate
//! temporary quoted forms on every recursive activation.

use egcl::cli;

#[test]
fn shallow_recursive_list_walk_has_bounded_gc_allocation() {
    // This integration test is its own process, so setting the tier threshold
    // before the backend's OnceLock is read cannot race another test.
    unsafe { std::env::set_var("EGCL_T1_THRESHOLD", "999999999") };

    let program = concat!(
        "(progn ",
        "(defun listsum (xs) ",
        "  (if (null xs) 0 (+ (car xs) (listsum (cdr xs))))) ",
        "(setq *recursive-call-test-list* '(1 2 3 4 5)) ",
        "(dotimes (i 20000) (listsum *recursive-call-test-list*)))",
    );
    let before = egcl_rt::heap_stats().bytes_allocated;
    let status = cli::run(&[
        "--no-bootstrap".into(),
        "--no-init".into(),
        "--eval".into(),
        program.into(),
    ])
    .expect("recursive list walk should complete");
    let allocated = egcl_rt::heap_stats().bytes_allocated - before;

    assert_eq!(status, 0);
    assert!(
        allocated < 512 * 1024,
        "20,000 shallow recursive list walks allocated {allocated} bytes; primitive dispatch should not grow with total calls"
    );
}
