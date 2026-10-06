// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn bit_vector_sequences_preserve_zero_bits_under_gc_stress() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_GC_STRESS", "1")
        .env("EGCL_GC_POISON", "1")
        .args([
            "--no-init",
            "--eval",
            r#"
              (assert (equalp (copy-seq #*101100) #*101100))
              (assert (equalp (subseq #*101100 1 5) #*0110))
              (assert (equalp (reverse #*101100) #*001101))
              (assert (equalp (copy-seq #*000000000) #*000000000))
              (assert (equalp (copy-seq #*) #*))
              (let ((x (copy-seq #*101100)))
                (replace x x :start1 1 :end1 5 :end2 4)
                (assert (equalp x #*110110)))
              (format t "BIT-VECTOR-INITIALIZATION-OK~%")
            "#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("BIT-VECTOR-INITIALIZATION-OK"));
}
