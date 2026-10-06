// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A DEFCONSTANT compiled in a file must be recognised as a constant for the
//! rest of that compilation (CLHS 3.2.2.3, bliss-vhr6e). Seeding only its value
//! left the name bound but not constant, so alexandria's DEFINE-CONSTANT — which
//! asks CONSTANTP before re-evaluating — reported "already bound non-constant
//! variable" when the fasl loaded, and iolib could not load.
use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no RESULT: line in output:\n{stdout}\n{stderr}"))
        .trim()
        .to_string()
}

/// Write SOURCE to its own temporary file, COMPILE-FILE it, and report what the
/// compilation left behind. The file is named after NAME so tests running in
/// parallel cannot collide.
fn after_compiling(name: &str, source: &str, report: &str) -> String {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("egcl-defconstant-{name}.lisp"));
    let fasl = dir.join(format!("egcl-defconstant-{name}.fasl"));
    std::fs::write(&src, source).unwrap();
    eval(&format!(
        "(progn (compile-file {src:?} :output-file {fasl:?}) (format t \"RESULT:~s\" {report}))",
        src = src.display().to_string(),
        fasl = fasl.display().to_string(),
    ))
}

#[test]
fn a_defconstant_compiled_in_a_file_is_a_constant_afterwards() {
    assert_eq!(
        after_compiling(
            "k",
            "(defconstant +probe-k+ 42)",
            "(list (boundp '+probe-k+) (constantp '+probe-k+) (symbol-value '+probe-k+))"
        ),
        "(T T 42)"
    );
}

#[test]
fn a_defparameter_compiled_in_a_file_is_not_a_constant() {
    assert_eq!(
        after_compiling(
            "p",
            "(defparameter *probe-p* 42)",
            "(list (boundp '*probe-p*) (constantp '*probe-p*))"
        ),
        "(NIL NIL)"
    );
}

#[test]
fn a_constant_can_be_redefined_to_the_same_value_after_compiling() {
    // alexandria's DEFINE-CONSTANT shape: re-evaluating a DEFCONSTANT is normal
    // (COMPILE-FILE evaluates it, then loading the fasl evaluates it again), and
    // a caller that checks CONSTANTP first must see the truth.
    assert_eq!(
        after_compiling(
            "c",
            "(defconstant +probe-c+ #\\#)",
            "(progn (defconstant +probe-c+ #\\#)
                    (list (constantp '+probe-c+) (symbol-value '+probe-c+)))"
        ),
        "(T #\\#)"
    );
}
