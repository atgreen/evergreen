// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Closures over bindings the lowerer used to keep only in frame slots
//! (bliss-gg3i1.7.17): MULTIPLE-VALUE-BIND variables, HANDLER-CASE clause
//! variables, and parameters and LET variables of local functions whose
//! capture only a macro expansion reveals. Functions shaped like SBCL's
//! QQ-TEMPLATE-1 and MAP-COMBINATION-ARGS-AND-TYPES bailed to the tree-walker
//! with `closure-captures-slot`; they must now compile and agree at every tier.

use std::path::Path;
use std::process::Command;

const EXPECTED: &[&str] = &[
    "qq-depth=(LIST* (QUOTE A) 1 (QUOTE (:K)))",
    "qq-const=((1 :K NIL) QUOTE)",
    "qq-eval=((A 1) QUOTE)",
    "map-args=(3 3)",
    "map-args-out=(3 (2 NUMBER (:INFO :I 2)) (X T (:INFO :I 1)) (1 NUMBER (:INFO :I 0)))",
    "map-args-plain=(2 2)",
    "combination=(2 3 2)",
    "combination-seen=((3 C 3 (:POS 2)) (2 B 3 (:POS 1)) (1 A 3 (:POS 0)))",
    "combination-short=(:SHORT -1)",
    "handler=\"bad 5\"",
];

/// A local function's parameter captured, through a macro, by a closure in its
/// body. When a capture like this is only revealed late (SBCL's
/// ADD-EQUALITY-CONSTRAINT), the enclosing compile's boxing retry has to reach
/// the local function's own lowering; this pins the shape's answer and that it
/// compiles.
const MACRO_REVEALED: &str = r#"
(defmacro cb-with-each ((var list) &body body)
  `(mapc (lambda (,var) ,@body) ,list))
(defun cb-revealed (items)
  (let ((total 0))
    (flet ((inherit (x scale)
             (cb-with-each (y items) (incf total (* x y scale)))))
      (inherit 1 10)
      (inherit 2 1)
      total)))
(dotimes (i 30) (cb-revealed '(1 2 3)))
(format t "revealed=~S~%" (cb-revealed '(1 2 3)))
"#;

fn run(args: &[&str], tier: &str) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init"])
        .args(args)
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LOG", "compile=trace")
        .env_remove("EGCL_HEAP_ENV")
        .env_remove("EGCL_GC_STRESS")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
    (stdout, stderr)
}

fn no_capture_bail(stderr: &str, prefix: &str, tier: &str) {
    let bails: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains(prefix) && line.contains("closure-captures-slot"))
        .collect();
    assert!(bails.is_empty(), "tier={tier}: {bails:#?}");
}

#[test]
fn slot_capture_shapes_compile_and_agree_at_every_tier() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capture_bail_shapes.lisp");
    for tier in ["interp", "t0", "t1", "t2"] {
        let (stdout, stderr) = run(&["--load", fixture.to_str().unwrap()], tier);
        let report: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|line| line.contains('='))
            .collect();
        assert_eq!(report, EXPECTED, "tier={tier}:\n{stdout}");
        if tier != "interp" {
            no_capture_bail(&stderr, "CB-", tier);
        }
    }
}

#[test]
fn local_parameter_captured_through_a_macro_compiles() {
    for tier in ["t0", "t2"] {
        let (stdout, stderr) = run(&["--eval", &format!("(progn {MACRO_REVEALED})")], tier);
        assert!(stdout.contains("revealed=72"), "tier={tier}: {stdout}");
        no_capture_bail(&stderr, "CB-REVEALED", tier);
    }
}
