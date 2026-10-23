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

/// Expanding a source-kept form at compile time must not walk a definer's
/// names as code: `(defgeneric (setf name) ...)` was walked as a call to SETF,
/// whose expander signalled a message-less PROGRAM-ERROR, so usocket's
/// `option.lisp` stopped compiling (GitHub #561).
#[test]
fn setf_named_definers_compile_and_load() {
    let dir = scratch("setf-named-definers");
    let source = dir.join("setf-definers.lisp");
    std::fs::write(
        &source,
        r#"(defgeneric (setf socket-option) (new-value socket option &key)
  (:documentation "Set a socket's internal options"))
(defgeneric (setf sd-b) (new obj))
(defgeneric (setf sd-c) (new obj &key))
(defmethod (setf sd-d) (new (obj t)) new)
(defun (setf sd-e) (new obj) (declare (ignore obj)) new)
(defgeneric sd-f (obj option &key) (:documentation "doc"))
(defmethod (setf socket-option) (new (obj cons) option &key)
  (setf (car obj) (list option new)))
(defun use-them ()
  (let ((c (list 1)))
    (setf (socket-option c :opt) 5)
    (list c (setf (sd-e nil) 7) (setf (sd-d 3) 9))))
"#,
    )
    .unwrap();
    let compiled = run(&format!(
        r#"(format t "~&RESULT ~s~%"
             (handler-case (cdr (multiple-value-list (compile-file "{}")))
               (error (e) (list :error (type-of e)))))"#,
        source.display()
    ));
    assert!(compiled.contains("RESULT (NIL NIL)"), "{compiled}");
    let loaded = run(&format!(
        r#"(progn (load "{}") (format t "~&RESULT ~s~%" (use-them)))"#,
        dir.join("setf-definers.fasl").display()
    ));
    assert!(loaded.contains("RESULT (((:OPT 5)) 7 9)"), "{loaded}");
}
