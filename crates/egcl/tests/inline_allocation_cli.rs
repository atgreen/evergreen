// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Optimized code allocates conses and double-float boxes inline, bumping the
//! thread's TLAB without calling the runtime (bliss-8yo5p). These programs
//! warm their functions into T2 and then allocate through enough TLABs and
//! collections that a value held in a compiled frame across inline
//! allocations is moved by the collector; every answer must be the one the
//! runtime allocation gives.

use std::process::Command;

fn run(program: &str, env: &[(&str, &str)]) -> String {
    let mut command = Command::new("timeout");
    command
        .args(["--kill-after=10", "900", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_INLINE_ALLOC")
        .env("EGCL_HEAP_SIZE", "64m");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "egcl failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // --eval prints the form's value, NIL here, after what it wrote.
    stdout.strip_suffix("NIL\n").unwrap_or(&stdout).to_owned()
}

const BUILDERS: &str = r#"
(defun build-ordinary (n)
  (let ((acc nil) (x (list :a :b)))
    (dotimes (i n) (setq acc (cons (cons i x) acc)))
    acc))
(defun build-mapped (n)
  (let ((out nil))
    (dotimes (i n)
      (let ((l nil))
        (dotimes (j 5) (push (list* i j (list j i)) l))
        (setq out (cons l out))))
    out))
(defun build-strings (n)
  (let ((acc nil))
    (dotimes (i n)
      (let ((s (format nil "~d" i)))
        (setq acc (cons (list* i s (list i (1+ i))) acc))))
    acc))
(defun build-wide (n)
  (let ((acc nil))
    (dotimes (i n)
      (setq acc (list* (list i 1 2 3 4 5 6 i) (list* i 1 2 3 4 5 6 7 i) acc)))
    acc))
(defun build-doubles (n)
  (let ((x 0d0) (acc nil))
    (dotimes (i n) (setq x (* (+ x 1d0) 1d0)) (setq acc (cons x acc)))
    acc))
(defun sum-tree (x)
  (let ((sum 0))
    (loop
      (cond ((consp x) (setq sum (+ sum (sum-tree (car x)))) (setq x (cdr x)))
            (t (return (+ sum (cond ((null x) 0)
                                    ((integerp x) x)
                                    ((floatp x) (round x))
                                    ((stringp x) (length x))
                                    ((symbolp x) 1)
                                    (t 0)))))))))
(defun mv-progn (x) (multiple-value-list (progn (floor x 2) (cons x x))))
(defun mv-tail (x) (floor x 2) (cons x x))
(defun mv-list (x) (floor x 3) (list x x x))
"#;

const ALL_BUILDERS: &str = "build-ordinary build-mapped build-strings build-wide build-doubles";

fn checksums(n: u32, env: &[(&str, &str)]) -> String {
    checksums_of(ALL_BUILDERS, n, env)
}

fn checksums_of(builders: &str, n: u32, env: &[(&str, &str)]) -> String {
    let program = format!(
        "(progn {BUILDERS}
           (dolist (f '({builders}))
             (dotimes (k 40) (funcall f 20))
             (format t \"~a ~d~%\" f (sum-tree (funcall f {n}))))
           (dotimes (k 40) (mv-progn k) (mv-tail k) (mv-list k))
           (format t \"MV ~s ~s ~s~%\" (mv-progn 7) (multiple-value-list (mv-tail 7))
                   (multiple-value-list (mv-list 7))))"
    );
    run(&program, env)
}

fn expected(n: u64) -> String {
    expected_of(ALL_BUILDERS, n)
}

fn expected_of(builders: &str, n: u64) -> String {
    let ordinary = n * (n - 1) / 2 + 2 * n;
    let mapped = (0..n)
        .map(|i| (0..5).map(|j| 2 * (i + j)).sum::<u64>())
        .sum::<u64>();
    let strings = (0..n)
        .map(|i| i.to_string().len() as u64 + 3 * i + 1)
        .sum::<u64>();
    let wide = (0..n).map(|i| (2 * i + 21) + (2 * i + 28)).sum::<u64>();
    let doubles = n * (n + 1) / 2;
    let mut out = String::new();
    for (name, sum) in [
        ("BUILD-ORDINARY", ordinary),
        ("BUILD-MAPPED", mapped),
        ("BUILD-STRINGS", strings),
        ("BUILD-WIDE", wide),
        ("BUILD-DOUBLES", doubles),
    ] {
        if builders
            .to_uppercase()
            .split(' ')
            .any(|builder| builder == name)
        {
            out += &format!("{name} {sum}\n");
        }
    }
    out + "MV ((7 . 7)) ((7 . 7)) ((7 7 7))\n"
}

#[test]
fn inline_lists_and_boxes_survive_collections() {
    // A 16 MiB nursery: these sizes collect many times, with verification
    // and poisoned from-space.
    let n = 60_000;
    let out = checksums(n, &[("EGCL_GC_VERIFY", "1"), ("EGCL_GC_POISON", "1")]);
    assert_eq!(out, expected(u64::from(n)));
}

#[test]
fn inline_allocation_matches_runtime_allocation() {
    let n = 5_000;
    assert_eq!(
        checksums(n, &[("EGCL_INLINE_ALLOC", "0")]),
        expected(u64::from(n))
    );
}

#[test]
#[ignore = "slow (~4 minutes): compiling under EGCL_GC_STRESS collects on every allocation"]
fn inline_allocation_under_gc_stress() {
    // Stress keeps the inline window to a few objects, so the collecting
    // fallback runs every few conses with inline-allocated values live.
    // Compiling under stress dominates the run, so this takes fewer shapes.
    let builders = "build-ordinary build-mapped build-doubles";
    let n = 200;
    let out = checksums_of(
        builders,
        n,
        &[
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ("EGCL_GC_POISON", "1"),
        ],
    );
    assert_eq!(out, expected_of(builders, u64::from(n)));
}

/// A cons allocated inline and held in a T2 frame is moved by a later
/// collection and comes back intact.
#[test]
fn a_frame_value_allocated_inline_is_relocated() {
    let out = run(
        r#"(progn
             (defun addr (x) (egcl-internal::%object-address x))
             (defvar *seen* nil)
             (defun build (n)
               (let* ((first (cons :first (list 1 2 3))) (a0 (addr first)) (acc nil))
                 (dotimes (i n) (setq acc (cons (list* i first i) acc)))
                 (setq *seen* (list a0 (addr first)))
                 (list first (length acc) (car (last acc)))))
             (dotimes (k 40) (build 10))
             (format t "~s ~a~%" (build 300000) (/= (first *seen*) (second *seen*))))"#,
        &[("EGCL_GC_POISON", "1")],
    );
    assert_eq!(out, "((:FIRST 1 2 3) 300000 (0 (:FIRST 1 2 3) . 0)) T\n");
}

#[test]
fn threads_allocate_inline_concurrently() {
    let out = run(
        r#"(progn
             (defun build (n tag)
               (let ((acc nil))
                 (dotimes (i n) (setq acc (cons (list* i tag (list i i)) acc)))
                 acc))
             (defun checksum (l)
               (let ((sum 0) (k (1- (length l))))
                 (dolist (e l)
                   (if (and (eql (first e) k) (equal (cddr e) (list k k)))
                       (setq sum (+ sum (first e)))
                       (setq sum -1000000000))
                   (setq k (1- k)))
                 sum))
             (defun worker (tag rounds n)
               (let ((total 0))
                 (dotimes (r rounds) (setq total (+ total (checksum (build n tag)))))
                 total))
             (dotimes (k 40) (worker :warm 1 50))
             (let ((threads (loop for tag in '(:a :b :c :d)
                                  collect (let ((tag tag))
                                            (egcl-thread:make-thread
                                             (lambda () (worker tag 10 20000)))))))
               (format t "~s~%" (mapcar #'egcl-thread:join-thread threads))))"#,
        &[("EGCL_GC_VERIFY", "1")],
    );
    let each = 10 * (20_000u64 * 19_999 / 2);
    assert_eq!(out, format!("({each} {each} {each} {each})\n"));
}
