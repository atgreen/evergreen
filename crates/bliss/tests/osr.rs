//! On-stack replacement (OSR) differential tests (bliss-izt.1).
//!
//! A function that is one long-running loop is promoted into native T1 code
//! *mid-execution* — without ever returning and being re-called — once its
//! back-edge count crosses `BLISS_OSR_THRESHOLD`. The invariant: entering the
//! native loop from the live interpreter frame produces exactly what pure
//! interpretation produces. Each case forces a tiny threshold so OSR fires
//! early, and asserts stdout matches the tree-walker byte for byte.

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
        // The running sum overflows fixnum range into a bignum mid-loop: the
        // non-speculating OSR code must promote via c2i exactly like the
        // interpreter (result printed mod a fixnum so both print a small int).
        "(defun bigsum (n) (let ((s 0) (i 1)) \
           (block d (tagbody top (when (> i n) (return-from d (mod s 1000000007))) \
             (setq s (+ s (* i i i))) (setq i (+ i 1)) (go top))))) \
         (format t \"~a~%\" (bigsum 4000))",
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
    ];
    for c in cases {
        assert_osr_matches(c);
    }
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
