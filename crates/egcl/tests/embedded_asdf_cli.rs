// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! (require :asdf) works without the source tree the binary was built from.
//! It loaded CARGO_MANIFEST_DIR/../../lib/asdf.lisp, a path that exists only
//! on the build machine (bliss-bp4q). An empty EGCL_ASDF_FILE selects the copy
//! embedded in the binary, which is what a binary run elsewhere falls back to.
use std::process::Command;

#[test]
fn require_asdf_loads_the_embedded_copy() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            "(require :asdf)",
            "--eval",
            "(format t \"~&ASDF ~a ~a~%\" (asdf:asdf-version) (not (null (find-package :uiop))))",
        ])
        .env("EGCL_ASDF_FILE", "")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("ASDF 3.3.7 T"), "{stdout}\n{stderr}");
}

#[test]
fn egcl_asdf_file_names_the_asdf_to_load() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", "(require :asdf)"])
        .env("EGCL_ASDF_FILE", "/nonexistent/bliss-bp4q/asdf.lisp")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("/nonexistent/bliss-bp4q/asdf.lisp"),
        "{stderr}"
    );
}
