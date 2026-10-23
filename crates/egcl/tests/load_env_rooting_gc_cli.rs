// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `--load` must keep its `Env` rooted from the first allocation (bliss-md2ec).
//!
//! The command-line `--load` path entered `load_path_into_env` with an `Env`
//! that no caller had rooted, and the load's prologue allocates: it interns
//! symbols and builds the `*LOAD-PATHNAME*` namestring and pathname. A
//! collection there relocated objects that the `Env` still referenced, so the
//! `Env` held stale pointers by the time `read_eval_all_env` rooted it. The
//! collection after that, inside the reader, failed `EGCL_GC_VERIFY`:
//!
//! ```text
//! gc-verify(holder): stale 0x... in slot 0x... held by rooted node ...
//! ```
//!
//! The file's contents did not matter, and `--eval` was unaffected because it
//! roots the `Env` before allocating. A forced collection on every allocation
//! after startup reproduces the failure deterministically. The nested load
//! also checks that the caller's `*LOAD-PATHNAME*` is restored correctly
//! after the inner load returns.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "egcl-load-env-rooting-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn load_under_verify(file: &PathBuf) -> (String, String) {
    // No EGCL_GC_POISON: with poison set, this defect's stale reference is not
    // reported by the verifier, and the test would pass without the fix.
    let output = Command::new(BIN)
        .args(["--no-init", "--load"])
        .arg(file)
        .env("EGCL_GC_STRESS", "1")
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env_remove("EGCL_GC_POISON")
        .env("EGCL_GC_VERIFY", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "--load under EGCL_GC_VERIFY + stress failed ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        !stderr.contains("gc-verify"),
        "the verifier reported a stale reference:\n{stderr}"
    );
    (stdout, stderr)
}

#[test]
fn a_single_form_load_passes_the_gc_verifier() {
    let dir = scratch_dir("single");
    let file = dir.join("trivial.lisp");
    std::fs::write(&file, "(format t \"T-OK~%\")\n").unwrap();
    let (stdout, _) = load_under_verify(&file);
    assert_eq!(stdout.trim(), "T-OK");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_nested_load_passes_the_gc_verifier_and_restores_load_pathname() {
    let dir = scratch_dir("nested");
    let outer = dir.join("outer.lisp");
    let inner = dir.join("inner.lisp");
    std::fs::write(
        &outer,
        "(defvar *md2ec-before* (namestring *load-pathname*))\n\
         (load (merge-pathnames \"inner.lisp\" *load-pathname*))\n\
         (format t \"OUTER ~a~%\" (string= (namestring *load-pathname*) *md2ec-before*))\n\
         ; a trailing comment makes the reader work again before EOF\n",
    )
    .unwrap();
    std::fs::write(
        &inner,
        "(format t \"INNER ~a~%\" (pathname-name *load-pathname*))\n",
    )
    .unwrap();
    let (stdout, _) = load_under_verify(&outer);
    assert_eq!(
        stdout.lines().map(str::trim).collect::<Vec<_>>(),
        ["INNER inner", "OUTER T"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
