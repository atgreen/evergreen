// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

#[test]
fn saved_closures_preserve_definition_frames_and_special_declarations() {
    let dir = std::env::temp_dir().join(format!("egcl-closure-scope-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _fixture = Fixture(dir.clone());
    let core = dir.join("scope.core");
    let source = format!(r#"
      (defvar *scope-reader*
        (let ((x :lexical))
          (locally (declare (special x)) (lambda () x))))
      (defvar *local-reader*
        (let ((x :definition))
          (flet ((read-x () x)) (let ((x :reference)) #'read-x))))
      (egcl-ext:save-lisp-and-die {core:?})
    "#);
    let built = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "interp")
        .args(["--no-init", "--eval", &source]).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let restored = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "interp")
        .args(["--no-init", "--image"]).arg(&core)
        .args(["--eval", r#"
          (let ((x :dynamic))
            (declare (special x))
            (let ((x :caller))
              (assert (eq (funcall *scope-reader*) :dynamic))
              (assert (eq (funcall *local-reader*) :definition))))
          (format t "RESTORED-SCOPES-OK~%")
        "#]).output().unwrap();
    assert!(restored.status.success(), "{}\n{}",
        String::from_utf8_lossy(&restored.stdout), String::from_utf8_lossy(&restored.stderr));
    assert!(String::from_utf8_lossy(&restored.stdout).contains("RESTORED-SCOPES-OK"));
}
