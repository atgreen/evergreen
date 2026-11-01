// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Named calls through function-object entry words (bliss-gg3i1.7.15.10.1),
//! end to end: redefinition while a caller loops, calls to things that are
//! not interpreted functions, calls from other threads, a moving collector
//! under stress, and an image save/restore round trip. Each scenario also
//! runs with `EGCL_LEAN_CALLS=0` and must print the same result.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::path::Path;
use std::process::Command;

const DEFS: &str = r#"
(declaim (notinline bp-leaf bp-mid bp-alloc-leaf bp-alloc-mid))
(defun bp-leaf (a b c) (declare (ignore a b)) (+ c 1))
(defun bp-mid (a b c) (+ 1 (bp-leaf a b c)))
(defun bp-loop (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (bp-mid 0 1 i))))))
(defun bp-alloc-leaf (a b c) (declare (ignore b)) (list a c (make-list 3 :initial-element c)))
(defun bp-alloc-mid (a b c) (let ((x (cons a b))) (list (bp-alloc-leaf a b c) x)))
(defun bp-alloc-loop (n)
  (let ((s 0))
    (dotimes (i n s)
      (let ((r (bp-alloc-mid (list i) (list (+ i 1)) i)))
        (setq s (+ s (car (first (second r))) (car (cdr (second r)))
                   (second (first r)) (reduce #'+ (third (first r)))))))))
(dotimes (i 300) (bp-loop 20) (bp-alloc-loop 5))
"#;

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
        "egcl-backpack-{label}-{}-{nonce}",
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

/// Run `program` after loading the definitions, lean calls on and off, and
/// require the same `marker` line from both.
fn both(label: &str, program: &str, marker: &str, envs: &[(&str, &str)]) -> String {
    let dir = scratch(label);
    let mut lines = Vec::new();
    for lean in ["1", "0"] {
        let mut envs = envs.to_vec();
        envs.push(("EGCL_LEAN_CALLS", lean));
        let out = egcl(&dir, &["--eval", r#"(load "defs.lisp")"#, "--eval", program], &envs);
        lines.push(line(&out, marker).to_owned());
    }
    assert_eq!(lines[0], lines[1], "lean and descriptor calls agree");
    std::fs::remove_dir_all(&dir).ok();
    lines.swap_remove(0)
}

#[test]
fn redefinition_while_the_caller_loops_takes_effect() {
    let out = both(
        "redefine",
        r#"(let ((s 0))
             (dotimes (i 200000)
               (when (= i 100000)
                 (eval '(defun bp-leaf (a b c) (declare (ignore a b)) (- c 1))))
               (setq s (+ s (bp-mid 0 1 i))))
             (format t "REDEFINED ~D ~D~%" s (bp-mid 0 1 10)))"#,
        "REDEFINED",
        &[],
    );
    // i+2 before the redefinition, i after it.
    let expected: i64 = (0..200_000i64)
        .map(|i| if i < 100_000 { i + 2 } else { i })
        .sum();
    assert_eq!(out, format!("REDEFINED {expected} 10"));
}

#[test]
fn calls_to_non_functions_signal_or_dispatch_as_before() {
    let out = both(
        "non-function",
        r#"(progn
             (fmakunbound 'bp-leaf)
             (let ((name (handler-case (bp-mid 0 1 2)
                           (undefined-function (c) (cell-error-name c)))))
               (defgeneric bp-leaf (a b c))
               (defmethod bp-leaf (a b (c integer)) (declare (ignore a b)) (* c 10))
               (format t "NONFN ~A ~D ~D~%" name (bp-mid 0 1 4) (bp-loop 100))))"#,
        "NONFN",
        &[],
    );
    assert_eq!(out, "NONFN BP-LEAF 41 49600");
}

#[test]
fn other_threads_call_through_their_own_path() {
    let out = both(
        "threads",
        r#"(let* ((threads (loop repeat 4 collect
                             (egcl-thread:make-thread
                               (lambda () (let ((s 0)) (dotimes (k 50 s) (setq s (+ s (bp-loop 2000)))))))))
                  (results (mapcar #'egcl-thread:join-thread threads)))
             (format t "THREADS ~{~D~^ ~} ~D~%" results (bp-loop 2000)))"#,
        "THREADS",
        &[],
    );
    let per_loop: i64 = (0..2000i64).map(|i| i + 2).sum();
    assert_eq!(
        out,
        format!("THREADS {0} {0} {0} {0} {per_loop}", per_loop * 50)
    );
}

#[test]
fn lean_calls_survive_a_moving_collector_under_stress() {
    let program = r#"(format t "STRESS ~D ~D~%" (bp-alloc-loop 400) (bp-loop 400))"#;
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

#[test]
fn an_image_round_trip_rederives_entry_words() {
    let dir = scratch("image");
    let out = egcl(
        &dir,
        &[
            "--eval",
            r#"(load "defs.lisp")"#,
            "--eval",
            r#"(progn (format t "SAVED ~D~%" (bp-loop 1000))
                      (egcl-ext:save-lisp-and-die "bp.core"))"#,
        ],
        &[],
    );
    let saved = line(&out, "SAVED").to_owned();
    for lean in ["1", "0"] {
        let out = egcl(
            &dir,
            &[
                "--image",
                "bp.core",
                "--eval",
                r#"(progn (dotimes (i 300) (bp-loop 20) (bp-alloc-loop 5))
                          (format t "SAVED ~D~%" (bp-loop 1000))
                          (format t "ALLOC ~D~%" (bp-alloc-loop 50)))"#,
            ],
            &[("EGCL_LEAN_CALLS", lean)],
        );
        assert_eq!(line(&out, "SAVED"), saved, "restored result");
        let expected: i64 = (0..50i64).map(|i| i + (i + 1) + i + 3 * i).sum();
        assert_eq!(line(&out, "ALLOC"), format!("ALLOC {expected}"));
    }
    std::fs::remove_dir_all(&dir).ok();
}
