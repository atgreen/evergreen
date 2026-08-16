//! T1 codegen-execution tests (bliss-nmq.2): hot bytecode functions compile to
//! native code, are called through the i2c adapter, call back into the
//! interpreter via c2i, and produce results identical to pure interpretation.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

/// Run a program forcing T1 promotion on the first call (threshold 1), and again
/// under the pure tree-walker; assert identical stdout + exit status.
fn assert_t1_matches(program: &str) {
    let tw = Command::new(BIN)
        .args(["--eval", program])
        .env("BLISS_BACKEND", "tree-walker")
        .output()
        .expect("spawn");
    let t1 = Command::new(BIN)
        .args(["--eval", program])
        .env("BLISS_T1_THRESHOLD", "1")
        .output()
        .expect("spawn");
    assert_eq!(
        String::from_utf8_lossy(&tw.stdout),
        String::from_utf8_lossy(&t1.stdout),
        "T1 output must match interpretation:\n  {program}"
    );
    assert_eq!(tw.status.success(), t1.status.success(), "exit mismatch: {program}");
}

#[test]
fn t1_native_execution_matches_interpretation() {
    let cases = &[
        // Leaf functions (call only primitives via c2i) promote to native.
        "(defun sq (x) (* x x)) (list (sq 2) (sq 7) (sq 100))",
        "(defun f (a b c) (+ (* a b) c)) (list (f 2 3 4) (f 5 6 7))",
        // Branches in native code.
        "(defun sgn (n) (if (< n 0) -1 (if (= n 0) 0 1))) (list (sgn -5) (sgn 0) (sgn 7))",
        // c2i into an interpreted builtin returning a heap object.
        "(defun mk (x) (cons x x)) (list (mk 1) (mk 2))",
        // Non-fixnum args through i2c/c2i.
        "(defun ec (c) (char-code c)) (ec #\\A)",
        // A T1 native function whose c2i call errors — re-raised, catchable.
        "(defun bad (x) (car x)) (defun safe (x) (handler-case (bad x) (type-error () (quote caught)))) (list (safe 5) (safe 6))",
    ];
    for c in cases {
        assert_t1_matches(c);
    }
}

/// Deep recursion must stay T0 (BlissStack-bounded) even with aggressive T1
/// promotion, raising a catchable STORAGE-CONDITION rather than aborting.
#[test]
fn t1_does_not_break_deep_recursion_bound() {
    let out = Command::new(BIN)
        .args([
            "--eval",
            "(handler-case (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0)) \
               (storage-condition () :caught))",
        ])
        .env("BLISS_T1_THRESHOLD", "1")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "deep recursion should be caught, not abort");
    assert!(
        String::from_utf8_lossy(&out.stdout).to_uppercase().contains("CAUGHT"),
        "recursive function must stay BlissStack-bounded under T1"
    );
}

/// bliss-jtc.25: tagbody/go loops (with block/return-from exits) compile to
/// native T1 code with results identical to interpretation. Each case prints a
/// value that must match between the tree-walker and the T1 backend. These
/// specifically exercise the loop-codegen paths: backward `Go`, `ReturnFrom`
/// operand-stack reset, and the `ClearMv` after SETQ.
#[test]
fn t1_native_loops_match_interpretation() {
    let cases = &[
        // Countdown accumulator (backward go, SETQ/ClearMv in the loop body).
        "(defun sumto (n) (let ((acc 0)) \
           (tagbody top (when (> n 0) (setq acc (+ acc n)) (setq n (- n 1)) (go top))) \
           acc)) \
         (format t \"~a~%\" (list (sumto 10) (sumto 100) (sumto 0)))",
        // Early exit via return-from carrying a computed value out of the loop.
        "(defun firstsq (n) \
           (block done (let ((i 0)) \
             (tagbody top (when (>= i n) (return-from done (* i i))) (setq i (+ i 1)) (go top)) \
             -1))) \
         (format t \"~a~%\" (list (firstsq 5) (firstsq 0)))",
        // return-from nested inside an operand-pushing expression: the native
        // path must reset the operand stack to the block's entry depth.
        "(defun g (n) (block b (+ 1000 (progn (when (> n 0) (return-from b n)) 0)))) \
         (format t \"~a~%\" (list (g 7) (g 0)))",
        // Loop building a heap object (cons) via c2i.
        "(defun rng (n) (let ((acc nil) (i 0)) \
           (tagbody top (when (>= i n) (go done)) (setq acc (cons i acc)) (setq i (+ i 1)) (go top) done) \
           acc)) \
         (format t \"~a~%\" (rng 5))",
        // SETQ of a multiple-valued primitive must not leak extra values.
        "(defun h () (let ((x 0)) (setq x (floor 7 2)) x)) \
         (format t \"~a~%\" (multiple-value-list (h)))",
        // Nested tagbody loops.
        "(defun mul (a b) (let ((s 0) (i 0)) \
           (tagbody io (when (>= i a) (go ie)) \
             (let ((j 0)) (tagbody jo (when (>= j b) (go je)) (setq s (+ s 1)) (setq j (+ j 1)) (go jo) je)) \
             (setq i (+ i 1)) (go io) ie) \
           s)) \
         (format t \"~a~%\" (list (mul 3 4) (mul 0 9) (mul 5 5)))",
    ];
    for c in cases {
        assert_t1_matches(c);
    }
}
