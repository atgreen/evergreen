// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Special bindings in ordinary-ABI T2 frames (bliss-gg3i1.7.4).
//!
//! A function whose only dynamic scopes are special bindings gets the
//! ordinary x86-64 Lisp T2 entry: its bindings live on the execution's native
//! binding stack instead of a mapped activation's ledger. These programs check
//! that such functions really install that entry, and that every way out of a
//! binding restores it: normal return (including a forwarded multiple-value
//! result), an error caught by HANDLER-CASE, THROW, a restart, a direct
//! RETURN-FROM in the caller, nested and recursive bindings, and a numeric
//! deoptimization inside the binding, which hands the binding to the
//! interpreter. Each answer is compared under every forced tier.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [Option<&str>; 5] = [Some("interp"), Some("t0"), Some("t1"), Some("t2"), None];

fn run(tier: Option<&str>, program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "600", BIN, "--no-init", "--eval", program]);
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_FORCE_TIER",
        "EGCL_T2_LOG",
        "EGCL_T2_THRESHOLD",
        "EGCL_DISABLE_ORDINARY_SPECIALS",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        cmd.env_remove(key);
    }
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "tier {tier:?}: program failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    (stdout, stderr)
}

fn result_line(tier: Option<&str>, out: &str) -> String {
    out.lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("tier {tier:?}: no RESULT line in:\n{out}"))
        .to_string()
}

fn installed_ordinary(log: &str, name: &str) -> bool {
    log.lines().any(|line| {
        line.contains(&format!(
            "{name}: optimized worker result installed with ordinary x86-64 Lisp T2 ABI"
        ))
    })
}

/// Definitions shared by the programs below. Every function that binds is
/// called often enough to reach T2.
const DEFINITIONS: &str = r#"
  (defvar *sb-x* 0)
  (defvar *sb-y* :y0)
  (declaim (notinline sb-rd sb-rdy sb-boom sb-values))
  (defun sb-rd () *sb-x*)
  (defun sb-rdy () *sb-y*)
  (defun sb-boom (v) (if (eql v :boom) (error "boom ~a" *sb-x*) v))
  (defun sb-values (a) (values a (1+ a) (+ a 2)))
  (defun sb-bind (v) (let ((*sb-x* v)) (sb-rd)))
  (defun sb-nested (a b) (let ((*sb-x* a)) (let ((*sb-y* b)) (list (sb-rd) (sb-rdy)))))
  (defun sb-shadow (a) (let ((*sb-x* a)) (list (sb-rd) (let ((*sb-x* (1+ a))) (sb-rd)) (sb-rd))))
  (defun sb-forward (a) (let ((*sb-x* a)) (sb-values (sb-rd))))
  (defun sb-after (a) (let ((*sb-x* a)) (sb-rd)) (sb-rd))
  (defun sb-let-call (a) (let ((*sb-x* (sb-values a))) (sb-rd)))
  (defun sb-if (a) (if (let ((*sb-x* a)) (sb-rd)) (list :yes (sb-rd)) :no))
  (defun sb-error (v) (let ((*sb-x* v)) (sb-boom v)))
  (defun sb-throw-inner (v) (let ((*sb-x* v)) (sb-boom v) (throw 'sb-tag (sb-rd))))
  (defun sb-deep (n) (if (= n 0) (sb-rd) (let ((*sb-x* n)) (+ (sb-rd) (sb-deep (1- n))))))
  (defun sb-arith (a) (let ((*sb-x* a)) (let ((*sb-y* (+ a 1))) (list (+ a 1) (sb-rd) (sb-rdy)))))
  (defun sb-round (k)
    (list (sb-bind k) (sb-nested k (* 2 k)) (sb-shadow k)
          (multiple-value-list (sb-forward k)) (sb-after k) (sb-let-call k) (sb-if k)
          (handler-case (sb-error :boom) (error (e) (list :caught *sb-x* (princ-to-string e))))
          (catch 'sb-tag (sb-throw-inner k))
          (block out (handler-bind ((error (lambda (c) (declare (ignore c)) (return-from out (list :escaped *sb-x*)))))
                       (sb-error :boom)))
          (restart-case (let ((*sb-x* :outer)) (sb-error :boom))
            (use-value (v) (list v *sb-x*)))
          (sb-deep 4) (sb-arith k)
          *sb-x* *sb-y*))
  (defun sb-drive (n)
    (handler-bind ((error (lambda (c) (declare (ignore c))
                            (when (find-restart 'use-value) (invoke-restart 'use-value :restarted)))))
      (let ((last nil))
        (dotimes (i n last) (setq last (sb-round 5))))))
"#;

const EXPECTED_ROUND: &str = r#"(5 (5 10) (5 6 5) (5 6 7) 0 5 (:YES 0) (:CAUGHT 0 "boom BOOM") 5 (:ESCAPED :BOOM) (:RESTARTED 0) 11 (6 5 6) 0 :Y0)"#;

/// Every exit from a binding restores it, in every tier, after the binding
/// functions have had time to reach T2 under the default policy. A handler
/// runs inside the binding it escapes from (`:ESCAPED :BOOM`).
#[test]
fn every_exit_restores_special_bindings_in_every_tier() {
    let program = format!(
        r#"(progn {DEFINITIONS}
  (format t "RESULT ~S ~S ~S~%" (sb-drive 400) *sb-x* *sb-y*))"#
    );
    let expected = format!("RESULT {EXPECTED_ROUND} 0 :Y0");
    for tier in TIERS {
        let (out, _) = run(tier, &program, &[("EGCL_T2_THRESHOLD", "50")]);
        assert_eq!(result_line(tier, &out), expected, "tier {tier:?}:\n{out}");
    }
}

/// The binding functions install the ordinary entry, not the mapped one, and
/// run there without deoptimizing once warm.
#[test]
fn binding_functions_install_the_ordinary_entry() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = format!(
        r#"(progn {DEFINITIONS}
  (sb-drive 400)
  (let ((before (egcl-ext:deopt-count)) (r nil))
    (setq r (sb-drive 2000))
    (format t "RESULT ~S DEOPTS ~D TIERS ~S~%" (equal r (sb-drive 1)) (- (egcl-ext:deopt-count) before)
            (mapcar #'egcl-ext:function-tier '(sb-bind sb-nested sb-shadow sb-forward sb-after sb-let-call sb-if sb-deep sb-arith)))))"#
    );
    let (out, log) = run(
        None,
        &program,
        &[("EGCL_T2_LOG", "stderr"), ("EGCL_T2_THRESHOLD", "50")],
    );
    assert_eq!(
        result_line(None, &out),
        "RESULT T DEOPTS 0 TIERS (2 2 2 2 2 2 2 2 2)",
        "{out}\n{log}"
    );
    for name in [
        "SB-BIND",
        "SB-NESTED",
        "SB-SHADOW",
        "SB-FORWARD",
        "SB-AFTER",
        "SB-LET-CALL",
        "SB-IF",
        "SB-ERROR",
        "SB-DEEP",
        "SB-ARITH",
    ] {
        assert!(
            installed_ordinary(&log, name),
            "{name} is not ordinary:\n{log}"
        );
    }
    // Disabling the feature keeps them on the mapped entry with the same answers.
    let (out, log) = run(
        None,
        &program,
        &[
            ("EGCL_T2_LOG", "stderr"),
            ("EGCL_T2_THRESHOLD", "50"),
            ("EGCL_DISABLE_ORDINARY_SPECIALS", "1"),
        ],
    );
    assert!(result_line(None, &out).starts_with("RESULT T "), "{out}");
    assert!(!installed_ordinary(&log, "SB-BIND"), "{log}");
}

/// A speculation failure inside two bindings resumes the interpreter with
/// both bindings in effect, and they are restored when it leaves them.
#[test]
fn deoptimizing_inside_a_binding_keeps_and_then_restores_it() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = format!(
        r#"(progn {DEFINITIONS}
  (dotimes (i 3000) (sb-arith i))
  (let ((before (egcl-ext:deopt-count)))
    (format t "RESULT ~S ~S ~S ~S ~S~%"
            (sb-arith 1.5) (list *sb-x* *sb-y*) (sb-arith 7)
            (handler-case (let ((*sb-x* 1)) (sb-arith :not-a-number)) (error () (list :error *sb-x*)))
            (> (- (egcl-ext:deopt-count) before) 0))))"#
    );
    let (out, log) = run(
        None,
        &program,
        &[("EGCL_T2_LOG", "stderr"), ("EGCL_T2_THRESHOLD", "50")],
    );
    assert_eq!(
        result_line(None, &out),
        "RESULT (2.5 1.5 2.5) (0 :Y0) (8 7 8) (:ERROR 0) T",
        "{out}\n{log}"
    );
    assert!(installed_ordinary(&log, "SB-ARITH"), "{log}");
}

/// The same program under a collection at every allocation, poisoning freed
/// nursery memory: the native binding stack's saved values are roots.
#[test]
fn native_bindings_survive_forced_collections() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = format!(
        r#"(progn {DEFINITIONS}
  (defun sb-cons-bind (v) (let ((*sb-x* (list v v))) (let ((*sb-y* (list (sb-rd) v))) (list (sb-rd) (sb-rdy)))))
  (let ((r nil))
    (dotimes (i 60)
      (setq r (list (sb-cons-bind (list i)) (sb-nested (list i) i) (multiple-value-list (sb-forward i))
                    (catch 'sb-tag (sb-throw-inner (list i))) (sb-deep 3))))
    (format t "RESULT ~S ~S ~S~%" r *sb-x* *sb-y*)))"#
    );
    let expected = |out: &str| result_line(None, out);
    let (plain, _) = run(None, &program, &[("EGCL_T2_THRESHOLD", "20")]);
    let (stressed, log) = run(
        None,
        &program,
        &[
            ("EGCL_T2_THRESHOLD", "20"),
            ("EGCL_T2_LOG", "stderr"),
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ],
    );
    assert_eq!(expected(&stressed), expected(&plain), "{stressed}\n{log}");
    assert!(installed_ordinary(&log, "SB-CONS-BIND"), "{log}");
}
