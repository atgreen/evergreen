// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn class_names_do_not_make_strings_or_symbols_instances() {
    let program = r#"
      (defpackage :named-types (:use :cl))
      (in-package :named-types)
      (defclass widget () ())
      (defclass child-widget (widget) ())
      (defun widget-p (value) (typep value 'widget))
      (defun classify (value)
        (typecase value (widget :widget) (string :string) (symbol :symbol) (t :other)))
      (dolist (value '("widget" "WIDGET" "NAMED-TYPES::WIDGET" widget :widget))
        (assert (not (eval (list 'typep (list 'quote value) '(quote widget)))))
        (assert (not (funcall #'typep value 'widget)))
        (dotimes (i 100)
          (assert (not (widget-p value)))
          (assert (eq (classify value) (if (stringp value) :string :symbol)))))
      (dolist (class '(widget child-widget))
        (let ((value (make-instance class)))
          (assert (typep value 'widget))
          (assert (typep value (find-class 'widget)))
          (assert (widget-p value))
          (assert (eq (classify value) :widget))))
      (format t "NAMED-TYPE-IDENTITY-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_T1_THRESHOLD", "10")
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("NAMED-TYPE-IDENTITY-OK"), "{stdout}");
}
