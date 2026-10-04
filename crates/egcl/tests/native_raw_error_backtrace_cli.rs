// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check_native_unbound_chain(tier: &str, installed: u8) {
    for failure in [
        "(native-error-outer 42 t)",
        "(unwind-protect (native-error-outer 42 t) (ignore-errors (error \"handled cleanup error\")))",
    ] {
        let program = format!(
            r#"
                (defun native-error-leaf (value fail)
                  (if fail native-missing-variable value))
                (defparameter *native-error-leaf* #'native-error-leaf)
                (defun native-error-outer (value fail)
                  (funcall *native-error-leaf* value fail))
                (dotimes (i 100) (assert (= 42 (native-error-outer 42 nil))))
                (assert (= {installed} (egcl-ext:function-tier 'native-error-leaf)))
                (assert (= {installed} (egcl-ext:function-tier 'native-error-outer)))
                (format t "NATIVE-TIERS-CONFIRMED~%")
                {failure}
                "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("NATIVE-TIERS-CONFIRMED"),
            "{tier}: {stdout}\n{stderr}"
        );
        assert_eq!(output.status.code(), Some(1), "{tier}: {stderr}");
        assert!(
            stderr.contains("unbound variable: NATIVE-MISSING-VARIABLE"),
            "{tier}: {stderr}"
        );
        for name in ["NATIVE-ERROR-LEAF", "NATIVE-ERROR-OUTER"] {
            assert_eq!(stderr.matches(name).count(), 1, "{tier}: {stderr}");
        }
        assert!(stderr.find("NATIVE-ERROR-LEAF").unwrap() < stderr.find("NATIVE-ERROR-OUTER").unwrap());
    }
}

#[test]
fn t1_unbound_error_retains_the_original_call_chain() {
    check_native_unbound_chain("t1", 1);
}

#[test]
fn t2_unbound_error_retains_the_original_call_chain() {
    check_native_unbound_chain("t2", 2);
}

#[test]
fn native_raw_conditions_keep_their_type_and_signal_once() {
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let program = format!(
            r#"
          (defvar *native-handler-calls* 0)
          (defun native-handled-leaf (fail)
            (if fail native-handler-missing 42))
          (defparameter *native-handled-leaf* #'native-handled-leaf)
          (defun native-handled-outer (fail) (funcall *native-handled-leaf* fail))
          (dotimes (i 100) (assert (= 42 (native-handled-outer nil))))
          (assert (= {installed} (egcl-ext:function-tier 'native-handled-leaf)))
          (assert (= {installed} (egcl-ext:function-tier 'native-handled-outer)))
          (assert (eq :caught
            (handler-case
                (handler-bind ((unbound-variable
                                (lambda (condition)
                                  (assert (eq 'native-handler-missing (cell-error-name condition)))
                                  (incf *native-handler-calls*)
                                  (handler-case (error "nested handled error")
                                    (error () nil)))))
                  (native-handled-outer t))
              (unbound-variable (condition)
                (assert (eq 'native-handler-missing (cell-error-name condition)))
                :caught))))
          (assert (= 1 *native-handler-calls*))
          (format t "NATIVE-HANDLER-ONCE~%")
        "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("NATIVE-HANDLER-ONCE"),
            "{tier}: {stdout}\n{stderr}"
        );
        assert!(
            !stderr.contains("Backtrace"),
            "handled errors must stay silent: {stderr}"
        );
    }
}

fn check_native_direct_type_chain(tier: &str, installed: u8) {
    let program = format!(
        r#"
      (defun native-type-leaf (value) (char-code value))
      (defparameter *native-type-leaf* #'native-type-leaf)
      (defun native-type-outer (value) (funcall *native-type-leaf* value))
      (dotimes (i 100) (assert (= 65 (native-type-outer #\A))))
      (assert (= {installed} (egcl-ext:function-tier 'native-type-leaf)))
      (assert (= {installed} (egcl-ext:function-tier 'native-type-outer)))
      (format t "DIRECT-TIERS-CONFIRMED~%")
      (native-type-outer 42)
    "#
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("DIRECT-TIERS-CONFIRMED"),
        "{tier}: {stdout}\n{stderr}"
    );
    assert_eq!(output.status.code(), Some(1), "{tier}: {stderr}");
    assert!(
        stderr.contains("type error:") && stderr.contains("character"),
        "{tier}: {stderr}"
    );
    for name in ["NATIVE-TYPE-LEAF", "NATIVE-TYPE-OUTER"] {
        assert_eq!(stderr.matches(name).count(), 1, "{tier}: {stderr}");
    }
    assert!(stderr.find("NATIVE-TYPE-LEAF").unwrap() < stderr.find("NATIVE-TYPE-OUTER").unwrap());
}
#[test]
fn t1_direct_type_error_retains_the_original_call_chain() {
    check_native_direct_type_chain("t1", 1);
}
#[test]
fn t2_direct_type_error_retains_the_original_call_chain() {
    check_native_direct_type_chain("t2", 2);
}

#[test]
fn osr_error_keeps_its_loop_frame_and_condition_type() {
    let program = r#"
      (defun native-osr-failure ()
        (let ((i 0))
          (tagbody again
            (if (= i 100) native-osr-missing)
            (setq i (+ i 1))
            (go again))))
      (handler-bind ((unbound-variable
                      (lambda (condition)
                        (assert (eq 'native-osr-missing (cell-error-name condition)))
                        (assert (> (egcl-ext:function-osr-count 'native-osr-failure) 0))
                        (format t "ERROR-AFTER-OSR~%"))))
        (native-osr-failure))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_FORCE_TIER")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "1000000")
        .env("EGCL_DISABLE_T2", "1")
        .env("EGCL_OSR_THRESHOLD", "20")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("ERROR-AFTER-OSR"), "{stdout}\n{stderr}");
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("unbound variable: NATIVE-OSR-MISSING"),
        "{stderr}"
    );
    assert_eq!(stderr.matches("NATIVE-OSR-FAILURE").count(), 1, "{stderr}");
}

#[test]
fn native_keyword_binding_error_keeps_its_callee() {
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let program = format!(
            r#"
              (defun native-keyword-leaf (&key value) value)
              (dotimes (i 100) (assert (= 42 (native-keyword-leaf :value 42))))
              (assert (= {installed} (egcl-ext:function-tier 'native-keyword-leaf)))
              (format t "KEYWORD-TIER-CONFIRMED~%")
              (native-keyword-leaf :unknown 42)
            "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("KEYWORD-TIER-CONFIRMED"),
            "{tier}: {stdout}\n{stderr}"
        );
        assert_eq!(output.status.code(), Some(1), "{tier}: {stderr}");
        assert!(
            stderr.contains("unexpected keyword argument: UNKNOWN"),
            "{tier}: {stderr}"
        );
        assert_eq!(
            stderr.matches("NATIVE-KEYWORD-LEAF").count(),
            1,
            "{tier}: {stderr}"
        );
    }
}
