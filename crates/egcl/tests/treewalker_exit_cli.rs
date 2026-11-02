// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shapes that used to keep a function, or a top-level form, in the
//! tree-walker for good (Backpack stage 6b, bliss-gg3i1.7.15.7). Each program
//! is run tree-walked, in T0, T1 and T2 and under the default policy, and the
//! answers must agree; each test also checks that the shape now compiles, and
//! runs once under GC stress with freed nursery poisoned.

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
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_LOG",
        "EGCL_BYTECODE_TRACE_NAMES",
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

/// Run `program` in every tier and under GC stress; every run must print
/// `expected`.
fn agree_everywhere(program: &str, expected: &str) {
    for tier in TIERS {
        let (out, _) = run(tier, program, &[("EGCL_T2_THRESHOLD", "50")]);
        assert_eq!(result_line(tier, &out), expected, "tier {tier:?}:\n{out}");
    }
    for tier in [Some("t0"), None] {
        let (out, _) = run(
            tier,
            program,
            &[
                ("EGCL_T2_THRESHOLD", "50"),
                ("EGCL_GC_STRESS", "1"),
                ("EGCL_GC_POISON", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ],
        );
        assert_eq!(
            result_line(tier, &out),
            expected,
            "GC stress, tier {tier:?}:\n{out}"
        );
    }
}

/// The names in `log` (an `EGCL_LOG=compile=trace` stderr) that were left to
/// the tree-walker.
fn declined(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| line.contains("not compiled to bytecode"))
        .map(str::to_string)
        .collect()
}

// ── An inline LAMBDA application with &AUX ──────────────────────────────

/// SBCL's MAKE-MACRO-LAMBDA binds a user's &WHOLE as an &AUX variable, and
/// DEF-IR1-TRANSLATOR / DEF-TYPE-TRANSLATOR apply that lambda inline. Such an
/// application used to bail as a computed operator (IR1-CONVERT-SETQ,
/// PARSE-<SATISFIES>).
const INLINE_AUX: &str = r#"
  (defvar *ia-special* :global)
  (declaim (notinline ia-read))
  (defun ia-read () *ia-special*)
  (defun ia-translator (start form)
    ((lambda (expr env &aux (source expr) (parts (list :parts source)))
       "docstring"
       (declare (ignore env))
       (destructuring-bind (op &rest things) (cdr expr)
         (list start op things source parts)))
     form :lexenv))
  (defun ia-type (spec)
    (if (listp spec)
        ((lambda (expr &aux (whole expr)) (list (car whole) (length expr))) spec)
        :atom))
  ;; Arguments are evaluated outside every binding: X and Y swap.
  (defun ia-swap (x y) ((lambda (x y &aux (z (list x y))) (list z x)) y x))
  ;; A SPECIAL declaration keeps the original path, with the same answer.
  (defun ia-special (v)
    ((lambda (x &aux (*ia-special* x)) (declare (special *ia-special*)) (ia-read)) v))
  (defun ia-round (k)
    (list (ia-translator k '(setq a 1 b 2)) (ia-type '(satisfies evenp)) (ia-type 'fixnum)
          (ia-swap k :b) (ia-special k) (ia-read)))
"#;

#[test]
fn inline_lambda_with_aux_agrees_in_every_tier() {
    let program = format!(
        r#"(progn {INLINE_AUX}
  (let ((last nil)) (dotimes (i 200) (setq last (ia-round 3)))
    (format t "RESULT ~S~%" last)))"#
    );
    agree_everywhere(
        &program,
        "RESULT ((3 A (1 B 2) (SETQ A 1 B 2) (:PARTS (SETQ A 1 B 2))) (SATISFIES 2) :ATOM \
         ((:B 3) :B) 3 :GLOBAL)",
    );
}

#[test]
fn inline_lambda_with_aux_compiles() {
    let program = format!(
        r#"(progn {INLINE_AUX}
  (dotimes (i 50) (ia-round 3))
  (format t "RESULT ~S~%" (ia-round 3)))"#
    );
    let (out, log) = run(Some("t0"), &program, &[("EGCL_LOG", "compile=trace")]);
    assert!(result_line(None, &out).starts_with("RESULT "), "{out}");
    let declined = declined(&log);
    for name in ["IA-TRANSLATOR", "IA-TYPE", "IA-SWAP"] {
        assert!(
            !declined
                .iter()
                .any(|line| line.contains(&format!("] {name}:"))),
            "{name} stayed tree-walked:\n{declined:#?}"
        );
    }
}

// ── DESTRUCTURING-BIND in a lambda of compiled code ─────────────────────

/// DESTRUCTURING-BIND lowers only in portable mode. A lambda in an
/// opportunistically compiled top-level form used to be handed to the
/// tree-walker for it (SBCL's cold-setup DEFUN macro function); it now
/// retries its nested body portably.
const NESTED_DBIND: &str = r#"
  (defvar *nd-expander* (lambda (form env) (declare (ignore env)) form))
  (setf (macro-function 'nd-defun)
        (lambda (whole env)
          (funcall *nd-expander*
                   (destructuring-bind (name ll . rest) (cdr whole)
                     `(defun ,name ,ll ,@rest))
                   env)))
  (defvar *nd-fn* nil)
  (let ((k :k))
    (setq *nd-fn* (lambda (x) (destructuring-bind (a &optional (b k) &key (c 0)) x (list a b c)))))
"#;

#[test]
fn destructuring_bind_in_a_lambda_compiles() {
    let program = format!(
        r#"(progn {NESTED_DBIND}
  (let ((last nil))
    (dotimes (i 50)
      (setq last (list (macroexpand-1 '(nd-defun f (x) (1+ x)))
                       (funcall *nd-fn* '(1)) (funcall *nd-fn* '(1 2 :c 3)))))
    (format t "RESULT ~S~%" last)))"#
    );
    agree_everywhere(&program, "RESULT ((DEFUN F (X) (1+ X)) (1 :K 0) (1 2 3))");
    let census = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "treewalker-exit-dbind-{}.census",
        std::process::id()
    ));
    let census_path = census.to_string_lossy().into_owned();
    let (out, _) = run(
        None,
        &program,
        &[("EGCL_HANDOFF_CENSUS", census_path.as_str())],
    );
    assert!(result_line(None, &out).starts_with("RESULT "), "{out}");
    let table = std::fs::read_to_string(&census).expect("handoff census written");
    let _ = std::fs::remove_file(&census);
    assert!(
        !table.contains("lower:"),
        "a lambda was handed to the tree-walker:\n{table}"
    );
}
