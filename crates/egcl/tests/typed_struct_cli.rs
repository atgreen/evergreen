// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn typed_structures_use_sequences_for_construction_access_and_copying() {
    let program = r#"
      (defstruct (registers (:type (vector (unsigned-byte 32)))
                            (:constructor initial-registers ())
                            (:constructor registers-with (a &optional b))
                            (:copier clone-registers))
        (a 17) (b 23))
      (let* ((r (initial-registers)) (copy (clone-registers r)))
        (assert (typep r '(simple-array (unsigned-byte 32) (2))))
        (assert (= (registers-a r) 17))
        (setf (registers-b r) 99)
        (assert (= (aref r 1) 99))
        (assert (= (registers-b copy) 23))
        (replace copy r)
        (assert (equalp copy r))
        (assert (not (eq copy r))))
      (assert (equalp (registers-with 42) #(42 23)))
      (assert (not (fboundp 'registers-p)))
      (assert (null (find-class 'registers nil)))
      (defstruct (pair (:type list)) (left 1) (right 2))
      (let ((p (make-pair :right 8)))
        (assert (equal p '(1 8)))
        (setf (pair-left p) 9)
        (assert (equal p '(9 8)))
        (assert (equal (copy-pair p) p)))
      (format t "TYPED-STRUCT-OK~%")
    "#;
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("TYPED-STRUCT-OK"));

    let source =
        std::env::temp_dir().join(format!("egcl-typed-struct-{}.lisp", std::process::id()));
    let fasl = source.with_extension("bfasl");
    std::fs::write(&source, program).unwrap();
    let compiled = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            &format!(
                "(compile-file {:?} :output-file {:?})",
                source.to_str().unwrap(),
                fasl.to_str().unwrap()
            ),
        ])
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let loaded = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load", fasl.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(String::from_utf8_lossy(&loaded.stdout).contains("TYPED-STRUCT-OK"));
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(fasl).unwrap();
}
