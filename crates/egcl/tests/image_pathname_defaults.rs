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
fn restored_pathname_defaults_use_the_process_directory_before_hooks() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("egcl-image-cwd-{}-{nonce}", std::process::id())),
    );
    let builder = fixture.0.join("builder");
    let user = fixture.0.join("user");
    fs::create_dir_all(&builder).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("local.lisp"), "(defparameter *relative-load* 42)").unwrap();
    fs::write(
        user.join("cwd-system.asd"),
        "(asdf:defsystem \"cwd-system\" :components ((:file \"component\")))",
    )
    .unwrap();
    fs::write(
        user.join("component.lisp"),
        "(defparameter *relative-system* 73)",
    )
    .unwrap();

    for executable in [false, true] {
        let image = fixture
            .0
            .join(if executable { "egcl-app" } else { "egcl.core" });
        let source = format!(
            r#"
            (require :asdf)
            (defvar *hook-directory* nil)
            (defun remember-startup-directory ()
              (setq *hook-directory* *default-pathname-defaults*))
            (push 'remember-startup-directory egcl-ext:*init-hooks*)
            (egcl-ext:save-lisp-and-die {image:?} :executable {})
            "#,
            if executable { "t" } else { "nil" }
        );
        let saved = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .current_dir(&builder)
            .args(["--no-init", "--eval", &source])
            .output()
            .unwrap();
        assert!(
            saved.status.success(),
            "{}",
            String::from_utf8_lossy(&saved.stderr)
        );

        let mut restored = Command::new(if executable {
            image.as_os_str()
        } else {
            std::ffi::OsStr::new(env!("CARGO_BIN_EXE_egcl"))
        });
        if !executable {
            restored.arg("--image").arg(&image);
        }
        let output = restored
            .current_dir(&user)
            // Saving bootstraps ASDF normally; stress the restored process,
            // including the fresh directory pathname and its startup hooks.
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_POISON", "1")
            .args([
                "--no-init", "--eval",
                r#"(assert (equal *default-pathname-defaults* (truename (egcl-ext:getcwd))))
                   (assert (equal *hook-directory* *default-pathname-defaults*))
                   (load "local.lisp")
                   (assert (= *relative-load* 42))
                   (asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
                   (asdf:initialize-output-translations '(:output-translations :disable-cache :ignore-inherited-configuration))
                   (push #p"./" asdf:*central-registry*)
                   (asdf:load-system "cwd-system")
                   (assert (= *relative-system* 73))
                   (format t "RESTORED-CWD-OK~%")"#,
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "executable={executable}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("RESTORED-CWD-OK"), "{stdout}");
    }
}
