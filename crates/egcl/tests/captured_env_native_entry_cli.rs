// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Functions whose own bindings are captured by the closures they create enter
//! the mapped native activation on x86-64 Linux. The entry builds the heap
//! EnvFrame exactly as the interpreter does, and native code reaches captured
//! variables through runtime helpers on that one frame, which OSR, deopt and
//! transfer recovery hand on unchanged (bliss-shih7.27).
use std::process::{Command, Output};

/// Hot capturing functions: a mutated captured local, two closures sharing one
/// binding, captured `&optional`, `&key` and `&rest` parameters, a fresh
/// binding per iteration and one binding shared by every iteration, nested
/// captured LETs, cleanup, restart, special-binding and error paths, and the
/// installed-wrapper shape of bliss-kg667. Prints the results, then each tier.
const PROGRAM: &str = r#"
(progn
  (defvar *cap-dyn* 0)
  (defun cap-sum (n)
    (let ((total 0))
      (let ((add (lambda (x) (setq total (+ total x)))))
        (dotimes (i n) (funcall add i)))
      total))
  (defun cap-direct (start n)
    (let ((acc start))
      (let ((peek (lambda () acc)))
        (dotimes (i n) (setq acc (+ acc 1)))
        (list acc (funcall peek)))))
  (defun cap-pair (seed)
    (let ((cell seed))
      (list (lambda (v) (setq cell v)) (lambda () cell))))
  (defun cap-pair-use (k)
    (let* ((p (cap-pair k)) (w (first p)) (r (second p)))
      (funcall w (* k 2))
      (list (funcall r) (progn (funcall w (list k)) (funcall r)))))
  (defun cap-param (a &optional (b 10))
    (let ((f (lambda () (setq a (+ a b)))))
      (funcall f) (funcall f)
      a))
  (defun cap-key (pred lst &key key)
    (remove-if-not (lambda (x) (funcall pred (if key (funcall key x) x))) lst))
  (defun cap-rest (&rest xs)
    (let ((g (lambda () (setq xs (cdr xs)) (length xs))))
      (list (funcall g) xs)))
  (defun cap-fresh (i n acc)
    (if (>= i n)
        (mapcar #'funcall acc)
        (cap-fresh (1+ i) n (cons (lambda () i) acc))))
  (defun cap-fresh-map (lst)
    (mapcar #'funcall (mapcar (lambda (i) (lambda () i)) lst)))
  (defun cap-shared (lst)
    (let ((j 0) (fns nil))
      (mapc (lambda (i) (setq j i) (push (lambda () j) fns)) lst)
      (mapcar #'funcall fns)))
  (defun cap-let-child (x)
    (let ((a (list x)))
      (let ((f (lambda () a)))
        (let ((b (* x 2)))
          (let ((g (lambda () (setq b (+ b 1)))))
            (funcall g)
            (list (funcall f) b
                  (handler-case (if (evenp x) (error "boom ~a" b) b)
                    (error (e) (princ-to-string e)))))))))
  (defun cap-unwind (x)
    (let ((log nil))
      (let ((note (lambda (v) (push v log))))
        (catch 'out
          (unwind-protect (progn (funcall note x) (when (oddp x) (throw 'out nil)))
            (funcall note :cleanup)))
        log)))
  (defun cap-restart (x)
    (let ((seen x))
      (let ((bump (lambda () (setq seen (+ seen 1)))))
        (restart-case (progn (funcall bump)
                             (if (> x 3) (invoke-restart 'use (* seen 10)) seen))
          (use (v) (list v seen))))))
  (defun cap-special (x)
    (let ((*cap-dyn* x) (c (list x)))
      (let ((f (lambda () (list *cap-dyn* c))))
        (funcall f))))
  (defun cap-error (x)
    (let ((v x))
      (let ((f (lambda () v)))
        (if (> x 100) (error "big ~a" (funcall f)) (funcall f)))))
  (defun kg-target (a b) (list a b))
  (let ((original (symbol-function 'kg-target)))
    (setf (symbol-function 'kg-target)
          (lambda (&rest a) (cons :wrapped (apply original a)))))
  (defun all-calls (i)
    (list (cap-sum 10) (cap-direct i 5) (cap-pair-use i) (cap-param i) (cap-param i 3)
          (cap-key #'evenp (list 1 2 3 i) :key #'1+) (cap-key #'oddp (list 1 2 i))
          (cap-rest) (cap-rest i 2 3) (cap-fresh 0 3 nil) (cap-fresh-map (list i 2 3))
          (cap-shared (list i 2 3)) (cap-let-child i) (cap-unwind i) (cap-restart i)
          (cap-special i) (handler-case (cap-error (* i 30)) (error (e) (princ-to-string e)))
          (kg-target i :b)))
  (defparameter *fns* '(cap-sum cap-direct cap-pair cap-pair-use cap-param cap-key cap-rest
                        cap-fresh cap-fresh-map cap-shared cap-let-child cap-unwind
                        cap-restart cap-special cap-error))
  ;; T2 compiles on a background thread: keep calling (bounded) until every
  ;; function is published, then take the answers from the promoted code.
  (loop for i from 0 below 3000
        do (all-calls i)
        until (every (lambda (f) (eql (egcl-ext:function-tier f) 2)) *fns*))
  (print (all-calls 7))
  (print (all-calls 4))
  (print (list (cap-sum 3000) (cap-direct 0 3000)
               (cap-direct (- most-positive-fixnum 5) 20)
               (handler-case (cap-error 1000) (error (e) (princ-to-string e)))
               (loop for i below 200 collect (kg-target i i))))
  (terpri)
  (format t "TIERS ~{~a~^ ~}~%" (mapcar #'egcl-ext:function-tier *fns*)))
"#;

const THRESHOLDS: &[(&str, &str)] = &[
    ("EGCL_T0_T1_THRESHOLD", "2"),
    ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
    ("EGCL_T2_LOG", "stderr"),
];

const ALL_T2: &str = "2 2 2 2 2 2 2 2 2 2 2 2 2 2 2";

const MAPPED_LINUX: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

fn egcl(program: &str, envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
    for name in [
        "EGCL_FORCE_TIER",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_OSR_THRESHOLD",
        "EGCL_DISABLE_T2",
        "EGCL_T2",
    ] {
        command.env_remove(name);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

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

#[test]
fn captured_environments_reach_t2_with_identical_results() {
    let tiered = egcl(PROGRAM, THRESHOLDS);
    let (results, tiers) = results_and_tiers(&tiered);
    // Pin the semantics, so an oracle drifting with the code cannot hide it.
    assert!(
        results.contains(
            "(45 (12 12) (14 (7)) 27 13 (1 3 7) (1 7) (0 NIL) (2 (2 3)) (2 1 0) (7 2 3) \
             (3 3 3) ((7) 15 15) (:CLEANUP 7) (80 8) (7 (7)) \"big 210\" (:WRAPPED 7 :B))"
        ),
        "{results}"
    );
    assert!(results.contains("((4) 9 \"boom 9\")"), "{results}");
    assert!(
        results.contains(
            "(4498500 (3000 3000) (1152921504606846990 1152921504606846990) \"big 1000\""
        ),
        "{results}"
    );
    if MAPPED_LINUX {
        let log = String::from_utf8_lossy(&tiered.stderr);
        assert_eq!(tiers, ALL_T2, "{log}");
        for name in [
            "CAP-SUM",
            "CAP-KEY",
            "CAP-REST",
            "CAP-LET-CHILD",
            "CAP-RESTART",
        ] {
            assert!(
                log.contains(&format!(
                    "[T2] {name}: optimized worker result installed with mapped T2 ABI"
                )),
                "{name} was not installed at mapped T2:\n{log}"
            );
        }
    }
    for envs in [
        &[("EGCL_FORCE_TIER", "interp")][..],
        &[("EGCL_BACKEND", "tree-walker")][..],
        &[("EGCL_DISABLE_T2", "1"), ("EGCL_T0_T1_THRESHOLD", "2")][..],
    ] {
        let (other, _) = results_and_tiers(&egcl(PROGRAM, envs));
        assert_eq!(results, other, "results differ under {envs:?}");
    }
}

/// Every captured-variable access allocates nothing itself, but the entry,
/// the closures and the bodies around them do. Under a collection on every
/// allocation, with freed nursery poisoned, the promoted code must compute the
/// same values.
#[test]
fn captured_environment_entry_survives_gc_stress() {
    let (plain, _) = results_and_tiers(&egcl(PROGRAM, THRESHOLDS));
    let mut envs = THRESHOLDS.to_vec();
    envs.extend([
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
    ]);
    let stressed = egcl(PROGRAM, &envs);
    let (results, tiers) = results_and_tiers(&stressed);
    assert_eq!(plain, results);
    if MAPPED_LINUX {
        assert_eq!(
            tiers,
            ALL_T2,
            "{}",
            String::from_utf8_lossy(&stressed.stderr)
        );
    }
}

/// A T1 activation whose loop reads and writes a captured binding enters T2
/// through OSR, carrying its EnvFrame; a later fixnum overflow in that loop
/// deoptimizes into the interpreter on the same frame.
#[test]
fn captured_loop_enters_t2_by_osr_and_deopts_on_its_frame() {
    let program = r#"
(progn
  (defun osr-acc (start n)
    (let ((acc start) (log nil))
      (let ((peek (lambda () acc))
            (note (lambda (x) (push x log))))
        (dotimes (i n) (setq acc (+ acc 1)))
        (funcall note (funcall peek))
        (list acc (funcall peek) log))))
  (defun osr-call (n)
    (let ((total 0))
      (let ((add (lambda (x) (setq total (+ total x)))))
        (dotimes (i n) (funcall add i)))
      total))
  (osr-acc 0 3) (osr-acc 1 3) (osr-acc 2 3) (osr-call 3) (osr-call 4) (osr-call 5)
  (let ((d0 (egcl-ext:deopt-count)))
    (format t "R1 ~a~%" (osr-acc 0 200000))
    (format t "R2 ~a~%" (osr-call 200000))
    (format t "R3 ~a~%" (osr-acc (- most-positive-fixnum 100000) 200000))
    (format t "OSR ~a ~a DEOPTS ~a TIERS ~a ~a~%"
            (egcl-ext:function-osr-count 'osr-acc) (egcl-ext:function-osr-count 'osr-call)
            (- (egcl-ext:deopt-count) d0)
            (egcl-ext:function-tier 'osr-acc) (egcl-ext:function-tier 'osr-call))))
"#;
    let run = |envs: &[(&str, &str)]| {
        let output = egcl(program, envs);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
    };
    let tiered = run(&[
        ("EGCL_T0_T1_THRESHOLD", "2"),
        ("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000"),
        ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "1000"),
        ("EGCL_T2_THREADS", "1"),
    ]);
    let oracle = run(&[("EGCL_FORCE_TIER", "interp")]);
    let results = |out: &str| {
        out.lines()
            .filter(|line| line.starts_with('R'))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(results(&tiered), results(&oracle), "{tiered}");
    assert!(oracle.contains("R2 19999900000"), "{oracle}");
    if MAPPED_LINUX {
        assert!(
            tiered.contains("OSR 1 1 DEOPTS 1 TIERS 2 2"),
            "both loops must OSR into T2 and the overflow must deopt once: {tiered}"
        );
    }
}

/// bliss-kg667: an installed anonymous wrapper that captured the original
/// definition must keep finding it once it is hot. Such a closure body reads
/// its creator's binding, so it stays interpreted; this pins its results.
#[test]
fn installed_capturing_wrapper_keeps_its_binding_when_hot() {
    let program = r#"
(progn
  (defun f (a b) (list a b))
  (let ((original (symbol-function 'f)))
    (setf (symbol-function 'f) (lambda (&rest a) (cons :wrapped (apply original a)))))
  (defun drive (i) (f i (* i 2)))
  (let ((r nil))
    (dotimes (i 5000) (setq r (drive i)))
    (format t "RESULT ~s ~s~%" r (f 1 2))))
"#;
    for envs in [
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_T2_LOG", "stderr"),
        ][..],
        &[][..],
        &[("EGCL_FORCE_TIER", "interp")][..],
    ] {
        let output = egcl(program, envs);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success()
                && stdout.contains("RESULT (:WRAPPED 4999 9998) (:WRAPPED 1 2)"),
            "{envs:?}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
