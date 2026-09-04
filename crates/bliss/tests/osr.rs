//! On-stack replacement (OSR) differential tests (bliss-izt.1/izt.2).
//!
//! A function that is one long-running loop is promoted into native T1 code
//! *mid-execution* — without ever returning and being re-called — once its
//! back-edge count crosses `BLISS_OSR_THRESHOLD`. OSR now speculates: it inlines
//! fixnum arithmetic and, when a guard fails mid-loop, state-transfer deopt
//! (bliss-izt.2) resumes the interpreter at that exact bytecode position on the
//! live frame. The invariant across all of it: entering (and, on a guard
//! failure, leaving) the native loop from the live interpreter frame produces
//! exactly what pure interpretation produces. Each case forces a tiny threshold
//! so OSR fires early, and asserts stdout matches the tree-walker byte for byte.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

/// Run `program` with OSR forced on (threshold 50) and under the pure
/// tree-walker; assert identical stdout and exit status.
fn assert_osr_matches(program: &str) {
    let osr = Command::new(BIN)
        .args(["--eval", program])
        .env("BLISS_OSR_THRESHOLD", "50")
        .output()
        .expect("spawn osr");
    let tw = Command::new(BIN)
        .args(["--eval", program])
        .env("BLISS_BACKEND", "tree-walker")
        .output()
        .expect("spawn tw");
    assert_eq!(
        String::from_utf8_lossy(&osr.stdout),
        String::from_utf8_lossy(&tw.stdout),
        "OSR output must match interpretation:\n  {program}"
    );
    assert_eq!(
        osr.status.success(),
        tw.status.success(),
        "exit mismatch: {program}"
    );
}

#[test]
fn osr_matches_interpretation_across_loop_shapes() {
    let cases: &[&str] = &[
        // Counted accumulator: the canonical hot loop.
        "(defun sumto (n) (let ((s 0) (i 0)) \
           (block d (tagbody top (when (>= i n) (return-from d s)) \
             (setq s (+ s i)) (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (sumto 5000))",
        // dotimes (lowers to block/let/tagbody/go) with a result form.
        "(defun f (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (* i i)))))) \
         (format t \"~a~%\" (f 3000))",
        // A multiply-heavy hot loop promoted through speculating OSR (bliss-izt.2
        // inlines the fixnum `*`/`+`); the running sum stays in fixnum range, so
        // it runs entirely native. Result printed mod a fixnum.
        "(defun bigsum (n) (let ((s 0) (i 1)) \
           (block d (tagbody top (when (> i n) (return-from d (mod s 1000000007))) \
             (setq s (+ s (* i i i))) (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (bigsum 4000))",
        // Overflow AFTER OSR fires (bliss-izt.2): with threshold 50 the loop is
        // promoted to speculating native at iteration 50, then `s` overflows the
        // fixnum range around iteration ~115. Each overflow makes the inlined `+`
        // guard fail and state-transfer deopt resumes T0 mid-loop at that exact
        // `CallNamed`, finishing in bignum arithmetic — the value must still match
        // the tree-walker. Exercises the OsrOutcome::Deopt resume path directly.
        "(defun ov (n) (let ((s 0) (i 0)) \
           (block d (tagbody top (when (>= i n) (return-from d (mod s 1000000007))) \
             (setq s (+ s 10000000000000000)) (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (ov 200))",
        // Loop that builds a heap list via CONS (c2i allocation inside native).
        "(defun rng (n) (let ((acc nil) (i 0)) \
           (block d (tagbody top (when (>= i n) (return-from d acc)) \
             (setq acc (cons i acc)) (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (length (rng 2000)))",
        // Early return carrying a computed value out of a hot loop.
        "(defun firstbig (n) (let ((i 0)) \
           (block d (tagbody top (when (> (* i i) n) (return-from d i)) \
             (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (firstbig 10000000))",
        // A hot loop calling another user function (crosses c2i to the
        // interpreter each iteration).
        "(defun sq (x) (* x x)) \
         (defun sos (n) (let ((s 0) (i 0)) \
           (block d (tagbody top (when (>= i n) (return-from d s)) \
             (setq s (+ s (sq i))) (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (sos 3000))",
        // Nested loops: the inner loop is the hot one.
        "(defun mul (a b) (let ((s 0) (i 0)) \
           (tagbody io (when (< i a) \
             (let ((j 0)) (tagbody jo (when (< j b) (setq s (+ s 1)) (setq j (+ j 1)) (go jo)))) \
             (setq i (+ i 1)) (go io))) \
           s)) \
         (format t \"~a~%\" (mul 200 200))",
        // Single-float accumulator (bliss-izt.3): the SECOND speculation. `s` is
        // an immediate single-float, so the fixnum guard on `(+ s ...)` / `(* ...)`
        // / `(- ...)` fails and the inlined single-float path runs — WITHOUT
        // deopting — while the fixnum index `i` and the `(>= i n)` test take the
        // fixnum path. All three float ops (addss/subss/mulss) execute native, and
        // f32 rounding must match the interpreter's f32 arithmetic bit for bit.
        "(defun fops (n) (let ((s 0.0) (i 0)) \
           (block d (tagbody top (when (>= i n) (return-from d s)) \
             (setq s (+ s 2.0)) (setq s (* s 1.001)) (setq s (- s 0.5)) \
             (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (fops 3000))",
        // Both float-fast and the single-float guard's deopt fallback in one
        // loop, all through deopt-safe primitives so speculation stays on:
        // `(+ i 0.5)` mixes a fixnum with a single-float, so BOTH the fixnum and
        // the single-float guard miss and it deopts to T0 (fixnum⊕float ⇒
        // single-float by contagion); `(+ s <that>)` then takes the native
        // single-float path. The interleaved deopt must still land on the exact
        // interpreted value.
        "(defun fmix (n) (let ((s 0.0) (i 0)) \
           (block d (tagbody top (when (>= i n) (return-from d s)) \
             (setq s (+ s (+ i 0.5))) (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (fmix 400))",
    ];
    for c in cases {
        assert_osr_matches(c);
    }
}

/// PROMOTION must actually happen — output parity alone cannot see a dead OSR
/// path (bliss-j1o7: the main dispatch path never threaded the registry
/// symbol, so `maybe_osr` rejected every back-edge and every one of these
/// differential tests still passed). `bliss-ext:function-osr-count` counts
/// real native OSR entries (FUNCTION-TIER does not reflect OSR), so assert a
/// cold first-call hot loop enters native code at least once (bliss-f88w).
#[test]
fn cold_hot_loop_enters_native_via_osr() {
    let prog = "(defun sumto (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s i))))) \
       (format t \"~a ~a~%\" (sumto 5000) (bliss-ext:function-osr-count 'sumto))";
    let out = Command::new(BIN)
        .args(["--eval", prog])
        .env("BLISS_OSR_THRESHOLD", "50")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("12497500 1"),
        "cold hot loop must compute the right sum AND enter OSR native code \
         exactly once (got: {stdout:?})"
    );
}

/// The same regression guard at the DEFAULT OSR threshold: the tiny-threshold
/// tests exercise the machinery, but bliss-j1o7 would have slipped past them
/// even if they had asserted promotion under an overridden threshold only.
/// 200k iterations cross the default 100k back-edge threshold mid-run.
#[test]
fn cold_hot_loop_enters_native_at_default_threshold() {
    let prog = "(defun dsum (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s i))))) \
       (format t \"~a ~a~%\" (dsum 200000) (bliss-ext:function-osr-count 'dsum))";
    let out = Command::new(BIN)
        .args(["--eval", prog])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("19999900000 1"),
        "default-threshold hot loop must OSR exactly once (got: {stdout:?})"
    );
}

/// A deeply-iterating loop that raises a catchable condition inside the loop
/// body (via a c2i error) must surface identically under OSR and interpretation.
#[test]
fn osr_error_inside_hot_loop_is_catchable() {
    let prog = "(defun f (n) \
        (let ((i 0)) \
          (handler-case \
            (block d (tagbody top (when (>= i n) (return-from d :done)) \
              (setq i (+ i 1)) \
              (when (= i 3000) (car 5)) \
              (go top))) \
            (type-error () :caught)))) \
       (format t \"~a~%\" (f 10000000))";
    assert_osr_matches(prog);
}
