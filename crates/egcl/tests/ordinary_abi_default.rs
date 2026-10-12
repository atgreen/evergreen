// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native Lisp bodies use the ordinary register ABI at both installed tiers.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

fn check_native(program: &str, functions: &[&str], tier: &str) {
    let mut source = program.to_owned();
    for function in functions {
        source.push_str(&format!(
            "\n(format t \"ABI-LISTING-{function}-BEGIN~%\")\n\
                 (disassemble #'{function})\n\
                 (format t \"ABI-LISTING-{function}-END~%\")\n"
        ));
    }
    source.push_str("(format t \"ORDINARY-ABI-RESULTS-OK~%\")");
    let output = Command::new("timeout")
        .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", &source])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1")
        .env_remove("EGCL_NATIVE_TRANSFER")
        .env_remove("EGCL_NO_DIRECT_SELF_CALL")
        .output()
        .expect("run ordinary ABI acceptance program");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("ORDINARY-ABI-RESULTS-OK"),
        "{tier}: {stdout}"
    );
    for function in functions {
        let listing = stdout
            .split_once(&format!("ABI-LISTING-{function}-BEGIN\n"))
            .expect("listing start")
            .1
            .split_once(&format!("ABI-LISTING-{function}-END"))
            .expect("listing end")
            .0;
        assert!(
            listing.contains(&format!("; {} —", tier.to_uppercase())),
            "{function} did not reach {tier}: {listing}"
        );
        assert!(
            listing.contains("Ordinary x86-64 Lisp ABI — installed native code."),
            "{function} at {tier} does not use the ordinary ABI: {listing}"
        );
        // Check decoded instructions as well as the descriptor's ABI label.
        assert!(listing.contains("push rbp"), "{listing}");
        assert!(listing.contains("mov rbp,rsp"), "{listing}");
        assert!(listing.contains("r13"), "{listing}");
        assert!(
            !listing.contains("Mapped native transfer ABI —"),
            "{listing}"
        );
    }
}

fn recursive_fibonacci_uses_ordinary_abi_at_both_native_tiers(tier: &str) {
    check_native(
        r#"
        (defun abi-fib (n)
          (if (< n 2) n (+ (abi-fib (- n 1)) (abi-fib (- n 2)))))
        (dotimes (i 40) (assert (= (abi-fib 10) 55)))
        (assert (= (abi-fib 0) 0))
        (assert (= (abi-fib 1) 1))
        (assert (= (abi-fib 12) 144))
        "#,
        &["abi-fib"],
        tier,
    );
}

fn branching_cross_function_continuations_use_ordinary_abi(tier: &str) {
    check_native(
        r#"
        (defvar *abi-effects* 0)
        (defun abi-left (x) (incf *abi-effects*) (+ x 10))
        (defun abi-right (x) (incf *abi-effects*) (* x 3))
        (defun abi-branch (x)
          (if (< x 0)
              (+ (abi-left x) (abi-right (- x)))
              (+ (abi-right x) (abi-left (+ x 1)))))
        (dotimes (i 40)
          (assert (= (abi-branch -2) 14))
          (assert (= (abi-branch 2) 19)))
        (assert (= *abi-effects* 160))
        (setq *abi-effects* 0)
        (assert (= (abi-branch -3) 16))
        (assert (= (abi-branch 3) 23))
        (assert (= *abi-effects* 4))
        "#,
        &["abi-branch"],
        tier,
    );
}

fn overflow_arguments_keep_their_order_across_native_calls(tier: &str) {
    check_native(
        r#"
        (defun abi-six (a b c d e f)
          (+ a (* 2 b) (* 3 c) (* 4 d) (* 5 e) (* 6 f)))
        (defun abi-six-caller (x) (abi-six x 2 3 4 5 6))
        (dotimes (i 40)
          (assert (= (abi-six 1 2 3 4 5 6) 91))
          (assert (= (abi-six-caller 1) 91)))
        (assert (= (abi-six 6 5 4 3 2 1) 56))
        (assert (= (abi-six-caller 10) 100))
        "#,
        &["abi-six", "abi-six-caller"],
        tier,
    );
}

fn ordinary_indirect_calls_preserve_captured_state_and_multiple_values(tier: &str) {
    check_native(
        r#"
        (defun abi-maker (seed)
          (let ((hits 0))
            (lambda (delta)
              (incf hits)
              (incf seed delta)
              (values seed hits))))
        (defun abi-indirect (f x) (funcall f x))
        (let ((a (abi-maker 10)) (b (abi-maker 100)))
          (dotimes (i 40)
            (assert (equal (multiple-value-list (abi-indirect a 2))
                           (list (+ 12 (* i 2)) (+ i 1)))))
          (assert (equal (multiple-value-list (abi-indirect b 3)) '(103 1)))
          (assert (equal (multiple-value-list (abi-indirect a 1)) '(91 41))))
        "#,
        &["abi-indirect"],
        tier,
    );
}

fn ordinary_callers_preserve_optional_defaults_and_rest_arguments(tier: &str) {
    check_native(
        r#"
        (defun abi-options (a &optional (b 7 supplied) &rest rest)
          (values (+ a b (length rest)) supplied rest))
        (defun abi-default (x) (abi-options x))
        (defun abi-rest (x) (abi-options x 2 3 4 5))
        (dotimes (i 40)
          (assert (equal (multiple-value-list (abi-default 1)) '(8 nil nil)))
          (assert (equal (multiple-value-list (abi-rest 1)) '(6 t (3 4 5)))))
        (assert (equal (multiple-value-list (abi-default 10)) '(17 nil nil)))
        (assert (equal (multiple-value-list (abi-rest 10)) '(15 t (3 4 5))))
        "#,
        &["abi-default", "abi-rest"],
        tier,
    );
}

macro_rules! at_both_tiers {
    ($case:ident, $t1:ident, $t2:ident) => {
        #[test]
        fn $t1() {
            $case("t1");
        }
        #[test]
        fn $t2() {
            $case("t2");
        }
    };
}

at_both_tiers!(
    recursive_fibonacci_uses_ordinary_abi_at_both_native_tiers,
    fib_t1,
    fib_t2
);

at_both_tiers!(
    branching_cross_function_continuations_use_ordinary_abi,
    branch_t1,
    branch_t2
);

at_both_tiers!(
    overflow_arguments_keep_their_order_across_native_calls,
    overflow_t1,
    overflow_t2
);

at_both_tiers!(
    ordinary_indirect_calls_preserve_captured_state_and_multiple_values,
    indirect_t1,
    indirect_t2
);

at_both_tiers!(
    ordinary_callers_preserve_optional_defaults_and_rest_arguments,
    optional_rest_t1,
    optional_rest_t2
);

fn ordinary_primitives_preserve_global_and_cons_semantics(tier: &str) {
    check_native(
        r#"
        (defvar *abi-global* 10)
        (defun abi-global-read () *abi-global*)
        (defun abi-global-store (x) (setq *abi-global* x))
        (defun abi-pair (x) (cons x (cons (+ x 1) nil)))
        (dotimes (i 40)
          (assert (= (abi-global-store i) i))
          (assert (= (abi-global-read) i))
          (assert (equal (abi-pair i) (list i (+ i 1)))))
        (assert (= (abi-global-store 73) 73))
        (assert (= (abi-global-read) 73))
        (assert (equal (abi-pair 20) '(20 21)))
        "#,
        &["abi-global-read", "abi-global-store", "abi-pair"],
        tier,
    );
}
at_both_tiers!(
    ordinary_primitives_preserve_global_and_cons_semantics,
    primitives_t1,
    primitives_t2
);
