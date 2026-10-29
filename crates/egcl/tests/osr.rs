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
    let osr_comparison = if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "="
    } else {
        ">"
    };
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
      (assert ({osr_comparison} (egcl-ext:function-osr-count 'versioned-loop) 0))
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

/// Every target must prove actual native entry; x86-64 Linux enters the
/// function's mapped code at the loop header (bliss-s0v50).
/// PROMOTION must actually happen — output parity alone cannot see a dead OSR
/// path (bliss-j1o7: the main dispatch path never threaded the registry
/// symbol, so `maybe_osr` rejected every back-edge and every one of these
/// differential tests still passed). `egcl-ext:function-osr-count` counts
/// real native OSR entries (FUNCTION-TIER does not reflect OSR), so assert a
/// cold first-call hot loop enters native code at least once (bliss-f88w).
#[test]
fn cold_hot_loop_obeys_platform_osr_policy() {
    let prog = "(defun sumto (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s i))))) \
       (format t \"~a ~a~%\" (sumto 5000) (egcl-ext:function-osr-count 'sumto))";
    let out = Command::new(BIN)
        .args(["--eval", prog])
        .env("EGCL_OSR_THRESHOLD", "50")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("12497500 ") && !stdout.starts_with("12497500 0"),
        "cold hot loop must preserve its sum and obey platform OSR admission (got: {stdout:?})"
    );
}

/// The same regression guard at the DEFAULT OSR threshold: the tiny-threshold
/// tests exercise the machinery, but bliss-j1o7 would have slipped past them
/// even if they had asserted promotion under an overridden threshold only.
/// 200k iterations cross the default 100k back-edge threshold mid-run.
#[test]
fn cold_hot_loop_obeys_platform_osr_policy_at_default_threshold() {
    let prog = "(defun dsum (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s i))))) \
       (format t \"~a ~a~%\" (dsum 200000) (egcl-ext:function-osr-count 'dsum))";
    let out = Command::new(BIN)
        .args(["--eval", prog])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("19999900000 ") && !stdout.starts_with("19999900000 0"),
        "default-threshold hot loop must obey platform OSR admission (got: {stdout:?})"
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
        if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            "SCOPE 1 :CONTROL-ERROR NIL"
        } else {
            "SCOPE 1 :CONTROL-ERROR T"
        },
        "OSR must preserve effects and follow the platform native-entry policy"
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

/// A loop in a function called only once leaves T0 by OSR (bliss-s0v50):
/// every non-local exit, guard failure, redefinition, closure, nested loop and
/// multiple-value shape below must still print exactly what the interpreter
/// prints, and the plain loops must actually have entered native code.
#[test]
fn once_called_loops_enter_native_code_and_match_interpretation() {
    let program = r#"
(progn
  (defun oc-deopt (n) (let ((s 0)) (dotimes (i n s) (setq s (if (< i 600) (+ s i) (+ s 0.5))))))
  (defun oc-bignum (n) (let ((s 1)) (dotimes (i n s) (setq s (if (< i 600) (+ s 1) (* s 3))) (when (> s (expt 10 60)) (setq s 7)))))
  (defun oc-return (n) (dotimes (i n :never) (when (= i 600) (return-from oc-return (list :early i)))))
  (defun oc-throw (n) (dotimes (i n) (when (= i 600) (throw :tag (list :thrown i)))))
  (defun oc-error (n) (let ((s 0)) (dotimes (i n s) (when (= i 600) (error "boom ~a" i)) (incf s))))
  (defun oc-inner-handler (n) (let ((s 0)) (dotimes (i n s) (handler-case (if (= (mod i 100) 0) (error "x") (incf s)) (error () (decf s))))))
  (defun oc-inner-cleanup (n) (let ((s 0) (c 0)) (dotimes (i n (list s c)) (unwind-protect (incf s) (incf c)))))
  (defvar *oc-special* 1)
  (defun oc-inner-special (n) (let ((s 0)) (dotimes (i n s) (let ((*oc-special* (mod i 3))) (incf s *oc-special*)))))
  (defun oc-redefine (n) (let ((s 0)) (dotimes (i n s) (when (= i 600) (eval '(defun oc-redefine (n) (list :new n)))) (incf s))))
  (defun oc-closures (n) (let ((fs nil)) (dotimes (i n) (when (zerop (mod i 300)) (let ((j i)) (push (lambda () j) fs)))) (mapcar #'funcall fs)))
  (defun oc-nested (n) (let ((s 0)) (dotimes (i n s) (dotimes (j 10) (incf s (* i j))))))
  (defun oc-values (n) (let ((s 0)) (dotimes (i n) (incf s i)) (values s (* 2 s) :done)))
  (defun oc-return-values (n) (dotimes (i n) (when (= i 800) (return (values i :x :y)))))
  (defun oc-cons (n) (let ((acc nil)) (dotimes (i n) (push (cons i i) acc)) (list (length acc) (reduce #'+ acc :key #'car))))
  (format t "~s~%" (list (oc-deopt 1200) (oc-bignum 1200) (oc-return 1200)
                         (catch :tag (oc-throw 1200))
                         (handler-case (oc-error 1200) (error (c) (princ-to-string c)))
                         (oc-inner-handler 1200) (oc-inner-cleanup 1200)
                         (let ((log nil)) (list (catch :tag (unwind-protect (oc-throw 1200) (push :cleaned log))) log))
                         (oc-inner-special 1200) (oc-redefine 1200) (oc-redefine 1)
                         (oc-closures 1200) (oc-nested 300)
                         (multiple-value-list (oc-values 1200))
                         (multiple-value-list (oc-return-values 1200))
                         (oc-cons 1200)))
  (format t "OSR ~s~%" (every #'plusp (mapcar #'egcl-ext:function-osr-count
                                              '(oc-deopt oc-return oc-throw oc-error oc-nested oc-values oc-cons)))))
"#;
    let run = |tier: Option<&str>| {
        let mut command = Command::new(BIN);
        command
            .args(["--no-init", "--eval", program])
            .env_remove("EGCL_FORCE_TIER")
            .env("EGCL_OSR_THRESHOLD", "200")
            .env("EGCL_T1_T2_BACKEDGE_THRESHOLD", "100");
        if let Some(tier) = tier {
            command.env("EGCL_FORCE_TIER", tier);
        }
        let output = command.output().expect("run once-called loops");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let tiered = run(None);
    let oracle = run(Some("interp"));
    assert_eq!(tiered.lines().next(), oracle.lines().next(), "{tiered}");
    assert!(
        tiered.contains("OSR T"),
        "the once-called loops must enter native code: {tiered}"
    );
}

/// Optimized code proves a loop-invariant parameter a fixnum before its loop.
/// A T0 activation resumed by that proof's failure later re-enters the code at
/// the loop header with the non-fixnum still in its frame, so the entry must
/// test it again rather than compare a float as a fixnum forever.
#[test]
fn osr_entry_rechecks_a_proof_made_before_the_loop() {
    let program = "(progn
      (defun osr-bound (n)
        (let ((r 0) (i 0))
          (loop (when (>= i n) (return r))
                (incf r) (when (> i 100) (return :runaway)) (setq i (+ i 1)))))
      (dotimes (k 40) (osr-bound 6))
      (format t \"BOUND ~s ~s ~s~%\" (osr-bound 6) (osr-bound 50.5)
              (plusp (egcl-ext:function-osr-count 'osr-bound))))";
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_T1_T2_BACKEDGE_THRESHOLD", "5")
        .env("EGCL_OSR_THRESHOLD", "10")
        .output()
        .expect("run OSR bound regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("BOUND 6 51 T"), "{stdout}");
}

/// Top-level loops and loops in closures leave T0 mid-loop (bliss-of682).
/// Each form below is its own top-level form, run once: a compiled thunk
/// gets a private identity for the run, so its hot loop enters mapped code by
/// OSR, including inside TIME (which now compiles) and around specials,
/// calls, consing, non-local exits and a mid-loop deopt. The output must
/// match the tree-walker's, and the log must show the entries.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn toplevel_and_closure_loops_enter_native_code_and_match_interpretation() {
    let program = r#"
(defvar *tl-special* 0)
(defun tl-step (s i) (+ s i))
(defun tl-run-it (f) (catch 'tl-tag (funcall f)))
(defun tl-make-summer () (lambda (n) (let ((s 0)) (dotimes (i n s) (incf s i)))))
(let ((s 0)) (dotimes (i 3000) (incf s i)) (print s))
(print (time (let ((s 0)) (dotimes (i 3000 s) (incf s i)))))
(print (multiple-value-list (time (let ((s 0)) (dotimes (i 3000 (values s :mv)) (incf s i))))))
(dotimes (i 3000) (incf *tl-special* i))
(print *tl-special*)
(print (let ((s 0)) (dotimes (i 3000 s) (setf s (tl-step s i)))))
(print (let ((r nil)) (dotimes (i 3000 (list (length r) (reduce #'+ r :key #'car))) (push (cons i i) r))))
(print (loop for i below 3000 sum i))
(print (let ((n 3000)) (loop for i below n collect (* i i) into xs finally (return (reduce #'+ xs)))))
(print (let ((s 0)) (dotimes (i 3000 s) (incf s (if (< i 2500) 1 0.5)))))
(print (block out (dotimes (i 3000) (when (= i 2500) (return-from out (list :out i))))))
(print (tl-run-it (lambda () (let ((s 0)) (dotimes (i 3000) (incf s i) (when (= i 2500) (throw 'tl-tag (list s i))))))))
(print (handler-case (let ((s 0)) (dotimes (i 3000 s) (incf s i) (when (= i 2500) (error "boom ~a" s)))) (error (e) (princ-to-string e))))
(print (let ((s 0)) (dotimes (i 60) (dotimes (j 60) (incf s (* i j)))) s))
(print (list (funcall (tl-make-summer) 3000) (funcall (tl-make-summer) 3000)))
(print (labels ((run (n) (let ((s 0)) (dotimes (i n s) (incf s i))))) (run 3000)))
"#;
    let file = std::env::temp_dir().join(format!("egcl-toplevel-osr-{}.lisp", std::process::id()));
    std::fs::write(&file, program).unwrap();
    let run = |tier: Option<&str>| {
        let mut command = Command::new(BIN);
        command
            .args(["--no-init", "--load"])
            .arg(&file)
            .env_remove("EGCL_FORCE_TIER")
            .env("EGCL_OSR_THRESHOLD", "1000")
            .env("EGCL_LOG", "osr=debug");
        if let Some(tier) = tier {
            command.env("EGCL_FORCE_TIER", tier);
        }
        let output = command.output().expect("run top-level loops");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{stderr}");
        let stdout = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.contains("seconds of") && !line.contains("bytes consed"))
            .collect::<Vec<_>>()
            .join("\n");
        (stdout, stderr)
    };
    let (tiered, log) = run(None);
    let (oracle, _) = run(Some("interp"));
    std::fs::remove_file(&file).unwrap();
    assert_eq!(tiered, oracle, "{log}");
    let toplevel = log
        .matches("<toplevel>: T0 activation entered mapped")
        .count();
    assert!(
        toplevel >= 10,
        "top-level loops must enter native code ({toplevel}):\n{log}"
    );
    assert!(
        log.contains("<lambda>: T0 activation entered mapped"),
        "{log}"
    );
}

/// Runs `program` from a file with low promotion thresholds, natively and under
/// the tree-walker, and returns both outputs and the native run's log.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn run_against_interpreter(
    name: &str,
    program: &str,
    env: &[(&str, &str)],
) -> (String, String, String) {
    let file = std::env::temp_dir().join(format!("egcl-{name}-{}.lisp", std::process::id()));
    std::fs::write(&file, program).unwrap();
    let run = |tier: Option<&str>| {
        let mut command = Command::new(BIN);
        command
            .args(["--no-init", "--load"])
            .arg(&file)
            .env_remove("EGCL_FORCE_TIER")
            .env("EGCL_OSR_THRESHOLD", "200")
            .env("EGCL_T1_T2_BACKEDGE_THRESHOLD", "100")
            .env("EGCL_LOG", "osr=debug");
        for (key, value) in env {
            command.env(key, value);
        }
        if let Some(tier) = tier {
            command.env("EGCL_FORCE_TIER", tier);
        }
        let output = command.output().expect("run special-variable loops");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{stderr}");
        (String::from_utf8_lossy(&output.stdout).into_owned(), stderr)
    };
    let (tiered, log) = run(None);
    let (oracle, _) = run(Some("interp"));
    std::fs::remove_file(&file).unwrap();
    (tiered, oracle, log)
}

/// The program's output without its final native-entry report.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn without_osr_report(output: &str) -> String {
    output
        .lines()
        .filter(|line| !line.starts_with("OSR "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Native code reads and assigns a special's value cell inline while no
/// execution binds it (bliss-csc8v). Bindings in callers, callees, PROGV and
/// other threads, unbound and constant variables, SYMBOL-VALUE and SET, and a
/// mid-loop deopt must all still print what the interpreter prints.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn special_variable_access_in_native_loops_matches_interpretation() {
    let program = r#"
(defvar *sv-x* 0)
(defvar *sv-y* 10)
(defvar *sv-u*)
(defvar *sv-l* nil)
(defconstant +sv-k+ 7)
(defmacro sv-check (form)
  `(print (handler-case ,form
            (unbound-variable (c) (list :unbound (cell-error-name c)))
            (error (c) (list :error (type-of c))))))
(defun sv-sum (n) (dotimes (i n) (incf *sv-x* i)) *sv-x*)
(defun sv-read-x () *sv-x*)
(defun sv-bump () (incf *sv-x*))
(defun sv-caller (n) (let ((*sv-x* 100)) (dotimes (i n) (sv-bump)) (sv-read-x)))
(defun sv-read-u (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s *sv-u*)))))
(defun sv-unbind-midway (n)
  (let ((s 0)) (dotimes (i n s) (when (= i 1500) (makunbound '*sv-u*)) (setq s (+ s *sv-u*)))))
(defun sv-progv (n) (progv '(*sv-x* *sv-y*) '(1000 2000) (dotimes (i n) (incf *sv-x*) (incf *sv-y* 2)) (list *sv-x* *sv-y* (sv-read-x))))
(defun sv-set (n) (dotimes (i n) (set '*sv-x* (+ (symbol-value '*sv-x*) 1))) *sv-x*)
(defun sv-setf (n) (dotimes (i n) (setf (symbol-value '*sv-x*) (+ *sv-x* 2))) (symbol-value '*sv-x*))
(defun sv-read-k (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s +sv-k+)))))
(defun sv-set-k () (set '+sv-k+ 8))
(defun sv-deopt (n) (dotimes (i n) (when (= i 2000) (setq *sv-x* 0.5d0)) (incf *sv-x*)) *sv-x*)
(defun sv-heap (n) (setq *sv-l* nil) (dotimes (i n) (setq *sv-l* (cons (list i) *sv-l*))) (list (length *sv-l*) (car *sv-l*)))
(defun sv-thread (k) (let ((*sv-x* (* k 1000))) (dotimes (i 3000) (incf *sv-x*)) *sv-x*))
(sv-check (progn (setq *sv-x* 0) (sv-sum 3000)))
(sv-check (progn (setq *sv-x* 0) (dotimes (k 30) (sv-sum 100)) *sv-x*))
(sv-check (progn (setq *sv-x* 5) (list (sv-caller 3000) *sv-x*)))
(sv-check (sv-read-u 3000))
(sv-check (progn (setq *sv-u* 1) (sv-read-u 3000)))
(sv-check (progn (setq *sv-u* 1) (sv-unbind-midway 3000)))
(sv-check (list (boundp '*sv-u*) (let ((*sv-u* 4)) (sv-read-u 3000)) (boundp '*sv-u*)))
(sv-check (progn (setq *sv-x* 1 *sv-y* 10) (list (sv-progv 3000) *sv-x* *sv-y*)))
(sv-check (progn (setq *sv-x* 0) (sv-set 3000)))
(sv-check (progn (setq *sv-x* 0) (sv-setf 3000)))
(sv-check (progn (setq *sv-x* 0) (list (let ((*sv-x* 50)) (sv-set 3000)) *sv-x*)))
(sv-check (sv-read-k 3000))
(sv-check (sv-set-k))
(sv-check (progn (setq *sv-x* 0) (sv-deopt 3000)))
(sv-check (sv-heap 3000))
(sv-check (progn (setq *sv-x* 77)
                 (let ((ts (loop for k from 1 to 4
                                 collect (let ((k k)) (egcl-thread:make-thread (lambda () (sv-thread k)))))))
                   (list (mapcar #'egcl-thread:join-thread ts) *sv-x*))))
(setq *sv-x* 0)
(dotimes (i 3000) (incf *sv-x* i))
(sv-check *sv-x*)
(format t "~%OSR ~s~%" (every #'plusp (mapcar #'egcl-ext:function-osr-count
                                              '(sv-sum sv-read-u sv-set sv-setf sv-deopt sv-heap))))
"#;
    let (tiered, oracle, log) = run_against_interpreter("special-access", program, &[]);
    assert_eq!(
        without_osr_report(&tiered),
        without_osr_report(&oracle),
        "{log}"
    );
    assert!(
        tiered.contains("OSR T"),
        "the loops must enter native code: {tiered}\n{log}"
    );
    // The inline path is what this exercises; the runtime path must agree too.
    let (fallback, _, log) = run_against_interpreter(
        "special-access-off",
        program,
        &[("EGCL_NO_INLINE_SPECIALS", "1")],
    );
    assert_eq!(
        without_osr_report(&fallback),
        without_osr_report(&oracle),
        "{log}"
    );
}

/// A hot loop running inside its own LET of a special leaves T0 with the
/// binding live (bliss-csc8v): the binding moves to the mapped activation,
/// stays visible to callees, survives a mid-loop deopt and is undone on every
/// exit, including an error, a THROW and a nested rebinding of the same name.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn loops_inside_special_bindings_enter_native_code_and_match_interpretation() {
    let program = r#"
(defvar *sb-x* 0)
(defvar *sb-y* 2)
(defun sb-read-x () *sb-x*)
(defun sb-around (n) (let ((*sb-x* 0)) (dotimes (i n) (incf *sb-x* i)) (list *sb-x* (sb-read-x))))
(defun sb-deopt (n)
  (let ((*sb-x* 0) (*sb-y* :outer))
    (dotimes (i n) (when (= i 2000) (setq *sb-x* (- most-positive-fixnum 5))) (incf *sb-x*))
    (list *sb-x* *sb-y* (sb-read-x))))
(defun sb-float (n) (let ((*sb-x* 0)) (dotimes (i n) (when (= i 2000) (setq *sb-x* 0.5d0)) (incf *sb-x*)) *sb-x*))
(defun sb-error (n) (let ((*sb-x* 0)) (dotimes (i n) (incf *sb-x*) (when (= i 2500) (error "stop ~a" *sb-x*)))))
(defun sb-throw (n) (let ((*sb-x* 0)) (dotimes (i n) (incf *sb-x*) (when (= i 2600) (throw 'sb-out (list :thrown *sb-x*))))))
(defun sb-nested (n) (let ((*sb-x* :a)) (let ((*sb-x* 0)) (dotimes (i n) (incf *sb-x*)) (setq *sb-y* *sb-x*)) *sb-x*))
(defun sb-callee (n) (let ((*sb-x* 5) (s 0)) (dotimes (i n s) (setq s (+ s (sb-read-x))) (when (= i 1500) (setq *sb-x* 6)))))
(defun sb-thread (k) (let ((*sb-x* (* k 1000))) (dotimes (i 3000) (incf *sb-x*)) *sb-x*))
(setq *sb-x* 9)
(print (list (sb-around 3000) *sb-x*))
(print (list (sb-deopt 3000) *sb-x* *sb-y*))
(print (list (sb-float 3000) *sb-x*))
(print (list (handler-case (sb-error 3000) (error (c) (princ-to-string c))) *sb-x*))
(print (list (catch 'sb-out (sb-throw 3000)) *sb-x*))
(print (list (sb-nested 3000) *sb-y* *sb-x*))
(print (list (sb-callee 3000) *sb-x*))
(print (let ((ts (loop for k from 1 to 4 collect (let ((k k)) (egcl-thread:make-thread (lambda () (sb-thread k)))))))
         (list (mapcar #'egcl-thread:join-thread ts) *sb-x*)))
(let ((*sb-x* 1)) (dotimes (i 3000) (incf *sb-x*)) (print *sb-x*))
(print *sb-x*)
(format t "~%OSR ~s~%" (every #'plusp (mapcar #'egcl-ext:function-osr-count
                                              '(sb-around sb-deopt sb-float sb-error sb-nested sb-callee))))
"#;
    let (tiered, oracle, log) = run_against_interpreter("special-bindings", program, &[]);
    assert_eq!(
        without_osr_report(&tiered),
        without_osr_report(&oracle),
        "{log}"
    );
    assert!(
        tiered.contains("OSR T"),
        "the bound loops must enter native code: {tiered}\n{log}"
    );
    for name in ["SB-AROUND", "SB-DEOPT", "SB-ERROR", "SB-NESTED"] {
        assert!(
            log.contains(&format!("{name}: T0 activation entered mapped")),
            "{name}:\n{log}"
        );
    }
}

/// A guard deopt inside one long-running activation no longer strands the
/// rest of that loop in T0 (bliss-k51a). Each function is warmed until T2 is
/// installed, then called with a loop whose arithmetic changes phase halfway:
/// fixnum to single-float, fixnum to bignum, single-float to fixnum. The
/// resumed loop re-warms, its failed version is retired for generic T1, and
/// it re-enters native code by OSR in the same activation, a bounded number
/// of times. A site that alternates every iteration settles on generic code.
/// Results must match the tree-walker.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn a_deoptimized_long_loop_reenters_native_code_after_its_phase_change() {
    let program = r#"
(defun pc-float (n h) (let ((acc 0)) (dotimes (i n acc) (setq acc (+ acc (if (< i h) 1 1.0f0))))))
(defun pc-bignum (n h) (let ((acc (- most-positive-fixnum h))) (dotimes (i n (- acc most-positive-fixnum)) (setq acc (+ acc 1)))))
(defun pc-fixnum (n h) (let ((acc 0) (x 0)) (dotimes (i n (list acc x)) (setq x (+ (if (< i h) 0.5f0 1) i)) (setq acc (+ acc 1)))))
(defun pc-alternate (n h) (declare (ignore h)) (let ((acc 0) (x 0)) (dotimes (i n acc) (setq x (if (evenp i) 1 1.0f0)) (setq acc (+ acc (* x 2))))))
(dolist (f '(pc-float pc-bignum pc-fixnum pc-alternate))
  (dotimes (k 30) (funcall f 40 100)))
(format t "~s~%" (list (pc-float 20000 10000) (pc-bignum 20000 10000) (pc-fixnum 20000 10000) (pc-alternate 20000 0)
                       (pc-float 20000 10000) (pc-bignum 20000 10000) (pc-fixnum 20000 10000) (pc-alternate 20000 0)))
(format t "OSR ~s~%" (mapcar #'egcl-ext:function-osr-count '(pc-float pc-bignum pc-fixnum pc-alternate)))
"#;
    let file = std::env::temp_dir().join(format!("egcl-phase-reosr-{}.lisp", std::process::id()));
    std::fs::write(&file, program).unwrap();
    let run = |tier: Option<&str>| {
        let mut command = Command::new(BIN);
        command
            .args(["--no-init", "--load"])
            .arg(&file)
            .env_remove("EGCL_FORCE_TIER")
            // A dispatch that queues T2 waits for it, so the phase change
            // always fails a guard of installed T2 code.
            .env("EGCL_T2", "1")
            .env("EGCL_OSR_THRESHOLD", "1000")
            .env("EGCL_T2_LOG", "stderr");
        if let Some(tier) = tier {
            command.env("EGCL_FORCE_TIER", tier);
        }
        let output = command.output().expect("run phase-change loops");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{stderr}");
        (String::from_utf8_lossy(&output.stdout).into_owned(), stderr)
    };
    let (tiered, log) = run(None);
    let (oracle, _) = run(Some("interp"));
    std::fs::remove_file(&file).unwrap();
    assert_eq!(tiered.lines().next(), oracle.lines().next(), "{log}");
    assert_eq!(
        tiered.lines().next(),
        Some("(20000.0 10000 (20000 20000) 40000.0 20000.0 10000 (20000 20000) 40000.0)"),
        "{log}"
    );
    let reentries = |name: &str| {
        log.matches(&format!("{name}: loop re-warmed after a guard deopt"))
            .count()
    };
    for name in ["PC-FLOAT", "PC-BIGNUM", "PC-FIXNUM"] {
        let count = reentries(name);
        assert!(
            (1..=4).contains(&count),
            "{name} re-entered {count} times:\n{log}"
        );
    }
    assert!(reentries("PC-ALTERNATE") <= 4, "{log}");
}

/// A loop in a once-called function whose accumulator is a double-float
/// leaves T0 by OSR into raw-double T2 (bliss-vwpaz, GitHub #642). Before,
/// such a loop stayed interpreted for the whole call and the function still
/// reported tier 0. The OSR companion of the raw T2 version unboxes the
/// loop's doubles at the header; NaN, infinities, -0.0, fixnum and
/// single-float contagion, a double turning fixnum mid-loop, a double
/// escaping into a list and a callee returning doubles must all print what
/// the interpreter prints.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn once_called_double_float_loops_enter_t2_and_match_interpretation() {
    let program = r#"
(defvar *df-inf* (* 1d308 10d0))
(defun df-sum (n) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s 1d0)))))
(defun df-negzero (n) (let ((s -0d0)) (dotimes (i n s) (setq s (* s 1d0)))))
(defun df-inf (n) (let ((s 1d300)) (dotimes (i n s) (setq s (* s 1.5d0)))))
(defun df-nan (n) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s (if (= i 100) (- *df-inf* *df-inf*) 0.5d0))))))
(defun df-fixnum (n) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s i)))))
(defun df-single (n) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s 1.5f0)))))
(defun df-to-fixnum (n) (let ((s 0d0) (k (floor n 2))) (dotimes (i n s) (setq s (if (>= i k) (+ (floor s) 1) (+ s 1d0))))))
(defun df-escape (n) (let ((acc nil) (s 0d0)) (dotimes (i n (list (length acc) (car acc) (reduce #'+ acc))) (setq s (+ s 0.25d0)) (when (zerop (mod i 1000)) (push s acc)))))
(defun df-step (i) (* i 1d-3))
(defun df-call (n) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s (df-step i))))))
(format t "~s~%" (list (df-sum 20000) (df-negzero 20000) (df-inf 20000) (df-nan 20000) (df-fixnum 20000)
                       (df-single 20000) (df-to-fixnum 20000) (df-escape 20000) (df-call 20000)))
(format t "OSR ~s~%" (mapcar (lambda (f) (list (egcl-ext:function-tier f) (plusp (egcl-ext:function-osr-count f))))
                             '(df-sum df-negzero df-inf df-nan df-fixnum df-single df-call)))
"#;
    let file = std::env::temp_dir().join(format!("egcl-double-osr-{}.lisp", std::process::id()));
    std::fs::write(&file, program).unwrap();
    let run = |tier: Option<&str>| {
        let mut command = Command::new(BIN);
        command
            .args(["--no-init", "--load"])
            .arg(&file)
            .env_remove("EGCL_FORCE_TIER")
            // A loop that queues T2 waits for it, so the entry is deterministic.
            .env("EGCL_T2", "1")
            .env("EGCL_OSR_THRESHOLD", "200")
            .env("EGCL_T2_LOG", "stderr");
        if let Some(tier) = tier {
            command.env("EGCL_FORCE_TIER", tier);
        }
        let output = command.output().expect("run double-float loops");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{stderr}");
        (String::from_utf8_lossy(&output.stdout).into_owned(), stderr)
    };
    let (tiered, log) = run(None);
    let (oracle, _) = run(Some("interp"));
    std::fs::remove_file(&file).unwrap();
    assert_eq!(tiered.lines().next(), oracle.lines().next(), "{log}");
    assert_eq!(
        tiered.lines().nth(1),
        Some("OSR ((2 T) (2 T) (2 T) (2 T) (2 T) (2 T) (2 T))"),
        "each once-called double loop must reach T2 by OSR:\n{log}"
    );
    assert!(
        log.contains("DF-SUM: raw double OSR companion emitted"),
        "{log}"
    );
    assert!(
        log.contains("DF-SUM: double-float loop enters T2 by OSR"),
        "{log}"
    );
}
