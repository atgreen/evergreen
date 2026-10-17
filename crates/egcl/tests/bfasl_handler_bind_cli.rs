// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! HANDLER-BIND tables round-trip through a compiled unit: handlers that
//! muffle, decline and transfer control behave the same after the fasl loads in
//! a fresh process. Before the tables had an encoding, a function using
//! HANDLER-BIND was left out of a saved image.
use std::process::{Command, Output};

fn egcl(dir: &std::path::Path, program: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .current_dir(dir)
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap()
}

#[test]
fn handler_bind_functions_load_from_a_fasl_and_behave() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fasl-handler-bind-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("handlers.lisp"),
        r#"(defun muffled (thunk)
  (handler-bind ((warning #'muffle-warning))
    (funcall thunk)))
(defun first-error-message (thunk)
  (block found
    (handler-bind ((error (lambda (c) (return-from found (princ-to-string c)))))
      (funcall thunk))))
(defun declined-then-caught (thunk)
  (let ((seen '()))
    (values
     (handler-case
         (handler-bind ((simple-error (lambda (c) (declare (ignore c)) (push :inner seen)))
                        (warning (lambda (c) (declare (ignore c)) (push :wrong seen))))
           (funcall thunk))
       (error () :outer))
     seen)))
"#,
    )
    .unwrap();
    let compiled = egcl(&dir, r#"(compile-file "handlers.lisp")"#);
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let loaded = egcl(
        &dir,
        r#"(progn
      (load "handlers.fasl")
      (assert (eq (muffled (lambda () (warn "quiet") :done)) :done))
      (assert (equal (first-error-message (lambda () (error "boom ~D" 42))) "boom 42"))
      (multiple-value-bind (result seen) (declined-then-caught (lambda () (error "x")))
        (assert (eq result :outer))
        (assert (equal seen '(:inner))))
      (format t "HANDLER-BIND-OK~%"))"#,
    );
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    assert!(
        loaded.status.success() && stdout.contains("HANDLER-BIND-OK"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
}
