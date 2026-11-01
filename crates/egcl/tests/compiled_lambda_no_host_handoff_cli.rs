// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A `(lambda …)` or `#'(lambda …)` in opportunistically compiled code is
//! compiled by the bytecode lowerer as a nested function behind `MakeClosure`,
//! not handed to the tree-walker through `EvalHost`/`MakeClosureEnv` with the
//! bytecode activation's frame installed (bliss-gg3i1.7.15.7, Backpack stage 6).
//!
//! Top-level forms without a loop are the shape that used to reach the
//! handoff at run time: the thunk compiler keeps its opportunistic result,
//! and that result built every lambda in the tree-walker. SBCL's cold-load
//! files do exactly this (`(let (... (fun ...)) (set-macro-character ...
//! (lambda (stream char) ...)))`).
//!
//! Every case must print the tree-walker's answer at T0, T1 and T2, also under
//! forced moving collections, and the handoff census
//! (`EGCL_HANDOFF_CENSUS`) must show no runtime handoff for any of them.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Closures created by top-level thunks: capturing, non-capturing, `&optional`
/// and `&key` defaults over outer variables, a `RETURN-FROM` to the creator's
/// block, a `GO` to its tag, a local SPECIAL declaration, a LOCALLY special
/// context, a PROGV, MACROLET and SYMBOL-MACROLET scope, shared mutable captures, and
/// a closure nested in a closure.
const PROGRAM: &str = r#"
(defvar *g* nil)
(defvar *h* nil)
(defvar *sv* 5)
(defvar *pv* 0)
(let ((fun (lambda (a b) (list a b))))
  (setq *g* (lambda (stream char)
              (let ((string (funcall fun stream char)))
                (if (equal string '(1 2)) :matched string)))))
(let ((n 0))
  (setq *h* (list (lambda (&optional (d n) &key (e (* 2 n))) (incf n d) (list n e))
                  (lambda () n)
                  #'(lambda (x) (declare (special *sv*)) (+ x *sv*))
                  (lambda (a) (lambda (b) (list n a b))))))
(locally (declare (special *sv*))
  (setq *h* (append *h* (list (lambda () *sv*)))))
(macrolet ((twice (f) `(* 2 ,f)))
  (setq *h* (append *h* (list (lambda (x) (twice x))))))
(let ((c 3))
  (symbol-macrolet ((cc (* c 10)))
    (setq *h* (append *h* (list (lambda (x) (+ x cc)))))))
(setq *h* (append *h*
                  (list (block outer
                          (funcall (lambda () (return-from outer :returned)))
                          :fell-through)
                        (let ((r :start))
                          (tagbody
                             (funcall (lambda () (setq r :went) (go done)))
                             (setq r :fell-through)
                           done)
                          r)
                        (let ((k 4))
                          (lambda () (progv '(*pv*) (list (* k 10)) (+ k (symbol-value '*pv*))))))))
(let ((r nil))
  (dotimes (i 20) (push (funcall *g* i 2) r))
  (format t "~&RESULT ~S~%"
          (list (funcall *g* 1 2) (length r)
                (funcall (first *h*)) (funcall (first *h*) 3 :e 9)
                (funcall (second *h*))
                (let ((*sv* 100)) (funcall (third *h*) 1))
                (funcall (funcall (fourth *h*) 7) 8)
                (let ((*sv* 42)) (funcall (fifth *h*)))
                (funcall (sixth *h*) 21)
                (funcall (seventh *h*) 1)
                (eighth *h*)
                (ninth *h*)
                (funcall (tenth *h*))
                *pv*)))
"#;

const EXPECTED: &str =
    "RESULT (:MATCHED 20 (0 0) (3 9) 3 101 (3 7 8) 42 42 31 :RETURNED :WENT 44 0)";

fn run(tier: &str, extra: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(BIN);
    command
        .args(["--no-init", "--eval", PROGRAM])
        .env("EGCL_FORCE_TIER", tier);
    for (key, value) in extra {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
    (stdout, stderr)
}

fn assert_result(tier: &str, stdout: &str, stderr: &str) {
    assert!(
        stdout.lines().any(|line| line.trim() == EXPECTED),
        "tier={tier}: expected {EXPECTED:?} in:\n{stdout}\n{stderr}"
    );
}

#[test]
fn top_level_lambdas_agree_at_every_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let (stdout, stderr) = run(tier, &[]);
        assert_result(tier, &stdout, &stderr);
    }
}

#[test]
fn top_level_lambdas_agree_under_gc_stress() {
    for tier in ["t0", "t1", "t2"] {
        let (stdout, stderr) = run(
            tier,
            &[
                ("EGCL_GC_STRESS", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
                ("EGCL_GC_POISON", "1"),
            ],
        );
        assert_result(tier, &stdout, &stderr);
    }
}

#[test]
fn top_level_lambdas_never_reach_the_host_handoff() {
    for tier in ["t0", "t1", "t2"] {
        let path = std::env::temp_dir().join(format!(
            "egcl-handoff-census-{}-{tier}.txt",
            std::process::id()
        ));
        let census = path.to_str().expect("utf-8 temp path").to_string();
        let (stdout, stderr) = run(tier, &[("EGCL_HANDOFF_CENSUS", &census)]);
        assert_result(tier, &stdout, &stderr);
        let table = std::fs::read_to_string(&path).expect("census written at exit");
        let _ = std::fs::remove_file(&path);
        // The census must actually be live: every run records lowering and
        // calls of the bootstrap's own closures.
        assert!(!table.trim().is_empty(), "tier={tier}: empty census");
        // No closure in this program is built by the tree-walker on behalf of
        // compiled code, and no opportunistic lambda is lowered to a handoff
        // by default.
        for line in table.lines() {
            let key = line.trim_start().split_once(' ').map_or("", |(_, key)| key);
            assert!(
                !key.starts_with("lower:EvalHost:nonportable")
                    && !key.starts_with("lower:MakeClosureEnv:nonportable"),
                "tier={tier}: default host handoff lowered: {line}\n{table}"
            );
            let runtime = key.starts_with("run:") || key.starts_with("call:handoff-closure");
            let ours = [
                "STREAM CHAR",
                "(A B)",
                "*SV*",
                "TWICE",
                "OUTER",
                "DONE",
                "CC",
            ]
            .iter()
            .any(|marker| key.contains(marker));
            assert!(
                !(runtime && ours),
                "tier={tier}: runtime handoff of a program lambda: {line}\n{table}"
            );
        }
    }
}
