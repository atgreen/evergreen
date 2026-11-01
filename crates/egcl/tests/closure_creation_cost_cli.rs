// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Closure creation and calls keep their answers at every forced tier and
//! under collection stress (bliss-gg3i1.7.12).
//!
//! The changes this guards make each closure instance cheaper in three ways,
//! and each one has its own way of going wrong:
//!
//!   * **A capturing closure no longer snapshots its creator's block/tag
//!     scope unless its body exits to one.** A closure that does exit by
//!     RETURN-FROM or GO must still find the creator's block or tag after it
//!     escapes into a callee (`find-first`, `tagbody-exit`, `nested-return`).
//!   * **A fresh private closure identity is published with two insertions**
//!     instead of a redefinition's generation check and removals. Every fresh
//!     instance must still dispatch to its own body and its own captures, so
//!     the program calls many instances of one template and of several
//!     templates, interleaved, and checks each answer.
//!   * **A minor collection no longer traces the captures of interpreted
//!     functions it is not otherwise required to visit.** The remembered
//!     capture logs must cover every frame written in the epoch: the program
//!     writes young objects into frames captured by closures that have
//!     already survived collections, then collects and reads them back.
//!
//! Each program prints its answers. The answers at every forced tier must
//! match the default run, and a run with a collection on every allocation
//! (poisoning freed nursery memory) must print exactly what the plain run
//! printed: a stale capture reads poison or the wrong value rather than
//! failing loudly.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str, tier: Option<&str>, stress: bool) -> Output {
    let mut command = Command::new(BIN);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        command.env_remove(key);
    }
    if let Some(tier) = tier {
        command.env("EGCL_FORCE_TIER", tier);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    assert!(
        output.status.success(),
        "tier={tier:?} stress={stress} status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// The `RESULT` lines a run printed, in order.
fn results(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("RESULT "))
        .map(str::to_owned)
        .collect()
}

/// Definitions shared by the programs below. `*n*` sets the iteration count,
/// so the stress run can use a count it finishes in reasonable time.
const DEFINITIONS: &str = r#"
(defun make-adder (x) (lambda (y) (+ x y)))
(defun make-counter (start) (let ((n start)) (lambda (k) (setq n (+ n k)) n)))
(defun make-pair (v)
  (let ((cell v))
    (cons (lambda () cell) (lambda (new) (setq cell new)))))
(defun create-and-call (x) (let ((f (lambda (y) (* x y)))) (funcall f 3)))
(defun immediate (x) (funcall (lambda (y) (list x y)) 7))
(defun labels-sum (x n)
  (labels ((walk (k acc) (if (= k 0) acc (walk (1- k) (+ acc x)))))
    (walk n 0)))
(defun flet-sum (x)
  (flet ((g (y) (list x y)) (h (y) (cons y x)))
    (list (g 1) (h 2))))
;; Closures that exit to their creator's block or tag from inside a callee.
(defun find-first (pred list)
  (block found
    (mapc (lambda (e) (when (funcall pred e) (return-from found e))) list)
    :none))
(defun tagbody-exit (limit)
  (let ((seen nil))
    (tagbody
       (mapc (lambda (e) (when (> e limit) (setq seen e) (go done))) '(1 2 3 4 5 6))
       (setq seen :none)
     done)
    seen))
(defun nested-return (x)
  (block outer
    (let ((f (lambda () (return-from outer (* x 10)))))
      (funcall f)
      :not-reached)))
(defun garbage (n)
  (let ((junk nil)) (dotimes (i n) (push (list i (* i i)) junk)) (length junk)))
"#;

fn program(n: usize) -> String {
    format!(
        r#"
{DEFINITIONS}
(defparameter *n* {n})
(let ((adders nil) (sum 0))
  ;; Many instances of one template, kept, then all called.
  (dotimes (i *n*) (push (make-adder i) adders))
  (dolist (f adders) (setq sum (+ sum (funcall f 1))))
  (format t "RESULT adders ~d ~d~%" (length adders) sum))
(let ((sum 0))
  ;; Fresh instances created and called once, from several templates.
  (dotimes (i *n*)
    (setq sum (+ sum (create-and-call i) (second (immediate i))
                 (labels-sum i 3) (length (flet-sum i)))))
  (format t "RESULT fresh ~d~%" sum))
(let ((counters (loop for i below 8 collect (make-counter (* i 100)))))
  ;; Captured frames written over and over, interleaved with garbage that
  ;; forces collections between the writes and the reads.
  (dotimes (round 5)
    (garbage (* *n* 10))
    (loop for c in counters for k from 1 do (funcall c k)))
  (format t "RESULT counters ~s~%" (mapcar (lambda (c) (funcall c 0)) counters)))
(let ((pairs (loop for i below 6 collect (make-pair (list :initial i)))))
  ;; The closures survive collections, THEN their shared frame receives a
  ;; young list; collections follow before the reader sees it.
  (garbage (* *n* 20))
  (loop for p in pairs for i from 0
        do (funcall (cdr p) (list :written i (make-string 3 :initial-element #\x))))
  (garbage (* *n* 20))
  (format t "RESULT pairs ~s~%" (mapcar (lambda (p) (funcall (car p))) pairs)))
(format t "RESULT exits ~s ~s ~s ~s ~s~%"
        (find-first #'evenp '(1 3 4 5)) (find-first #'zerop '(1 2))
        (tagbody-exit 3) (tagbody-exit 9) (nested-return 4))
(let ((hits 0))
  ;; Exits from many closure instances, created in a loop.
  (dotimes (i *n*)
    (when (eql (find-first (lambda (e) (> e (mod i 4))) '(0 1 2 3 4)) (1+ (mod i 4)))
      (incf hits)))
  (format t "RESULT exit-loop ~d~%" hits))
"#
    )
}

#[test]
fn closure_answers_agree_at_every_tier() {
    let text = program(3000);
    let expected = results(&run(&text, None, false));
    assert_eq!(expected.len(), 6, "every section printed: {expected:?}");
    // 3000 adders over 0..2999, each called with 1.
    assert_eq!(expected[0], "RESULT adders 3000 4501500");
    // Counter I starts at 100*I and is bumped by I+1 in each of 5 rounds.
    assert_eq!(
        expected[2],
        "RESULT counters (5 110 215 320 425 530 635 740)"
    );
    assert!(
        expected[3].starts_with("RESULT pairs ((:WRITTEN 0 \"xxx\") (:WRITTEN 1 \"xxx\")"),
        "{}",
        expected[3]
    );
    assert_eq!(expected[4], "RESULT exits 4 :NONE 4 :NONE 40");
    assert_eq!(expected[5], "RESULT exit-loop 3000");
    for tier in ["interp", "t0", "t1", "t2"] {
        assert_eq!(
            results(&run(&text, Some(tier), false)),
            expected,
            "tier={tier}"
        );
    }
}

#[test]
fn closure_answers_survive_collection_stress() {
    // Every allocation collects here, so keep the counts small; the plain run
    // with the same counts is the reference.
    let text = program(40);
    let expected = results(&run(&text, None, false));
    assert_eq!(expected.len(), 6, "every section printed: {expected:?}");
    let stressed = run(&text, None, true);
    assert!(
        String::from_utf8_lossy(&stressed.stderr)
            .contains("[gc-stress] EGCL_GC_STRESS_AFTER_INIT:"),
        "the stress run must actually stress the program"
    );
    assert_eq!(results(&stressed), expected);
}
