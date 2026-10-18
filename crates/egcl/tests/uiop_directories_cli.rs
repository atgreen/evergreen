// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! UIOP's CHDIR and DELETE-DIRECTORY-TREE on EGCL, through egcl-ext:chdir and
//! egcl-ext:delete-directory. ocicl calls both; each used to signal "Not
//! (currently) implemented on EGCL".
use std::process::Command;

#[test]
fn uiop_chdir_and_delete_directory_tree_work() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("uiop-directories-{}", std::process::id()));
    let tree = dir.join("tree");
    std::fs::create_dir_all(tree.join("a/b")).unwrap();
    std::fs::write(tree.join("a/b/file.txt"), "x").unwrap();
    std::fs::write(tree.join("top.txt"), "y").unwrap();
    let program = format!(
        r##"(progn
      (uiop:chdir "{dir}/")
      (assert (equal (uiop:getcwd) (uiop:ensure-directory-pathname "{dir}")))
      (uiop:delete-directory-tree (uiop:ensure-directory-pathname "{tree}") :validate t)
      (assert (not (probe-file "{tree}/")))
      (handler-case (progn (uiop:chdir "{dir}/no-such-directory/") (error "chdir succeeded"))
        (file-error () nil))
      (format t "DIRECTORIES-OK~%"))"##,
        dir = dir.display(),
        tree = tree.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", "(require :asdf)", "--eval", &program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("DIRECTORIES-OK"),
        "{stdout}\n{stderr}"
    );
    assert!(!tree.exists());
}
