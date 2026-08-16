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
