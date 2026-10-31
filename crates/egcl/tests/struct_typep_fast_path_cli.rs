// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! TYPEP of a structure instance against a structure type name answers from a
//! one-lock fast path, and FUNCTIONP-style checks read a per-layout cached
//! funcallable slot instead of matching slot names as strings
//! (bliss-gg3i1.7.1). Both must answer exactly as the general path does, in
//! every tier and under GC stress, including the cases the fast path declines.

use std::process::Command;

const PROGRAM: &str = r#"
(defpackage "STFP-OTHER" (:use "CL"))
(defstruct stfp-base a)
(defstruct (stfp-left (:include stfp-base)) b)
(defstruct (stfp-deep (:include stfp-left)) c)
(defstruct (stfp-right (:include stfp-base)) d)
(defstruct stfp-separate e)
(defstruct (stfp-other::stfp-base (:constructor stfp-other::make-other)
                                   (:predicate stfp-other::other-p)
                                   (:conc-name stfp-other::other-)
                                   (:copier nil))
  z)
(defclass stfp-class () ())
(defclass stfp-callable ()
  ()
  (:metaclass egcl-ext:funcallable-standard-class))
(defun stfp-kind (x)
  (typecase x
    (stfp-deep :deep) (stfp-left :left) (stfp-right :right)
    (stfp-base :base) (stfp-separate :separate) (t :other)))
(defun stfp-row (x)
  (list (stfp-kind x)
        (typep x 'stfp-base) (typep x 'stfp-left) (typep x 'stfp-deep)
        (typep x 'stfp-right) (typep x 'stfp-separate)
        (typep x 'stfp-other::stfp-base) (typep x 'structure-object)
        (typep x 'standard-object) (typep x 'function) (functionp x)
        (stfp-base-p x) (stfp-left-p x) (stfp-right-p x)))
(defun stfp-rows (objects) (mapcar #'stfp-row objects))
(let* ((callable (make-instance 'stfp-callable))
       (objects (list (make-stfp-base) (make-stfp-left) (make-stfp-deep)
                      (make-stfp-right) (make-stfp-separate)
                      (stfp-other::make-other) (make-instance 'stfp-class)
                      callable 7 'stfp-base "s" nil))
       (expected (stfp-rows objects)))
  (dotimes (i STFP-ITERATIONS)
    (unless (equal (stfp-rows objects) expected)
      (error "STFP answers changed on iteration ~D" i)))
  (egcl-ext:set-funcallable-instance-function callable (lambda (x) (* 2 x)))
  (assert (= (funcall callable 21) 42))
  (assert (functionp callable))
  (dolist (row expected) (print row)))
(terpri)
(write-line "STFP-OK")
"#;

fn run(tier: &str, stress: bool) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=5", "600", env!("CARGO_BIN_EXE_egcl")])
        // GC stress collects on every 17th allocation; a short loop still
        // crosses the T2 entry the tier forces.
        .args(["--no-init", "--eval"])
        .arg(PROGRAM.replace("STFP-ITERATIONS", if stress { "60" } else { "3000" }))
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .output()
        .expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success() && stdout.contains("STFP-OK"),
        "tier {tier} stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn structure_typep_fast_path_matches_general_answers_in_every_tier() {
    let reference = run("interp", false);
    let rows: Vec<&str> = reference
        .lines()
        .map(str::trim_end)
        .filter(|l| l.starts_with('('))
        .collect();
    assert_eq!(
        rows,
        [
            "(:BASE T NIL NIL NIL NIL NIL T NIL NIL NIL T NIL NIL)",
            "(:LEFT T T NIL NIL NIL NIL T NIL NIL NIL T T NIL)",
            "(:DEEP T T T NIL NIL NIL T NIL NIL NIL T T NIL)",
            "(:RIGHT T NIL NIL T NIL NIL T NIL NIL NIL T NIL T)",
            "(:SEPARATE NIL NIL NIL NIL T NIL T NIL NIL NIL NIL NIL NIL)",
            "(:OTHER NIL NIL NIL NIL NIL T T NIL NIL NIL NIL NIL NIL)",
            "(:OTHER NIL NIL NIL NIL NIL NIL NIL T NIL NIL NIL NIL NIL)",
            "(:OTHER NIL NIL NIL NIL NIL NIL NIL T T T NIL NIL NIL)",
            "(:OTHER NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL)",
            "(:OTHER NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL)",
            "(:OTHER NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL)",
            "(:OTHER NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL)",
        ]
    );
    for tier in ["t0", "t2"] {
        assert_eq!(run(tier, false), reference, "tier {tier}");
    }
    assert_eq!(run("t2", true), reference, "t2 under GC stress");
}
