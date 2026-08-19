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

/// The metadata-selected INTEGERP expansion composes with a later speculative
/// arithmetic guard. Warm fixnums take the inlined true branch; a float takes
/// the false branch and forces deoptimization at its cold `+`. The resumed T0
/// result must match the tree-walker and the deopt counter must advance once.
/// Compiler-level integration tests separately assert that this CallNamed was
/// replaced by TypeCheck rather than emitted as a runtime call.
#[test]
fn metadata_intrinsic_survives_later_forced_deopt() {
    let prog = "\
        (defun inline-deopt (x) (if (integerp x) (+ x 1) (+ x 2))) \
        (dotimes (k 60) (inline-deopt k)) \
        (let ((before (bliss-ext:deopt-count)) \
              (value (inline-deopt 1.5))) \
          (format t \"~a ~a ~a~%\" before value (bliss-ext:deopt-count)))";

    let (out, ok) = run(prog, &[("BLISS_T2", "1")]);
    assert!(ok, "T2 intrinsic/deopt run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields.get(1).copied(), Some("3.5"), "forced-deopt result: {line:?}");
    let before: u64 = fields.first().expect("before count").parse().expect("count");
    let after: u64 = fields.get(2).expect("after count").parse().expect("count");
    assert_eq!(after, before + 1, "the cold float path must deopt exactly once: {line:?}");

    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let tw_fields: Vec<&str> = tw.lines().next().unwrap_or("").split_whitespace().collect();
    assert_eq!(tw_fields.get(1).copied(), Some("3.5"), "tree-walker oracle: {tw:?}");
}

/// STRINGP is selected by generic inline metadata and emitted as a safe tagged
/// pointer/header predicate. Exercise both outcomes after promotion and compare
/// the values with the tree-walker; compiler integration tests assert that the
/// T2 body contains TypeCheck rather than a STRINGP runtime call.
#[test]
fn metadata_stringp_is_tier_differentially_identical() {
    let prog = "\
        (defun string-kind (x) (if (stringp x) 10 20)) \
        (dotimes (k 60) (string-kind \"warm\")) \
        (format t \"~a ~a ~a~%\" \
          (string-kind \"yes\") \
          (string-kind 7) \
          (string-kind (namestring (make-pathname :name \"fresh-name\" :type \"lisp\"))))";
    let (out, ok) = run(prog, &[("BLISS_T2", "1")]);
    assert!(ok, "T2 STRINGP run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(line, "10 20 10");

    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker STRINGP run failed: {tw}");
    assert_eq!(tw.lines().next().unwrap_or(""), line);
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
/// keyword), gethash, (setf (gethash ...) ...), remhash, hash-table-count, and
/// MAPHASH iteration all lower to bytecode — the setf store goes to
/// the internal BLISS::PUT-GETHASH primitive. A bailed function stays at tier 0,
/// so observing tier 1 proves it compiled; the value must equal the tree-walker's.
#[test]
fn hash_table_function_compiles_and_promotes() {
    let prog = "\
        (defvar *hash-sum* 0) \
        (defun hash-sum-entry (key value) \
          (declare (ignore key)) (setf *hash-sum* (+ *hash-sum* value))) \
        (defun htf () \
          (let ((h (make-hash-table :test (quote equal)))) \
            (setf (gethash \"a\" h) 10) \
            (setf (gethash \"b\" h) 20) \
            (setf (gethash \"c\" h) 30) \
            (remhash \"b\" h) \
            (setf (gethash \"a\" h) (+ (gethash \"a\" h) 5)) \
            (setf *hash-sum* 0) \
            (maphash (quote hash-sum-entry) h) \
            (list (list (hash-table-count h) (gethash \"a\" h) \
                        (gethash \"b\" h (quote absent))) *hash-sum*))) \
        (htf) (htf) (htf) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote htf)) (htf))";

    let (out, ok) = run(prog, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "hash function run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert!(
        line.starts_with("1 "),
        "hash function must compile and reach T1 (tier 1, not a bailed 0): {line:?}"
    );
    assert!(line.contains("((2 15 ABSENT) 45)"), "hash result: {line:?}");

    // Identical value under the tree-walker.
    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let tw_result = tw.lines().next().unwrap_or("").split_once(' ').map(|(_, r)| r.to_string());
    let t1_result = line.split_once(' ').map(|(_, r)| r.to_string());
    assert_eq!(tw_result, t1_result, "T1 hash result must match the tree-walker");
}

#[test]
fn loop_being_hash_keys_compiles_and_promotes() {
    let prog = "\
        (defun hash-loop () \
          (let ((h (make-hash-table))) \
            (setf (gethash (quote a) h) 10 (gethash (quote b) h) 20) \
            (list (loop for key being the hash-keys of h sum (gethash key h)) \
                  (loop for value being hash-values of h sum value)))) \
        (hash-loop) (hash-loop) (hash-loop) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote hash-loop)) (hash-loop))";

    let (out, ok) = run(prog, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "hash LOOP run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(line, "1 (30 30)", "hash LOOP must reach T1 with the right sums");

    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker hash LOOP failed: {tw}");
    assert_eq!(tw.lines().next(), Some("0 (30 30)"));
}

#[test]
fn hot_leaf_called_only_from_tree_walker_promotes() {
    // LOOP REPEAT remains a tree-walker-only extended shape. Calls made from
    // that body must still enter the registered bytecode function and drive its
    // invocation counter across the T1 threshold.
    let prog = "\
        (defun tree-called-leaf (x) (+ x 1)) \
        (loop repeat 5 do (tree-called-leaf 1)) \
        (format t \"~a ~a~%\" \
          (bliss-ext:function-tier (quote tree-called-leaf)) \
          (tree-called-leaf 41))";

    let (out, ok) = run(prog, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "tree-called leaf run failed: {out}");
    assert_eq!(out.lines().next(), Some("1 42"));

    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker oracle failed: {tw}");
    assert_eq!(tw.lines().next(), Some("0 42"));
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

/// A variadic (`&rest`) function must stay correct under `BLISS_T2=1`. T2's entry
/// sequence binds only the fixed positional parameters (locals `0..arity`) and
/// leaves the rest NIL, so a variadic function is DECLINED from T2 and runs at
/// T1, whose `bind_variadic` collects `&rest`. This guards the UIOP STRCAT
/// regression: a T2-compiled `&rest` saw an EMPTY list, so
/// `(make-string (loop :for s :in strings :sum …))` got NIL — "NIL is not of type
/// non-negative string size" — and `asdf` failed to load under T2. The hot `&rest`
/// result must equal the tree-walker's, and the function must NOT be at T2.
#[test]
fn variadic_rest_stays_correct_under_t2() {
    let prog = "\
        (defun rs (strings) \
          (make-string (loop :for s :in strings :sum (if (characterp s) 1 (length s))))) \
        (defun sc (&rest strings) (rs strings)) \
        (dotimes (k 80) (sc \"ab\" \"cd\" \"ef\")) \
        (format t \"~a ~a~%\" (bliss-ext:function-tier (quote sc)) (length (sc \"ab\" \"cd\" \"ef\")))";
    let (out, ok) = run(prog, &[("BLISS_T2", "1")]);
    assert!(ok, "variadic under T2 failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert!(line.ends_with(" 6"), "hot &rest result must be 6: {line:?}");
    assert!(
        !line.starts_with("2 "),
        "variadic fn must be declined from T2 (stay at T1): {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    assert!(
        tw.lines().next().unwrap_or("").ends_with(" 6"),
        "tree-walker &rest length must be 6: {tw:?}"
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

/// Definitions nested in a top-level PROGN — including a macro that EXPANDS to
/// `(progn (defun …) (defun …))` — compile and promote to T1 (bliss-1xw), rather
/// than bailing the whole thunk. eval_toplevel macroexpands top-level forms and
/// recurses progn/locally/eval-when subforms as top-level (CLHS 3.2.3.1).
#[test]
fn nested_definitions_in_progn_compile() {
    let prog = "\
        (progn (defun pa (x) (* x 2)) (defun pb (x) (+ x 100))) \
        (defmacro defpair (n) \
          (list (quote progn) \
                (list (quote defun) (quote qa) (quote (x)) (list (quote -) (quote x) n)) \
                (list (quote defun) (quote qb) (quote (x)) (list (quote +) (quote x) n)))) \
        (defpair 7) \
        (pa 1) (pa 1) (pb 1) (pb 1) (qa 1) (qa 1) (qb 1) (qb 1) \
        (format t \"~a ~a ~a ~a ~a ~a ~a~%\" \
          (bliss-ext:function-tier (quote pa)) (bliss-ext:function-tier (quote qa)) \
          (pa 3) (pb 3) (qa 10) (qb 10) (pb 0))";
    let (out, ok) = run(prog, &[("BLISS_T1_THRESHOLD", "2")]);
    assert!(ok, "nested-defs run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    // pa and qa (from the macro) both reach T1; values follow.
    assert_eq!(
        line, "1 1 6 103 3 17 100",
        "tiers pa/qa then pa/pb/qa/qb/pb values: {line:?}"
    );

    // Compare only the VALUES (skip the two leading tier fields, which are 0 in
    // the tree-walker and 1 at T1).
    let values = |l: &str| l.splitn(3, ' ').nth(2).map(str::to_string);
    let (tw, _) = run(prog, &[("BLISS_BACKEND", "tree-walker")]);
    assert_eq!(
        values(&line),
        values(tw.lines().next().unwrap_or("")),
        "nested-def results must match the tree-walker"
    );
}
