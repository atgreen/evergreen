// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Unrelated definitions must not permanently disable native builtin calls.
use std::process::Command;

fn run(forms: &[&str]) -> (String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .arg("--no-init")
        .env("EGCL_T1_THRESHOLD", "1")
        .env("EGCL_DISABLE_T2", "1")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_DIRECT_BUILTIN_STATS", "1");
    for form in forms {
        command.args(["--eval", form]);
    }
    let output = command.output().expect("run native dispatch probe");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

#[test]
fn unchanged_builtin_revalidates_after_unrelated_definition() {
    let (stdout, stderr) = run(&[
        "(defun preserved-length (x) (length x))",
        "(assert (= 3 (preserved-length '(1 2 3))))",
        "(assert (>= (egcl-ext:function-tier 'preserved-length) 1))",
        "(defun unrelated-epoch-change () 42)",
        "(dotimes (i 20) (assert (= 3 (preserved-length '(1 2 3)))))",
        "(format t \"REVALIDATED~%\")",
    ]);
    assert!(stdout.contains("REVALIDATED"), "{stdout}");
    let stats = stderr
        .lines()
        .find(|line| line.starts_with("[direct-builtin] native:"))
        .expect("native dispatch counters");
    let direct: u64 = stats
        .split("direct=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        direct >= 20,
        "probe must exercise native builtin dispatch: {stats}"
    );
    assert!(
        stats.contains("fallback=0 |"),
        "unchanged builtin stayed on the slow path: {stats}"
    );
}

#[test]
fn revalidated_builtin_still_observes_function_cell_replacement() {
    let (stdout, _) = run(&[
        "(defun replaced-length (x) (length x))",
        "(assert (= 3 (replaced-length '(1 2 3))))",
        "(defun unrelated-epoch-change () 42)",
        "(assert (= 3 (replaced-length '(1 2 3))))",
        "(setf (symbol-function 'length) (lambda (x) (declare (ignore x)) (values 91 92)))",
        "(assert (equal '(91 92) (multiple-value-list (replaced-length '(1 2 3)))))",
        "(format t \"REPLACEMENT-SEEN~%\")",
    ]);
    assert!(stdout.contains("REPLACEMENT-SEEN"), "{stdout}");
}
