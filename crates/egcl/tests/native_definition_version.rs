// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A native activation keeps its definition through redefinition and deopt.
use std::process::Command;

fn check_deopt_cleanup_roots(tier: &str, tier_number: u8) {
    // Per-allocation stress plus heap verification took about 372 seconds in
    // the CI release profile (bliss-q2enn) and overran this budget on s390x
    // (bliss-v2b6o): nearly all of it was the ~100,000 bootstrap allocations,
    // each paying a verified minor collection before the probe even started.
    // EGCL_GC_STRESS_AFTER_INIT starts the stress at the first user form, which
    // is what this test is probing; stress, poison, verification and every
    // result/path assertion are unchanged.
    let timeout_seconds = "900";
    let program = format!(
        r#"
      (defun cleanup-root-helper (x)
        (unwind-protect (list 41 42)
          (list x 91 92 93 94)))
      (defun cleanup-root-caller (x)
        (cleanup-root-helper (+ x 1)))
      (dotimes (i 10)
        (assert (equal (cleanup-root-caller 1) '(41 42))))
      (assert (= (egcl-ext:function-tier 'cleanup-root-caller) {tier_number}))
      (disassemble 'cleanup-root-caller)
      (let* ((before (egcl-ext:deopt-count))
             (answer (cleanup-root-caller (expt 2 61))))
        (format t "ANSWER=~s DEOPTS=~a~%"
                answer (- (egcl-ext:deopt-count) before))
        (assert (> (egcl-ext:deopt-count) before))
        (assert (equal answer '(41 42))))
      (format t "DEOPT-CLEANUP-ROOT-OK~%")
    "#
    );
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            timeout_seconds,
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            &program,
        ])
        .env_remove("EGCL_BACKEND")
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_GC_STRESS", "1")
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .env("EGCL_GC_VERIFY", "1")
        .env("EGCL_DEOPT_PATH_DBG", "1")
        .output()
        .expect("run deopt cleanup rooting regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "deopt cleanup probe tier={tier}, status={}, timeout={timeout_seconds}s (124 means timeout):\n{stdout}\n{stderr}",
        output.status,
    );
    assert!(
        stdout.contains("DEOPT-CLEANUP-ROOT-OK"),
        "{stdout}\n{stderr}"
    );
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        // Ordinary guard recovery has its own entry, without the checked ABI's
        // Single/Inlined diagnostic. Tier, real guard failure, exact values and
        // stressed cleanup roots remain mandatory above.
        assert!(
            stdout.contains("Ordinary x86-64 Lisp ABI"),
            "{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("native precise deopt veneer"),
            "{stdout}\n{stderr}"
        );
    } else {
        let path = if tier == "t1" { "Single" } else { "Inlined" };
        assert!(
            stderr.contains(&format!("[deopt-path] {path}")),
            "must exercise the intended resume path: {stderr}"
        );
    }
}

#[test]
fn t1_deopt_roots_cleanup_values_during_collection() {
    check_deopt_cleanup_roots("t1", 1);
}

#[test]
fn t2_deopt_roots_cleanup_values_during_collection() {
    check_deopt_cleanup_roots("t2", 2);
}

fn check_deopt_version(unbind: bool, tier: &str, tier_number: u8) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let replacement = std::env::temp_dir().join(format!(
        "egcl-deopt-version-{}-{nonce}.lisp",
        std::process::id()
    ));
    std::fs::write(
        &replacement,
        "(defun deopt-version (x)
           (let ((value (+ (replace-deopt-version-once x) 7)))
             (+ value 29)))",
    )
    .unwrap();
    let mutation = if unbind {
        "(fmakunbound 'deopt-version)".to_owned()
    } else {
        format!("(load {replacement:?})")
    };
    let after = if unbind {
        "(assert (not (fboundp 'deopt-version)))"
    } else {
        "(assert (= (deopt-version input) (+ input 36)))"
    };
    let program = format!(
        r#"
      (defvar *replace-deopt-version* nil)
      (defun replace-deopt-version-once (x)
        (when *replace-deopt-version*
          (setq *replace-deopt-version* nil)
          {mutation})
        x)
      (defun deopt-version (x)
        (let ((value (+ (replace-deopt-version-once x) 1)))
          (+ value 11)))
      (dotimes (i 100) (assert (= (deopt-version 1) 13)))
      (assert (= (egcl-ext:function-tier 'deopt-version) {tier_number}))
      (setq *replace-deopt-version* t)
      (let* ((input (expt 2 61))
             (before (egcl-ext:deopt-count))
             (answer (deopt-version input)))
        (format t "ANSWER=~a EXPECTED=~a DEOPTS=~a~%"
                answer (+ input 12) (- (egcl-ext:deopt-count) before))
        (assert (= answer (+ input 12)))
        (assert (> (egcl-ext:deopt-count) before))
        {after})
      (format t "DEOPT-VERSION-OK~%")
    "#
    );
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "60",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            &program,
        ])
        .env_remove("EGCL_BACKEND")
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .expect("run native deopt version regression");
    std::fs::remove_file(replacement).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("DEOPT-VERSION-OK"), "{stdout}\n{stderr}");
}

#[test]
fn t1_deopt_resumes_original_definition_after_redefinition() {
    check_deopt_version(false, "t1", 1);
}

#[test]
fn t1_deopt_resumes_original_definition_after_unbinding() {
    check_deopt_version(true, "t1", 1);
}

#[test]
fn t2_deopt_resumes_original_definition_after_redefinition() {
    check_deopt_version(false, "t2", 2);
}

#[test]
fn t2_deopt_resumes_original_definition_after_unbinding() {
    check_deopt_version(true, "t2", 2);
}

#[test]
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
fn t2_deopt_retains_original_inlined_frames() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let replacement = std::env::temp_dir().join(format!(
        "egcl-inline-deopt-version-{}-{nonce}.lisp",
        std::process::id()
    ));
    std::fs::write(
        &replacement,
        r#"
      (defun deopt-inline-leaf (s)
        (progn (uiop/utility:first-char s) :replacement))
      (defun deopt-inline-caller (s)
        (if (deopt-inline-leaf (replace-deopt-inline-once s)) 211 212))
    "#,
    )
    .unwrap();
    let program = format!(
        r#"
      (defpackage :uiop/utility (:use :cl) (:export :first-char))
      (in-package :uiop/utility)
      (defun first-char (s)
        (and (stringp s) (plusp (length s)) (char s 0)))
      (in-package :cl-user)
      (defvar *replace-deopt-inline* nil)
      (defun replace-deopt-inline-once (s)
        (when *replace-deopt-inline*
          (setq *replace-deopt-inline* nil)
          (load {replacement:?}))
        s)
      (defun deopt-inline-leaf (s) (uiop/utility:first-char s))
      (defun deopt-inline-caller (s)
        (if (deopt-inline-leaf (replace-deopt-inline-once s)) 111 112))
      (dotimes (i 100) (assert (= (deopt-inline-caller "warm") 111)))
      (assert (= (egcl-ext:function-tier 'deopt-inline-caller) 2))
      (setq *replace-deopt-inline* t)
      (let* ((before (egcl-ext:deopt-count))
             (answer (deopt-inline-caller "")))
        (format t "ANSWER=~a EXPECTED=112 DEOPTS=~a~%"
                answer (- (egcl-ext:deopt-count) before))
        (assert (= answer 112))
        (assert (> (egcl-ext:deopt-count) before))
        (assert (= (deopt-inline-caller "") 211)))
      (format t "INLINE-DEOPT-VERSION-OK~%")
    "#
    );
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "60",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            &program,
        ])
        .env_remove("EGCL_BACKEND")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_DEOPT_PATH_DBG", "1")
        .output()
        .expect("run inlined frame version regression");
    std::fs::remove_file(replacement).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("INLINE-DEOPT-VERSION-OK"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stderr
            .lines()
            .filter(|line| line.starts_with("[deopt-path] Inlined"))
            .filter_map(|line| line.split("scopes=").nth(1))
            .filter_map(|count| count.parse::<usize>().ok())
            .any(|count| count >= 2),
        "test must recover multiple real inlined frames, not a single native call: {stderr}"
    );
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn t2_deopt_retains_both_active_mapped_definitions() {
    // Mapped admission currently supports one logical frame per native entry.
    // Enter both real native definitions before replacing them; unlike mutation
    // in a named call's argument, this unambiguously requires both old bodies.
    let program = r#"(declaim (notinline version-child version-mutate))
(defvar *version-replace* nil)
(defvar *version-effects* 0)
(defun version-mutate (x)
  (when *version-replace*
    (setq *version-replace* nil)
    (incf *version-effects*)
    (setf (symbol-function 'version-child) (lambda (x) (+ x 7)))
    (setf (symbol-function 'version-parent) (lambda (x) (+ (version-child x) 29)))
    (egcl-ext:gc))
  x)
(defun version-child (x) (+ (version-mutate x) 1))
(defun version-parent (x) (+ (version-child x) 11))
(dotimes (i 100) (assert (= (version-parent 1) 13)))
(assert (= (egcl-ext:function-tier 'version-parent) 2))
(assert (= (egcl-ext:function-tier 'version-child) 2))
(setq *version-replace* t)
(let* ((input (expt 2 61)) (before (egcl-ext:deopt-count)) (answer (version-parent input)))
  (format t "ANSWER ~A EXPECTED ~A DEOPTS ~A EFFECTS ~A~%" answer (+ input 12) (- (egcl-ext:deopt-count) before) *version-effects*)
  (assert (= answer (+ input 12)))
  (assert (= (- (egcl-ext:deopt-count) before) 2))
  (assert (= *version-effects* 1))
  (assert (= (version-parent input) (+ input 36))))

(format t "NESTED-DEOPT-VERSION-OK~%")
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_BACKEND")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .expect("run mapped frame version regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("NESTED-DEOPT-VERSION-OK"),
        "{stdout}\n{stderr}"
    );
}
