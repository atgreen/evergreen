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
