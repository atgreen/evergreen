// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn ensure_directories_exist_preserves_directory_only_leaf() {
    let root = std::env::temp_dir().join(format!(
        "egcl-ensure-directory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let form = r#"(progn
      (dolist (path (list "string/leaf/" #P"parsed/leaf/"
                         (make-pathname :directory '(:relative "constructed" "leaf"))))
        (multiple-value-bind (returned created) (ensure-directories-exist path)
          (assert (eq returned path))
          (assert created)
          (assert (probe-file path)))
        (assert (not (nth-value 1 (ensure-directories-exist path))))
        (with-open-file (stream (merge-pathnames "distinfo.txt" path)
                               :direction :output :if-exists :supersede)
          (write-line "test" stream)))
      (ensure-directories-exist #P"file-parent/example.txt")
      (assert (probe-file #P"file-parent/"))
      (assert (not (probe-file #P"file-parent/example.txt")))
      (with-open-file (stream "blocker" :direction :output)
        (write-line "file, not a directory" stream))
      (assert (handler-case (progn (ensure-directories-exist "blocker/leaf/") nil)
                (file-error () t)))
      (write-line "ENSURE-DIRECTORIES-PASS"))"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .current_dir(&root)
        .args(["--no-init", "--eval", form])
        .output()
        .expect("run directory creation checks");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("ENSURE-DIRECTORIES-PASS"),
        "{stdout}\n{stderr}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

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
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &form])
        .output()
        .expect("run missing-directory checks");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("MISSING-DIRECTORY-PASS"),
        "{stdout}\n{stderr}"
    );
}
