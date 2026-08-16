//! Tiering observability through the real binary (bliss-jtc.10).
//!
//! The stage-5 gate requires that "a hot loop is observably promoted through
//! tiers with identical results at each tier". These tests drive the shipping
//! `bliss-cli` binary and use the Lisp-visible introspection builtins
//! (`bliss-ext:function-tier`, `-invoke-count`, `-back-edge-count`) to observe
//! the promotion, then confirm the observed result is unchanged from pure
//! interpretation.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

fn run(program: &str, envs: &[(&str, &str)]) -> (String, bool) {
    let mut cmd = Command::new(BIN);
    cmd.args(["--eval", program]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn bliss-cli");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// A hot loop's back-edge counter is observable and reflects the trip count,
/// while a once-called function stays in T0 (invocation-count promotion never
/// fires for it) — the classic OSR-shaped case.
#[test]
fn hot_loop_back_edges_are_observable() {
    let program = "\
        (defun spin (n) \
          (block done \
            (tagbody \
             top (when (<= n 0) (return-from done nil)) \
                 (setq n (- n 1)) \
                 (go top)))) \
        (spin 1000) \
        (format t \"~a ~a ~a~%\" \
                (bliss-ext:function-tier (quote spin)) \
                (bliss-ext:function-invoke-count (quote spin)) \
                (bliss-ext:function-back-edge-count (quote spin)))";
    let (out, ok) = run(program, &[]);
    assert!(ok, "program must succeed; got:\n{out}");
    let line = out.lines().next().unwrap_or("");
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields.len(), 3, "expected 3 fields, got: {line:?}");
    assert_eq!(fields[0], "0", "once-called loop stays T0: {line:?}");
    assert_eq!(fields[1], "1", "called exactly once: {line:?}");
    let back_edges: u32 = fields[2].parse().expect("back-edge count is a fixnum");
    assert!(
        back_edges >= 1000,
        "a 1000-iteration loop must record >=1000 back-edges, got {back_edges}"
    );
}

/// A function called past the T1 threshold is observably promoted to tier 1,
/// and the value it computes is identical to what the tree-walker computes —
/// the gate's "promoted through tiers with identical results" in miniature.
#[test]
fn promotion_to_t1_is_observable_and_result_identical() {
    let program = "\
        (defun sq (x) (* x x)) \
        (sq 2) (sq 3) (sq 4) (sq 5) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote sq)) (sq 9))";

    // Under a low T1 threshold, sq promotes to tier 1 and still returns 81.
    let (t1_out, t1_ok) = run(program, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(t1_ok, "T1 run must succeed; got:\n{t1_out}");
    let t1_line = t1_out.lines().next().unwrap_or("");
    let t1_fields: Vec<&str> = t1_line.split_whitespace().collect();
    assert_eq!(t1_fields.first().copied(), Some("1"), "sq must reach T1: {t1_line:?}");
    assert_eq!(t1_fields.get(1).copied(), Some("81"), "T1 result must be 81: {t1_line:?}");

    // Under the pure tree-walker, the result is identical (tier is 0 there).
    let (tw_out, tw_ok) = run(program, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run must succeed; got:\n{tw_out}");
    let tw_result = tw_out
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(str::to_string);
    assert_eq!(
        tw_result.as_deref(),
        Some("81"),
        "tree-walker result must match the T1 result (81)"
    );
}

/// The gate in miniature for an actual hot loop (bliss-jtc.25): a tagbody/go
/// loop function is observably promoted to tier 1 and, running as native T1
/// code, returns the identical value the tree-walker computes. Asserting the
/// tier explicitly guarantees the loop codegen path is exercised — not merely
/// that two interpreter runs agree.
#[test]
fn hot_loop_promotes_to_t1_with_identical_result() {
    let program = "\
        (defun sumto (n) \
          (let ((acc 0)) \
            (tagbody top (when (> n 0) (setq acc (+ acc n)) (setq n (- n 1)) (go top))) \
            acc)) \
        (sumto 10) (sumto 10) (sumto 10) (sumto 10) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote sumto)) (sumto 100))";

    let (t1_out, t1_ok) = run(program, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(t1_ok, "T1 run must succeed; got:\n{t1_out}");
    let fields: Vec<&str> = t1_out.lines().next().unwrap_or("").split_whitespace().collect();
    assert_eq!(fields.first().copied(), Some("1"), "the loop must reach T1: {t1_out:?}");
    assert_eq!(fields.get(1).copied(), Some("5050"), "T1 loop result must be 5050");

    // Identical under pure interpretation.
    let (tw_out, tw_ok) = run(program, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run must succeed; got:\n{tw_out}");
    let tw_result = tw_out
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(str::to_string);
    assert_eq!(tw_result.as_deref(), Some("5050"), "interpreter result must also be 5050");
}

/// Idiomatic DOTIMES/DOLIST loops (bliss-jtc.28) lower to bytecode and promote
/// to T1 — not just explicit tagbody/go — with results identical to the
/// interpreter. This is what makes the hotspot engine reach real-world loops.
#[test]
fn dotimes_and_dolist_promote_to_t1() {
    // DOTIMES accumulator: sum of 0..99 = 4950.
    let dt = "\
        (defun tri (n) (let ((acc 0)) (dotimes (i n acc) (setq acc (+ acc i))))) \
        (tri 5) (tri 5) (tri 5) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote tri)) (tri 100))";
    let (out, ok) = run(dt, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "dotimes run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(f.first().copied(), Some("1"), "dotimes loop must reach T1: {out:?}");
    assert_eq!(f.get(1).copied(), Some("4950"), "dotimes result");

    // DOLIST sum.
    let dl = "\
        (defun sm (lst) (let ((s 0)) (dolist (x lst s) (setq s (+ s x))))) \
        (sm (list 1 2 3)) (sm (list 1 2 3)) (sm (list 1 2 3)) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote sm)) (sm (list 10 20 30 40)))";
    let (out, ok) = run(dl, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "dolist run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(f.first().copied(), Some("1"), "dolist loop must reach T1: {out:?}");
    assert_eq!(f.get(1).copied(), Some("100"), "dolist result");
}

/// The simple LOOP form (all-compound body, terminated by an explicit RETURN)
/// lowers to bytecode and promotes to T1 (bliss-jtc.28 follow-up), while the
/// extended LOOP (FOR/…/keywords) stays in the tree-walker at T0 — both correct.
#[test]
fn simple_loop_promotes_extended_loop_stays_t0() {
    let simple = "\
        (defun g (n) (let ((s 0) (i 0)) \
          (loop (when (>= i n) (return s)) (setq s (+ s i)) (setq i (+ i 1))))) \
        (g 5) (g 5) (g 5) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote g)) (g 100))";
    let (out, ok) = run(simple, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "simple loop run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(f.first().copied(), Some("1"), "simple loop must reach T1: {out:?}");
    assert_eq!(f.get(1).copied(), Some("4950"), "simple loop result");

    // Extended LOOP stays interpreted (tier 0) but returns the same value.
    let extended = "\
        (defun h (n) (let ((s 0)) (loop for i from 1 to n do (setq s (+ s i))) s)) \
        (h 5) (h 5) (h 5) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote h)) (h 100))";
    let (out, ok) = run(extended, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "extended loop run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(f.first().copied(), Some("0"), "extended loop stays T0: {out:?}");
    assert_eq!(f.get(1).copied(), Some("5050"), "extended loop result");
}
