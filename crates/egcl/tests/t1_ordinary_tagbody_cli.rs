// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T1 ordinary entries for loop bodies (bliss-gg3i1.7.15.10.7).
//!
//! A body that is scope-free apart from local TAGBODYs (every DOTIMES,
//! DOLIST, DO and LOOP) and primary-value binders (INCF, PUSH) used to be a
//! mapped-only T1 body until T2 promotion, which a function called fewer
//! than 4096 times never reaches. T1 now gives it an ordinary entry, as T2
//! does. These programs keep such functions at T1, check they install the
//! ordinary entry, and compare every answer under each forced tier and with
//! the feature off: plain returns, errors and throws leaving a loop,
//! mapped callers, a speculation failure inside the loop, and forced
//! collections.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [Option<&str>; 5] = [Some("interp"), Some("t0"), Some("t1"), Some("t2"), None];

/// Keep everything at T1 under the default policy.
const STAY_T1: [(&str, &str); 2] = [
    ("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000"),
    ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "2000000000"),
];

fn run(tier: Option<&str>, program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "600", BIN, "--no-init", "--eval", program]);
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_FORCE_TIER",
        "EGCL_T2_LOG",
        "EGCL_T2_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_T1_ORDINARY_TAGBODY",
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

fn t1_abi<'a>(log: &'a str, name: &str) -> Option<&'a str> {
    log.lines().rev().find_map(|line| {
        line.strip_prefix(&format!("[T2] {name}: T1 installed with "))
            .and_then(|rest| rest.strip_suffix(" ABI"))
    })
}

const DEFINITIONS: &str = r#"
  (declaim (notinline tl-count tl-sum tl-collect tl-find tl-boom tl-thrower tl-each-boom
                      tl-each-throw tl-guarded tl-arith tl-do))
  (defun tl-count (l) (let ((n 0)) (dolist (x l n) (when x (incf n)))))
  (defun tl-sum (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s i)))))
  (defun tl-collect (l) (let ((r nil)) (dolist (x l (nreverse r)) (push (list x) r))))
  (defun tl-find (x l) (loop for y in l for i from 0 when (eql x y) return i))
  (defun tl-do (n) (do ((i 0 (1+ i)) (acc nil (cons i acc))) ((>= i n) acc)))
  (defun tl-boom (x) (if (eql x :boom) (error "boom ~a" x) x))
  (defun tl-thrower (x) (if (eql x :throw) (throw 'tl-tag (list :thrown x)) x))
  (defun tl-each-boom (l) (let ((n 0)) (dolist (x l n) (tl-boom x) (incf n))))
  (defun tl-each-throw (l) (let ((n 0)) (dolist (x l n) (tl-thrower x) (incf n))))
  (defun tl-guarded (l) (handler-case (list :ok (tl-each-boom l)) (error (e) (list :caught (princ-to-string e)))))
  (defun tl-arith (l) (let ((s 0)) (dolist (x l s) (setq s (+ s x)))))
  (defun tl-round (k)
    (list (tl-count (list 1 nil k)) (tl-sum k) (tl-collect (list k :a)) (tl-find :a (list k :a))
          (tl-do 4)
          (tl-guarded (list 1 2 3)) (tl-guarded (list 1 :boom 3))
          (handler-case (tl-each-boom (list :boom)) (error () :outer))
          (catch 'tl-tag (tl-each-throw (list 1 :throw 3))) (tl-each-throw (list 1 2))
          (tl-arith (list 1 2 k))))
  (defun tl-drive (n) (let ((last nil)) (dotimes (i n last) (setq last (tl-round 5)))))
"#;

const EXPECTED_ROUND: &str =
    r#"(2 10 ((5) (:A)) 1 (3 2 1 0) (:OK 3) (:CAUGHT "boom BOOM") :OUTER (:THROWN :THROW) 2 8)"#;

#[test]
fn loop_bodies_agree_in_every_tier() {
    let program = format!("(progn {DEFINITIONS} (format t \"RESULT ~S~%\" (tl-drive 300)))");
    let expected = format!("RESULT {EXPECTED_ROUND}");
    for tier in TIERS {
        let (out, _) = run(tier, &program, &STAY_T1);
        assert_eq!(result_line(tier, &out), expected, "tier {tier:?}:\n{out}");
    }
    let mut off = STAY_T1.to_vec();
    off.push(("EGCL_T1_ORDINARY_TAGBODY", "0"));
    let (out, _) = run(None, &program, &off);
    assert_eq!(result_line(None, &out), expected, "feature off:\n{out}");
}

#[test]
fn loop_bodies_install_the_ordinary_t1_entry() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = format!(
        "(progn {DEFINITIONS} (tl-drive 300) (sleep 0.2) (format t \"RESULT ~S ~S~%\" (tl-drive 300) (mapcar #'egcl-ext:function-tier '(tl-count tl-sum tl-collect tl-each-boom))))"
    );
    let mut envs = STAY_T1.to_vec();
    envs.push(("EGCL_T2_LOG", "stderr"));
    let (out, log) = run(None, &program, &envs);
    assert_eq!(
        result_line(None, &out),
        format!("RESULT {EXPECTED_ROUND} (1 1 1 1)"),
        "{out}\n{log}"
    );
    for name in [
        "TL-COUNT",
        "TL-SUM",
        "TL-COLLECT",
        "TL-EACH-BOOM",
        "TL-EACH-THROW",
        "TL-ARITH",
    ] {
        assert_eq!(
            t1_abi(&log, name),
            Some("ordinary x86-64 Lisp"),
            "{name}:\n{log}"
        );
    }
    let mut off = envs.clone();
    off.push(("EGCL_T1_ORDINARY_TAGBODY", "0"));
    let (_, log) = run(None, &program, &off);
    assert_eq!(t1_abi(&log, "TL-COUNT"), Some("mapped"), "{log}");
}

/// A fixnum speculation in a T1 ordinary loop fails on a float and resumes
/// the interpreter mid-loop without replaying completed iterations.
#[test]
fn deoptimizing_inside_a_t1_ordinary_loop() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = format!(
        "(progn {DEFINITIONS}
  (dotimes (i 300) (tl-arith (list 1 2 i)))
  (sleep 0.2)
  (dotimes (i 300) (tl-arith (list 1 2 i)))
  (let ((before (egcl-ext:deopt-count)))
    (format t \"RESULT ~S ~S ~S~%\" (tl-arith (list 1 2.5 3)) (tl-arith (list 4 5))
            (> (- (egcl-ext:deopt-count) before) 0))))"
    );
    let mut envs = STAY_T1.to_vec();
    envs.push(("EGCL_T2_LOG", "stderr"));
    let (out, log) = run(None, &program, &envs);
    assert_eq!(result_line(None, &out), "RESULT 6.5 9 T", "{out}\n{log}");
    assert_eq!(
        t1_abi(&log, "TL-ARITH"),
        Some("ordinary x86-64 Lisp"),
        "{log}"
    );
}

#[test]
fn t1_ordinary_loops_survive_forced_collections() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = format!("(progn {DEFINITIONS} (format t \"RESULT ~S~%\" (tl-drive 80)))");
    let (plain, _) = run(None, &program, &STAY_T1);
    let mut envs = STAY_T1.to_vec();
    envs.extend([
        ("EGCL_T2_LOG", "stderr"),
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
    ]);
    let (stressed, log) = run(None, &program, &envs);
    assert_eq!(
        result_line(None, &stressed),
        result_line(None, &plain),
        "{stressed}\n{log}"
    );
    assert_eq!(
        t1_abi(&log, "TL-COLLECT"),
        Some("ordinary x86-64 Lisp"),
        "{log}"
    );
}
