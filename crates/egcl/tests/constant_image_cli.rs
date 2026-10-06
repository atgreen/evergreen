// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::{Command, Output};

fn require_success(output: Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

#[test]
fn constants_remain_constants_in_a_restored_image() {
    let image = std::env::temp_dir().join(format!("egcl-constant-{}.core", std::process::id()));
    let program = format!(
        r#"
          (defconstant +saved-constant+ 37)
          (egcl-ext:save-lisp-and-die {image:?})
        "#
    );
    require_success(
        Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap(),
    );
    let restored = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--image")
        .arg(&image)
        .args([
            "--no-init",
            "--eval",
            r#"
              (assert (constantp '+saved-constant+))
              (defconstant +saved-constant+ 37)
              (assert (eq (handler-case (setq +saved-constant+ 41)
                            (program-error () :caught)) :caught))
              (assert (= (symbol-value '+saved-constant+) 37))
              (format t "CONSTANT-IMAGE-OK~%")
            "#,
        ])
        .output()
        .unwrap();
    std::fs::remove_file(&image).unwrap();
    assert!(
        require_success(restored)
            .lines()
            .any(|line| line == "CONSTANT-IMAGE-OK")
    );
}
