// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Callable metadata restored from another process must select local dispatchers.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

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
        .env("EGCL_NATIVE_TRANSFER", "1")
        .env("EGCL_T0_T1_THRESHOLD", "1")
        .env("EGCL_T2_THRESHOLD", "3")
        .output()
        .expect("run bounded native callable image test");
    assert!(
        output.status.success(),
        "expected {marker}; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(marker));
    if marker == "CALLABLE-RESTORE-OK" {
        assert!(stdout.contains("Mapped native transfer ABI"), "{stdout}");
    }
}

#[test]
fn restored_callable_dispatch_preserves_multiple_values() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("egcl-callable-{}-{nonce}.bimg", std::process::id()));
    let filename = path.to_str().unwrap();
    let save = format!(
        r#"
      (defun image-callable (x) (values (+ x 1) :old))
      (defvar *image-callable* #'image-callable)
      (defun image-invoke (f x) (funcall f x))
      (dotimes (i 40)
        (assert (equal '(42 :old) (multiple-value-list (image-invoke *image-callable* 41)))))
      (format t "CALLABLE-SAVE-OK~%")
      (save-lisp-and-die "{filename}")
    "#
    );
    run(&["--eval", &save], "CALLABLE-SAVE-OK");
    run(
        &[
            "--image",
            filename,
            "--eval",
            r#"
      (dotimes (i 40)
        (assert (equal '(42 :old) (multiple-value-list (image-invoke *image-callable* 41)))))
      (disassemble #'image-invoke)
      (format t "CALLABLE-RESTORE-OK~%")
    "#,
        ],
        "CALLABLE-RESTORE-OK",
    );
    std::fs::remove_file(path).unwrap();
}
