// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check_values(name: &str, call: &str, argument: &str, expected: &str) {
    check_values_in_tiers(
        name,
        call,
        argument,
        expected,
        &[
            ("interp", "0", 0),
            ("t0", "0", 0),
            ("t1", "0", 1),
            ("t2", "0", 2),
            ("t2", "1", 2),
        ],
    );
}

fn check_values_in_tiers(
    name: &str,
    call: &str,
    argument: &str,
    expected: &str,
    tiers: &[(&str, &str, u8)],
) {
    let operand_call = call.replace(" x", &format!(" (values {argument} :extra)"));
    let constant_call = call.replace(" x", &format!(" {argument}"));
    for &(tier, native, expected_tier) in tiers {
        if tier == "t2" && !cfg!(all(target_arch = "x86_64", unix)) {
            continue;
        }
        if tier == "t1" && !cfg!(target_arch = "x86_64") {
            continue;
        }
        let program = format!(
            r#"
          (defparameter *effects* nil)
          (defun prior-many (x) (push :called *effects*) (values 1 2) {call})
          (defun prior-none (x) (values) {call})
          (defun operand-many () {operand_call})
          (defun constant-many () (values 1 2) {constant_call})
          (dotimes (i 40)
            (prior-many {argument}) (prior-none {argument})
            (operand-many) (constant-many))
          (format t "TIERS ~S~%" (mapcar #'egcl-ext:function-tier
            '(prior-many prior-none operand-many constant-many)))
          (format t "RETURNED ~S ~S ~S ~S~%"
            (multiple-value-list (prior-many {argument}))
            (multiple-value-list (prior-none {argument}))
            (multiple-value-list (operand-many))
            (multiple-value-list (constant-many)))
          (assert (equal (multiple-value-list (prior-many {argument})) (list {expected})))
          (assert (equal (multiple-value-list (prior-none {argument})) (list {expected})))
          (assert (equal (multiple-value-list (operand-many)) (list {expected})))
          (assert (equal (multiple-value-list (constant-many)) (list {expected})))
          (setq *effects* nil)
          (setf (symbol-function '{name})
            (lambda (&rest args) (declare (ignore args)) (values 110 :replacement)))
          (setq *replacement-result* (multiple-value-list (prior-many {argument})))
          (fmakunbound '{name})
          (assert (equal *replacement-result* '(110 :replacement)))
          (assert (equal *effects* '(:called)))
          (format t "INTRINSIC-VALUES-OK~%")
        "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_NATIVE_TRANSFER", native)
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run intrinsic multiple-value regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected_value = expected.trim_start_matches('\'').to_uppercase();
        let expected_output = format!("RETURNED ({0}) ({0}) ({0}) ({0})", expected_value,);
        let expected_tiers = format!("TIERS ({0} {0} {0} {0})", expected_tier);
        assert!(
            output.status.success()
                && stdout.contains("INTRINSIC-VALUES-OK")
                && stdout.lines().any(|line| line == expected_output)
                && stdout.lines().any(|line| line == expected_tiers),
            "{name}, argument={argument}, tier={tier}, native={native}: {}\n{stdout}\n{stderr}",
            output.status
        );
    }
}

#[test]
fn symbolp_returns_one_value() {
    check_values("SYMBOLP", "(symbolp x)", ":yes", "t");
}

#[test]
fn integerp_returns_one_value() {
    check_values("INTEGERP", "(integerp x)", "42", "t");
}

#[test]
fn null_returns_one_value() {
    check_values("NULL", "(null x)", "nil", "t");
}

#[test]
fn eq_returns_one_value() {
    check_values("EQ", "(eq x x)", "42", "t");
}

#[test]
fn car_returns_one_value() {
    check_values("CAR", "(car x)", "'(7 . 8)", "7");
}

#[test]
fn cdr_returns_one_value() {
    check_values("CDR", "(cdr x)", "'(7 . 8)", "8");
}

#[test]
fn t1_arithmetic_templates_return_one_value() {
    for (name, call, argument, expected) in [
        ("1+", "(1+ x)", "2", "3"),
        ("ZEROP", "(zerop x)", "0", "t"),
        ("+", "(+ x 2)", "2", "4"),
        ("*", "(* x 2)", "3", "6"),
        ("<", "(< x 4)", "3", "t"),
    ] {
        check_values_in_tiers(
            name,
            call,
            argument,
            expected,
            &[("interp", "0", 0), ("t0", "0", 0), ("t1", "0", 1)],
        );
    }
}

#[test]
fn cons_accessors_preserve_heap_results_and_nil() {
    check_values("CAR", "(car x)", "'((7 . 8))", "'(7 . 8)");
    check_values("CDR", "(cdr x)", "'(7 8 9)", "'(8 9)");
    // T1 has a NIL fast path; the T2 cons specialization deliberately deopts.
    for (name, call) in [("CAR", "(car x)"), ("CDR", "(cdr x)")] {
        check_values_in_tiers(
            name,
            call,
            "nil",
            "nil",
            &[("interp", "0", 0), ("t0", "0", 0), ("t1", "0", 1)],
        );
    }
}

#[test]
fn t2_speculated_arithmetic_returns_one_value() {
    for (name, call, expected) in [
        ("+", "(+ x 2)", "5"),
        ("-", "(- x 2)", "1"),
        ("*", "(* x 2)", "6"),
    ] {
        check_values(name, call, "3", expected);
    }
}

#[test]
fn t2_speculated_unary_arithmetic_returns_one_value() {
    for (name, call, expected) in [
        ("1+", "(1+ x)", "4"),
        ("1-", "(1- x)", "2"),
        ("-", "(- x)", "-3"),
    ] {
        check_values(name, call, "3", expected);
    }
}

#[test]
fn t2_speculated_comparisons_return_one_value() {
    for (name, call) in [
        ("<", "(< x 4)"),
        (">", "(> x 2)"),
        ("<=", "(<= x 3)"),
        (">=", "(>= x 3)"),
        ("=", "(= x 3)"),
    ] {
        check_values(name, call, "3", "t");
    }
}

#[test]
fn t2_speculated_bitwise_calls_return_one_value() {
    for (name, call, expected) in [
        ("LOGAND", "(logand x 6)", "2"),
        ("LOGIOR", "(logior x 6)", "7"),
        ("LOGXOR", "(logxor x 6)", "5"),
        ("LOGNOT", "(lognot x)", "-4"),
    ] {
        check_values(name, call, "3", expected);
    }
}

#[test]
fn t2_speculated_constant_operand_calls_return_one_value() {
    check_values("MOD", "(mod x 4)", "7", "3");
    check_values("ASH", "(ash x 2)", "3", "12");
}

#[test]
fn t2_speculated_not_returns_one_value() {
    check_values("NOT", "(not x)", "nil", "t");
}

#[test]
fn t2_speculated_single_float_calls_return_one_value() {
    for (name, call, argument, expected) in [
        ("+", "(+ x 2.0)", "3.0", "5.0"),
        ("-", "(- x 2.0)", "3.0", "1.0"),
        ("*", "(* x 2.0)", "3.0", "6.0"),
        ("<", "(< x 4.0)", "3.0", "t"),
        ("=", "(= x 3.0)", "3.0", "t"),
        ("+", "(+ x 2)", "3.0", "5.0"),
    ] {
        check_values_in_tiers(
            name,
            call,
            argument,
            expected,
            &[("interp", "0", 0), ("t2", "0", 2), ("t2", "1", 2)],
        );
    }
}
