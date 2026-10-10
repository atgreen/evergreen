// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

// ASSERT/SETF macro expansion itself calls CAR and APPLY. Capture results while
// the builtin is replaced, then unbind the replacement before expanding checks.
fn check_rebinding(name: &str, call: &str, initial: &str) {
    let program = format!(
        r#"
          (defclass rebound-callable () () (:metaclass egcl-ext:funcallable-standard-class))
          (defparameter *replacement* (make-instance 'rebound-callable))
          (egcl-ext:set-funcallable-instance-function *replacement*
            (lambda (&rest args) (declare (ignore args)) 110))
          (defun original-callback (x) (+ x 1000))
          (defun ordinary-target (x) (+ x 1000))
          (defun warmed-caller () {call})
          (defun cold-caller () {call})
          (dotimes (i 40) (assert (eql (warmed-caller) {initial})))
          (assert (= (egcl-ext:function-tier 'warmed-caller) EXPECTED-TIER))
          (setf (symbol-function '{name}) *replacement*)
          (defun installed-after-rebinding () {call})
          (setq *compiled-after-result* (installed-after-rebinding)
                *warm-result* (warmed-caller)
                *warm-again* (warmed-caller)
                *cold-result* (cold-caller)
                *symbol-result* (symbol-function '{name})
                *definition-result* (fdefinition '{name})
                *function-result* #'{name})
          (fmakunbound '{name})
          (assert (= *compiled-after-result* 110))
          (assert (= *warm-result* 110))
          (assert (= *warm-again* 110))
          (assert (= *cold-result* 110))
          (assert (eq *symbol-result* *replacement*))
          (assert (eq *definition-result* *replacement*))
          (assert (eq *function-result* *replacement*))
          (egcl-ext:set-funcallable-instance-function *replacement*
            (lambda (&rest args) (declare (ignore args)) 210))
          (setf (symbol-function '{name}) *replacement*)
          (egcl-ext:gc :full t)
          (setq *updated-result* (warmed-caller))
          (fmakunbound '{name})
          (assert (= *updated-result* 210))
          (format t "CALLABLE-REBINDING-OK~%")
        "#
    );
    run_tiers(name, &program);
}

fn run_tiers(name: &str, program: &str) {
    for (tier, native) in [
        ("interp", "0"),
        ("t0", "0"),
        ("t1", "0"),
        ("t2", "0"),
        ("t2", "1"),
    ] {
        if tier == "t2" && !cfg!(all(target_arch = "x86_64", unix)) {
            continue;
        }
        let expected_tier = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "0",
        };
        let program = program.replace("EXPECTED-TIER", expected_tier);
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_NATIVE_TRANSFER", native)
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("CALLABLE-REBINDING-OK"),
            "{name}, tier={tier}, native={native}: {}\n{stdout}\n{stderr}",
            output.status
        );
    }
}

#[test]
fn callable_instance_replaces_funcall() {
    check_rebinding("funcall", "(funcall #'original-callback 10)", "1010");
}

#[test]
fn callable_instance_replaces_apply() {
    check_rebinding("apply", "(apply #'original-callback '(10))", "1010");
}

#[test]
fn callable_instance_replaces_direct_builtin() {
    check_rebinding("car", "(car '(10))", "10");
}

#[test]
fn callable_instance_replaces_compiled_definition() {
    check_rebinding("ordinary-target", "(ordinary-target 10)", "1010");
}

#[test]
fn callable_instance_aliases_preserve_identity_and_multiple_values() {
    run_tiers(
        "aliases",
        r#"
      (defclass alias-callable () () (:metaclass egcl-ext:funcallable-standard-class))
      (defparameter *alias-instance* (make-instance 'alias-callable))
      (egcl-ext:set-funcallable-instance-function *alias-instance*
        (lambda (a b c d) (values (+ a b c d) :instance)))
      (defun alias-target (a b c d) (declare (ignore a b c d)) -1)
      (defun alias-caller () (alias-target 1 2 3 4))
      (dotimes (i 40) (assert (= (alias-caller) -1)))
      (assert (= (egcl-ext:function-tier 'alias-caller) EXPECTED-TIER))
      (setf (symbol-function 'alias-target) *alias-instance*)
      (setf (symbol-function 'fresh-alias) (symbol-function 'alias-target))
      (defparameter *private-alias* (gensym "ALIAS"))
      (setf (symbol-function *private-alias*) *alias-instance*)
      (dolist (name (list 'alias-target 'fresh-alias *private-alias*))
        (assert (fboundp name))
        (assert (eq (symbol-function name) *alias-instance*))
        (assert (equal (multiple-value-list (funcall name 1 2 3 4)) '(10 :instance)))
        (assert (equal (multiple-value-list (apply name '(1 2 3 4))) '(10 :instance))))
      (dotimes (i 40)
        (assert (equal (multiple-value-list (alias-caller)) '(10 :instance))))
      (assert (equal (multiple-value-list
        (eval (list *private-alias* 1 2 3 4))) '(10 :instance)))
      (flet ((alias-target (&rest args) (declare (ignore args)) :lexical))
        (assert (eq (alias-target) :lexical))
        (assert (= (funcall 'alias-target 1 2 3 4) 10)))
      (fmakunbound 'alias-target)
      (assert (not (fboundp 'alias-target)))
      (assert (= (funcall *alias-instance* 1 2 3 4) 10))
      (format t "CALLABLE-REBINDING-OK~%")
    "#,
    );
}

#[test]
fn builtin_rebinding_during_a_native_activation_does_not_replay_effects() {
    run_tiers(
        "live replacement",
        r#"
      (defclass live-callable () () (:metaclass egcl-ext:funcallable-standard-class))
      (defparameter *live-replacement* (make-instance 'live-callable))
      (egcl-ext:set-funcallable-instance-function *live-replacement* (lambda (x) 110))
      (defparameter *armed* nil)
      (defparameter *effects* 0)
      (defun install-if-armed ()
        (if *armed* (egcl::set-symbol-function 'car *live-replacement*) nil))
      (defun changing-caller (x)
        (setq *effects* (1+ *effects*))
        (install-if-armed)
        (car x))
      (dotimes (i 40) (assert (= (changing-caller '(10)) 10)))
      (assert (= (egcl-ext:function-tier 'changing-caller) EXPECTED-TIER))
      (setq *armed* t *effects* 0)
      (setq *actual* (changing-caller '(10)))
      (fmakunbound 'car)
      (assert (= *actual* 110))
      (assert (= *effects* 1))
      (format t "CALLABLE-REBINDING-OK~%")
    "#,
    );
}

#[test]
fn callable_instance_replaces_constant_predicate() {
    check_rebinding("null", "(null nil)", "t");
}

#[test]
fn callable_instance_replaces_speculated_arithmetic() {
    check_rebinding("+", "(+ 10 20)", "30");
}

#[test]
fn legacy_self_symbol_binding_keeps_builtin_dispatch() {
    run_tiers(
        "self symbol",
        r#"
      (setf (symbol-function 'car) 'car)
      (setq *direct* (car '(10))
            *indirect* (funcall 'car '(20)))
      (fmakunbound 'car)
      (assert (= *direct* 10))
      (assert (= *indirect* 20))
      (format t "CALLABLE-REBINDING-OK~%")
    "#,
    );
}
