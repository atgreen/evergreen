// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Argument effects may change a named target, but cannot mix two definitions.
use std::process::Command;

fn check_selection(call: &str, mutation: &str, accepted: &[&str]) {
    let program = format!(
        r#"
      (defparameter *effects* nil)
      (defparameter *old-target*
        (let ((capture :old-capture))
          (lambda (x y) (values (list :old capture x y) :old-mv))))
      (defparameter *new-target*
        (let ((capture :new-capture))
          (lambda (x y) (values (list :new capture x y) :new-mv))))
      (setf (symbol-function 'selection-target) *old-target*)
      (defun selection-caller (change)
        ({call}
          (progn (when change (push :first *effects*) {mutation}) 99)
          (progn (when change (push :second *effects*)) 101)))
      (dotimes (i 40) (selection-caller nil))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'selection-caller))
      (handler-case
        (format t "RESULT ~S~%" (multiple-value-list (selection-caller t)))
        (undefined-function () (format t "UNDEFINED~%")))
      (format t "EFFECTS ~S~%" *effects*)
    "#
    );
    for (tier, native, installed) in [
        ("interp", "0", 0),
        ("t0", "0", 0),
        ("t1", "0", 1),
        ("t2", "0", 2),
        ("t2", "1", 2),
    ] {
        if tier == "t2" && !cfg!(all(target_arch = "x86_64", unix)) {
            continue;
        }
        if tier == "t1" && !cfg!(target_arch = "x86_64") {
            continue;
        }
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_NATIVE_TRANSFER", native)
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run call selection regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier}, native={native}: {}\n{stdout}\n{stderr}",
            output.status
        );
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("TIER {installed}")),
            "{stdout}"
        );
        assert!(
            accepted
                .iter()
                .any(|expected| stdout.lines().any(|line| line == *expected)),
            "tier={tier}, native={native}: definition/capture mismatch: {stdout}"
        );
        assert!(
            stdout
                .lines()
                .any(|line| line == "EFFECTS (:SECOND :FIRST)"),
            "argument effects must run once, left to right: {stdout}"
        );
    }
}

#[test]
fn replacement_during_arguments_keeps_body_capture_and_values_together() {
    // CLHS 3.1.2.1.2.3 permits selecting the named function before or after
    // argument evaluation. Either definition is valid; a mixture is not.
    check_selection(
        "selection-target",
        "(setf (symbol-function 'selection-target) *new-target*)",
        &[
            "RESULT ((:OLD :OLD-CAPTURE 99 101) :OLD-MV)",
            "RESULT ((:NEW :NEW-CAPTURE 99 101) :NEW-MV)",
        ],
    );
}

#[test]
fn unbinding_during_arguments_keeps_selected_capture_alive() {
    check_selection(
        "selection-target",
        "(fmakunbound 'selection-target)",
        &["RESULT ((:OLD :OLD-CAPTURE 99 101) :OLD-MV)", "UNDEFINED"],
    );
}

#[test]
fn explicit_function_object_is_selected_before_later_arguments() {
    check_selection(
        "funcall #'selection-target",
        "(setf (symbol-function 'selection-target) *new-target*)",
        &["RESULT ((:OLD :OLD-CAPTURE 99 101) :OLD-MV)"],
    );
}

#[test]
fn lazy_publication_does_not_attach_selected_old_body_to_replacement() {
    let program = r#"
      (defparameter *effects* nil)
      (setf (symbol-function 'staging-target)
        (lambda (x) (values :new x :new-mv)))
      (defparameter *replacement* (symbol-function 'staging-target))
      ;; Reifying the closure gives it a private name without a function cell.
      ;; Warm its invocation counter through the object, without compiling it
      ;; under the public name that the argument below is about to replace.
      (dotimes (i 12) (funcall *replacement* i))
      (defun lazy-target (x) (values :old x :old-mv))
      (defun lazy-selector ()
        (lazy-target
          (progn (push :argument *effects*)
                 (setf (symbol-function 'lazy-target) *replacement*) 99)))
      (format t "BEFORE ~D~%" (egcl-ext:function-tier 'lazy-target))
      (format t "FIRST ~S~%" (multiple-value-list (lazy-selector)))
      (format t "CURRENT ~S~%" (multiple-value-list (lazy-target 100)))
      (defun lazy-invoke-current (x) (lazy-target x))
      (dotimes (i 40) (lazy-invoke-current i))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'lazy-invoke-current))
      (format t "WARM ~S~%" (multiple-value-list (lazy-invoke-current 101)))
      (format t "EFFECTS ~S~%" *effects*)
    "#;
    for native in ["0", "1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            // Any FORCE_TIER setting disables lazy compilation, even "interp".
            .env_remove("EGCL_FORCE_TIER")
            .env("EGCL_NATIVE_TRANSFER", native)
            .env("EGCL_LAZY_COMPILE", "1")
            .env("EGCL_LAZY_THRESHOLD", "8")
            .env("EGCL_T0_T1_THRESHOLD", "1")
            .env("EGCL_T2_THRESHOLD", "3")
            .output()
            .expect("run lazy call selection regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "native={native}: {stdout}\n{stderr}"
        );
        for expected in [
            "BEFORE 0",
            "CURRENT (:NEW 100 :NEW-MV)",
            "WARM (:NEW 101 :NEW-MV)",
            "EFFECTS (:ARGUMENT)",
        ] {
            assert!(
                stdout.lines().any(|line| line == expected),
                "native={native}: {stdout}"
            );
        }
        assert!(
            stdout
                .lines()
                .any(|line| matches!(line, "FIRST (:OLD 99 :OLD-MV)" | "FIRST (:NEW 99 :NEW-MV)")),
            "native={native}: {stdout}"
        );
        if cfg!(all(target_arch = "x86_64", unix)) {
            assert!(
                stdout.lines().any(|line| line == "TIER 2"),
                "native={native}: {stdout}"
            );
        }
    }
}

#[test]
fn global_definition_uses_its_own_lexical_environment() {
    let program = r#"
      (defun selected-global () hidden-caller-local)
      (let ((definition-local :captured))
        (defun selected-captured () definition-local))
      (let ((hidden-caller-local :caller) (definition-local :caller))
        (format t "SCOPE ~S ~S ~S ~S~%"
          (handler-case (selected-global) (unbound-variable () :unbound))
          (handler-case (funcall #'selected-global) (unbound-variable () :unbound))
          (selected-captured) (funcall #'selected-captured)))
      (setf (symbol-value 'definition-local) :global)
      (locally (declare (special definition-local))
        (format t "SPECIAL-SCOPE ~S ~S~%"
          (selected-captured) (funcall #'selected-captured)))
    "#;
    for tier in ["interp", "t0"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_NATIVE_TRANSFER", "0")
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run selected function lexical scope regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "tier={tier}: {stdout}\n{stderr}");
        for expected in [
            "SCOPE :UNBOUND :UNBOUND :CAPTURED :CAPTURED",
            "SPECIAL-SCOPE :CAPTURED :CAPTURED",
        ] {
            assert!(
                stdout.lines().any(|line| line == expected),
                "tier={tier}: {stdout}"
            );
        }
    }
}
