// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A foreign library handle saved in an image stays usable after `--image`
//! (GitHub #440): restore reopens each library the image recorded, in load
//! order, into the token the saved FOREIGN-LIBRARY holds. A library that cannot
//! be reopened leaves a dead handle whose lookups name it and whose close is a
//! no-op, which CFFI's close-then-load reload path relies on.
//!
//! Runs on every Linux target with a loader backend for compiled fixtures: the
//! static ELF loader (musl, or glibc without `egcl-rt/c-ffi`) on x86-64 and
//! AArch64, and dlopen with `c-ffi`.
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn egcl(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env(
            "EGCL_INIT_FILE",
            std::env::temp_dir().join("egcl-tests-nonexistent-init.lisp"),
        )
        .args(args)
        .output()
        .expect("run egcl")
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// `dir/name.so`, whose `fn_name` returns `value`.
fn build_library(dir: &Path, name: &str, fn_name: &str, value: i32) -> PathBuf {
    let source = dir.join(format!("{name}.c"));
    let library = dir.join(format!("{name}.so"));
    std::fs::write(
        &source,
        format!("int {fn_name}(void) {{ return {value}; }}\n"),
    )
    .unwrap();
    // -fno-stack-protector: __stack_chk_guard does not resolve through the
    // static loader's host-symbol table on every target (bliss-d3smh).
    let status = Command::new(std::env::var("EGCL_TEST_CC").as_deref().unwrap_or("cc"))
        .args(["-shared", "-fPIC", "-fno-stack-protector"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .status()
        .expect("run cc");
    assert!(status.success(), "cc failed for {name}");
    library
}

const TRY: &str = r#"(defun try (label form)
  (format t "~a ~s~%" label
          (handler-case (eval form)
            (error (e) (list :error (princ-to-string e))))))"#;

/// Load two libraries, close the second (leaving a gap in the token table),
/// load a third, and save. Returns the core path.
fn save_core(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let first = build_library(dir, "libfirst", "egcl_first", 42);
    let closed = build_library(dir, "libclosed", "egcl_closed", 7);
    let third = build_library(dir, "libthird", "egcl_third", 99);
    let core = dir.join("libs.core");
    let output = egcl(&[
        "--no-init",
        "--eval",
        &format!(
            r#"(progn
                 (defvar *first* (egcl-ffi:load-foreign-library {first:?}))
                 (defvar *closed* (egcl-ffi:load-foreign-library {closed:?}))
                 (egcl-ffi:close-foreign-library *closed*)
                 (defvar *third* (egcl-ffi:load-foreign-library {third:?}))
                 (egcl-ext:save-lisp-and-die {core:?}))"#
        ),
    ]);
    assert!(output.status.success(), "{}", text(&output));
    (core, first, third)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("egcl-ffi-image-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn saved_library_handles_are_reopened_after_restore() {
    let dir = scratch("reopen");
    let (core, _, _) = save_core(&dir);
    let output = egcl(&[
        "--image",
        core.to_str().unwrap(),
        "--no-init",
        "--eval",
        TRY,
        "--eval",
        r#"(progn
             (try "FIRST" '(egcl-ffi:foreign-call
                             (egcl-ffi:foreign-symbol-pointer "egcl_first" *first*) :int '() '()))
             (try "THIRD" '(egcl-ffi:foreign-call
                             (egcl-ffi:foreign-symbol-pointer "egcl_third" *third*) :int '() '()))
             (try "CLOSED" '(egcl-ffi:foreign-symbol-pointer "egcl_closed" *closed*))
             (try "CLOSE-FIRST" '(egcl-ffi:close-foreign-library *first*))
             (try "AFTER-CLOSE" '(egcl-ffi:foreign-symbol-pointer "egcl_first" *first*))
             (try "THIRD-AGAIN" '(egcl-ffi:foreign-call
                                   (egcl-ffi:foreign-symbol-pointer "egcl_third" *third*) :int '() '())))"#,
    ]);
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    assert!(out.contains("FIRST 42"), "{out}");
    assert!(out.contains("THIRD 99"), "{out}");
    // A library closed before the save stays closed: its token is not reused.
    assert!(
        out.contains("CLOSED (:ERROR \"invalid or closed library handle\")"),
        "{out}"
    );
    assert!(out.contains("CLOSE-FIRST NIL"), "{out}");
    assert!(out.contains("AFTER-CLOSE (:ERROR"), "{out}");
    assert!(out.contains("THIRD-AGAIN 99"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_library_missing_at_restore_leaves_a_dead_handle_that_closes_quietly() {
    let dir = scratch("dead");
    let (core, first, _) = save_core(&dir);
    std::fs::remove_file(&first).unwrap();
    let output = egcl(&[
        "--image",
        core.to_str().unwrap(),
        "--no-init",
        "--eval",
        TRY,
        "--eval",
        r#"(progn
             (try "LOOKUP" '(egcl-ffi:foreign-symbol-pointer "egcl_first" *first*))
             (try "CLOSE" '(egcl-ffi:close-foreign-library *first*))
             (try "CLOSE-AGAIN" '(egcl-ffi:close-foreign-library *first*))
             (try "THIRD" '(egcl-ffi:foreign-call
                             (egcl-ffi:foreign-symbol-pointer "egcl_third" *third*) :int '() '())))"#,
    ]);
    let out = text(&output);
    assert!(output.status.success(), "{out}");
    let lookup = out.lines().find(|l| l.starts_with("LOOKUP ")).unwrap_or("");
    assert!(
        lookup.contains("could not be reopened") && lookup.contains("libfirst.so"),
        "{out}"
    );
    assert!(out.contains("CLOSE NIL"), "{out}");
    assert!(out.contains("CLOSE-AGAIN NIL"), "{out}");
    // A later library in load order is unaffected.
    assert!(out.contains("THIRD 99"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
