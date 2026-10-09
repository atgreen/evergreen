// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", unix))]
use std::process::Command;

fn run(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE-FUNCALL-OK"));
}

#[test]
fn native_funcall_preserves_register_slice_values_and_arity_errors() {
    run(r#"
      (defun invoke-one (f x) (funcall f x))
      (defun invoke-four (f a b c d) (funcall f a b c d))
      (defun invoke-zero (f) (funcall f))
      (defun make-one (seed) (lambda (x) (values (+ seed x) seed)))
      (defun make-four (seed) (lambda (a b c d) (+ seed a b c d)))
      (let ((one (make-one 10)) (four (make-four 100)))
        (dotimes (i 40)
          (multiple-value-bind (value seed) (invoke-one one i)
            (assert (= value (+ 10 i))) (assert (= seed 10))))
        (dotimes (i 40) (assert (= (invoke-four four i 2 3 4) (+ i 109))))
        (dotimes (i 3)
          (assert (handler-case (progn (invoke-one four 1) nil) (program-error () t)))
          (assert (handler-case (progn (invoke-four one 1 2 3 4) nil) (program-error () t)))
          (assert (handler-case (progn (invoke-zero one) nil) (program-error () t)))))
      (assert (= 2 (egcl-ext:function-tier 'invoke-one)))
      (assert (= 2 (egcl-ext:function-tier 'invoke-four)))
      (format t "NATIVE-FUNCALL-OK~%")
    "#);
}

#[test]
fn native_funcall_keeps_saved_function_identity_after_redefinition() {
    run(r#"
      (defun identity-target (x) (+ x 1))
      (defun identity-caller (f x) (funcall f x))
      (defvar *saved-identity* #'identity-target)
      (dotimes (i 40) (assert (= (identity-caller *saved-identity* i) (+ i 1))))
      (defun identity-target (x) (+ x 100))
      (dotimes (i 40) (assert (= (identity-target i) (+ i 100))))
      (assert (= 2 (egcl-ext:function-tier 'identity-target)))
      (assert (= 2 (egcl-ext:function-tier 'identity-caller)))
      (dotimes (i 40) (assert (= (identity-caller *saved-identity* i) (+ i 1))))
      (fmakunbound 'identity-target)
      (dotimes (i 40) (assert (= (identity-caller *saved-identity* i) (+ i 1))))
      (format t "NATIVE-FUNCALL-OK~%")
    "#);
}

#[test]
fn nested_native_funcall_restores_captures_after_gc_and_nonlocal_exit() {
    run(r#"
      (defun invoke-nested (f g x) (funcall f g x))
      (defun make-outer (seed)
        (lambda (g x)
          (setq seed (list x seed))
          (funcall g x)
          (car seed)))
      (defun make-inner (seed)
        (lambda (x)
          (setq seed (list x seed))
          (egcl-ext:gc :full t)
          (car seed)))
      (defun throw-callback (x) (throw 'callback-exit x))
      (let ((outer (make-outer (list 1))) (inner (make-inner (list 2))))
        (dotimes (i 40) (assert (= (invoke-nested outer inner i) i)))
        (dotimes (i 3)
          (assert (= (catch 'callback-exit (invoke-nested outer #'throw-callback 91)) 91))
          (assert (= (invoke-nested outer inner 92) 92))))
      (format t "NATIVE-FUNCALL-OK~%")
    "#);
}

#[test]
fn callback_redefinition_of_funcall_cannot_publish_a_stale_dispatcher() {
    run(r#"
      (defun redefining-invoke (f x) (funcall f x))
      (defun redefining-leaf (x) (+ x 1))
      (defun replace-funcall-during-call (x)
        (setf (symbol-function 'funcall) (lambda (f value) (+ value 100)))
        x)
      (dotimes (i 40) (assert (= (redefining-invoke #'redefining-leaf i) (+ i 1))))
      (assert (= 2 (egcl-ext:function-tier 'redefining-invoke)))
      (assert (= (redefining-invoke #'replace-funcall-during-call 7) 7))
      (assert (= (redefining-invoke #'redefining-leaf 7) 107))
      (format t "NATIVE-FUNCALL-OK~%")
    "#);
}
