//! T1 speculative deoptimization (bliss-jtc.27) — the second half of the S5
//! gate: "an invalidated speculation deoptimizes and still returns the correct
//! result."
//!
//! Pure functions promote to T1 with inlined fixnum arithmetic (+ - * and the
//! comparisons), guarded on fixnum operands and no overflow. Binary + - * also
//! have a native single-float fast path (bliss-izt.3), so single-float operands
//! run native rather than deopting. When neither guard applies — a ratio/bignum
//! operand, a fixnum⊕float mix, a float comparison, or a result that overflows
//! the 61-bit fixnum range — the native code deoptimizes and the interpreter
//! re-runs the call, producing the value the tree-walker would. These tests
//! drive the shipping binary and compare against pure interpretation.

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

/// Unary 1+, 1-, and negation have inlined fixnum fast paths that match
/// interpretation across signs and zero, and deoptimize correctly at the
/// overflow edge (incrementing the max fixnum, negating the most-negative
/// fixnum) and for non-fixnum operands (bliss-jtc.27).
#[test]
fn unary_fixnum_ops_match_and_deopt_at_edges() {
    let cases = &[
        "(defun f (x) (1+ x)) (f 0) (format t \"~a~%\" (list (f 5) (f -3) (f 0)))",
        "(defun f (x) (1- x)) (f 0) (format t \"~a~%\" (list (f 5) (f -3) (f 0)))",
        "(defun f (x) (- x)) (f 0) (format t \"~a~%\" (list (f 5) (f -7) (f 0)))",
        // overflow: (1+ (2^60-1)) and (- most-negative-fixnum) deopt to bignum,
        // reduced mod a fixnum for a printable comparison.
        "(defun f (x) (mod (1+ x) 1000000)) (f 0) (format t \"~a~%\" (f 1152921504606846975))",
        "(defun f (x) (mod (- x) 1000000)) (f 0) (format t \"~a~%\" (f -1152921504606846976))",
        // non-fixnum operand deopts to generic negation.
        "(defun f (x) (- x)) (f 0) (format t \"~a~%\" (f 2.5))",
    ];
    for c in cases {
        assert_matches(c);
    }
}

/// Inlined fixnum predicates (zerop/plusp/minusp/evenp/oddp) match
/// interpretation across sign and parity, and deopt for non-fixnum operands
/// (bliss-jtc.27).
#[test]
fn fixnum_predicates_match_interpretation() {
    let cases = &[
        "(defun f (x) (list (zerop x) (plusp x) (minusp x) (evenp x) (oddp x))) (f 0) \
           (format t \"~a~%\" (list (f 0) (f 5) (f -4) (f 7) (f -6)))",
        // predicate driving a branch and a counting loop.
        "(defun cnt (n) (let ((c 0) (i 0)) \
           (loop (when (>= i n) (return c)) (when (oddp i) (setq c (1+ c))) (setq i (1+ i))))) \
         (cnt 3) (format t \"~a~%\" (list (cnt 10) (cnt 7) (cnt 0)))",
        // non-fixnum operands deopt to the interpreter's generic predicates.
        "(defun f (x) (list (zerop x) (plusp x) (minusp x))) (f 0) \
           (format t \"~a~%\" (list (f 0.0) (f 1.5) (f -2.5)))",
    ];
    for c in cases {
        assert_matches(c);
    }
}

/// Inlined car/cdr/first/rest (bliss-jtc.27): NIL yields NIL, a cons loads the
/// field, and a non-list operand deoptimizes to the interpreter's type error —
/// crucially without dereferencing a non-cons (no segfault). Matches
/// interpretation, and a list-walking loop promotes to T1.
#[test]
fn cons_accessors_match_and_deopt_safely() {
    let cases = &[
        "(defun f (x) (car x)) (f (list 1)) \
           (format t \"~a~%\" (list (f (list 1 2 3)) (f nil) (f (cons 7 8))))",
        "(defun f (x) (cdr x)) (f (list 1)) \
           (format t \"~a~%\" (list (f (list 1 2 3)) (f nil) (f (cons 7 8))))",
        "(defun f (x) (list (first x) (rest x))) (f (list 1)) \
           (format t \"~a~%\" (list (f (list 9 8 7)) (f nil)))",
        // list-summing loop with inlined car/cdr.
        "(defun s (lst) (let ((a 0)) \
           (loop (when (null lst) (return a)) (setq a (+ a (car lst))) (setq lst (cdr lst))))) \
         (s (list 1)) (format t \"~a~%\" (s (list 10 20 30 40)))",
        // non-list operand: the interpreter's type error, reached via deopt, not
        // a crash — both backends print the same error line.
        "(defun f (x) (car x)) (f (list 1)) (format t \"~a~%\" (f 5))",
    ];
    for c in cases {
        assert_matches(c);
    }
}

/// Total predicates null/not/consp/atom and eq inline as compare+cmov with no
/// guard and no deopt — correct for every operand type (bliss-jtc.27). A
/// realistic list loop using them promotes to T1.
#[test]
fn total_predicates_and_eq_match_interpretation() {
    let cases = &[
        "(defun f (x) (list (null x) (not x) (consp x) (atom x))) (f 1) \
           (format t \"~a~%\" (list (f nil) (f (list 1)) (f 5) (f 'a)))",
        "(defun f (a b) (eq a b)) (f 1 1) \
           (format t \"~a~%\" (list (f 'x 'x) (f 1 1) (f (list 1) (list 1)) (f nil nil)))",
        // length via null/1+/cdr, and membership via null/eq/car/cdr — both
        // fully inlined list loops.
        "(defun len (lst) (let ((n 0)) \
           (loop (when (null lst) (return n)) (setq n (1+ n)) (setq lst (cdr lst))))) \
         (len (list 1)) (format t \"~a~%\" (list (len (list 1 2 3 4 5)) (len nil)))",
        "(defun mem (x lst) \
           (loop (when (null lst) (return nil)) (when (eq x (car lst)) (return t)) \
                 (setq lst (cdr lst)))) \
         (mem 1 (list 1)) \
         (format t \"~a~%\" (list (mem 'c (list 'a 'b 'c)) (mem 'z (list 'a 'b))))",
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

/// Operands outside the fixnum/single-float fast paths (ratio, fixnum⊕float mix,
/// float comparison) fall back to the interpreter's generic arithmetic, still
/// correct. (A pure single-float `*`/`+`/`-` instead runs native — see
/// `hot_single_float_loop_no_deopt`.)
#[test]
fn non_fixnum_operands_deoptimize() {
    let cases = &[
        // pure single-float `+`/`*`: native fast path, result still correct.
        "(defun g (a b) (+ (* a a) (* b b))) (g 1 1) (format t \"~a~%\" (g 1.5 2.0))",
        // float comparison: no float compare fast path, so it deopts.
        "(defun h (a b) (< a b)) (h 1 1) (format t \"~a~%\" (list (h 1/3 1/2) (h 1/2 1/3)))",
        // fixnum and float mixed: the single-float guard misses, so it deopts.
        "(defun k (a b) (* a b)) (k 1 1) (format t \"~a~%\" (k 3 2.5))",
    ];
    for c in cases {
        assert_matches(c);
    }
}

/// A hot loop whose accumulator is a single-float DEOPTS under T1's fixnum-only
/// speculation — with no profile, T1 guesses fixnum and a float operand abandons
/// the native path to the interpreter — yet still returns the interpreter's exact
/// result. This float-hot case is precisely what the profiler observes so the
/// optimising tier can later commit to *float*; T1 itself never speculates both.
#[test]
fn single_float_loop_deopts_but_stays_correct() {
    let program = "\
        (defun fsum (n) (let ((s 0.0) (i 0)) \
          (tagbody top (when (< i n) (setq s (+ s 1.5)) (setq i (+ i 1)) (go top))) \
          s)) \
        (fsum 5) (fsum 5) (fsum 5) \
        (format t \"~a ~a~%\" \
                (bliss-ext:deopt-count) \
                (fsum 100))";
    let out = eval(program, &[("BLISS_T1_THRESHOLD", "2")]);
    let fields: Vec<&str> = out.split_whitespace().collect();
    let deopts: u32 = fields.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    assert!(deopts >= 1, "single-float loop must deopt under fixnum-only T1: {out:?}");
    // 1.5 added 100 times = 150.0 — correct despite the deopt to the interpreter.
    let tw_val = eval(
        "(defun fsum (n) (let ((s 0.0) (i 0)) \
           (tagbody top (when (< i n) (setq s (+ s 1.5)) (setq i (+ i 1)) (go top))) s)) \
         (format t \"~a~%\" (fsum 100))",
        &[("BLISS_BACKEND", "tree-walker")],
    );
    assert_eq!(fields.get(1).copied(), Some(tw_val.as_str()), "result matches interpretation despite deopt");
}

/// The deopt counter is observable and increments only on an actual
/// deoptimization: zero for fixnum calls, one per non-fixnum/overflow call.
#[test]
fn deopt_count_is_observable_and_precise() {
    // T1 speculates fixnum only, so a float, ratio, or bignum arg all deopt. A
    // ratio is used here as the non-overflow deopt (a float would deopt too).
    let program = "\
        (defun sq (x) (* x x)) \
        (sq 2) (sq 3) (sq 4) \
        (sq 5) (sq 6) \
        (format t \"~a\" (bliss-ext:deopt-count)) \
        (sq 1/2) \
        (format t \" ~a\" (bliss-ext:deopt-count)) \
        (sq 9999999999) \
        (format t \" ~a~%\" (bliss-ext:deopt-count))";
    let out = eval(program, &[("BLISS_T1_THRESHOLD", "2")]);
    assert_eq!(
        out, "0 1 2",
        "fixnum calls deopt 0 times; a ratio arg then an overflow each add one"
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

/// A function that keeps deoptimizing (always called outside its speculated
/// fixnum/single-float domain — here, with ratios) is blacklisted once its
/// deopt count crosses the configured threshold: its speculative native code is
/// uninstalled, it drops back to T0, the deopt count stops climbing, and results
/// stay correct (bliss-jtc.27).
#[test]
fn repeated_deopts_blacklist_and_back_off_to_t0() {
    // Ratios (not fixnum, not single-float) deopt on every call; single-float
    // args would instead take the native float path (bliss-izt.3) and never
    // blacklist, so this uses ratios to keep exercising the backoff path.
    let program = "\
        (defun g (a b) (+ (* a a) (* b b))) \
        (g 1 1) (g 2 2) \
        (g 1/2 1/3) (g 1/2 1/3) (g 1/2 1/3) (g 1/2 1/3) (g 1/2 1/3) (g 1/2 1/3) \
        (format t \"~a ~a ~a~%\" \
                (bliss-ext:function-tier (quote g)) \
                (bliss-ext:deopt-count) \
                (g 3.0 4.0))";
    let out = eval(program, &[("BLISS_T1_THRESHOLD", "2"), ("BLISS_DEOPT_BLACKLIST_THRESHOLD", "3")]);
    let fields: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(fields.first().copied(), Some("0"), "blacklisted function drops to T0: {out:?}");
    // The count stops at the threshold: once blacklisted there is no native code
    // left to deopt, so the six float calls produce exactly three deopts.
    assert_eq!(fields.get(1).copied(), Some("3"), "deopt count plateaus at threshold: {out:?}");
    // (+ (* 3.0 3.0) (* 4.0 4.0)) = 25.0, printed as the tree-walker prints it.
    let tw_val = eval(
        "(defun g (a b) (+ (* a a) (* b b))) (format t \"~a~%\" (g 3.0 4.0))",
        &[("BLISS_BACKEND", "tree-walker")],
    );
    assert_eq!(fields.get(2).copied(), Some(tw_val.as_str()), "result stays correct after blacklist");
}

/// Blacklisting is not triggered by a pure fixnum hot loop: it never deopts, so
/// it stays at T1 indefinitely (bliss-jtc.27 — backoff only punishes functions
/// whose speculation actually fails).
#[test]
fn pure_fixnum_loop_is_never_blacklisted() {
    let program = "\
        (defun sumsq (n) (let ((s 0) (i 0)) \
          (tagbody top (when (< i n) (setq s (+ s (* i i))) (setq i (+ i 1)) (go top))) \
          s)) \
        (sumsq 10) (sumsq 10) (sumsq 10) (sumsq 10) (sumsq 10) \
        (format t \"~a ~a ~a~%\" \
                (bliss-ext:function-tier (quote sumsq)) \
                (bliss-ext:deopt-count) \
                (sumsq 100))";
    let out = eval(program, &[("BLISS_T1_THRESHOLD", "2"), ("BLISS_DEOPT_BLACKLIST_THRESHOLD", "3")]);
    let fields: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(fields.first().copied(), Some("1"), "fixnum loop stays T1: {out:?}");
    assert_eq!(fields.get(1).copied(), Some("0"), "fixnum loop never deopts: {out:?}");
    assert_eq!(fields.get(2).copied(), Some("328350"), "result correct");
}
