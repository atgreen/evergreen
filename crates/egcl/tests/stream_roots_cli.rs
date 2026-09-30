// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Moving GC must update caller copies across stream operations.
use std::process::Command;

#[test]
fn reader_and_sequence_stream_paths_preserve_values_under_gc_stress() {
    let program = r#"
      (let ((s (make-string-output-stream)) (v (format nil "abc")))
        (write-string v s)
        (write-line v s)
        (write-sequence v s)
        (format t "~S~%" (equal (get-output-stream-string s)
                                 (format nil "abcabc~%abc"))))
      (let ((s (make-string-input-stream " abc")) (v (make-string 3)))
        (peek-char t s)
        (read-sequence v s)
        (format t "~S~%" (equal v "abc")))
      (let ((end (list "done")) (s (make-string-input-stream "")))
        (format t "~S~%" (eq end (read-byte s nil end))))
      (let ((s (make-string-input-stream "(a b) x")))
        (format t "~S ~S~%" (read s) (read s)))
      (load (make-string-input-stream "(setq *stream-root-probe* (list 1 2))"))
      (format t "~S~%" (equal *stream-root-probe* '(1 2)))
    "#;
    for backend in ["tree-walker", "bytecode"] {
        for stress in ["0", "1"] {
            let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
                .args(["--no-init", "--no-bootstrap", "--eval", program])
                .env("EGCL_BACKEND", backend)
                .env("EGCL_GC_STRESS", stress)
                .env("EGCL_GC_POISON", "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout).replace('\r', "");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "{backend} stress={stress}: {stdout}\n{stderr}"
            );
            assert_eq!(
                stdout, "T\nT\nT\n(A B) X\nT\nNIL\n",
                "{backend} stress={stress}: {stderr}"
            );
        }
    }
}
