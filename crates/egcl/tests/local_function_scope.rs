// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn check(source: &str) {
    for tier in ["interp", "t0", "t1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_FORCE_TIER", tier)
            .args(["--no-init", "--eval", source]).output().unwrap();
        assert!(output.status.success(), "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn local_functions_use_their_definition_site_bindings() {
    check(r#"
      (let ((x 10))
        (flet ((read-x () x) (write-x (v) (setq x v)))
          (let ((x 20))
            (assert (= (read-x) 10))
            (write-x 11)
            (assert (= x 20))
            (assert (= (funcall #'read-x) 11)))
          (assert (= x 11))))
      (let ((x 12))
        (labels ((read-x (n) (if (zerop n) x (read-x (1- n)))))
          (let ((x 20)) (assert (= (read-x 3) 12)))))
    "#);
}

#[test]
fn local_functions_capture_special_declarations_not_caller_declarations() {
    check(r#"
      (let ((x 10))
        (declare (special x))
        (flet ((read-x () x))
          (let ((x 20)) (assert (= (read-x) 10))
            (assert (= (funcall #'read-x) 10)))
          (let ((x 30)) (declare (special x)) (assert (= (read-x) 30)))))
      (let ((x 10))
        (flet ((read-x () x))
          (let ((x 20)) (declare (special x))
            (assert (= (read-x) 10)))))
    "#);
}

#[test]
fn free_special_declarations_override_outer_lexical_bindings() {
    check(r#"
      (let ((x :good))
        (declare (special x))
        (let ((x :bad))
          (assert (eq (let () (declare (special x)) x) :good))
          (assert (eq (let* () (declare (special x)) x) :good))
          (assert (eq x :bad))))
    "#);
}

#[test]
fn parameters_shadow_captured_special_declarations() {
    check(r#"
      (let ((x 10))
        (declare (special x))
        (flet ((read-x (x &optional (y x)) (list x y)))
          (assert (equal (read-x 7) '(7 7))))
        (let ((f (lambda (x &optional (y x)) (list x y))))
          (assert (equal (funcall f 8) '(8 8)))))
    "#);
}

#[test]
fn anonymous_closures_keep_their_special_reference_scope() {
    check(r#"
      (let ((x :dynamic))
        (declare (special x))
        (let ((f (let ((x :lexical))
                   (locally (declare (special x)) (lambda () x)))))
          (let ((x :caller)) (assert (eq (funcall f) :dynamic)))))
    "#);
}

#[test]
fn dotimes_preserves_special_bindings_and_result_declarations() {
    check(r#"
      (let ((i 0) (seen nil) (bound 4))
        (declare (special i))
        (flet ((read-i () i))
          (dotimes (i bound) (declare (special i)) (push (read-i) seen)))
        (assert (equal seen '(3 2 1 0))))
      (let ((x :good))
        (declare (special x))
        (let ((x :bad))
          (assert (eq (dotimes (i 3 x) (declare (special x))) :good))))
      (assert (= (dotimes (i -3 i)) 0))
      (assert (= (dotimes (i (return 17))) 17))
      (let ((closures nil))
        (dotimes (i 3) (push (lambda () i) closures))
        (assert (equal (mapcar #'funcall closures) '(2 1 0))))
    "#);
}
