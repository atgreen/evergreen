// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn missing_wildcard_directories_return_nil() {
    let root = std::env::temp_dir().join(format!(
        "egcl-missing-directory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!root.exists());
    let root = root.to_string_lossy().replace('\\', "/");
    let form = format!(
        r#"(progn
             (dolist (suffix '("*.lisp" "*.cl" "*/*.lisp" "**/*.lisp"))
               (assert (null (directory (concatenate 'string "{root}/" suffix)))))
             (write-line "MISSING-DIRECTORY-PASS"))"#
    );
    for stress in ["0", "1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_GC_STRESS", stress)
            .env("EGCL_GC_POISON", "1")
            .args(["--no-init", "--eval", &form])
            .output()
            .expect("run missing-directory checks");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stress={stress}: {stdout}\n{stderr}");
        assert!(stdout.contains("MISSING-DIRECTORY-PASS"), "{stdout}\n{stderr}");
    }
}
