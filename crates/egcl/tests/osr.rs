// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! On-stack replacement (OSR) differential tests (bliss-izt.1/izt.2).
//!
//! A function that is one long-running loop is promoted into native T1 code
//! *mid-execution* — without ever returning and being re-called — once its
//! back-edge count crosses `EGCL_OSR_THRESHOLD`. OSR now speculates: it inlines
//! fixnum arithmetic and, when a guard fails mid-loop, state-transfer deopt
//! (bliss-izt.2) resumes the interpreter at that exact bytecode position on the
//! live frame. The invariant across all of it: entering (and, on a guard
//! failure, leaving) the native loop from the live interpreter frame produces
//! exactly what pure interpretation produces. Each case forces a tiny threshold
//! so OSR fires early, and asserts stdout matches the tree-walker byte for byte.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

#[test]
fn active_loop_osr_preserves_its_definition_after_redefinition() {
    let replacement = std::env::temp_dir().join(format!(
        "egcl-active-osr-replacement-{}.lisp",
        std::process::id()
    ));
    std::fs::write(
        &replacement,
        "(defun versioned-loop (n)
           (let ((sum 0))
             (dotimes (i n sum)
               (replace-active-loop-once)
               (incf sum 7))))",
    )
    .unwrap();
    let program = format!(
        r#"
      (defvar *replace-active-loop* nil)
      (defun replace-active-loop-once ()
        (when *replace-active-loop*
          (setq *replace-active-loop* nil)
          (load {replacement:?})
          (assert (= (versioned-loop 1) 7))))
      (defun versioned-loop (n)
        (let ((sum 0))
          (dotimes (i n sum)
            (replace-active-loop-once)
            (incf sum 1))))
      (setq *replace-active-loop* t)
      (let ((old-result (versioned-loop 1000)))
        (format t "OLD-RESULT=~a~%" old-result)
        (assert (= old-result 1000)))
      (assert (> (egcl-ext:function-osr-count 'versioned-loop) 0))
      (assert (= (versioned-loop 1000) 7000))
      (format t "ACTIVE-VERSION-OSR-OK~%")
    "#
    );
    let output = Command::new("timeout")
        .args(["--kill-after=5", "60", BIN, "--no-init", "--eval", &program])
        .env_remove("EGCL_FORCE_TIER")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "1000000")
        .env("EGCL_DISABLE_T2", "1")
        .env("EGCL_OSR_THRESHOLD", "20")
        .output()
        .expect("run active-version OSR regression");
    std::fs::remove_file(replacement).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("ACTIVE-VERSION-OSR-OK"),
        "{stdout}\n{stderr}"
    );
}

/// Run `program` with OSR forced on (threshold 50) and under the pure
/// tree-walker; assert identical stdout and exit status.
fn assert_osr_matches(program: &str) {
    let osr = Command::new(BIN)
        .args(["--eval", program])
        .env("EGCL_OSR_THRESHOLD", "50")
        .output()
        .expect("spawn osr");
    let tw = Command::new(BIN)
        .args(["--eval", program])
        .env("EGCL_BACKEND", "tree-walker")
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
/// differential tests still passed). `egcl-ext:function-osr-count` counts
/// real native OSR entries (FUNCTION-TIER does not reflect OSR), so assert a
/// cold first-call hot loop enters native code at least once (bliss-f88w).
#[test]
fn cold_hot_loop_enters_native_via_osr() {
    let prog = "(defun sumto (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s i))))) \
       (format t \"~a ~a~%\" (sumto 5000) (egcl-ext:function-osr-count 'sumto))";
    let out = Command::new(BIN)
        .args(["--eval", prog])
        .env("EGCL_OSR_THRESHOLD", "50")
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
       (format t \"~a ~a~%\" (dsum 200000) (egcl-ext:function-osr-count 'dsum))";
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

/// bliss-kqdr: the T1→T2 back-edge OSR transfer handed T2 code the *T1* frame,
/// which is sized for the bytecode's slot count. T2 code that syncs its native
/// roots through shadow slots addresses `num_slots + shadow_root_slots`, so it
/// stored past the end of that activation and into the adjacent frame's header
/// — silently overwriting the caller's own parameter with a raw pointer. A hot
/// `(dotimes (i n r) (setq r (f o)))` then returned a garbage FIXNUM instead of
/// `o`, at T2 only, with interp/T0/T1 all correct.
///
/// The shape matters: the result must feed a loop-carried variable (discarding
/// it needs no post-call reload), the call must be to a user function (a builtin
/// pushes no activation), and the value must come from a parameter (a global is
/// re-read from its symbol cell). The tier-differential corpus cannot catch this
/// — its loops never reach the T2 promotion threshold.
#[test]
fn t2_osr_does_not_clobber_the_callers_parameter() {
    // Identity callee: whatever `h` returns must be exactly what was passed in.
    let program = "(progn \
         (defun pf (x) x) \
         (defun h (o n) (let ((r nil)) (dotimes (i n r) (setq r (pf o))))) \
         (print (h 42 30000)))";
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("spawn egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("42"),
            "tier {tier}: identity through a hot loop must return 42, got: {stdout}"
        );
    }
}

/// An OSR body must not skip the interpreter's scope transitions, or a later deopt
/// delivers a THROW to an already-expired CATCH and REPLAYS the side effects that
/// follow it (bliss-57da).
///
/// Native T1 elides PushBlock/PushTag/PopHandler/ReturnFrom because a T1-eligible
/// function is a leaf whose transfers are all lexically local, so the handler state
/// is dead. That reasoning does NOT hold for OSR: an OSR body is entered with the
/// interpreter's handler stack already live — the CATCH enclosing a hot loop is still
/// on it — and OSR compiles only the loop, so nothing made the function bail.
///
/// Measured on x86-64 before the fix: T0 ran the side effect once, OSR ran it TWICE.
/// s390x hit this first and fixed it the same way; this is the x86 reproduction the
/// bead asked for, kept as a regression test.
#[test]
fn osr_does_not_replay_effects_after_an_expired_catch() {
    let program = r#"
      (defvar *effects* 0)
      (defun expired-catch ()
        (catch 'expired
          (let ((i 0))
            (block done (tagbody top
              (if (>= i 10) (return-from done i))
              (setq i (1+ i)) (go top)))))
        ;; The CATCH has returned, so its tag is expired: this THROW must be a
        ;; CONTROL-ERROR, and the increment above it must happen exactly once.
        (setq *effects* (1+ *effects*))
        (throw 'expired 99))
      ;; The same hazard reached through NESTED tagbodies, where the inner GO
      ;; crosses to the outer one — the shape s390x guards separately at its GO.
      ;; On x86 deopting the scope transitions covers this too, but it exercises a
      ;; different path to the same replay and fails identically without the fix.
      (defvar *nested-effects* 0)
      (defun nested-expired-catch ()
        (catch 'gone
          (let ((i 0) (visits 0))
            (block done (tagbody outer
              (setq visits (1+ visits))
              (if (>= visits 3) (return-from done (list i visits)))
              (tagbody inner
                (if (>= i 10) (go outer))
                (setq i (1+ i)) (go inner))))))
        (setq *nested-effects* (1+ *nested-effects*))
        (throw 'gone 1))
      (let ((outcome (handler-case (expired-catch)
                       (control-error () :control-error)
                       (error (e) (list :other (type-of e)))))
            (nested (handler-case (nested-expired-catch)
                      (control-error () :control-error)
                      (error (e) (list :other (type-of e))))))
        (format t "SCOPE ~s ~s ~s~%" *effects* outcome
                (> (egcl-ext:function-osr-count 'expired-catch) 0))
        (format t "NESTED ~s ~s~%" *nested-effects* nested))
    "#;
    // The interpreter's answer, with OSR effectively off.
    let interpreted = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_T0_T1_THRESHOLD", "1000000")
        .env("EGCL_DISABLE_T2", "1")
        .output()
        .expect("the CLI runs");
    // The same program with OSR firing almost immediately and uncommon traps on,
    // which is the configuration that exposed it.
    let osr = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_T0_T1_THRESHOLD", "1000000")
        .env("EGCL_OSR_THRESHOLD", "2")
        .env("EGCL_DISABLE_T2", "1")
        .env("EGCL_OSR_TRAPS", "1")
        .output()
        .expect("the CLI runs");
    let line = |out: &std::process::Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|line| line.starts_with("SCOPE "))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    assert_eq!(
        line(&interpreted),
        "SCOPE 1 :CONTROL-ERROR NIL",
        "the interpreter must run the effect once and signal"
    );
    assert_eq!(
        line(&osr),
        "SCOPE 1 :CONTROL-ERROR T",
        "OSR must agree with the interpreter — and must actually have fired, or this \
         test would pass by never exercising the path"
    );

    let nested = |out: &std::process::Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|line| line.starts_with("NESTED "))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    assert_eq!(nested(&interpreted), "NESTED 1 :CONTROL-ERROR");
    assert_eq!(
        nested(&osr),
        "NESTED 1 :CONTROL-ERROR",
        "the nested-tagbody route to the same replay must agree too"
    );
}
