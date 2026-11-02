// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A native closure creator must retain the lexical exit scope inherited by
//! the closure it creates. Ordinary creator admission keeps these mapped.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"(progn
  (defvar *hc-cleanups* 0)
  (defvar *hc-return-outer* nil)
  (defvar *hc-return-inner* nil)
  (defvar *hc-go-outer* nil)
  (defvar *hc-go-inner* nil)
  (declaim (notinline hc-call))
  (defun hc-call (function) (funcall function))
  (defun hc-return (n)
    (block destination
      (unwind-protect
        (let ((outer
                (lambda ()
                  (let ((inner (lambda () (return-from destination (values (list n :heap) :returned)))))
                    (setq *hc-return-inner* inner)
                    (hc-call inner)))))
          (setq *hc-return-outer* outer)
          (hc-call outer))
        (incf *hc-cleanups*))))
  (defun hc-go (n)
    (let ((result :wrong))
      (tagbody
        (let ((outer
                (lambda ()
                  (let ((inner (lambda () (setq result (list n :went)) (go destination))))
                    (setq *hc-go-inner* inner)
                    (hc-call inner)))))
          (setq *hc-go-outer* outer)
          (hc-call outer))
        (setq result :fell-through)
      destination)
      result))
  (dotimes (i 50)
    (assert (equal (multiple-value-list (hc-return i)) (list (list i :heap) :returned)))
    (assert (equal (hc-go i) (list i :went))))
  (when NATIVE
    ;; HC-GO establishes the scope; its OUTER closure is the creator under
    ;; regression, and INNER is the native named-exit callee. Check those
    ;; actual callable instances, independently of the container's tier.
    (dolist (function (list #'hc-return *hc-return-outer* *hc-return-inner*
                           *hc-go-outer* *hc-go-inner*))
      (assert (plusp (egcl-ext:function-tier function)))))
  (assert (= *hc-cleanups* 50))
  (format t "RESULT ~S ~S ~D~%" (multiple-value-list (hc-return 7)) (hc-go 8) *hc-cleanups*))"#;

fn run(tier: &str, stress: bool) {
    let program = PROGRAM.replace("NATIVE", if tier == "interp" { "nil" } else { "t" });
    let mut cmd = Command::new("timeout");
    cmd.args([
        "--kill-after=5",
        "600",
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
        "--eval",
        &program,
    ]);
    for key in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_DISABLE_ORDINARY_HEAP_CREATORS",
        "EGCL_T2_NO_QUEUE",
        "EGCL_PROFILING_DISABLED",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LOG", "stderr");
    if stress {
        cmd.env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = cmd.output().expect("run nested control creator regression");
    let out = String::from_utf8_lossy(&output.stdout);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "tier={tier} stress={stress}: {out}\n{log}"
    );
    assert!(
        out.lines()
            .any(|line| line == "RESULT ((7 :HEAP) :RETURNED) (8 :WENT) 51"),
        "{out}"
    );
    if tier == "t2" {
        assert!(
            log.lines().any(|line| line
                .contains("HC-RETURN: optimized worker result installed with mapped T2 ABI")),
            "HC-RETURN must execute the actual mapped native control path: {log}"
        );
    }
}

#[test]
fn nested_return_and_go_creators_keep_lexical_control_in_native_tiers() {
    for tier in ["interp", "t1", "t2"] {
        run(tier, false);
    }
}

#[test]
fn nested_native_control_creators_preserve_results_under_gc_stress() {
    run("t2", true);
}
