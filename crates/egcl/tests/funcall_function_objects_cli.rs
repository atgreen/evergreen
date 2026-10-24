// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FUNCALL and APPLY of every kind of function object from warmed, compiled
//! callers (bliss-vwgb0).
//!
//! A function object whose installed T2 code (or builtin kernel) has already
//! been selected is called through a cached target instead of re-selecting it
//! on every call. These programs call through function objects well past the
//! tiering thresholds and check that the result is the tree-walker's: argument
//! count and keyword errors, `&optional`/`&rest`/`&key`, multiple values,
//! RETURN-FROM and THROW across the call, redefinition after warm-up, and a
//! closure over a variable that both sides mutate.
//!
//! The expected lines were taken from the binary before this change, under the
//! tree-walker. Each program also runs with no forced tier, which is where the
//! cached targets are used.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [Option<&str>; 5] = [Some("interp"), Some("t0"), Some("t1"), Some("t2"), None];

fn run(tier: Option<&str>, program: &str) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", program]);
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_FORCE_TIER",
    ] {
        cmd.env_remove(key);
    }
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tier {tier:?}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn check_all_tiers(program: &str, expected: &str) {
    for tier in TIERS {
        let out = run(tier, program);
        let line = out
            .lines()
            .find(|line| line.starts_with("RESULT "))
            .unwrap_or_else(|| panic!("tier {tier:?}: no RESULT line in:\n{out}"));
        assert_eq!(line, expected, "tier {tier:?}:\n{out}");
    }
}

#[test]
fn every_function_kind_through_funcall_and_apply() {
    check_all_tiers(
        r#"
(progn
  (defun fo-add (a b) (+ a b))
  (defun fo-opt (a &optional (b 10) (c 100 c-p)) (+ a b (if c-p c 0)))
  (defun fo-rest (a &rest more) (+ a (length more)))
  (defun fo-key (a &key (b 1) (c 2)) (+ a (* b c)))
  (defun fo-drive (f n &rest extra)
    (let ((s 0))
      (dotimes (i n s)
        (setq s (+ s (apply f i extra))))))
  (defun fo-funcall2 (f n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (funcall f i 3))))))
  (defun fo-make-adder (k) (lambda (x y) (+ x y k)))
  (let* ((n 3000)
         (lam (lambda (x y) (- x y)))
         (adder (fo-make-adder 7))
         (results
           (list (fo-funcall2 #'+ n)
                 (fo-funcall2 #'fo-add n)
                 (fo-funcall2 lam n)
                 (fo-funcall2 adder n)
                 (fo-funcall2 #'max n)
                 (fo-funcall2 'fo-add n)
                 (fo-funcall2 #'fo-opt n)
                 (fo-drive #'fo-opt n)
                 (fo-drive #'fo-opt n 1 2)
                 (fo-funcall2 #'fo-rest n)
                 (fo-drive #'fo-rest n 1 2 3 4)
                 (fo-drive #'fo-key n :c 5)
                 (fo-drive #'fo-key n :c 5 :b 2)
                 (fo-drive #'fo-key n :b 3 :allow-other-keys t :zz 1)
                 (fo-drive (lambda (x &key (y 4)) (* x y)) n :y 2)
                 (fo-drive #'1+ n)
                 (fo-drive #'abs n))))
    (format t "RESULT ~S~%" results)))
"#,
        "RESULT (4507500 4507500 4489500 4528500 4498506 4507500 4507500 4528500 4507500 4501500 4510500 4513500 4528500 4516500 8997000 4501500 4498500)",
    );
}

#[test]
fn argument_count_and_keyword_errors_after_warm_up() {
    check_all_tiers(
        r#"
(progn
  (defun fe-add (a b) (+ a b))
  (defun fe-key (a &key (b 1)) (+ a b))
  (defun fe-call (f &rest args) (apply f args))
  (defun fe-call2 (f a b) (funcall f a b))
  (dotimes (i 3000) (fe-call2 #'fe-add i 1) (fe-call #'fe-key i :b 2) (fe-call #'car (list i)))
  (flet ((try (thunk)
           (handler-case (progn (funcall thunk) :no-error)
             (program-error () :program-error)
             (type-error () :type-error)
             (error () :error))))
    (format t "RESULT ~S~%"
            (list (try (lambda () (fe-call #'fe-add 1)))
                  (try (lambda () (fe-call #'fe-add 1 2 3)))
                  (try (lambda () (fe-call #'fe-key 1 :c 2)))
                  (try (lambda () (fe-call #'fe-key 1 :b)))
                  (try (lambda () (fe-call #'car 1 2)))
                  (try (lambda () (fe-call #'car 1)))
                  (try (lambda () (fe-call2 #'fe-add 1 :x)))
                  (try (lambda () (fe-call (lambda (x) x))))
                  (try (lambda () (fe-call2 (lambda (x) x) 1 2)))
                  (fe-call2 #'fe-add 40 2)
                  (fe-call #'fe-key 1 :b 2)))))
"#,
        "RESULT (:PROGRAM-ERROR :PROGRAM-ERROR :PROGRAM-ERROR :PROGRAM-ERROR :PROGRAM-ERROR :TYPE-ERROR :TYPE-ERROR :PROGRAM-ERROR :PROGRAM-ERROR 42 3)",
    );
}

#[test]
fn multiple_values_and_nonlocal_exits_through_the_call() {
    check_all_tiers(
        r#"
(progn
  (defun fm-values (a b) (values a b (+ a b)))
  (defun fm-one (a b) (+ a b))
  (defun fm-mv (f a b) (multiple-value-list (funcall f a b)))
  (defun fm-apply-mv (f a b) (multiple-value-list (apply f a b nil)))
  (defun fm-thrower (a b) (if (> a 2990) (throw 'fm-tag (list :thrown a b)) (+ a b)))
  (defun fm-catch (f a) (catch 'fm-tag (funcall f a 1)))
  (defun fm-escaper (f) (block out (funcall f (lambda (v) (return-from out (list :escaped v)))) :normal))
  (defvar *fm-cleanups* 0)
  (defun fm-protected (f a) (catch 'fm-tag (unwind-protect (funcall f a 1) (incf *fm-cleanups*))))
  (let ((last-mv nil) (last-one nil) (last-floor nil) (last-catch nil) (escapes 0) (last-prot nil))
    (dotimes (i 3000)
      (setq last-mv (fm-mv #'fm-values i 1))
      (setq last-one (fm-apply-mv #'fm-one i 1))
      (setq last-floor (fm-mv #'floor (+ i 7) 2))
      (setq last-catch (fm-catch #'fm-thrower i))
      (setq last-prot (fm-protected #'fm-thrower i))
      (when (consp (fm-escaper (lambda (k) (if (evenp i) (funcall k i) i))))
        (incf escapes)))
    (format t "RESULT ~S~%"
            (list last-mv last-one last-floor last-catch last-prot escapes *fm-cleanups*
                  (multiple-value-list (funcall #'fm-values 1 2))
                  (fm-mv (lambda (a b) (declare (ignore a b)) (values)) 1 2)))))
"#,
        "RESULT ((2999 1 3000) (3000) (1503 0) (:THROWN 2999 1) (:THROWN 2999 1) 1500 3000 (1 2 3) NIL)",
    );
}

#[test]
fn redefinition_and_mutated_closure_after_warm_up() {
    check_all_tiers(
        r#"
(progn
  (defun fr-target (x) (+ x 1))
  (defun fr-drive (f n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (funcall f i))))))
  (let* ((old #'fr-target)
         (a (fr-drive #'fr-target 3000))
         (b (fr-drive 'fr-target 3000)))
    (defun fr-target (x) (* x 10))
    (let* ((c (fr-drive #'fr-target 3000))
           (d (fr-drive 'fr-target 3000))
           (e (fr-drive old 3000))
           (count 0)
           (counter (lambda (x) (incf count x))))
      (fr-drive counter 3000)
      (setq count (- count))
      (let ((f (fr-drive counter 10)))
        (setf (symbol-function 'fr-target) (lambda (x) (- x)))
        (format t "RESULT ~S~%"
                (list a b c d e count f (fr-drive 'fr-target 3000)
                      (fr-drive (symbol-function 'fr-target) 3)))))))
"#,
        "RESULT (4501500 4501500 44985000 44985000 4501500 -4498455 -44984835 -4498500 -3)",
    );
}
