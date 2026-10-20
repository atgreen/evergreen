// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! CAR/CDR of NIL in native code (bliss-jmwqr).
//!
//! The native CAR/CDR expansion guards its operand as a cons. NIL used to fail
//! that guard, and eight such deopts blacklisted the whole function back to
//! T0 -- which is how CL:CDDR stayed at tier 0 after 2,711 calls while loading
//! Babel: `(CDDR '(X))` takes `(CDR NIL)`. NIL now has its own arm, so these
//! functions stay native, never deopt on NIL, and answer exactly as the
//! tree-walker does. A non-list operand still deopts and signals TYPE-ERROR.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const DEFINITIONS: &str = r#"
(defun nil-tail2 (x) (cdr (cdr x)))
(defun nil-second (x) (car (cdr x)))
(defun nil-head (x) (car x))
(defun nil-pairs (l)
  (let ((n 0))
    (loop (when (null l) (return n))
          (when (eql (car l) (cadr l)) (incf n))
          (setq l (cdr l)))))
(defparameter *operands* (list nil (list 1) (list 1 1) (list 1 2 3) (list 4 4 4 5)))
(defun nil-results ()
  (mapcar (lambda (x)
            (list (nil-tail2 x) (nil-second x) (nil-head x) (nil-pairs x)
                  (cddr x) (cadr x) (cdar (list x)) (cadr (cdr x))))
          *operands*))
"#;

fn run(program: &str, envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(BIN);
    cmd.args(["--no-init", "--eval", program]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_DEOPT_BLACKLIST_THRESHOLD",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("EGCL_LAZY_COMPILE", "0");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn line<'a>(stdout: &'a str, tag: &str) -> &'a str {
    stdout
        .lines()
        .find(|line| line.starts_with(tag))
        .unwrap_or_else(|| panic!("no {tag} line in:\n{stdout}"))
}

/// T1 keeps every NIL-taking accessor native: no deopt after warm-up, and the
/// same answers as the tree-walker.
#[test]
fn t1_car_cdr_of_nil_stays_native_and_matches_the_interpreter() {
    let program = format!(
        "{DEFINITIONS}
(dotimes (i 50) (nil-results))
(let ((before (egcl-ext:deopt-count)))
  (dotimes (i 50) (nil-results))
  (format t \"DEOPTS ~D~%\" (- (egcl-ext:deopt-count) before)))
(format t \"TIERS ~S~%\"
  (mapcar #'egcl-ext:function-tier '(nil-tail2 nil-second nil-head nil-pairs)))
(format t \"RESULTS ~S~%\" (nil-results))"
    );
    let native = run(
        &program,
        &[("EGCL_T0_T1_THRESHOLD", "1"), ("EGCL_DISABLE_T2", "1")],
    );
    assert_eq!(line(&native, "DEOPTS"), "DEOPTS 0", "{native}");
    assert_eq!(line(&native, "TIERS"), "TIERS (1 1 1 1)", "{native}");
    let interpreted = run(&program, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(
        line(&native, "RESULTS"),
        line(&interpreted, "RESULTS"),
        "native:\n{native}\ninterpreted:\n{interpreted}"
    );
    assert_eq!(
        line(&native, "RESULTS"),
        "RESULTS ((NIL NIL NIL 0 NIL NIL NIL NIL) (NIL NIL 1 0 NIL NIL NIL NIL) \
         (NIL 1 1 1 NIL 1 (1) NIL) ((3) 2 1 0 (3) 2 (2 3) 3) ((4 5) 4 4 2 (4 5) 4 (4 4 5) 4))"
    );
}

/// The Babel shape itself: CL:CDDR, fed lists that end early, reaches T2
/// instead of being blacklisted to T0. A non-list still signals TYPE-ERROR.
#[test]
fn cl_cddr_reaches_t2_when_its_operands_end_in_nil() {
    let program = r#"
(defun feed-cddr ()
  (dolist (x '(nil (a) (a b) (a b c)) t) (cddr x) (cadr x)))
(loop repeat 2000
      do (feed-cddr)
      when (and (= 2 (egcl-ext:function-tier 'cddr))
                (= 2 (egcl-ext:function-tier 'cadr)))
        return t
      do (sleep 0.001)
      finally (format t "STUCK ~S~%"
                (list (egcl-ext:function-tier 'cddr) (egcl-ext:function-tier 'cadr))))
(let ((before (egcl-ext:deopt-count)))
  (dotimes (i 100) (feed-cddr))
  (format t "DEOPTS ~D~%" (- (egcl-ext:deopt-count) before)))
(format t "TIERS ~S~%" (list (egcl-ext:function-tier 'cddr) (egcl-ext:function-tier 'cadr)))
(format t "RESULTS ~S~%"
  (list (cddr '(a)) (cddr nil) (cddr '(a b c)) (cadr '(a)) (cadr '(a b))
        (handler-case (cddr 5) (type-error () :type-error))
        (handler-case (cddr '(a . 5)) (type-error () :type-error))))
"#;
    let native = run(
        program,
        // Default T0 -> T1 gate, so NIL reaches the T1 guard well before T2
        // is requested: the path that used to blacklist CADR/CDDR to T0.
        &[("EGCL_T1_T2_INVOKE_THRESHOLD", "200")],
    );
    assert!(!native.contains("STUCK"), "{native}");
    assert_eq!(line(&native, "DEOPTS"), "DEOPTS 0", "{native}");
    assert_eq!(line(&native, "TIERS"), "TIERS (2 2)", "{native}");
    let interpreted = run(program, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(
        line(&native, "RESULTS"),
        line(&interpreted, "RESULTS"),
        "native:\n{native}\ninterpreted:\n{interpreted}"
    );
    assert_eq!(
        line(&native, "RESULTS"),
        "RESULTS (NIL NIL (C) NIL B :TYPE-ERROR :TYPE-ERROR)"
    );
}
