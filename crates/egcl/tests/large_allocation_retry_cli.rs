// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn discarded_large_strings_reclaim_default_heap_without_explicit_gc() {
    let program = r#"(progn
      (let ((kept (make-string 100000 :initial-element #\k)))
        (dotimes (i 600)
          (let ((value (make-string 100000 :initial-element #\x)))
            (assert (= (length value) 100000))
            (assert (char= (char value 99999) #\x))))
        (assert (char= (char kept 0) #\k))
        (assert (char= (char kept 99999) #\k)))
      (write-line "LARGE-ALLOCATION-OK"))"#;
    let mut observed = Vec::new();
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        // Exercise the default heap that failed in SBCL's custom-reader path.
        command
            .env_remove("EGCL_HEAP_SIZE")
            .env_remove("EGCL_HEAP_MB")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        } else {
            command
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_POISON");
        }
        let output = command
            .args(["--no-init", "--eval", program])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}: {stdout}\n{stderr}"
        );
        assert!(stdout.contains("LARGE-ALLOCATION-OK"), "{stdout}\n{stderr}");
        observed.push(stdout.into_owned());
    }
    assert_eq!(observed[0], observed[1]);
}
