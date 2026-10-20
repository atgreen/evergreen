// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A saved image keeps every special proclamation, not only the ones a
//! restored world could guess.
//!
//! The host registry section did not carry the proclaimed-special set, so a
//! restore knew a symbol was special only by the earmuff convention or
//! because it had a global value. A valueless `(defvar flag?)` -- the shape
//! of Parenscript's compiler flag -- was lexical after a restore: a LET of it
//! no longer reached the compiled functions reading it, which saw the
//! unbound global.

use std::process::Command;

fn egcl(args: &[&str]) -> (i32, String, String) {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init"])
        .args(args)
        .output()
        .expect("run egcl");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const SAVE: &str = r#"
(defvar flag?)                     ; special, no value, no earmuffs
(declaim (special decl?))          ; the same, by DECLAIM
(defvar *star*)                    ; special, no value, earmuffs
(defvar valued? :global)           ; special, with a value
(defun read-flag () flag?)
(defun read-decl () decl?)
(defun read-star () *star*)
(defun read-valued () valued?)
;; a plain variable, never proclaimed: a LET of it must stay lexical
(defun read-lex () (if (boundp 'lex?) (symbol-value 'lex?) :lexical))
(defun probe ()
  (list (let ((flag? :bound)) (read-flag))
        (let ((decl? :bound)) (read-decl))
        (let ((*star* :bound)) (read-star))
        (let ((valued? :bound)) (read-valued))
        (let ((lex? :bound)) (read-lex))))
"#;

const EXPECTED: &str = "(BOUND BOUND BOUND BOUND LEXICAL)";

#[test]
fn a_valueless_defvar_is_still_special_after_a_restore() {
    let dir = std::env::temp_dir().join(format!("egcl-ips-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("defs.lisp");
    let image = dir.join("t.core");
    std::fs::write(&source, SAVE).expect("write source");
    let (code, stdout, stderr) = egcl(&[
        "--eval",
        &format!(
            "(progn (load {:?}) (format t \"~&BEFORE ~a~%\" (probe)) (save-lisp-and-die {:?}))",
            source.to_string_lossy(),
            image.to_string_lossy()
        ),
    ]);
    assert_eq!(code, 0, "save failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains(&format!("BEFORE {EXPECTED}")),
        "the probe is wrong before the save:\n{stdout}"
    );
    let (code, stdout, stderr) = egcl(&[
        "--image",
        &image.to_string_lossy(),
        "--eval",
        "(format t \"~&AFTER ~a~%\" (probe))",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, 0, "restore failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains(&format!("AFTER {EXPECTED}")),
        "a proclamation was lost across the image:\n{stdout}"
    );
}
