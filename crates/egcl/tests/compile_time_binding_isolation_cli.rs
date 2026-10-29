// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Constants that packages define while COMPILE-FILE runs stay theirs: a third
//! package reading the same name gets its own symbol. When ironclad and then
//! flexi-streams each compiled a `+BUFFER-SIZE+`, the compile-time seeding of
//! the second one gave a homeless `+BUFFER-SIZE+` a global value, the reader
//! then handed that symbol to drakma and salza2, and salza2's DEFPARAMETER of
//! it failed as an assignment to a constant.
use std::process::Command;

#[test]
fn compile_time_constants_do_not_leak_into_other_packages() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "compile-time-binding-isolation-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("first.lisp");
    let second = dir.join("second.lisp");
    std::fs::write(
        &first,
        "(defpackage :ctb-first (:use :cl))\n\
         (in-package :ctb-first)\n\
         (eval-when (:compile-toplevel :load-toplevel)\n\
           (defconstant +ctb-buffer-size+ (* 128 1024)))\n",
    )
    .unwrap();
    std::fs::write(
        &second,
        "(defpackage :ctb-second (:use :cl))\n\
         (in-package :ctb-second)\n\
         (defconstant +ctb-buffer-size+ 8192 \"Default size.\")\n",
    )
    .unwrap();
    // COMPILE-FILE is called from a function, as ASDF calls it, so each
    // compilation runs in its own activation frame.
    let program = format!(
        r##"(progn
      (defun compile-and-load (file) (load (compile-file file)))
      (compile-and-load "{first}")
      (compile-and-load "{second}")
      (defpackage :ctb-other (:use :cl))
      (let* ((*package* (find-package :ctb-other))
             (symbol (read-from-string "+ctb-buffer-size+")))
        (assert (eq (symbol-package symbol) (find-package :ctb-other)))
        (assert (not (constantp symbol)))
        (eval (read-from-string "(defparameter +ctb-buffer-size+ 7)"))
        (assert (= (symbol-value symbol) 7)))
      (assert (= (symbol-value (find-symbol "+CTB-BUFFER-SIZE+" :ctb-first)) 131072))
      (assert (= (symbol-value (find-symbol "+CTB-BUFFER-SIZE+" :ctb-second)) 8192))
      (format t "ISOLATION-OK~%"))"##,
        first = first.display(),
        second = second.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("ISOLATION-OK"),
        "{stdout}\n{stderr}"
    );
}

/// A CASE key list naming DEFCONSTANT is data, not a definition: ocicl's linter
/// has `((defconstant) …)`, and compiling it used to panic seeding NIL.
#[test]
fn defconstant_shaped_case_keys_compile() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("defconstant-case-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("case-keys.lisp");
    std::fs::write(
        &source,
        "(defun classify (head)\n\
           (case head\n\
             ((defconstant) :constant)\n\
             ((defvar defparameter) :special)\n\
             (t :other)))\n",
    )
    .unwrap();
    let program = format!(
        r##"(progn
      (load (compile-file "{source}"))
      (assert (eq (classify 'defconstant) :constant))
      (assert (eq (classify 'defvar) :special))
      (assert (not (constantp 'no-such-constant)))
      (format t "CASE-KEYS-OK~%"))"##,
        source = source.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("CASE-KEYS-OK"),
        "{stdout}\n{stderr}"
    );
}
