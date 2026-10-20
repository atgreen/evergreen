// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The prelude FIND and COUNT families test each element inline (bliss-15gms).
//!
//! They used to wrap the test in a local `(flet ((matchp (e) ...)))` that refers
//! to PRED, KEY, ITEM and TESTFN, i.e. a capturing closure, and T1 refuses any
//! function that builds a closure environment. A cold Babel load logged
//! `[T2] FIND: T1 declined (captured variables (closure environment))` for that
//! reason alone. COUNT, COUNT-IF and COUNT-IF-NOT were worse off: written with
//! DO before boot.lisp defines DO, they never compiled to bytecode at all, so
//! they never reached a tiering decision. The rewrite must keep every result,
//! including the ORDER in which :KEY and the test see the elements under
//! :FROM-END, which ansi checks.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const NAMES: [&str; 6] = [
    "FIND",
    "FIND-IF",
    "FIND-IF-NOT",
    "COUNT",
    "COUNT-IF",
    "COUNT-IF-NOT",
];

/// Each case runs 30 times so every function clears a lowered T0 -> T1 gate;
/// only the first result is printed. The trail records the :KEY and test calls
/// in order, so a change to the application order changes the output.
const PROGRAM: &str = r#"
(defvar *trail* nil)
(defun k (x) (push (list :key x) *trail*) x)
(defun tst (a b) (push (list :test a b) *trail*) (eql a b))
(defmacro case30 (form)
  `(let ((r nil))
     (dotimes (i 30)
       (setq *trail* nil)
       (let ((v (handler-case ,form (error (e) (list :error (type-of e))))))
         (when (= i 0) (setq r (list v (reverse *trail*))))))
     (prin1 r) (terpri)))
(case30 (find 3 '(1 2 3 4 3)))
(case30 (find 3 (vector 1 2 3 4 3) :from-end t :key #'k :test #'tst))
(case30 (find 3 '(1 2 3 4 3) :start 1 :end 3 :key #'k))
(case30 (find 3 '(3 3 1) :test-not #'tst))
(case30 (find 3 '(3 3 1) :test-not #'tst :from-end t))
(case30 (find #\b "abcb" :from-end t :test #'char=))
(case30 (find 9 '(1 2 3)))
(case30 (find 1 '(1 2) :start 3))
(case30 (find 1 '(1 2) :end 5))
(case30 (find-if #'evenp '(1 2 3 4) :key #'k :from-end t))
(case30 (find-if #'evenp '(1 2 3 4) :start 2 :key #'k))
(case30 (find-if-not #'evenp (vector 2 4 5 7) :key #'k :from-end t))
(case30 (find-if-not #'evenp '(2 4 6)))
(case30 (count 3 '(1 3 3 4 3)))
(case30 (count 3 (vector 1 3 3 4 3) :from-end t :key #'k :test #'tst))
(case30 (count 3 '(1 3 3 4 3) :start 1 :end 4 :key #'k :test #'tst))
(case30 (count 3 '(1 3 3 4 3) :test-not #'tst :from-end t))
(case30 (count #\a "banana"))
(case30 (count 1 '(1 2) :start 3))
(case30 (count-if #'oddp '(1 2 3 5) :key #'k :from-end t))
(case30 (count-if #'oddp (vector 1 2 3 5) :start 1 :end 3 :key #'k))
(case30 (count-if-not #'oddp '(1 2 3 4) :key #'k :from-end t))
(case30 (count-if-not #'oddp '(1 2 3 4) :end 9))
(format t "DONE~%")
"#;

fn run(tier: Option<&str>) -> (String, String) {
    let mut cmd = Command::new(BIN);
    cmd.args([
        "--no-init",
        "--eval",
        &format!("(progn {PROGRAM} (values))"),
    ])
    .env("EGCL_T2_LOG", "stderr")
    .env("EGCL_T0_T1_THRESHOLD", "5");
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    let out = cmd.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("DONE"),
        "corpus did not finish:\n{stdout}\n{stderr}"
    );
    (stdout, stderr)
}

#[test]
fn find_and_count_match_the_interpreter() {
    let (oracle, _) = run(Some("interp"));
    let (tiered, log) = run(None);
    assert_eq!(
        oracle, tiered,
        "tiered results differ from the interpreter:\n{log}"
    );
    // Pin a few results whose :FROM-END order ansi checks, so an oracle that
    // drifted with the code under test cannot hide it.
    assert!(
        oracle.contains("(3 ((:KEY 3) (:TEST 3 3)))"),
        "FIND :FROM-END must stop at the last match after one key/test:\n{oracle}"
    );
    assert!(
        oracle.contains(
            "(3 ((:KEY 3) (:TEST 3 3) (:KEY 4) (:TEST 3 4) (:KEY 3) (:TEST 3 3) \
             (:KEY 3) (:TEST 3 3) (:KEY 1) (:TEST 3 1)))"
        ),
        "COUNT :FROM-END must apply :KEY then the test right to left:\n{oracle}"
    );
    assert!(
        oracle.contains("((:ERROR TYPE-ERROR) NIL)"),
        "an :END past the sequence must still signal from ELT:\n{oracle}"
    );
}

/// The closure-environment decline is gone. Each function must still appear in
/// the T2 log (so the probe reached the tiering decision) with some other
/// outcome; today that is the &key decline that bliss-shih7.27 removes.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn find_and_count_no_longer_decline_for_a_closure_environment() {
    let (_, log) = run(None);
    for name in NAMES {
        let prefix = format!("[T2] {name}:");
        let ours: Vec<&str> = log.lines().filter(|l| l.starts_with(&prefix)).collect();
        assert!(
            !ours.is_empty(),
            "{name} never reached a tiering decision:\n{log}"
        );
        assert!(
            !ours.iter().any(|l| l.contains("closure environment")),
            "{name} still builds a capturing closure:\n{}",
            ours.join("\n")
        );
    }
}
