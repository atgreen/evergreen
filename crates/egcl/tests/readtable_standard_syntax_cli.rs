// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Standard syntax copied into another readtable with GET-MACRO-CHARACTER and
//! SET-MACRO-CHARACTER — as named-readtables' `(:merge :standard)` does — reads
//! nested forms with that readtable, and LOAD binds `*READTABLE*`.
use std::process::Command;

fn run(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("READTABLE-OK"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Defines `*MERGED*`: a readtable whose `"` reads `"<text>"`, with every other
/// standard macro character and `#` sub-character copied in from the standard
/// readtable through the GET-/SET- functions.
const MERGED: &str = r##"
  (defparameter *merged* (copy-readtable nil))
  (set-macro-character #\"
    (lambda (stream char)
      (declare (ignore char))
      (with-output-to-string (out)
        (write-char #\< out)
        (loop for c = (read-char stream t nil t)
              until (char= c #\")
              do (write-char c out))
        (write-char #\> out)))
    nil *merged*)
  (let ((standard (copy-readtable nil)))
    (dolist (c '(#\( #\) #\' #\; #\` #\,))
      (multiple-value-bind (function non-terminating-p) (get-macro-character c standard)
        (set-macro-character c function non-terminating-p *merged*)))
    (dolist (sub '(#\' #\( #\\ #\+ #\- #\|))
      (set-dispatch-macro-character #\# sub
        (get-dispatch-macro-character #\# sub standard) *merged*)))
"##;

#[test]
fn copied_standard_syntax_reads_nested_forms_with_the_current_readtable() {
    run(&format!(
        r##"(progn {MERGED}
      (let ((*readtable* *merged*))
        (assert (equal (read-from-string (format nil "(\"a\" ; \"b~%  \"c\")"))
                       '("<a>" "<c>")))
        (let ((form (read-from-string "(1 \"ab\" '(\"c\") #(\"d\") #+(or) \"e\" #\\x)")))
          (assert (equal (second form) "<ab>"))
          (assert (equal (third form) '(quote ("<c>"))))
          (assert (equalp (fourth form) #("<d>")))
          (assert (eql (fifth form) #\x))
          (assert (= (length form) 5)))
        (assert (equal (eval (read-from-string "`(,(car '(\"g\")))")) '("<g>")))
        (assert (equal (funcall (eval (read-from-string "#'(lambda () \"h\")"))) "<h>")))
      (format t "READTABLE-OK~%"))"##
    ));
}

#[test]
fn copied_standard_syntax_applies_to_a_loaded_file() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("readtable-standard-syntax-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("merged.lisp");
    std::fs::write(
        &file,
        "(eval-when (:compile-toplevel :load-toplevel :execute) (setf *readtable* *merged*))\n\
         (defparameter *loaded* (list \"ab\" '(\"c\") #(\"d\")))\n",
    )
    .unwrap();
    run(&format!(
        r##"(progn {MERGED}
      (defvar *before* *readtable*)
      (load "{file}")
      (assert (equal (first *loaded*) "<ab>"))
      (assert (equal (second *loaded*) '("<c>")))
      (assert (equalp (third *loaded*) #("<d>")))
      (assert (eq *readtable* *before*))
      (format t "READTABLE-OK~%"))"##,
        file = file.display()
    ));
}

#[test]
fn load_restores_the_callers_readtable() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("readtable-load-binding-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("switch.lisp");
    std::fs::write(&file, "(setf *readtable* (copy-readtable nil))\n").unwrap();
    run(&format!(
        r##"(let ((before *readtable*))
      (load "{file}")
      (assert (eq *readtable* before))
      (format t "READTABLE-OK~%"))"##,
        file = file.display()
    ));
}
