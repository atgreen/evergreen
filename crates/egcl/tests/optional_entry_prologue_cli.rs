// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `&optional` parameters compiled SBCL-style: positional slots, omitted
//! arguments filled with a marker on entry, and defaults computed by a prologue
//! in the function's own code, instead of the run-time lambda-list binder that
//! re-parsed the lambda list and tree-walked the defaults on every call.
use std::process::{Command, Output};

const DEFINITIONS: &str = r#"
(defun two-defaults (a &optional (b (* a 10)) (c (+ a b)))
  (list a b c))
(defun supplied (&optional (x :default x-p) (y nil y-p))
  (list x x-p y y-p))
(defun outer-reference ()
  (let ((x 10))
    (flet ((f (&optional (x x)) x))
      (list (f) (f 1)))))
(defun declared (a &optional (b 2))
  (declare (fixnum a b))
  (+ a b))
(defun only-a-string (&optional a)
  "the result, not a docstring")
(defun documented (&optional (a 1))
  "A docstring."
  (* a 2))
(defvar *dynamic* :global)
(defun read-dynamic () *dynamic*)
(defun binds-special (&optional (*dynamic* :bound))
  (read-dynamic))
(defun sum-to (n &optional (acc 0))
  (if (zerop n) acc (sum-to (1- n) (+ acc n))))
(defun hot (a &optional (b 1))
  (+ a b))
"#;

const CHECKS: &str = r#"
(progn
  (assert (equal (two-defaults 1) '(1 10 11)))
  (assert (equal (two-defaults 1 2) '(1 2 3)))
  (assert (equal (two-defaults 1 2 3) '(1 2 3)))
  (assert (equal (supplied) '(:default nil nil nil)))
  (assert (equal (supplied nil) '(nil t nil nil)))
  (assert (equal (supplied 1 nil) '(1 t nil t)))
  (assert (equal (outer-reference) '(10 1)))
  (assert (= (declared 3) 5))
  (assert (= (declared 3 4) 7))
  (assert (equal (only-a-string) "the result, not a docstring"))
  (assert (= (documented) 2))
  (assert (= (documented 5) 10))
  (assert (eq (binds-special) :bound))
  (assert (eq (binds-special :given) :given))
  (assert (eq *dynamic* :global))
  (assert (= (sum-to 100) 5050))
  (assert (= (sum-to 10 5) 60))
  (assert (handler-case (progn (two-defaults) nil) (program-error () t)))
  (assert (handler-case (progn (two-defaults 1 2 3 4) nil) (program-error () t)))
  (assert (handler-case (progn (funcall 'two-defaults 1 2 3 4) nil) (program-error () t)))
  (dotimes (i 500)
    (assert (= (hot i) (+ i 1)))
    (assert (= (hot i i) (* 2 i))))
  (assert (= (hot 41) 42))
  (assert (= (hot 40 2) 42))
  (format t "OPTIONAL-PROLOGUE-OK~%"))
"#;

fn egcl(dir: &std::path::Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.current_dir(dir).arg("--no-init").args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("definitions.lisp"), DEFINITIONS).unwrap();
    dir
}

fn assert_ok(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("OPTIONAL-PROLOGUE-OK"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn optional_parameters_bind_and_promote_from_source() {
    let dir = scratch("optional-prologue-source");
    assert_ok(&egcl(
        &dir,
        &["--eval", r#"(load "definitions.lisp")"#, "--eval", CHECKS],
        &[],
    ));
}

#[test]
fn optional_parameters_bind_and_promote_from_a_fasl() {
    let dir = scratch("optional-prologue-fasl");
    let compiled = egcl(
        &dir,
        &["--eval", r#"(compile-file "definitions.lisp")"#],
        &[],
    );
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert_ok(&egcl(
        &dir,
        &["--eval", r#"(load "definitions.fasl")"#, "--eval", CHECKS],
        &[],
    ));
}

/// The prologue rewrite allocates while it builds the new lambda list and body.
#[test]
fn optional_prologue_survives_gc_stress() {
    let dir = scratch("optional-prologue-stress");
    assert_ok(&egcl(
        &dir,
        &["--eval", r#"(load "definitions.lisp")"#, "--eval", CHECKS],
        &[
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ],
    ));
}

/// Hot `&optional` functions, called with and without their optional
/// arguments: a heap default, a default that overflows to a bignum, and
/// recursion that omits an argument. Prints the results, then each tier.
const T2_PROGRAM: &str = r#"
(progn
  (defun hot (a &optional (b 1)) (+ a b))
  (defun two-defaults (a &optional (b (* a 10)) (c (+ a b))) (list a b c))
  (defun hot-list (a &optional (b (list a a))) (list* a b))
  (defun overflow (a &optional (b most-positive-fixnum)) (+ a b))
  (defun sum-to (n &optional (acc 0)) (if (zerop n) acc (sum-to (1- n) (+ acc n))))
  (defun all-calls (i)
    (list (hot i) (hot i i) (two-defaults i) (two-defaults i 2)
          (two-defaults i 2 3) (hot-list i) (hot-list i :x)
          (overflow 0) (sum-to 20) (sum-to 5 i)))
  ;; T2 compiles on a background thread: keep calling (bounded) until every
  ;; function is published, then take the answers from the promoted code.
  (loop for i from 0 below 5000
        do (all-calls i)
        until (every (lambda (f) (eql (egcl-ext:function-tier f) 2))
                     '(hot two-defaults hot-list overflow sum-to)))
  (print (all-calls 199))
  (print (list (overflow 1) (overflow -1) (overflow 1 2) (sum-to 100)
               (handler-case (progn (two-defaults) nil) (program-error () :few))
               (handler-case (progn (funcall 'two-defaults 1 2 3 4) nil)
                 (program-error () :many))))
  (terpri)
  (format t "TIERS ~{~a~^ ~}~%"
          (mapcar #'egcl-ext:function-tier '(hot two-defaults hot-list overflow sum-to))))
"#;

const T2_THRESHOLDS: &[(&str, &str)] = &[
    ("EGCL_T0_T1_THRESHOLD", "2"),
    ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
    ("EGCL_T2_LOG", "stderr"),
];

/// Splits the program's output into its result lines and its tier line.
fn results_and_tiers(output: &Output) -> (String, String) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (results, tiers) = stdout.split_once("TIERS ").expect("tier line");
    let tiers = tiers.lines().next().unwrap_or_default();
    (results.trim().to_string(), tiers.to_string())
}

/// The mapped Linux entry pads omitted `&optional` arguments, so these
/// functions reach T2 instead of being declined at T1 (bliss-shih7.27), and
/// compute what the tree-walker and the T2-disabled engine compute.
#[test]
fn optional_parameters_reach_t2_with_identical_results() {
    let dir = scratch("optional-prologue-t2");
    let tiered = egcl(&dir, &["--eval", T2_PROGRAM], T2_THRESHOLDS);
    let (results, tiers) = results_and_tiers(&tiered);
    assert!(
        results.contains("1152921504606846976 1152921504606846974 3 5050 :FEW :MANY"),
        "{results}"
    );
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        assert_eq!(
            tiers,
            "2 2 2 2 2",
            "{}",
            String::from_utf8_lossy(&tiered.stderr)
        );
        let log = String::from_utf8_lossy(&tiered.stderr);
        for name in ["HOT", "TWO-DEFAULTS", "HOT-LIST", "OVERFLOW", "SUM-TO"] {
            assert!(
                log.contains(&format!("[T2] {name}: optimized worker result installed")),
                "{name} was not installed at T2:\n{log}"
            );
        }
    }
    for envs in [
        &[("EGCL_BACKEND", "tree-walker")][..],
        &[("EGCL_DISABLE_T2", "1"), ("EGCL_T0_T1_THRESHOLD", "2")][..],
    ] {
        let (other, _) = results_and_tiers(&egcl(&dir, &["--eval", T2_PROGRAM], envs));
        assert_eq!(results, other, "results differ under {envs:?}");
    }
}

/// The promoted entry binds a heap default in a managed frame slot. Under a
/// collection on every allocation it must still compute the same values.
#[test]
fn optional_t2_entry_survives_gc_stress() {
    let dir = scratch("optional-prologue-t2-stress");
    let (plain, _) = results_and_tiers(&egcl(&dir, &["--eval", T2_PROGRAM], T2_THRESHOLDS));
    let mut envs = T2_THRESHOLDS.to_vec();
    envs.extend([
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
    ]);
    let stressed = egcl(&dir, &["--eval", T2_PROGRAM], &envs);
    let (results, tiers) = results_and_tiers(&stressed);
    assert_eq!(plain, results);
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        assert_eq!(
            tiers,
            "2 2 2 2 2",
            "{}",
            String::from_utf8_lossy(&stressed.stderr)
        );
    }
}
