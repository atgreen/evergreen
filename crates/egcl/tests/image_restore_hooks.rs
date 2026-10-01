// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Process-local UIOP state must be refreshed before any saved program runs.
#![cfg(unix)]

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn ok(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn restored_uiop_hooks_refresh_process_state_before_user_code() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture =
        Fixture(std::env::temp_dir().join(format!("egcl-restore-{}-{nonce}", std::process::id())));
    let builder_home = fixture.0.join("builder");
    let user_home = fixture.0.join("user");
    fs::create_dir_all(&builder_home).unwrap();
    fs::create_dir_all(&user_home).unwrap();
    let probe = fixture.0.join("cache-probe.lisp");
    fs::write(&probe, "(defun restored-cache-probe () 42)").unwrap();
    let init = user_home.join(".egclrc");
    fs::write(
        &init,
        "(image-restore-check) (format t \"RESTORE-BEFORE-INIT~%\")",
    )
    .unwrap();
    for (name, executable, top) in [
        ("core", false, true),
        ("boot", true, false),
        ("app", true, true),
    ] {
        let image = fixture.0.join(name);
        let source = format!(
            r#"
(require :asdf)
(defvar *restore-count* 0)
(defun count-image-restore () (incf *restore-count*))
(uiop:register-image-restore-hook 'count-image-restore nil)
(defun image-restore-check ()
  (assert (= *restore-count* 1))
  (assert (equal asdf::*user-cache* (uiop:xdg-cache-home "common-lisp" :implementation)))
  (assert (= 0 (search (uiop:getenv "HOME") (namestring asdf::*user-cache*))))
  (assert (equal uiop:*command-line-arguments* '("new-argument")))
  (let* ((source (uiop:getenv "EGCL_RESTORE_SOURCE"))
         (output (asdf:apply-output-translations (compile-file-pathname source))))
    (assert (= 0 (search (namestring asdf::*user-cache*) (namestring output))))
    (ensure-directories-exist output)
    (multiple-value-bind (file warnings failure) (compile-file source :output-file output)
      (declare (ignore warnings))
      (assert (not failure))
      (assert (probe-file file))))
  (format t "RESTORE-HOOKS-OK~%"))
(assert (= *restore-count* 0))
(save-lisp-and-die {:?} :executable {} {})
"#,
            image.to_str().unwrap(),
            if executable { "t" } else { "nil" },
            if top {
                ":toplevel #'image-restore-check"
            } else {
                ""
            }
        );
        ok(Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &source])
            .env("HOME", &builder_home)
            .env("XDG_CACHE_HOME", builder_home.join("cache"))
            .output()
            .unwrap());
        let command = || {
            let mut cmd = Command::new(if executable {
                image.as_os_str()
            } else {
                std::ffi::OsStr::new(env!("CARGO_BIN_EXE_egcl"))
            });
            if !executable {
                cmd.arg("--image").arg(&image);
            }
            cmd.env("HOME", &user_home)
                .env_remove("XDG_CACHE_HOME")
                .env("EGCL_INIT_FILE", &init)
                .env("EGCL_RESTORE_SOURCE", &probe)
                .env_remove("ASDF_OUTPUT_TRANSLATIONS");
            cmd
        };
        // Explicit batch evaluation must see fresh state, even with --no-init.
        let output = ok(command()
            .args([
                "--no-init",
                "--eval",
                "(image-restore-check)",
                "--",
                "new-argument",
            ])
            .output()
            .unwrap());
        assert!(output.contains("RESTORE-HOOKS-OK"));
        if top {
            let output = ok(command().args(["--", "new-argument"]).output().unwrap());
            assert!(output.contains("RESTORE-HOOKS-OK"));
            assert!(!output.contains("RESTORE-BEFORE-INIT"));
        } else {
            // The ASDF boot executable must refresh state before reading ~/.egclrc.
            let mut child = command()
                .args(["--", "new-argument"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(b"(quit)\n").unwrap();
            let output = ok(child.wait_with_output().unwrap());
            assert!(output.contains("RESTORE-BEFORE-INIT"), "{output}");
        }
    }
}

#[test]
fn implementation_hooks_use_a_snapshot_and_propagate_errors() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("egcl-init-hooks-{}-{nonce}", std::process::id())),
    );
    fs::create_dir_all(&fixture.0).unwrap();
    let image = fixture.0.join("hooks.core");
    let source = format!(
        r#"
(defvar *hook-order* nil)
(defun first-init-hook ()
  (push :first *hook-order*)
  (setf egcl-ext:*init-hooks* nil)
  (when (egcl-ext:getenv "EGCL_TEST_HOOK_FAIL") (error "RESTORE-HOOK-FAILURE")))
(defun second-init-hook () (push :second *hook-order*))
(setf egcl-ext:*init-hooks* '(first-init-hook second-init-hook))
(assert (null *hook-order*))
(save-lisp-and-die {:?})
"#,
        image.to_str().unwrap()
    );
    ok(Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &source])
        .env_remove("EGCL_TEST_HOOK_FAIL")
        .output()
        .unwrap());
    let output = ok(Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--image").arg(&image).args(["--no-init", "--eval",
            "(assert (equal *hook-order* '(:second :first))) (assert (null egcl-ext:*init-hooks*)) (assert (equal *command-line-args* '(\"one\" \"two\"))) (format t \"INIT-HOOKS-OK~%\")", "--", "one", "two"])
        .env_remove("EGCL_TEST_HOOK_FAIL")
        .env("EGCL_GC_STRESS", "1").env("EGCL_GC_POISON", "1")
        .output().unwrap());
    assert!(output.contains("INIT-HOOKS-OK"));
    let failed = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--image")
        .arg(&image)
        .args(["--no-init", "--eval", "(format t \"USER-CODE-RAN~%\")"])
        .env("EGCL_TEST_HOOK_FAIL", "1")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("RESTORE-HOOK-FAILURE"));
    assert!(!String::from_utf8_lossy(&failed.stdout).contains("USER-CODE-RAN"));
}
