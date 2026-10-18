// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compiled files and literals that come from the reader: a string with a fill
//! pointer externalizes as its active characters, and a file that must still be
//! written source-only reads its `#.` forms against the source file, not the
//! fasl.
use std::process::Command;

fn run(dir: &std::path::Path, program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .current_dir(dir)
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("LITERALS-OK"),
        "{stdout}\n{stderr}"
    );
    stdout
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// cl-interpol's `#?"…"` reads to WRITE-STRING calls on strings with fill
/// pointers. One such literal used to make the whole file source-only.
#[test]
fn fill_pointer_string_literal_is_externalized() {
    let dir = scratch("fill-pointer-literal");
    std::fs::write(
        dir.join("interp.lisp"),
        "(defun greet (name)\n\
           (with-output-to-string (out)\n\
             (write-string #.(let ((s (make-array 8 :element-type 'character\n\
                                                   :fill-pointer 0 :adjustable t)))\n\
                               (dolist (c (coerce \"Hello, \" 'list) s)\n\
                                 (vector-push-extend c s)))\n\
                           out)\n\
             (princ name out)))\n",
    )
    .unwrap();
    let stdout = run(
        &dir,
        r##"(progn
      (load (compile-file "interp.lisp"))
      (assert (equal (greet "egcl") "Hello, egcl"))
      (format t "LITERALS-OK~%"))"##,
    );
    assert!(!stdout.contains("source-only"), "{stdout}");
}

#[test]
fn source_only_fasl_reads_against_the_source_file() {
    let dir = scratch("source-only-read-time");
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(
        dir.join("here.lisp"),
        "(defparameter *function* #.#'car)\n\
         (defparameter *here* #.(namestring (or *compile-file-pathname* *load-pathname*)))\n",
    )
    .unwrap();
    let stdout = run(
        &dir,
        r##"(let ((fasl (compile-file "here.lisp" :output-file "out/here.fasl")))
      (load fasl)
      (assert (eq *function* #'car))
      (assert (string= (pathname-name *here*) "here"))
      (assert (string= (pathname-type *here*) "lisp"))
      (assert (null *compile-file-pathname*))
      (format t "LITERALS-OK~%"))"##,
    );
    assert!(stdout.contains("source-only"), "the probe must exercise a source-only fasl: {stdout}");
}
