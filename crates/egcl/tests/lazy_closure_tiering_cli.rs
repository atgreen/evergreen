// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Under lazy compilation a closure made by a creator that has not been
//! compiled yet -- a DEFUN called once, or a toplevel LET -- is a tree-walker
//! closure. Once hot, it is compiled against its captured frame and tiers like
//! the closure a compiled creator returns (bliss-wojl9, GitHub #467).
use std::process::{Command, Output};

/// Every creator is called once and contains no loop, so lazy compilation
/// leaves it in the tree-walker. RUN drives each closure past the lazy
/// threshold. Prints the results, then the tier of each closure.
const PROGRAM: &str = r#"
(progn
  (defun run (f n) (let ((s nil)) (dotimes (i n) (setq s (funcall f i))) s))
  (defun outer (x) (let ((k 3)) (lambda (y) (+ x y k))))
  (defun counter () (let ((c 0)) (cons (lambda (i) (declare (ignore i)) (incf c))
                                        (lambda (i) (declare (ignore i)) c))))
  (defun optionals (b) (lambda (x &optional (y b) &key (z 10)) (list x y z)))
  (defun two-values (k) (lambda (x) (values x (+ x k))))
  (defun nested (a) (lambda (b) (lambda (c) (+ a b c))))
  (defun failing () (lambda (x) (if (> x 15) (error "boom ~a" x) x)))
  (defun rest-args (k) (lambda (&rest xs) (+ k (length xs))))
  (defun early-exit (k) (lambda (x) (when (> x k) (return-from early-exit x)) x))
  (defun go-out (k) (let ((r 0)) (tagbody (funcall (lambda () (setq r k) (go out))) out) r))
  (defun with-flet (k) (flet ((helper (x) (* x k))) (lambda (y) (helper y))))
  (defun with-special () (let ((*print-base* 2)) (declare (special *print-base*))
                           (lambda (x) (+ x *print-base*))))
  (defun helper-g (x) (* x 2))
  (defun calls-global () (lambda (x) (helper-g x)))
  (defun wrapped (x) (* x 2))
  (let ((original #'wrapped))
    (setf (symbol-function 'wrapped) (lambda (x) (1+ (funcall original x)))))
  (let ((x 10)) (defun shares-x () x) (defparameter *setx* (lambda (v) (setq x v))))
  (let* ((f (outer 1)) (p (counter)) (o (optionals 7)) (m (two-values 5)) (n (nested 1))
         (e (failing)) (r (rest-args 4)) (x (early-exit 1000)) (fl (with-flet 3))
         (sp (with-special)) (g (calls-global)))
    (print (list (let ((s 0)) (dotimes (i 1000) (incf s (funcall f i))) s)
                 (run (car p) 100) (funcall (cdr p) 0)
                 (run o 20) (funcall o 1 2 :z 3)
                 (progn (run m 20) (multiple-value-list (funcall m 3)))
                 (run (lambda (i) (funcall (funcall n i) 2)) 50)
                 (handler-case (run e 30) (error (c) (princ-to-string c)))
                 (run r 20) (funcall r 1 2 3)
                 (run x 50) (go-out 9) (run fl 20) (run sp 20)
                 (progn (run g 20) (defun helper-g (x) (* x 3)) (funcall g 5))
                 (loop for i below 30 sum (wrapped i))
                 (progn (run *setx* 20) (shares-x))))
    (terpri)
    (format t "TIERS ~{~a~^ ~}~%"
            (mapcar #'egcl-ext:function-tier
                    (list f (car p) o m n e r g #'wrapped 'wrapped *setx*)))))
"#;

const RESULTS: &str = "(503500 100 100 (19 7 10) (1 2 3) (3 8) 52 \"boom 16\" 5 7 49 9 57 29 \
                       15 900 19)";

const MAPPED_LINUX: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

fn egcl(envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", PROGRAM]);
    for name in [
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_LAZY_COMPILE",
        "EGCL_LAZY_THRESHOLD",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_DISABLE_T2",
        "EGCL_PROFILING_DISABLED",
    ] {
        command.env_remove(name);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn results_and_tiers(output: &Output) -> (String, Vec<String>) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (results, tiers) = stdout.split_once("TIERS ").expect("tier line");
    let tiers = tiers.lines().next().unwrap_or_default();
    (
        results.trim().to_string(),
        tiers.split(' ').map(str::to_string).collect(),
    )
}

#[test]
fn closures_of_uncompiled_creators_tier_with_identical_results() {
    let (results, tiers) = results_and_tiers(&egcl(&[]));
    assert_eq!(results, RESULTS);
    if MAPPED_LINUX {
        // Every closure here reached native code; before bliss-wojl9 each was
        // NIL (a tree-walker closure) or 0 (the reified wrapper's object).
        for (index, tier) in tiers.iter().enumerate() {
            assert!(
                tier == "1" || tier == "2",
                "closure {index} did not tier: {tiers:?}"
            );
        }
    }
    for envs in [
        &[("EGCL_FORCE_TIER", "interp")][..],
        &[("EGCL_LAZY_COMPILE", "0")][..],
        &[("EGCL_DISABLE_T2", "1")][..],
    ] {
        let (other, _) = results_and_tiers(&egcl(envs));
        assert_eq!(results, other, "results differ under {envs:?}");
    }
}

/// The compile happens on an ordinary call, with the closure's frame, lambda
/// list and body held across allocating lowering. Under a collection on every
/// allocation with freed nursery poisoned the answers must not change.
#[test]
fn lazy_closure_compilation_survives_gc_stress() {
    let stressed = egcl(&[
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
    ]);
    let (results, _) = results_and_tiers(&stressed);
    assert_eq!(results, RESULTS);
}
