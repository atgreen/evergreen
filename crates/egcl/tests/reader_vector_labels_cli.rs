// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn checked(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn circular_reader_labels_preserve_graph_identity() {
    assert!(checked(include_str!("fixtures/reader-vector-labels.lisp"))
        .contains("READER-VECTOR-LABELS-OK"));
}

#[test]
fn read_eval_keeps_disconnected_labeled_graphs_reachable_for_backpatching() {
    checked(
        "(let ((value '(#1=#.(progn '#2=(#1#) 42) #2#)))
               (assert (equal value '(42 (42)))))",
    );
}

#[test]
fn circular_reader_labels_survive_source_free_compiled_files() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("egcl-reader-labels-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("labels.lisp");
    let fasl = dir.join("labels.bfasl");
    std::fs::write(&source, include_str!("fixtures/reader-vector-labels.lisp")).unwrap();
    checked(&format!(
        "(compile-file {:?} :output-file {:?})",
        source.to_str().unwrap(),
        fasl.to_str().unwrap()
    ));
    std::fs::remove_file(source).unwrap();
    assert!(checked(&format!("(load {:?})", fasl.to_str().unwrap()))
        .contains("READER-VECTOR-LABELS-OK"));
    std::fs::remove_file(fasl).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
