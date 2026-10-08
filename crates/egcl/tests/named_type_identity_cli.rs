// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check_type_names(tier: &str) {
    let program = r#"
      (defpackage :type-home-a (:use :cl))
      (defpackage :type-home-b (:use :cl))
      (in-package :type-home-a)
      (defstruct digest state)
      (defclass widget () ())
      (define-condition trouble (error) ())
      (in-package :type-home-b)
      (defstruct digest state)
      (defclass widget () ())
      (define-condition trouble (error) ())
      (in-package :cl-user)
      (defun object-type-name (object) (type-of object))
      (let ((objects (list (type-home-a::make-digest)
                           (type-home-b::make-digest)
                           (make-instance 'type-home-a::widget)
                           (make-instance 'type-home-b::widget)
                           (make-condition 'type-home-a::trouble)
                           (make-condition 'type-home-b::trouble)))
            (names '(type-home-a::digest type-home-b::digest
                     type-home-a::widget type-home-b::widget
                     type-home-a::trouble type-home-b::trouble)))
        (loop for object in objects for name in names do
          (assert (eq (object-type-name object) name))
          (assert (eq (funcall #'type-of object) name))
          (assert (eq (eval (list 'type-of (list 'quote object))) name))
          (assert (eq (class-name (class-of object)) name))))
      (disassemble #'object-type-name)
      (format t "TYPE-NAME-PACKAGES-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", tier)
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    assert!(stdout.contains("TYPE-NAME-PACKAGES-OK"), "{stdout}");
    if tier != "interp" {
        assert!(
            stdout.contains(&format!("; {}", tier.to_uppercase())),
            "requested {tier} was not installed: {stdout}"
        );
    }
}

#[test]
fn type_of_preserves_defining_symbols_interpreted() {
    check_type_names("interp");
}

#[test]
fn type_of_preserves_defining_symbols_t0() {
    check_type_names("t0");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn type_of_preserves_defining_symbols_t1() {
    check_type_names("t1");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn type_of_preserves_defining_symbols_t2() {
    check_type_names("t2");
}

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
