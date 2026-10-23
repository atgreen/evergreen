// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A store to a name declared SPECIAL by an enclosing LOCALLY or binding form
//! writes the dynamic value, not the shadowed lexical binding, in every tier,
//! including inside a compiled RESTART-CASE clause and through a macro that
//! expands to SETQ (bliss-35xxb). The expected values are SBCL 2.6.9's.
use std::process::Command;

const PROGRAM: &str = r#"(progn
  (defmacro set-x (v) (list 'setq 'x v))
  (defun locally-store ()
    (let ((x :lexical))
      (locally (declare (special x)) (setq x :changed))
      (list x (symbol-value 'x))))
  (defun binding-store ()
    (let ((x :lexical))
      (let ((y 1)) (declare (special x)) (setf x (list :c y)))
      (list x (symbol-value 'x))))
  (defun restart-store ()
    (let ((x :lexical))
      (list (restart-case (invoke-restart 'r)
              (r () (locally (declare (special x)) (set-x :changed)) x))
            x
            (symbol-value 'x))))
  (defun set-store ()
    (let ((x :lexical))
      (set 'x :set)
      (list x (funcall (lambda () (set 'x :closure) x)) (symbol-value 'x))))
  (flet ((fresh (f) (setf (symbol-value 'x) :global) (funcall f)))
    (dotimes (i 50)
      (fresh #'locally-store) (fresh #'binding-store) (fresh #'restart-store) (fresh #'set-store))
    (format t "~&RESULT ~s~%"
            (list (fresh #'locally-store) (fresh #'binding-store) (fresh #'restart-store)
                  (fresh #'set-store)
                  (handler-case (set nil 1) (error () :error))))))"#;

#[test]
fn stores_to_a_locally_special_name_write_the_dynamic_value() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains(
                "RESULT ((:LEXICAL :CHANGED) (:LEXICAL (:C 1)) (:LEXICAL :LEXICAL :CHANGED) \
                 (:LEXICAL :LEXICAL :CLOSURE) :ERROR)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
