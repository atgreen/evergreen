// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn saved_applications_receive_arguments_without_runtime_parsing() {
    let dir = std::env::temp_dir().join(format!("egcl-application-args-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let app = dir.join("app");
    let source = format!(
        r#"
        (require :asdf)
        (defun application-main ()
          (format t "APP-ARGS:~S~%" (uiop:command-line-arguments)) t)
        (setf uiop:*image-entry-point* #'application-main)
        (uiop:dump-image {app:?} :executable t)
    "#
    );
    let saved = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &source])
        .output()
        .unwrap();
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    for (args, expected) in [
        (vec![], "APP-ARGS:NIL"),
        (vec!["doctor"], "APP-ARGS:(\"doctor\")"),
        (vec!["--help"], "APP-ARGS:(\"--help\")"),
        (vec!["--version"], "APP-ARGS:(\"--version\")"),
        (
            vec!["--eval", "not Lisp", "--", ""],
            "APP-ARGS:(\"--eval\" \"not Lisp\" \"--\" \"\")",
        ),
    ] {
        let output = Command::new(&app).args(&args).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{args:?}: {stdout}\n{stderr}");
        assert!(stdout.contains(expected), "{args:?}: {stdout}\n{stderr}");
    }
}

#[test]
fn resaving_an_application_replaces_its_payload() {
    let dir = std::env::temp_dir().join(format!("egcl-application-resave-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let first = dir.join("first");
    let second = dir.join("second");
    let source = format!(
        r#"
        (defun final-main () (format t "RESAVED-APPLICATION-OK~%"))
        (defun resave ()
          (egcl-ext:save-lisp-and-die {second:?}
             :executable t :application t :toplevel #'final-main))
        (egcl-ext:save-lisp-and-die {first:?}
           :executable t :application t :toplevel #'resave)
    "#
    );
    for output in [
        Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &source])
            .output()
            .unwrap(),
        Command::new(&first).arg("--help").output().unwrap(),
    ] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let runtime_size = |path: &std::path::Path| {
        let bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[bytes.len() - 16..bytes.len() - 8], b"EGCLAPP\0");
        let payload = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
        bytes.len() - 16 - payload
    };
    assert_eq!(runtime_size(&first), runtime_size(&second));
    let output = Command::new(&second).arg("--help").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("RESAVED-APPLICATION-OK"));
}
