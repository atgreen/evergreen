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
/// captured LETs, cleanup, restart, special-binding and error paths, and a
/// caller of the installed-wrapper shape of bliss-kg667. Prints the results,
/// then each tier.
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
/// definition must keep finding it once the wrapper itself is hot. The wrapper
/// is created by a compiled function, so it is a bytecode closure whose body
/// reads its creator's ORIGINAL; it now enters mapped native code, and the
/// environment it reads is the invoked instance's, never one found by looking
/// up the name it was installed under. A second wrapper of the same template
/// installed over a different function shares the compiled body and must still
/// reach its own ORIGINAL.
#[test]
fn installed_capturing_wrapper_runs_natively_with_its_own_binding() {
    let program = r#"
(progn
  (defun f (a b) (list a b))
  (defun g (a b) (list :g a b))
  (defun install (sym)
    (let ((original (symbol-function sym)))
      (setf (symbol-function sym) (lambda (&rest a) (cons :wrapped (apply original a))))))
  (install 'f)
  (install 'g)
  (defun drive (i) (list (f i (* i 2)) (g i i)))
  (let ((r nil))
    (dotimes (i 5000) (setq r (drive i)))
    (format t "RESULT ~s ~s ~s~%" r (f 1 2) (g 3 4))
    (format t "TIERS ~a ~a~%" (egcl-ext:function-tier 'f) (egcl-ext:function-tier 'g))))
"#;
    let lazy_off = [("EGCL_LAZY_COMPILE", "0")];
    let mut low = THRESHOLDS.to_vec();
    low.extend(lazy_off);
    for envs in [&low[..], &lazy_off[..], &[][..], &[("EGCL_FORCE_TIER", "interp")][..]] {
        let output = egcl(program, envs);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success()
                && stdout.contains(
                    "RESULT ((:WRAPPED 4999 9998) (:WRAPPED :G 4999 4999)) \
                     (:WRAPPED 1 2) (:WRAPPED :G 3 4)"
                ),
            "{envs:?}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if MAPPED_LINUX && envs.len() > 1 {
            assert!(stdout.contains("TIERS 2 2"), "{envs:?}: {stdout}");
        }
    }
}

/// Closure bodies that read and write bindings captured from an enclosing
/// scope (bliss-6fim1): fifty instances of one template with fifty different
/// environments, sibling closures and their creator sharing a mutated binding,
/// a closure in a function cell, closures made in loops, three levels of
/// nesting, `&key`/`&optional` defaults that read a captured binding, multiple
/// values, non-local exits, errors and dynamic bindings. Creators are compiled
/// eagerly so every closure is a bytecode closure. Prints results, then tiers.
const CLOSURE_PROGRAM: &str = r#"
(progn
  (defvar *dd* 1)
  (defun make-adder (n) (lambda (x) (+ x n)))
  (defun make-counter ()
    (let ((n 0))
      (list (lambda () (incf n)) (lambda () n) (lambda (v) (setq n v))
            (lambda () (list :creator n)))))
  (defun install-cell (base)
    (setf (symbol-function 'cell-fn) (lambda (x) (+ x base)))
    (lambda (b) (setq base b)))
  (defun loop-fns (n)
    (let ((acc nil)) (dotimes (i n) (let ((j i)) (push (lambda (x) (* x j)) acc))) acc))
  (defun lvl1 (a)
    (lambda (b) (lambda (c) (lambda (d) (setq a (+ a 1)) (list a b c d)))))
  (defun mkkey (base) (lambda (x &key (scale 1) (off base)) (+ (* x scale) off)))
  (defun mkopt (base) (lambda (x &optional (y base)) (list x y)))
  (defun mvc (k) (lambda (x) (values x k (+ x k))))
  (defun mkthrow (k) (lambda (x) (if (> x k) (throw 'tag (list :thrown x k)) x)))
  (defun mkerr (k) (lambda (x) (if (= x k) (error "bad ~a ~a" x k) (* x k))))
  (defun mkdyn (y) (lambda () (list y *dd*)))
  (defvar *adders*) (defvar *counters*) (defvar *setter*) (defvar *loop-fns*)
  (defvar *deep*) (defvar *key*) (defvar *opt*) (defvar *mv*) (defvar *throw*)
  (defvar *err*) (defvar *dyn*) (defvar *probes*)
  ;; Fresh instances of every template; the answers below come from instances
  ;; made after warm-up, which share the bodies compiled for the first ones.
  (defun setup ()
    (setq *adders* (loop for i below 50 collect (make-adder i))
          *counters* (loop repeat 4 collect (make-counter))
          *setter* (install-cell 100)
          *loop-fns* (loop-fns 20)
          *deep* (funcall (funcall (lvl1 10) 20) 30)
          *key* (mkkey 7)
          *opt* (mkopt 9)
          *mv* (mvc 4)
          *throw* (mkthrow 5)
          *err* (mkerr 3)
          *dyn* (mkdyn :y))
    (setq *probes*
          (list (first *adders*) (first (first *counters*)) (second (first *counters*))
                (symbol-function 'cell-fn) (first *loop-fns*) *deep* *key* *opt* *mv*
                *throw* *err* *dyn*)))
  (setup)
  (defun all-calls (i)
    (list (loop for f in *adders* sum (funcall f i))
          (progn (dolist (c *counters*) (funcall (first c)))
                 (mapcar (lambda (c) (funcall (second c))) *counters*))
          (cell-fn i)
          (loop for f in *loop-fns* sum (funcall f i))
          (funcall *deep* i)
          (list (funcall *key* i :scale 2) (funcall *key* i :off 1))
          (list (funcall *opt* i) (funcall *opt* i i))
          (multiple-value-list (funcall *mv* i))
          (catch 'tag (dotimes (k 10) (funcall *throw* (+ i k))))
          (handler-case (loop for k below 5 sum (funcall *err* (+ k (mod i 4))))
            (error (e) (princ-to-string e)))
          (let ((*dd* i)) (funcall *dyn*))))
  (loop for i from 0 below 3000
        do (all-calls i)
        until (every (lambda (f) (eql (egcl-ext:function-tier f) 2)) *probes*))
  (setup)
  (print (all-calls 7))
  (funcall (third (first *counters*)) 1000)
  (funcall *setter* 1)
  (print (list (funcall (fourth (first *counters*))) (funcall (second (second *counters*)))
               (cell-fn 5) (all-calls 2)))
  (terpri)
  (format t "TIERS ~{~a~^ ~}~%" (mapcar #'egcl-ext:function-tier *probes*)))
"#;

#[test]
fn closure_bodies_reading_enclosing_bindings_reach_t2_with_identical_results() {
    let mut envs = THRESHOLDS.to_vec();
    envs.push(("EGCL_LAZY_COMPILE", "0"));
    let tiered = egcl(CLOSURE_PROGRAM, &envs);
    let (results, tiers) = results_and_tiers(&tiered);
    // Pin the semantics, so an oracle drifting with the code cannot hide it.
    assert_eq!(
        results,
        "(1575 (1 1 1 1) 107 1330 (11 20 30 7) (21 8) ((7 9) (7 7)) (7 4 11) (:THROWN 7 5) \
         \"bad 3 3\" (:Y 7)) \n((:CREATOR 1000) 1 6 (1325 (1001 2 2 2) 3 380 (12 20 30 2) \
         (11 3) ((2 9) (2 2)) (2 4 6) (:THROWN 6 5) \"bad 3 3\" (:Y 2)))"
    );
    if MAPPED_LINUX {
        // Instances made after warm-up run the bodies their templates' first
        // instances compiled.
        assert_eq!(
            tiers,
            "2 2 2 2 2 2 2 2 2 2 2 2",
            "{}",
            String::from_utf8_lossy(&tiered.stderr)
        );
    }
    for other in [
        &[("EGCL_FORCE_TIER", "interp")][..],
        &[("EGCL_LAZY_COMPILE", "0"), ("EGCL_DISABLE_T2", "1"), ("EGCL_T0_T1_THRESHOLD", "2")][..],
        &[][..],
    ] {
        let (oracle, _) = results_and_tiers(&egcl(CLOSURE_PROGRAM, other));
        assert_eq!(results, oracle, "results differ under {other:?}");
    }
}

/// The same closures under a collection on every allocation with freed nursery
/// poisoned. Captured bindings live in heap EnvFrames the collector updates in
/// place; a value read through one by native code, and held in a native slot
/// across a later allocation, must be the moved object.
#[test]
fn closure_bodies_survive_gc_stress() {
    let mut envs = THRESHOLDS.to_vec();
    envs.push(("EGCL_LAZY_COMPILE", "0"));
    let (plain, _) = results_and_tiers(&egcl(CLOSURE_PROGRAM, &envs));
    envs.extend([
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
    ]);
    let (stressed, _) = results_and_tiers(&egcl(CLOSURE_PROGRAM, &envs));
    assert_eq!(plain, stressed);
    // A fresh cons stored into a captured binding is in the nursery; the next
    // allocation evacuates it while the native body still holds it in a slot.
    let program = r#"
(progn
  (defun mk (n)
    (let ((cell (list n)))
      (lambda (k)
        (setq cell (cons k cell))
        (let ((before cell)) (make-list 3) (list (eq before cell) (car cell) (length cell))))))
  (defun drive (f n) (let ((r nil)) (dotimes (i n) (setq r (funcall f i))) r))
  (let ((fs (loop for i below 3 collect (mk i))))
    (print (loop for f in fs collect (drive f 30)))
    (terpri)
    (format t "TIERS ~{~a~^ ~}~%" (mapcar #'egcl-ext:function-tier fs))))
"#;
    let (results, tiers) = results_and_tiers(&egcl(program, &envs));
    assert_eq!(results, "((T 29 31) (T 29 31) (T 29 31))");
    if MAPPED_LINUX {
        assert_eq!(tiers, "2 2 2");
    }
}

/// A closure whose loop writes a captured binding enters T2 by OSR with the
/// instance's environment; a fixnum overflow in that loop deoptimizes into the
/// interpreter on the same frame chain. Two instances of the template keep
/// separate totals.
#[test]
fn closure_loop_osr_and_deopt_keep_the_instance_environment() {
    let program = r#"
(progn
  (defun mkacc (base)
    (let ((tot base))
      (list (lambda (n) (dotimes (i n) (setq tot (+ tot 1))) tot) (lambda () tot))))
  (let ((a (mkacc 0)) (b (mkacc (- most-positive-fixnum 100000))))
    (dotimes (i 4) (funcall (first a) 1) (funcall (first b) 0))
    (let ((d0 (egcl-ext:deopt-count)))
      (format t "R1 ~a~%" (funcall (first a) 200000))
      (format t "R2 ~a ~a~%" (funcall (first b) 200000) (funcall (second b)))
      (format t "R3 ~a~%" (funcall (second a)))
      (format t "DEOPTS ~a TIER ~a~%" (- (egcl-ext:deopt-count) d0)
              (egcl-ext:function-tier (first a))))))
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
        ("EGCL_LAZY_COMPILE", "0"),
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
    assert!(oracle.contains("R1 200004"), "{oracle}");
    assert!(oracle.contains("R3 200004"), "{oracle}");
    if MAPPED_LINUX {
        assert!(tiered.contains("TIER 2"), "the loop must OSR into T2: {tiered}");
        assert!(!tiered.contains("DEOPTS 0 "), "the overflow must deopt: {tiered}");
    }
}

/// Ownership of a captured name is decided by name, not position: a function
/// that reads an enclosing captured X and also binds its own captured X is
/// admitted. Its reads before, inside and after the inner binding, and its
/// writes to both, must match the interpreter once it is installed at T2.
#[test]
fn shadowing_captured_names_match_the_interpreter() {
    let program = r#"
(progn
  (let ((x 5))
    (defun shadow-a (k)
      (list x (let ((x (+ k 1))) (funcall (lambda () x))) x
            (let ((x (* k 3))) (setq x (+ x 1)) (let ((g (lambda () x))) (funcall g))) x))
    (defun shadow-set (k)
      (setq x (+ x k))
      (list x (let ((x k)) (let ((h (lambda () (setq x (+ x 1))))) (funcall h) (list x (funcall h))))
            (prog1 x (setq x 5)))))
  (loop for i from 0 below 3000
        do (shadow-a 2) (shadow-set 4)
        until (and (eql (egcl-ext:function-tier 'shadow-a) 2)
                   (eql (egcl-ext:function-tier 'shadow-set) 2)))
  (print (list (shadow-a 7) (shadow-set 9)))
  (terpri)
  (format t "TIERS ~a ~a~%" (egcl-ext:function-tier 'shadow-a) (egcl-ext:function-tier 'shadow-set)))
"#;
    let tiered = egcl(program, THRESHOLDS);
    let (results, tiers) = results_and_tiers(&tiered);
    assert_eq!(results, "((5 8 5 22 5) (14 (10 11) 14))");
    if MAPPED_LINUX {
        assert_eq!(tiers, "2 2", "{}", String::from_utf8_lossy(&tiered.stderr));
    }
    let (oracle, _) = results_and_tiers(&egcl(program, &[("EGCL_FORCE_TIER", "interp")]));
    assert_eq!(results, oracle);
}
