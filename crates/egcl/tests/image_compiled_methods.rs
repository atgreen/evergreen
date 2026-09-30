// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Saved ASDF methods must keep their compiled bodies, not re-expand source.
use std::process::Command;

fn run(args: &[&str], marker: &str) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "240",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
        ])
        .args(args)
        .output()
        .expect("run bounded image test");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}

#[test]
fn restored_methods_keep_compilation_semantics_and_allow_redefinition() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("egcl-methods-{}-{nonce}.bimg", std::process::id()));
    let filename = path.to_str().unwrap();
    let save = format!(
        r#"
      (defmacro image-method-add (x) (list '+ x 1))
      (defmethod image-method ((x t)) (image-method-add x))
      (defmethod image-method :around ((x t))
        (values (call-next-method) :second))
      (defmethod image-callback ((x t))
        (funcall (lambda () (image-method-add x))))
      (defmethod image-callback :around ((x t))
        (funcall (lambda () (values (call-next-method) :callback))))
      (defmacro image-method-add (x)
        (declare (ignore x)) (error "Restored method re-expanded source"))
      (assert (equal '(42 :second) (multiple-value-list (image-method 41))))
      (assert (equal '(42 :callback) (multiple-value-list (image-callback 41))))
      (format t "METHOD-SAVE-OK~%")
      (save-lisp-and-die "{filename}")
    "#
    );
    run(&["--eval", &save], "METHOD-SAVE-OK");
    run(
        &[
            "--image",
            filename,
            "--eval",
            r#"
      (assert (equal '(42 :second) (multiple-value-list (image-method 41))))
      (assert (equal '(42 :callback) (multiple-value-list (image-callback 41))))
      (defmethod image-method ((x t)) (+ x 2))
      (assert (equal '(43 :second) (multiple-value-list (image-method 41))))
      (format t "METHOD-RESTORE-OK~%")
    "#,
        ],
        "METHOD-RESTORE-OK",
    );
    std::fs::remove_file(path).unwrap();
}
