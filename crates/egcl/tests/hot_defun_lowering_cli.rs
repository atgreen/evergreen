// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Hot global defuns must leave the tree-walker (bliss-gg3i1.7.16).
//!
//! A function whose body the bytecode lowerer bails on is tree-walked on every
//! call for the rest of the process. In SBCL's cross-compiler that was
//! GET-INFO-VALUE and PACKED-INFO-VALUE-INDEX (MULTIPLE-VALUE-SETQ), the
//! CALC-…-HASH functions (an inline `((lambda …) …)` application),
//! UPDATE-SYMBOL-INFO (`(setf (get …))`) and MAYBE-REPARSE-SPECIFIER (a
//! HANDLER-CASE clause typed `(or …)`): ~110k interpreted calls per compiled
//! file. Each shape below must reach T0 (FUNCTION-TIER >= 1) once hot, under
//! both lazy and eager compilation, with results identical to the
//! tree-walker's.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defvar *sv* 0)
  (define-condition hot-c1 (error) ())
  (defun hot-mvs (x)
    (let (a b c)
      (list (multiple-value-setq (a b c) (floor x 3)) a b c)))
  (defun hot-mvs-none (x) (multiple-value-setq () (values x 2)))
  (defun hot-mvs-special (x)
    (let ((a 0)) (multiple-value-setq (*sv* a) (floor x 3)) (list *sv* a)))
  (defun hot-mvs-swap (x y) (multiple-value-setq (x y) (values y x)) (list x y))
  (defun hot-lambda (x) ((lambda (a b) (declare (fixnum a)) (+ a b)) x 10))
  (defun hot-lambda-order (x)
    (let ((log nil))
      ((lambda (a b) (list a b (reverse log)))
       (progn (push 1 log) x) (progn (push 2 log) (1+ x)))))
  (defun hot-setf-get (s v) (setf (get s 'hot-p1) v (get s 'hot-p2 :default) (1+ v))
    (list (get s 'hot-p1) (get s 'hot-p2)))
  (defun hot-setf-get-order (s)
    (let ((log nil))
      (setf (get (progn (push 1 log) s) (progn (push 2 log) 'hot-p3) (progn (push 3 log) nil))
            (progn (push 4 log) 9))
      (reverse log)))
  (defun hot-setf-get-bad (x) (handler-case (setf (get x 'p) 1) (type-error () :type-error)))
  (defun hot-handler-or (k)
    (handler-case (case k (0 (error 'hot-c1)) (1 (signal 'warning) :signalled)
                          (2 (error "plain")) (t :none))
      ((or hot-c1 warning) (c) (list :or (type-of c)))
      (error () :error)))
  (let ((names '(hot-mvs hot-mvs-none hot-mvs-special hot-mvs-swap hot-lambda
                 hot-lambda-order hot-setf-get hot-setf-get-order
                 hot-setf-get-bad hot-handler-or)))
    (dotimes (i 40)
      (hot-mvs i) (hot-mvs-none i) (hot-mvs-special i) (hot-mvs-swap i 1)
      (hot-lambda i) (hot-lambda-order i) (hot-setf-get 'hot-s i)
      (hot-setf-get-order 'hot-s) (hot-setf-get-bad 3) (hot-handler-or (mod i 4)))
    (format t "TIERS ~S~%"
            (mapcar (lambda (n) (list n (egcl-ext:function-tier n))) names))
    (format t "VALUES ~S~%"
            (list (hot-mvs 10) (hot-mvs-none 4) (hot-mvs-special 7) *sv* (hot-mvs-swap 1 2)
                  (hot-lambda 1) (hot-lambda-order 5)
                  (hot-setf-get 'hot-t 5) (hot-setf-get-order 'hot-t)
                  (hot-setf-get-bad 3) (hot-setf-get-bad "s")
                  (hot-handler-or 0) (hot-handler-or 1) (hot-handler-or 2)
                  (hot-handler-or 3)))))
"#;

const EXPECTED_VALUES: &str = "VALUES ((3 3 1 NIL) 4 (2 1) 2 (2 1) 11 (5 6 (1 2)) \
(5 6) (1 2 3 4) :TYPE-ERROR :TYPE-ERROR (:OR HOT-C1) (:OR WARNING) :ERROR :NONE)";

fn run(envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(BIN);
    cmd.args(["--no-init", "--eval", PROGRAM]);
    for name in [
        "EGCL_LAZY_COMPILE",
        "EGCL_LAZY_THRESHOLD",
        "EGCL_FORCE_TIER",
        "EGCL_T2",
        "EGCL_PROFILING_DISABLED",
        "EGCL_LOG",
    ] {
        cmd.env_remove(name);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "egcl failed: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn check(envs: &[(&str, &str)]) {
    let out = run(envs);
    let tiers = out
        .lines()
        .find(|l| l.starts_with("TIERS "))
        .unwrap_or_else(|| panic!("no TIERS line: {out}"));
    for entry in tiers
        .trim_start_matches("TIERS ((")
        .trim_end_matches("))")
        .split(") (")
    {
        let (name, tier) = entry.rsplit_once(' ').expect("name tier");
        let tier: i64 = tier
            .parse()
            .unwrap_or_else(|_| panic!("tier of {name}: {tier}"));
        assert!(
            tier >= 1,
            "{name} stayed tree-walked (tier {tier}) under {envs:?}: {out}"
        );
    }
    assert!(
        out.lines().any(|l| l == EXPECTED_VALUES),
        "values changed under {envs:?}: {out}"
    );
}

#[test]
fn hot_defuns_with_mv_setq_lambda_forms_setf_get_and_or_handlers_leave_the_tree_walker() {
    check(&[]);
}

#[test]
fn hot_defun_shapes_compile_eagerly_too() {
    check(&[("EGCL_LAZY_COMPILE", "0")]);
}

#[test]
fn tree_walker_and_forced_tiers_give_the_same_values() {
    for tier in ["treewalk", "t0", "t1", "t2"] {
        let out = run(&[("EGCL_FORCE_TIER", tier)]);
        assert!(
            out.lines().any(|l| l == EXPECTED_VALUES),
            "values differ under EGCL_FORCE_TIER={tier}: {out}"
        );
    }
}
