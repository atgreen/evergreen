// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn large_vector_coerce_roundtrip_preserves_every_element() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (progn
            (dolist (n '(65532 65533 65537 1000000))
              (let ((a (make-array n)))
                (dotimes (i n) (setf (aref a i) i))
                (let ((b (coerce (coerce a 'list) 'vector)))
                  (assert (= n (length b)))
                  (assert (equalp a b)))))
            (format t "LARGE-VECTOR-COERCE-PASS~%"))
        "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("LARGE-VECTOR-COERCE-PASS"),
        "{stdout}\n{stderr}"
    );
}
