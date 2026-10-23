// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! COMPILE-FILE expands macros at compile time (CLHS 3.2.2.2), including in a
//! top-level form it keeps as source. A DEFUN whose body holds a DEFCLASS,
//! DEFMETHOD or DEFMACRO is kept that way, and its macros were expanded again
//! when the fasl loaded, in the loader's dynamic environment, where LIFT's
//! DEFTESTSUITE found `*compile-file-pathname*` NIL (GitHub #530).
use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn egcl(program: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap()
}

fn run(program: &str) -> String {
    let output = egcl(program);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

#[test]
fn loading_a_fasl_does_not_expand_a_source_kept_defun_again() {
    let dir = scratch("load-source-reexpansion");
    let source = dir.join("reexpand.lisp");
    std::fs::write(
        &source,
        r#"(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar *expansions* '()))
(defmacro counted (tag)
  (push (list tag (if *compile-file-pathname* :compile (if *load-pathname* :load :run)))
        *expansions*)
  `(list ',tag (and ,(and *compile-file-pathname* t) :expanded-while-compiling)))
(defgeneric rex-gf (x))
(defun with-class () (defclass rex-c () ()) (counted :class))
(defun with-method () (defmethod rex-gf ((x integer)) x) (counted :method))
(defun with-macro (&optional (when 1)) (defmacro rex-m () 1) (list when (counted :macro)))
"#,
    )
    .unwrap();
    let compiled = run(&format!(
        r#"(multiple-value-bind (out warnings-p failure-p) (compile-file "{}")
             (format t "~&RESULT ~s ~s ~s~%" (and out t) warnings-p failure-p))"#,
        source.display()
    ));
    assert!(compiled.contains("RESULT T NIL NIL"), "{compiled}");

    let loaded = run(&format!(
        r#"(progn (load "{}")
             (format t "~&CALLS ~s~%"
                     (list (with-class) (with-class) (with-method) (with-macro)))
             (format t "~&EXPANSIONS ~s~%" *expansions*))"#,
        dir.join("reexpand.fasl").display()
    ));
    assert!(
        loaded.contains(
            "CALLS ((:CLASS :EXPANDED-WHILE-COMPILING) (:CLASS :EXPANDED-WHILE-COMPILING) \
             (:METHOD :EXPANDED-WHILE-COMPILING) (1 (:MACRO :EXPANDED-WHILE-COMPILING)))"
        ),
        "{loaded}"
    );
    assert!(loaded.contains("EXPANSIONS NIL"), "{loaded}");
}
