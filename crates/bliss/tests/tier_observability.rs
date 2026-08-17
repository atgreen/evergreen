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
fn simple_and_numeric_for_loops_promote() {
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

    // Extended LOOP with an ascending numeric `for` now lowers to the same
    // block/let/tagbody/go shape as DOTIMES and promotes to T1 (bliss-x5y.3),
    // with the same value. (Non-numeric-for extended loops still bail to T0.)
    let extended = "\
        (defun h (n) (let ((s 0)) (loop for i from 1 to n do (setq s (+ s i))) s)) \
        (h 5) (h 5) (h 5) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote h)) (h 100))";
    let (out, ok) = run(extended, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "extended loop run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(f.first().copied(), Some("1"), "numeric-for loop reaches T1: {out:?}");
    assert_eq!(f.get(1).copied(), Some("5050"), "extended loop result");
}

/// A function that writes a global BEFORE a speculated guard reaches T2
/// (bliss-mzp: SetSymbolValue/SymbolValue emission), and its precise deopt
/// (bliss-mba) applies that write exactly once. `acc2` increments `*c*`, then
/// speculates `(+ a 100)`; calling it with a float fails that guard and deopts
/// AFTER the store has committed. Precise state-transfer resumes T0 past the
/// store, so `*c*` ends at 61 — a whole-function rerun would double it to 62.
/// The T2 result must equal the tree-walker's, byte for byte.
///
/// Runs only where the optimising tier is available (x86-64); on other targets
/// `BLISS_T2=1` is a no-op and the assertion below would still hold at T1, so we
/// keep it unconditional — it exercises the interpreter's global-store path too.
#[test]
fn global_store_before_guard_reaches_t2_and_deopts_once() {
    let prog = "\
        (defvar *c* 0) \
        (defun acc2 (a) (setf *c* (+ *c* 1)) (+ a 100)) \
        (dotimes (k 60) (acc2 k)) \
        (let ((r (acc2 1.5))) \
          (format t \"~a ~a~%\" r *c*))";

    // T2 on: acc2 promotes and the float call deopts after the store.
    let (out, ok) = run(prog, &[("BLISS_T2", "1")]);
    assert!(ok, "T2 run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(f.first().copied(), Some("101.5"), "deopt result: {line:?}");
    assert_eq!(
        f.get(1).copied(),
        Some("61"),
        "the global store must apply exactly once (not 62): {line:?}"
    );

    // Tree-walker: identical observable result.
    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let twl = tw.lines().next().unwrap_or("").to_string();
    assert_eq!(twl, line, "T2 result must match the tree-walker: {line:?} vs {twl:?}");
}

/// A global-accumulator LOOP reaches T2 (bliss-fe8: the builder's loop SSA is
/// stitched correctly and its loop-invariant phis are collapsed so it fits the
/// framed register budget), computes the interpreted result, and deopts
/// precisely when a guard fails mid-loop. `acc-loop` sums `step` into `*s*` five
/// times inside a `dotimes`; called with a fixnum it stays all-fixnum (promotes),
/// and with a float the `(+ *s* step)` guard fails on the first iteration and
/// deopts — the loop must finish in the interpreter with the exact float sum, and
/// `*s*` must match. All three observations must equal the tree-walker's.
#[test]
fn global_accumulator_loop_reaches_t2_and_deopts_precisely() {
    let prog = "\
        (defvar *s* 0) \
        (defun acc-loop (step) (setf *s* 0) (dotimes (i 5) (setf *s* (+ *s* step))) *s*) \
        (dotimes (k 60) (acc-loop 2)) \
        (let ((a (acc-loop 2)) (b (acc-loop 3)) (c (acc-loop 1.5))) \
          (format t \"~a ~a ~a ~a~%\" a b c *s*))";

    let (out, ok) = run(prog, &[("BLISS_T2", "1")]);
    assert!(ok, "T2 run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(
        line, "10 15 7.5 7.5",
        "loop results incl. the mid-loop float deopt (a b c *s*): {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let twl = tw.lines().next().unwrap_or("").to_string();
    assert_eq!(twl, line, "T2 loop result must match the tree-walker: {line:?} vs {twl:?}");
}

/// A loop with several call-local temporaries alongside a couple of loop-carried
/// values reaches T2 by placing the temporaries in caller-saved registers
/// (bliss-uox: a value whose live range crosses no call needs no callee-saved
/// register). `mid2` keeps `a`/`b`/`i` across the SymbolValue/SetSymbolValue calls
/// in the body (callee-saved) while `(+ a b)` and the running-sum add are
/// call-local; without the second register pool this exceeds the 5 callee-saved
/// registers and declines to T1. The forced float deopt also exercises a
/// call-local, deopt-live value being reconstructed out of a caller-saved
/// register. The T2 result — including that deopt — must equal the tree-walker's.
#[test]
fn call_local_temporaries_use_caller_saved_and_deopt_correctly() {
    let prog = "\
        (defvar *s* 0) \
        (defun mid2 (a b) (setf *s* 0) (dotimes (i 5) (setf *s* (+ *s* (+ a b)))) *s*) \
        (dotimes (k 60) (mid2 3 4)) \
        (let ((ok (mid2 3 4)) (dp (mid2 1.5 4))) \
          (format t \"~a ~a ~a~%\" ok dp *s*))";

    let (out, ok) = run(prog, &[("BLISS_T2", "1")]);
    assert!(ok, "T2 run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(
        line, "35 27.5 27.5",
        "all-fixnum sum, then the float-deopt sum and *s* (ok dp *s*): {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let twl = tw.lines().next().unwrap_or("").to_string();
    assert_eq!(twl, line, "T2 result must match the tree-walker: {line:?} vs {twl:?}");
}

/// A hash-table-using function compiles to bytecode (does NOT bail to the
/// tree-walker) and promotes to T1 (bliss-x5y.2). make-hash-table (with a :test
/// keyword), gethash, (setf (gethash ...) ...), remhash, and hash-table-count all
/// lower to CallNamed — the setf store goes to the internal BLISS::PUT-GETHASH
/// primitive. A bailed function stays at tier 0, so observing tier 1 proves it
/// compiled; the value must equal the tree-walker's. (Iteration via maphash with
/// an inline lambda still bails on closure lowering — a separate concern.)
#[test]
fn hash_table_function_compiles_and_promotes() {
    let prog = "\
        (defun htf () \
          (let ((h (make-hash-table :test (quote equal)))) \
            (setf (gethash \"a\" h) 10) \
            (setf (gethash \"b\" h) 20) \
            (setf (gethash \"c\" h) 30) \
            (remhash \"b\" h) \
            (setf (gethash \"a\" h) (+ (gethash \"a\" h) 5)) \
            (list (hash-table-count h) (gethash \"a\" h) (gethash \"b\" h (quote absent))))) \
        (htf) (htf) (htf) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote htf)) (htf))";

    let (out, ok) = run(prog, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "hash function run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert!(
        line.starts_with("1 "),
        "hash function must compile and reach T1 (tier 1, not a bailed 0): {line:?}"
    );
    assert!(line.contains("(2 15 ABSENT)"), "hash result: {line:?}");

    // Identical value under the tree-walker.
    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let tw_result = tw.lines().next().unwrap_or("").split_once(' ').map(|(_, r)| r.to_string());
    let t1_result = line.split_once(' ').map(|(_, r)| r.to_string());
    assert_eq!(tw_result, t1_result, "T1 hash result must match the tree-walker");
}

/// Variadic lambda lists (&optional/&key/&rest, with defaults and supplied-p)
/// compile to bytecode and promote to T1 (bliss-x5y.7) — previously the single
/// biggest function-level bail. Defaults (which may reference earlier params) and
/// keyword matching must match the tree-walker exactly; the binder reuses the
/// interpreter's own bind_lambda_list into a scratch env, then copies to slots.
#[test]
fn variadic_lambda_lists_compile_and_promote() {
    let prog = "\
        (defun f (x &optional (y 1) z) (list x y z)) \
        (defun g (a &key (b 1) (c (* a 2))) (list a b c)) \
        (defun h (x &rest r) (list x r)) \
        (f 0) (f 0) (g 0) (g 0) (h 0) (h 0) \
        (format t \"~a ~a ~a ~a~%\" \
          (bliss-ext:function-tier (quote f)) \
          (f 10 20) (g 5 :c 99) (h 1 2 3))";
    let (out, ok) = run(prog, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "variadic run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert!(line.starts_with("1 "), "&optional fn must reach T1: {line:?}");
    assert_eq!(
        line, "1 (10 20 NIL) (5 1 99) (1 (2 3))",
        "variadic results (tier f, f, g, h): {line:?}"
    );

    // Tree-walker agreement, including a supplied-p and too-few-args error.
    let prog2 = "\
        (defun sp (a &optional (b 9 bp)) (list a b bp)) \
        (defun r2 (a b) (+ a b)) \
        (dotimes (i 4) (sp 1) (r2 1 2)) \
        (format t \"~a ~a~%\" (sp 1) (handler-case (r2 1) (error () :few)))";
    let (t1, _) = run(prog2, &[("BLISS_T1_THRESHOLD", "2")]);
    let (tw, _) = run(prog2, &[("BLISS_BACKEND", "tree-walker")]);
    assert_eq!(
        t1.lines().next(),
        tw.lines().next(),
        "supplied-p + arity error must match the tree-walker"
    );
}

/// A non-top-level EVAL-WHEN (in a function body) compiles to bytecode and
/// promotes to T1 (bliss-x5y.6 follow-up) instead of bailing the whole function
/// to the tree-walker. Per CLHS 3.2.3.1 it reduces to (progn body) when its
/// situations fire; the result must match the tree-walker.
#[test]
fn nested_eval_when_compiles_and_promotes() {
    let prog = "\
        (defun compute (n) \
          (let ((acc 0)) \
            (eval-when (:execute) (dotimes (i n) (setq acc (+ acc (* i i))))) \
            acc)) \
        (defun skipped (x) (eval-when (:compile-toplevel) (setq x 999)) x) \
        (compute 3) (compute 3) (skipped 5) (skipped 5) \
        (format t \"~a ~a ~a~%\" \
          (bliss-ext:function-tier (quote compute)) (compute 10) (skipped 7))";
    let (out, ok) = run(prog, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "eval-when run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    // compute reaches T1; (* i i) for 0..9 sums to 285; the :compile-toplevel-only
    // eval-when does NOT fire at execute time, so skipped returns its arg (7).
    assert_eq!(line, "1 285 7", "eval-when compile result (tier, compute, skipped): {line:?}");

    let (tw, _) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    let tw_rest = tw.lines().next().unwrap_or("").split_once(' ').map(|(_, r)| r.to_string());
    let t1_rest = line.split_once(' ').map(|(_, r)| r.to_string());
    assert_eq!(t1_rest, tw_rest, "eval-when result must match the tree-walker");
}
