// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T1 codegen-execution tests (bliss-nmq.2): hot bytecode functions compile to
//! native code, are called through the i2c adapter, call back into the
//! interpreter via c2i, and produce results identical to pure interpretation.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Run a program forcing T1 promotion on the first call (threshold 1), and again
/// under the pure tree-walker; assert identical stdout + exit status.
fn assert_t1_matches(program: &str) {
    let tw = Command::new(BIN)
        .args(["--eval", program])
        .env("EGCL_BACKEND", "tree-walker")
        .output()
        .expect("spawn");
    let t1 = Command::new(BIN)
        .args(["--eval", program])
        .env("EGCL_T1_THRESHOLD", "1")
        .output()
        .expect("spawn");
    assert_eq!(
        String::from_utf8_lossy(&tw.stdout),
        String::from_utf8_lossy(&t1.stdout),
        "T1 output must match interpretation:\n  {program}"
    );
    assert_eq!(
        tw.status.success(),
        t1.status.success(),
        "exit mismatch: {program}"
    );
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

/// Non-leaf (non-terminal) functions — those that call other user functions —
/// promote to native T1 (bliss-x5y.4), not just leaf functions. Their calls
/// cross c2i into the interpreter, and the result must match interpretation.
#[test]
fn t1_promotes_non_leaf_functions() {
    let cases = &[
        // caller -> helper (a non-tail, non-leaf call).
        "(defun helper (x) (* x x)) (defun caller (x) (+ (helper x) (helper (+ x 1)))) \
           (caller 2) (caller 3) \
           (format t \"~a ~a~%\" (egcl-ext:function-tier (quote caller)) (caller 5))",
        // self-recursion (fib) promotes and stays correct.
        "(defun fib (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) (fib 5) (fib 6) \
           (format t \"~a ~a~%\" (egcl-ext:function-tier (quote fib)) (fib 20))",
        // mutual recursion.
        "(defun ev (n) (if (= n 0) t (od (- n 1)))) (defun od (n) (if (= n 0) nil (ev (- n 1)))) \
           (ev 2) (od 3) (format t \"~a ~a~%\" (ev 100) (od 100))",
    ];
    for c in cases {
        let t1 = Command::new(BIN)
            .args(["--eval", c])
            .env("EGCL_T1_THRESHOLD", "2")
            .output()
            .expect("spawn");
        let tw = Command::new(BIN)
            .args(["--eval", c])
            .env("EGCL_BACKEND", "tree-walker")
            .output()
            .expect("spawn");
        assert!(
            t1.status.success() && tw.status.success(),
            "runs must succeed:\n{c}"
        );
        // Compare only the result token(s), skipping the tier column (which is 0
        // under the tree-walker, 1 under T1).
        let last = |o: &std::process::Output| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(
            last(&t1),
            last(&tw),
            "T1 result must match interpretation:\n{c}"
        );
    }
    // The non-leaf caller actually reaches tier 1 (first stdout line).
    let out = Command::new(BIN)
        .args([
            "--eval",
            "(defun h (x) (* x x)) (defun c (x) (+ (h x) 1)) (c 1)(c 2)(c 3) \
                          (format t \"~a~%\" (egcl-ext:function-tier (quote c)))",
        ])
        .env("EGCL_T1_THRESHOLD", "2")
        // Observes tier promotion after a 3-call warm-up; pin eager so `c`
        // compiles at definition rather than deferring under the lazy default.
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .expect("spawn");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or(""),
        "1",
        "non-leaf function must reach T1"
    );
}

/// Deep recursion of a promoted non-leaf function must stay bounded and raise a
/// catchable STORAGE-CONDITION — the native depth cap hands off to the flat
/// EgclStack path — never a native stack-overflow abort (bliss-x5y.4). The
/// function must *compile* to bytecode for this to exercise the native path;
/// functions that bail to the tree-walker hit a separate, pre-existing
/// interpreter recursion limit (bliss-jtc.24) and are out of scope here.
#[test]
fn t1_non_leaf_deep_recursion_is_catchable() {
    for prog in [
        // Self-recursion that compiles + promotes. NOT a tail call: the `1+`
        // consumes the result, so a frame per level is required. In tail
        // position this returned 0 rather than :CAUGHT once self tail calls
        // started reusing the frame (bliss-ieajy.3) -- it exhausted nothing.
        "(defun countdown (n) (if (= n 0) 0 (1+ (countdown (- n 1))))) (countdown 3)(countdown 3) \
           (format t \"~a\" (handler-case (countdown 1000000) (storage-condition () :caught)))",
        // a labels-local recursive function (also compiles + promotes).
        "(format t \"~a\" (handler-case (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0)) \
           (storage-condition () :caught)))",
    ] {
        let out = Command::new(BIN)
            .args(["--eval", prog])
            .env("EGCL_T1_THRESHOLD", "1")
            .output()
            .expect("spawn");
        assert!(
            out.status.success(),
            "must not abort: {prog}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .to_uppercase()
                .contains("CAUGHT"),
            "deep non-leaf recursion must raise a catchable condition: {prog}"
        );
    }
}

/// Deep recursion must stay T0 (EgclStack-bounded) even with aggressive T1
/// promotion, raising a catchable STORAGE-CONDITION rather than aborting.
#[test]
fn t1_does_not_break_deep_recursion_bound() {
    let out = Command::new(BIN)
        .args([
            "--eval",
            "(handler-case (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0)) \
               (storage-condition () :caught))",
        ])
        .env("EGCL_T1_THRESHOLD", "1")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "deep recursion should be caught, not abort"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .to_uppercase()
            .contains("CAUGHT"),
        "recursive function must stay EgclStack-bounded under T1"
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
