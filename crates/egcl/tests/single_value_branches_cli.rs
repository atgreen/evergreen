// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A conditional whose arms differ in whether they may return multiple values
//! returns exactly one value from its single-valued arm (GitHub #467).
//!
//! Compiled code used to leave the values of an earlier producer active there:
//! `(let ((r (floor 7 2))) (if neg (not r) r))` returned 3 and 1, and
//! `(if neg (g n) 5)` returned 3 and 1 instead of 5 to MULTIPLE-VALUE-LIST,
//! and `(tagbody (floor n 2))` returned the quotient and remainder, not NIL.
//! The same missing proof made the T2 ordinary (direct-call) entry decline
//! these functions with "return of possibly multiple values".
use std::process::{Command, Output};

const PROGRAM: &str = r#"
(progn
  (defun g (x) (values x :g))
  (defmacro defs (&rest specs)
    `(progn ,@(loop for (name body) in specs
                    collect `(defun ,name (n neg) (let ((r (floor 7 2))) ,body)))))
  (defs (s-if (if neg (not r) r))
        (s-if2 (if neg r (g n)))
        (s-ifn (if neg (g n)))
        (s-if3 (if neg (g n) 5))
        (s-when (when neg (g r)))
        (s-unless (unless neg (g r)))
        (s-or (or r (g n)))
        (s-or3 (or neg (g n)))
        (s-and (and neg (g r)))
        (s-and2 (and neg r (g r)))
        (s-cond (cond (neg (g r)) (t r)))
        (s-cond3 (cond (neg (g r))))
        (s-case2 (case n (1 (g r))))
        (s-block (block b (if neg (return-from b (g n)) r)))
        (s-nested (if neg (if (> n 1) (g n) r) (if (> n 1) r (g n))))
        (s-let (let ((q (g n))) (if neg q (g r))))
        (s-progn (progn (g n) (if neg (g r) r)))
        (s-catch (catch 'c (if neg (throw 'c r) (g n))))
        (s-tagbody (tagbody (floor n 2))))
  (defparameter *names* '(s-if s-if2 s-ifn s-if3 s-when s-unless s-or s-or3 s-and s-and2
                          s-cond s-cond3 s-case2 s-block s-nested s-let s-progn s-catch s-tagbody))
  (defun all ()
    (loop for f in *names*
          collect (list (multiple-value-list (funcall f 2 nil))
                        (multiple-value-list (funcall f 2 t))
                        (multiple-value-list (funcall f 1 nil)))))
  (format t "COLD ~s~%" (all))
  (loop repeat 3000 do (all))
  (sleep 0.5)
  (loop repeat 200 do (all))
  (format t "WARM ~s~%" (all))
  (format t "TIERS ~{~a~^ ~}~%" (mapcar #'egcl-ext:function-tier *names*)))
"#;

const EXPECTED: &str = "(((3) (NIL) (3)) ((2 :G) (3) (1 :G)) ((NIL) (2 :G) (NIL)) \
((5) (2 :G) (5)) ((NIL) (3 :G) (NIL)) ((3 :G) (NIL) (3 :G)) ((3) (3) (3)) \
((2 :G) (T) (1 :G)) ((NIL) (3 :G) (NIL)) ((NIL) (3 :G) (NIL)) ((3) (3 :G) (3)) \
((NIL) (3 :G) (NIL)) ((NIL) (NIL) (3 :G)) ((3) (2 :G) (3)) ((3) (2 :G) (1 :G)) \
((3 :G) (2) (3 :G)) ((3) (3 :G) (3)) ((2 :G) (3) (1 :G)) ((NIL) (NIL) (NIL)))";

const MAPPED_LINUX: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

fn egcl(envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", PROGRAM]);
    for name in [
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_DISABLE_T2",
        "EGCL_T2_LOG",
    ] {
        command.env_remove(name);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn line<'a>(stdout: &'a str, tag: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(tag))
        .unwrap_or_else(|| panic!("no {tag} line in {stdout}"))
}

#[test]
fn single_valued_arms_return_one_value_at_every_tier() {
    for envs in [
        &[("EGCL_FORCE_TIER", "interp")][..],
        &[("EGCL_FORCE_TIER", "t0")][..],
        &[("EGCL_FORCE_TIER", "t1")][..],
        &[("EGCL_FORCE_TIER", "t2")][..],
        &[][..],
    ] {
        let output = egcl(envs);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{envs:?}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(line(&stdout, "COLD "), EXPECTED, "cold under {envs:?}");
        assert_eq!(line(&stdout, "WARM "), EXPECTED, "warm under {envs:?}");
    }
}

/// With the single value proven, the T2 ordinary entry no longer declines
/// these shapes: each function installs with the ordinary ABI.
#[test]
fn single_valued_branches_install_the_ordinary_entry() {
    if !MAPPED_LINUX {
        return;
    }
    let output = egcl(&[
        ("EGCL_T2_LOG", "stderr"),
        ("EGCL_T0_T1_THRESHOLD", "2"),
        ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{log}");
    assert_eq!(line(&stdout, "WARM "), EXPECTED);
    assert_eq!(
        line(&stdout, "TIERS "),
        "2 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2",
        "{log}"
    );
    assert!(
        !log.contains("return of possibly multiple values"),
        "an ordinary entry still declined:\n{log}"
    );
    for name in ["S-IF", "S-IF3", "S-WHEN", "S-AND2", "S-NESTED", "S-PROGN"] {
        assert!(
            log.contains(&format!(
                "[T2] {name}: optimized worker result installed with ordinary x86-64 Lisp T2 ABI"
            )),
            "{name} did not install the ordinary entry:\n{log}"
        );
    }
}
