// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Keyword defaults execute after earlier parameters bind, before later ones.
//! An indirect dynamic read must not move across a later special binding.

use std::process::Command;

const PROGRAM: &str = r#"(progn
  (defvar *ko-first* :outer-first)
  (defvar *ko-later* :outer-later)
  (declaim (notinline ko-read ko-plain-read ko-local-read))
  (defun ko-read () (list *ko-first* *ko-later*))
  (defun ko-order (*ko-first* &key (x (ko-read)) (*ko-later* :inner-later))
    (list x (ko-read)))
  ;; Proclaimed SPECIAL variables need the same rule without earmuffed names.
  (declaim (special ko-first ko-later))
  (defvar ko-first :outer-first)
  (defvar ko-later :outer-later)
  (defun ko-plain-read () (list ko-first ko-later))
  (defun ko-plain (ko-first &key (x (ko-plain-read)) (ko-later :inner-later))
    (list x (ko-plain-read)))
  ;; A local SPECIAL declaration also keeps the original binding order.
  (setf (symbol-value 'ko-local-first) :outer-first
        (symbol-value 'ko-local-later) :outer-later)
  (defun ko-local-read ()
    (declare (special ko-local-first ko-local-later))
    (list ko-local-first ko-local-later))
  (defun ko-local (ko-local-first &key (x (ko-local-read)) (ko-local-later :inner-later))
    (declare (special ko-local-first ko-local-later))
    (list x (ko-local-read)))
  ;; Lexical-only defaults still compile and see earlier defaulted values.
  (defun ko-lexical (a &key (x (list a)) (y (append x '(tail)))) (list x y))
  ;; Warm calls supply X: compilation must not change a later omitted call.
  (dotimes (i 50)
    (ko-order :bound :x :supplied)
    (ko-plain :bound :x :supplied)
    (ko-local :bound :x :supplied)
    (ko-lexical :bound))
  (format t "RESULT ~S~%"
    (list (ko-order :bound) (ko-order :bound :*ko-later* :explicit)
          (ko-plain :bound) (ko-local :bound) (ko-lexical :bound)
          (ko-read) (ko-plain-read) (ko-local-read)))
  (format t "LEXICAL-TIER ~S~%" (egcl-ext:function-tier 'ko-lexical)))"#;

const EXPECTED: &str = "RESULT (((:BOUND :OUTER-LATER) (:BOUND :INNER-LATER)) \
((:BOUND :OUTER-LATER) (:BOUND :EXPLICIT)) \
((:BOUND :OUTER-LATER) (:BOUND :INNER-LATER)) \
((:BOUND :OUTER-LATER) (:BOUND :INNER-LATER)) \
((:BOUND) (:BOUND TAIL)) \
(:OUTER-FIRST :OUTER-LATER) (:OUTER-FIRST :OUTER-LATER) (:OUTER-FIRST :OUTER-LATER))";

fn run(tier: Option<&str>, stress: Option<&str>) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args([
        "--kill-after=5",
        "600",
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
        "--eval",
        PROGRAM,
    ]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T2_THRESHOLD",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_LOG",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("EGCL_T2_THRESHOLD", "20");
    cmd.env("EGCL_LOG", "bail=trace,compile=trace");
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    if let Some(stress) = stress {
        cmd.env("EGCL_GC_STRESS", stress);
        cmd.env("EGCL_GC_POISON", "1");
        cmd.env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = cmd.output().expect("run keyword default binding probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "tier {tier:?}, stress {stress:?}: {stdout}\n{stderr}"
    );
    let result = stdout
        .lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("no result: {stdout}\n{stderr}"));
    assert_eq!(
        result, EXPECTED,
        "tier {tier:?}, stress {stress:?}: {stdout}\n{stderr}"
    );
    if tier == Some("t1") {
        assert!(
            stdout.lines().any(|line| line == "LEXICAL-TIER 1"),
            "lexical-only defaults must actually install native T1: {stdout}\n{stderr}"
        );
    }
    stderr.into_owned()
}

#[test]
fn keyword_defaults_preserve_dynamic_binding_order_in_every_tier() {
    for tier in [Some("interp"), Some("t0"), Some("t1"), Some("t2"), None] {
        run(tier, None);
    }
}

#[test]
fn special_defaults_keep_the_original_binder_while_lexical_defaults_compile() {
    let log = run(Some("t0"), None);
    assert!(
        log.lines()
            .any(|line| line.contains("KO-ORDER:") && line.contains("special-parameter-name")),
        "{log}"
    );
    assert!(
        log.lines()
            .any(|line| line.contains("KO-PLAIN:") && line.contains("special-parameter-name")),
        "{log}"
    );
    assert!(
        !log.lines()
            .any(|line| line.contains("KO-LEXICAL:") && line.contains("not compiled to bytecode")),
        "{log}"
    );
}

#[test]
fn keyword_default_binding_order_survives_gc_stress() {
    for stride in ["1", "7"] {
        run(Some("t0"), Some(stride));
    }
}
