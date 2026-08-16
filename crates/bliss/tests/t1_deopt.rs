//! T1 speculative deoptimization (bliss-jtc.27) — the second half of the S5
//! gate: "an invalidated speculation deoptimizes and still returns the correct
//! result."
//!
//! Pure functions promote to T1 with inlined fixnum arithmetic (+ - * and the
//! comparisons), guarded on fixnum operands and no overflow. When a guard fails
//! — a float/ratio/bignum operand, or a result that overflows the 61-bit fixnum
//! range — the native code deoptimizes and the interpreter re-runs the call,
//! producing the value the tree-walker would. These tests drive the shipping
//! binary and compare against pure interpretation.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

fn eval(program: &str, envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(BIN);
    cmd.args(["--eval", program]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn bliss-cli");
    String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string()
}

/// The T1 (threshold=1) output must equal the tree-walker output.
fn assert_matches(program: &str) {
    let tw = eval(program, &[("BLISS_BACKEND", "tree-walker")]);
    let t1 = eval(program, &[("BLISS_T1_THRESHOLD", "1")]);
    assert_eq!(tw, t1, "T1 must match interpretation for:\n  {program}");
}

/// Speculative fixnum arithmetic across signs, zero, and all comparison
/// operators produces results identical to interpretation.
#[test]
fn speculative_fixnum_arithmetic_matches() {
    let cases = &[
        "(defun f (a b) (+ a b)) (f 1 1) (format t \"~a~%\" (list (f 3 4) (f -3 4) (f -5 -5) (f 0 0)))",
        "(defun f (a b) (- a b)) (f 1 1) (format t \"~a~%\" (list (f 3 4) (f -3 4) (f 0 7)))",
        "(defun f (a b) (* a b)) (f 1 1) (format t \"~a~%\" (list (f 3 4) (f -3 4) (f -5 -5) (f 0 9)))",
        "(defun f (a b) (list (< a b) (> a b) (<= a b) (>= a b) (= a b))) (f 1 1) \
           (format t \"~a~%\" (list (f 3 4) (f 4 3) (f 5 5) (f -9 3)))",
    ];
    for c in cases {
        assert_matches(c);
    }
}

/// A fixnum-overflowing result deoptimizes and yields the correct bignum. The
/// bignum is reduced mod a fixnum so the comparison is over printable values
/// (bignums print opaquely in both backends).
#[test]
fn overflow_deoptimizes_to_correct_bignum() {
    let cases = &[
        // (+ (2^60-1) 1) = 2^60 overflows the 61-bit fixnum range.
        "(defun f (a b) (mod (+ a b) 1000000)) (f 1 1) \
           (format t \"~a~%\" (f 1152921504606846975 1))",
        // squaring a ~9.2e18 value overflows.
        "(defun sq (x) (mod (* x x) 1000000)) (sq 2) \
           (format t \"~a~%\" (sq 3037000500))",
        // most-negative-fixnum negated overflows (no positive counterpart).
        "(defun f (a b) (mod (* a b) 1000000)) (f 1 1) \
           (format t \"~a~%\" (f -1152921504606846976 -1))",
    ];
    for c in cases {
        assert_matches(c);
    }
}

/// Non-fixnum operands (float, ratio) fail the type guard and deoptimize to the
/// interpreter's generic arithmetic, still correct.
#[test]
fn non_fixnum_operands_deoptimize() {
    let cases = &[
        "(defun g (a b) (+ (* a a) (* b b))) (g 1 1) (format t \"~a~%\" (g 1.5 2.0))",
        "(defun h (a b) (< a b)) (h 1 1) (format t \"~a~%\" (list (h 1/3 1/2) (h 1/2 1/3)))",
        // fixnum and float mixed.
        "(defun k (a b) (* a b)) (k 1 1) (format t \"~a~%\" (k 3 2.5))",
    ];
    for c in cases {
        assert_matches(c);
    }
}

/// The deopt counter is observable and increments only on an actual
/// deoptimization: zero for fixnum calls, one per non-fixnum/overflow call.
#[test]
fn deopt_count_is_observable_and_precise() {
    let program = "\
        (defun sq (x) (* x x)) \
        (sq 2) (sq 3) (sq 4) \
        (sq 5) (sq 6) \
        (format t \"~a\" (bliss-ext:deopt-count)) \
        (sq 1.5) \
        (format t \" ~a\" (bliss-ext:deopt-count)) \
        (sq 9999999999) \
        (format t \" ~a~%\" (bliss-ext:deopt-count))";
    let out = eval(program, &[("BLISS_T1_THRESHOLD", "2")]);
    assert_eq!(
        out, "0 1 2",
        "fixnum calls deopt 0 times; a float arg then an overflow each add one"
    );
}

/// A hot loop whose body is speculative fixnum arithmetic promotes to T1, runs
/// entirely on the fast path (no deopts), and returns the interpreter's value.
#[test]
fn hot_speculative_loop_no_deopt() {
    let program = "\
        (defun sumsq (n) (let ((s 0) (i 0)) \
          (tagbody top (when (< i n) (setq s (+ s (* i i))) (setq i (+ i 1)) (go top))) \
          s)) \
        (sumsq 5) (sumsq 5) (sumsq 5) \
        (format t \"~a ~a ~a~%\" \
                (bliss-ext:function-tier (quote sumsq)) \
                (bliss-ext:deopt-count) \
                (sumsq 100))";
    let out = eval(program, &[("BLISS_T1_THRESHOLD", "2")]);
    let fields: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(fields.first().copied(), Some("1"), "loop must reach T1: {out:?}");
    assert_eq!(fields.get(1).copied(), Some("0"), "pure fixnum loop must not deopt: {out:?}");
    // sum of i*i for i in 0..99 = 328350
    assert_eq!(fields.get(2).copied(), Some("328350"), "result must match interpretation");
}
