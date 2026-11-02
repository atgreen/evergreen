// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FUNCALL and closures through the entry word (bliss-gg3i1.7.15.10.3), end
//! to end: every tier computes the same results, a closure template tiers up
//! while its instances are being called, redefinition, non-function values,
//! other threads, and a moving collector under stress. Each scenario also
//! runs with `EGCL_LEAN_FUNCALL=0` and must print the same result.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::path::Path;
use std::process::Command;

const DEFS: &str = r#"
(defun ce-global (x) (+ x 1))
(defun ce-adder (k) (lambda (x) (+ x k)))
(defun ce-reader (a b c) (lambda (x) (+ (+ x a) (+ b c))))
(defun ce-nocap () (lambda (x) (* x 2)))
(defun ce-consing (k) (lambda (x) (list x k (make-list 2 :initial-element k))))
(defun ce-loop (f n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (funcall f i))))))
(defun ce-loop-tail (f n) (if (= n 0) 0 (+ (funcall f n) (ce-loop-tail f (- n 1)))))
(defun ce-map (f l) (mapcar f l))
(defun ce-labels (n k)
  (let ((s 0))
    (labels ((add (x) (if (>= x 0) (+ x k) (add (- x)))))
      (dotimes (i n s) (setq s (+ s (add i)))))))
(defun ce-cons-loop (f n)
  (let ((s 0))
    (dotimes (i n s)
      (let ((r (funcall f (list i))))
        (setq s (+ s (car (first r)) (second r) (reduce #'+ (third r))))))))
(defun ce-all ()
  (list (ce-loop #'ce-global 1000)
        (ce-loop (ce-adder 7) 1000)
        (ce-loop (ce-reader 1 2 3) 1000)
        (ce-loop (ce-nocap) 1000)
        (ce-loop-tail (ce-adder 3) 200)
        (ce-map (ce-adder 10) '(1 2 3))
        (ce-labels 1000 4)
        (ce-cons-loop (ce-consing 5) 300)))
(defun ce-warm (n) (dotimes (i n) (ce-all)))
"#;

const ALL_EXPECTED: &str = "(500500 506500 505500 999000 20700 (11 12 13) 503500 49350)";

fn egcl(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut command = Command::new("timeout");
    command
        .current_dir(dir)
        .args([
            "--kill-after=5",
            "600",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
        ])
        .args(args);
    for name in [
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_T2_THRESHOLD",
        "EGCL_OSR_THRESHOLD",
        "EGCL_FORCE_TIER",
        "EGCL_LEAN_CALLS",
        "EGCL_LEAN_FUNCALL",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
    ] {
        command.env_remove(name);
    }
    command.envs(envs.iter().copied());
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "egcl {args:?} {envs:?} failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

fn scratch(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "egcl-closure-entry-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("defs.lisp"), DEFS).unwrap();
    dir
}

fn line<'a>(out: &'a str, marker: &str) -> &'a str {
    out.lines()
        .find(|line| line.starts_with(marker))
        .unwrap_or_else(|| panic!("no {marker} line: {out}"))
}

/// Run `program` after loading the definitions, the lean FUNCALL on and off,
/// and require the same `marker` line from both.
fn both(label: &str, program: &str, marker: &str, envs: &[(&str, &str)]) -> String {
    let dir = scratch(label);
    let mut lines = Vec::new();
    for lean in ["1", "0"] {
        let mut envs = envs.to_vec();
        envs.push(("EGCL_LEAN_FUNCALL", lean));
        let out = egcl(
            &dir,
            &["--eval", r#"(load "defs.lisp")"#, "--eval", program],
            &envs,
        );
        lines.push(line(&out, marker).to_owned());
    }
    assert_eq!(lines[0], lines[1], "lean and cell FUNCALL agree");
    std::fs::remove_dir_all(&dir).ok();
    lines.swap_remove(0)
}

#[test]
fn every_tier_computes_the_same_results() {
    let program = |warm: u32| format!(r#"(progn (ce-warm {warm}) (format t "ALL ~S~%" (ce-all)))"#);
    let expected = format!("ALL {ALL_EXPECTED}");
    assert_eq!(both("tiers-default", &program(30), "ALL", &[]), expected);
    // The interpreted tiers run each round slowly; they need no warming.
    for (tier, warm) in [("interp", 0), ("t0", 1), ("t1", 5), ("t2", 5)] {
        assert_eq!(
            both(
                &format!("tiers-{tier}"),
                &program(warm),
                "ALL",
                &[("EGCL_FORCE_TIER", tier)]
            ),
            expected,
            "EGCL_FORCE_TIER={tier}"
        );
    }
}

#[test]
fn a_template_tiers_up_while_its_instances_are_called() {
    // Fresh templates, defined after the warm-up, each first called while
    // T0 and promoted while a compiled caller keeps calling its instances.
    let out = both(
        "tier-up",
        r#"(progn
             (eval '(defun ce-fresh (k) (lambda (x) (+ x k k))))
             (let ((fs (loop for k below 8 collect (ce-fresh k)))
                   (s 0))
               (dotimes (round 400)
                 (dolist (f fs) (setq s (+ s (ce-loop f 50))))
                 (when (= round 200)
                   ;; New instances of the now-compiled template join in.
                   (setq fs (append fs (loop for k from 8 below 12 collect (ce-fresh k))))))
               (format t "TIERUP ~D~%" s)))"#,
        "TIERUP",
        &[
            ("EGCL_T0_T1_THRESHOLD", "3"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "20"),
        ],
    );
    // Each ce-loop of (lambda (x) (+ x k k)) over 50 is 1225 + 100k.
    let per = |k: i64| 1225 + 100 * k;
    let first: i64 = (0..8).map(per).sum::<i64>() * 400;
    let later: i64 = (8..12).map(per).sum::<i64>() * 199;
    assert_eq!(out, format!("TIERUP {}", first + later));
}

#[test]
fn redefinition_follows_function_objects_and_names() {
    let out = both(
        "redefine",
        r#"(let ((old (progn (ce-warm 100) #'ce-global)) (s 0) (named 0))
             (dotimes (i 200000)
               (when (= i 100000)
                 (eval '(defun ce-global (x) (- x 1))))
               (setq s (+ s (funcall old i)))
               (setq named (+ named (funcall (fdefinition 'ce-global) i))))
             ;; A template replaced by redefining its creator.
             (let ((before (ce-adder 1)))
               (eval '(defun ce-adder (k) (lambda (x) (- x k))))
               (format t "REDEFINED ~D ~D ~D ~D~%" s named
                       (ce-loop before 100) (ce-loop (ce-adder 1) 100))))"#,
        "REDEFINED",
        &[],
    );
    let s: i64 = (0..200_000i64).map(|i| i + 1).sum();
    let named: i64 = (0..200_000i64)
        .map(|i| if i < 100_000 { i + 1 } else { i - 1 })
        .sum();
    assert_eq!(out, format!("REDEFINED {s} {named} 5050 4850"));
}

#[test]
fn non_functions_signal_the_same_errors() {
    let out = both(
        "non-function",
        r#"(flet ((try (f) (handler-case (ce-loop f 3)
                             (error (c) (format nil "~A: ~A" (type-of c) c)))))
             (format t "NONFN ~A | ~A | ~A | ~A~%"
                     (try 42) (try nil) (try 'ce-undefined-function-name)
                     (try "string")))"#,
        "NONFN",
        &[],
    );
    assert!(out.contains("UNDEFINED-FUNCTION"), "{out}");
}

#[test]
fn other_threads_take_their_own_path() {
    let out = both(
        "threads",
        r#"(let* ((warm (ce-warm 100))
                  (adder (ce-adder 7))
                  (threads (loop repeat 4 collect
                             (egcl-thread:make-thread
                               (lambda ()
                                 (let ((s 0))
                                   (dotimes (k 50 s)
                                     (setq s (+ s (ce-loop adder 2000)
                                                (ce-loop (ce-reader 1 2 3) 100)))))))))
                  (results (mapcar #'egcl-thread:join-thread threads)))
             (declare (ignore warm))
             (format t "THREADS ~{~D~^ ~} ~D~%" results (ce-loop adder 2000)))"#,
        "THREADS",
        &[],
    );
    let adder: i64 = (0..2000i64).map(|i| i + 7).sum();
    let reader: i64 = (0..100i64).map(|i| i + 6).sum();
    let per_thread = (adder + reader) * 50;
    assert_eq!(out, format!("THREADS {0} {0} {0} {0} {adder}", per_thread));
}

#[test]
fn closures_survive_a_moving_collector_under_stress() {
    // Warm the callers and templates up first, also under stress.
    let program = r#"(progn
                       (dotimes (i 200)
                         (ce-cons-loop (ce-consing 5) 3) (ce-loop (ce-reader 1 2 3) 3))
                       (format t "STRESS ~D ~D ~D~%"
                               (ce-cons-loop (ce-consing 5) 50)
                               (ce-loop (ce-reader 1 2 3) 50)
                               (ce-map (ce-consing 2) '(1 2))))"#;
    let reference = both("stress-reference", program, "STRESS", &[]);
    let stressed = both(
        "stress",
        program,
        "STRESS",
        &[
            ("EGCL_GC_STRESS", "7"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ("EGCL_GC_POISON", "1"),
        ],
    );
    assert_eq!(reference, stressed);
}
