// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check_unwind(body: &str, expected: &str) {
    let program = format!(
        r#"
        (defvar *binding* :outside)
        (defvar *events* nil)
        (defun leave-two-bindings ()
          (let ((*binding* :first))
            (let ((*binding* :second)) (throw :exit :done))))
        (defun binding-probe () {body})
        (format t "RESULT ~S~%" (list (binding-probe) *binding* (reverse *events*)))
        "#
    );
    for tier in ["interp", "t0"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_NATIVE_TRANSFER", "0")
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run EGCL");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("RESULT {expected}")),
            "{tier}: expected {expected}, got {stdout}\n{stderr}"
        );
    }
}

#[test]
fn catch_retires_inner_special_binding() {
    check_unwind(
        "(progn (catch :exit (let ((*binding* :inner)) (throw :exit :done))) *binding*)",
        "(:OUTSIDE :OUTSIDE NIL)",
    );
}

#[test]
fn block_retires_inner_special_binding() {
    check_unwind(
        "(progn (block exit (let ((*binding* :inner)) (return-from exit :done))) *binding*)",
        "(:OUTSIDE :OUTSIDE NIL)",
    );
}

#[test]
fn local_go_retires_inner_special_binding() {
    check_unwind(
        "(block done (tagbody (let ((*binding* :inner)) (go finish)) finish (return-from done *binding*)))",
        "(:OUTSIDE :OUTSIDE NIL)",
    );
}

#[test]
fn handler_case_sees_binding_after_unwind() {
    check_unwind(
        "(handler-case (let ((*binding* :inner)) (error \"expected\")) (error () *binding*))",
        "(:OUTSIDE :OUTSIDE NIL)",
    );
}

#[test]
fn cleanup_retains_surrounding_binding_and_retires_inner_binding() {
    check_unwind(
        "(catch :exit (let ((*binding* :protected)) (unwind-protect (let ((*binding* :inner)) (throw :exit :done)) (push *binding* *events*))))",
        "(:DONE :OUTSIDE (:PROTECTED))",
    );
}

#[test]
fn activation_exit_restores_nested_bindings_in_reverse_order() {
    check_unwind("(catch :exit (leave-two-bindings))", "(:DONE :OUTSIDE NIL)");
}

#[test]
fn replacement_transfer_restores_bindings_before_outer_cleanup() {
    check_unwind(
        r#"
        (catch :replacement
          (catch :original
            (let ((*binding* :protected))
              (unwind-protect
                  (unwind-protect
                      (let ((*binding* :inner)) (throw :original :lost))
                    (push (list :inner-cleanup *binding*) *events*)
                    (let ((*binding* :replacement-outer))
                      (let ((*binding* :replacement))
                        (push (list :replacement *binding*) *events*)
                        (throw :replacement :done))))
                (push (list :outer-cleanup *binding*) *events*)))))
        "#,
        "(:DONE :OUTSIDE ((:INNER-CLEANUP :PROTECTED) (:REPLACEMENT :REPLACEMENT) (:OUTER-CLEANUP :PROTECTED)))",
    );
}
