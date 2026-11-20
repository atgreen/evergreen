// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! An interpreter closure is physically the cons `(EGCL::CLOSURE . id)`, but
//! its type is FUNCTION. The native leaf builtins for CONSP, ENDP, CAR and
//! CDR used a bare cons-tag test, so once a caller tiered up, CONSP answered
//! T for a closure and CAR returned the marker symbol. The SBCL cross-compiler
//! keeps such a closure as MAKE-ARRAY's source transform; its natively
//! compiled `(consp info)` then reported "redefinition of MAKE-ARRAY clobbers
//! structure accessor" and failed host-2 at src/code/array
//! (bliss-gg3i1.7.21).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defvar *closure* (eval '(let ((y 1)) (lambda (form) (list y form)))))
  (defun cc-consp (x) (consp x))
  (defun cc-endp (x) (handler-case (endp x) (type-error () :type-error)))
  (defun cc-car (x) (handler-case (car x) (type-error () :type-error)))
  (defun cc-cdr (x) (handler-case (cdr x) (type-error () :type-error)))
  (defun cc-first (x) (handler-case (first x) (type-error () :type-error)))
  (defun cc-rest (x) (handler-case (rest x) (type-error () :type-error)))
  (defun cc-row (x)
    (list (cc-consp x) (cc-endp x) (cc-car x) (cc-cdr x) (cc-first x) (cc-rest x)))
  (dotimes (i 3000) (cc-row *closure*) (cc-row '(1 . 2)))
  (let ((*print-pretty* nil))
    (format t "RESULT ~S ~S ~S ~S~%"
            (cc-row *closure*) (cc-row '(1 . 2)) (functionp *closure*)
            (funcall *closure* 2))))
"#;

const EXPECTED: &str = "RESULT (NIL :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR) (T NIL 1 2 1 2) T (1 2)";

fn run(tier: Option<&str>) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", PROGRAM]);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        cmd.env_remove(key);
    }
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tier {tier:?}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

#[test]
fn closure_conses_are_functions_at_every_tier() {
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        let out = run(tier);
        let line = out
            .lines()
            .find(|line| line.starts_with("RESULT "))
            .unwrap_or_else(|| panic!("tier {tier:?}: no RESULT line in:\n{out}"));
        assert_eq!(line, EXPECTED, "tier {tier:?}");
    }
}
