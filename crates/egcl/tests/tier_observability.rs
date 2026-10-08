// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tiering observability through the real binary (bliss-jtc.10).
//!
//! The stage-5 gate requires that "a hot loop is observably promoted through
//! tiers with identical results at each tier". These tests drive the shipping
//! `egcl` binary and use the Lisp-visible introspection builtins
//! (`egcl-ext:function-tier`, `-invoke-count`, `-back-edge-count`) to observe
//! the promotion, then confirm the observed result is unchanged from pure
//! interpretation.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run_output(program: &str, envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["--eval", program]);
    // Tier controls are per-test inputs, not ambient developer-shell state.
    for name in [
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_THRESHOLD",
        "EGCL_T2_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_LOOP_HEAT_THRESHOLD",
        "EGCL_T2",
        "EGCL_DISABLE_T2",
        "EGCL_OSR_THRESHOLD",
        "EGCL_T2_THREADS",
        "EGCL_COMPILE_QUEUE_SIZE",
        "EGCL_DEOPT_BLACKLIST_THRESHOLD",
        "EGCL_PROFILING_DISABLED",
        "EGCL_FORCE_TIER",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_LAZY_COMPILE",
        "EGCL_LOG",
        "EGCL_NN_DIRECT",
    ] {
        cmd.env_remove(name);
    }
    // These tests observe the tier LADDER given a compiled function, with small
    // per-test tier thresholds and short warm-up loops. Since EGCL_LAZY_COMPILE
    // is now the shipping default, a straight-line function would not compile
    // until the (separate, higher) lazy invoke threshold — well past these tests'
    // warm-up — so pin eager compilation here. The ladder mechanics are identical
    // however the function first compiled; lazy-trigger behaviour is covered by
    // the stage-5 gate and the lazy_compile_preserves_results acceptance test. A
    // test may still opt into lazy by passing EGCL_LAZY_COMPILE in `envs`.
    cmd.env("EGCL_LAZY_COMPILE", "0");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn egcl")
}

fn run(program: &str, envs: &[(&str, &str)]) -> (String, bool) {
    let out = run_output(program, envs);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    (text, out.status.success())
}

/// EGCL_PROFILING_DISABLED is the zero-overhead profiling mode from R4.58:
/// counters stay cold and automatic tier promotion is disabled, while ordinary
/// execution remains correct.
#[test]
fn profiling_disabled_omits_counters_and_tier_promotion() {
    let program = "\
        (defun profile-off-loop (n) \
          (let ((s 0)) \
            (dotimes (i n s) (setq s (+ s i))))) \
        (dotimes (k 20) (profile-off-loop 30)) \
        (format t \"~a ~a ~a ~a~%\" \
          (profile-off-loop 5) \
          (egcl-ext:function-tier (quote profile-off-loop)) \
          (egcl-ext:function-invoke-count (quote profile-off-loop)) \
          (egcl-ext:function-back-edge-count (quote profile-off-loop)))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_PROFILING_DISABLED", "1"),
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "2"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "2"),
        ],
    );
    assert!(ok, "profiling-disabled run failed: {out}");
    let line = out.lines().next().unwrap_or("");
    assert_eq!(
        line, "10 0 0 0",
        "disabled profiling must leave counters and tier cold: {line:?}"
    );
}

/// Generic dispatch records a bounded receiver profile for the first argument:
/// total calls keep increasing, but distinct receiver types are capped at the
/// fixed ring size required by R4.54. The disabled profiling mode keeps the
/// same profile cold.
#[test]
fn generic_dispatch_records_bounded_receiver_profile() {
    let program = "\
        (defclass rp-a () ()) \
        (defclass rp-b () ()) \
        (defclass rp-c () ()) \
        (defclass rp-d () ()) \
        (defclass rp-e () ()) \
        (defgeneric rp-g (x)) \
        (defmethod rp-g ((x rp-a)) 1) \
        (defmethod rp-g ((x rp-b)) 2) \
        (defmethod rp-g ((x rp-c)) 3) \
        (defmethod rp-g ((x rp-d)) 4) \
        (defmethod rp-g ((x rp-e)) 5) \
        (let ((a (make-instance (quote rp-a))) \
              (b (make-instance (quote rp-b))) \
              (c (make-instance (quote rp-c))) \
              (d (make-instance (quote rp-d))) \
              (e (make-instance (quote rp-e)))) \
          (rp-g a) (rp-g b) (rp-g c) (rp-g d) (rp-g a) (rp-g e) \
          (format t \"~a ~a~%\" \
            (egcl-ext:generic-receiver-profile-count (quote rp-g)) \
            (egcl-ext:generic-receiver-profile-distinct-count (quote rp-g))))";
    let (out, ok) = run(program, &[]);
    assert!(ok, "receiver-profile run failed: {out}");
    let line = out.lines().next().unwrap_or("");
    assert_eq!(
        line, "6 4",
        "profile should count all calls but retain a four-type ring: {line:?}"
    );

    let (disabled, disabled_ok) = run(program, &[("EGCL_PROFILING_DISABLED", "1")]);
    assert!(
        disabled_ok,
        "disabled receiver-profile run failed: {disabled}"
    );
    let disabled_line = disabled.lines().next().unwrap_or("");
    assert_eq!(
        disabled_line, "0 0",
        "disabled profiling must not record receiver profiles: {disabled_line:?}"
    );
}

/// A single long call starts in installed T1, queues T2 from a sampled
/// backward branch, then enters the optimized loop without returning to the
/// dispatcher. The high invocation and old T0-OSR thresholds rule out either
/// of those promotion paths. A back-edge count far below N is also direct
/// evidence that T1 stopped executing when T2 took over.
#[cfg(target_arch = "x86_64")]
#[test]
fn running_t1_loop_osrs_into_background_compiled_t2() {
    let program = "\
        (defun live-osr (n) \
          (let ((sum 0)) \
            (dotimes (i n sum) (setq sum (+ sum i))))) \
        (live-osr 1) (live-osr 2) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote live-osr))) \
        (let ((answer (live-osr 10000000))) \
          (format t \"~a ~a ~a~%\" answer \
            (egcl-ext:function-tier (quote live-osr)) \
            (egcl-ext:function-back-edge-count (quote live-osr))))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "1000"),
            ("EGCL_OSR_THRESHOLD", "100000000"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(ok, "live T1→T2 OSR failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("1"),
        "long call must start from T1: {out}"
    );
    let fields: Vec<_> = lines.get(1).unwrap_or(&"").split_whitespace().collect();
    assert_eq!(
        fields.first().copied(),
        Some("49999995000000"),
        "OSR result: {out}"
    );
    assert_eq!(
        fields.get(1).copied(),
        Some("2"),
        "T2 must publish during the call: {out}"
    );
    let back_edges: u32 = fields.get(2).unwrap_or(&"0").parse().unwrap_or(0);
    assert!(
        (1000..10_000_000).contains(&back_edges),
        "T1 should hand off before completing all iterations: {out}"
    );
}

/// A guard failure after the live T1→T2 handoff must preserve the operand stack
/// reconstructed by T2. This overflows one million iterations before loop end,
/// forcing precise T2→T0 deopt and bignum completion on the same activation.
#[cfg(target_arch = "x86_64")]
#[test]
fn t1_to_t2_osr_guard_deopts_with_exact_live_state() {
    let program = "\
        (defun live-osr-overflow (n) \
          (let ((sum 1152921504601846975)) \
            (dotimes (i n sum) (setq sum (+ sum 1))))) \
        (live-osr-overflow 1) (live-osr-overflow 2) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote live-osr-overflow))) \
        (let ((before (egcl-ext:deopt-count)) \
              (answer (live-osr-overflow 6000000))) \
          (format t \"~a ~a ~a~%\" answer \
            (egcl-ext:function-tier (quote live-osr-overflow)) \
            (- (egcl-ext:deopt-count) before)))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "1000"),
            ("EGCL_OSR_THRESHOLD", "100000000"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(ok, "live T2 guard deopt failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("1"),
        "long call must start from T1: {out}"
    );
    let fields: Vec<_> = lines.get(1).unwrap_or(&"").split_whitespace().collect();
    assert_eq!(
        fields.first().copied(),
        Some("1152921504607846975"),
        "OSR/deopt must complete with the exact bignum result: {out}"
    );
    assert_eq!(
        fields.get(1).copied(),
        Some("2"),
        "T2 must remain installed: {out}"
    );
    let deopts: u64 = fields.get(2).unwrap_or(&"0").parse().unwrap_or(0);
    assert!(deopts >= 1, "the overflow guard must deopt: {out}");
}

/// Dynamic control state surrounding a live T1→T2 OSR and later T2→T0 deopt
/// must remain intact. The loop reads the special `*osr-step*` every iteration:
/// the dynamic binding to 2 changes the final bignum result, the catch tag
/// receives that deopt-completed value, and the unwind-protect cleanup runs
/// exactly once while unwinding through the catch.
#[cfg(target_arch = "x86_64")]
#[test]
fn t1_to_t2_osr_deopt_preserves_dynamic_state() {
    let program = "\
        (defvar *osr-step* 1) \
        (defvar *osr-log* nil) \
        (defun dynamic-osr-loop (n) \
          (let ((sum 1152921504601846975)) \
            (dotimes (i n sum) (setq sum (+ sum *osr-step*))))) \
        (dynamic-osr-loop 1) (dynamic-osr-loop 2) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote dynamic-osr-loop))) \
        (let ((*osr-step* 2)) \
          (setq *osr-log* nil) \
          (let ((answer \
                  (catch (quote done) \
                    (unwind-protect \
                      (throw (quote done) (dynamic-osr-loop 4000000)) \
                      (setq *osr-log* (cons (quote cleanup) *osr-log*)))))) \
            (format t \"~a ~a ~a ~a ~a~%\" \
              answer *osr-step* *osr-log* \
              (egcl-ext:function-tier (quote dynamic-osr-loop)) \
              (egcl-ext:function-back-edge-count (quote dynamic-osr-loop)))))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "1000"),
            ("EGCL_OSR_THRESHOLD", "100000000"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(ok, "dynamic-state OSR/deopt failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("1"),
        "long call must start from T1: {out}"
    );
    let fields: Vec<_> = lines.get(1).unwrap_or(&"").split_whitespace().collect();
    assert_eq!(
        fields.first().copied(),
        Some("1152921504609846975"),
        "dynamic special binding must affect the deopt-completed result: {out}"
    );
    assert_eq!(
        fields.get(1).copied(),
        Some("2"),
        "dynamic binding must still be active: {out}"
    );
    assert_eq!(
        fields.get(2).copied(),
        Some("(CLEANUP)"),
        "unwind-protect cleanup: {out}"
    );
    assert_eq!(
        fields.get(3).copied(),
        Some("2"),
        "T2 must publish during the call: {out}"
    );
    let back_edges: u32 = fields.get(4).unwrap_or(&"0").parse().unwrap_or(0);
    assert!(
        (1000..4_000_000).contains(&back_edges),
        "T1 should hand off to T2 before completing the loop: {out}"
    );
}

/// A heap object live in the activation must remain rooted while the function
/// OSRs from T1 to T2 and then deopts back to T0. GC stress/poison forces
/// moving collections around allocation-heavy setup and deopt completion; if
/// the transition loses the `root` slot, the final CAR/LENGTH check will crash
/// or read poison.
#[cfg(target_arch = "x86_64")]
#[test]
fn t1_to_t2_osr_deopt_keeps_heap_roots_under_gc_stress() {
    let program = "\
        (defun gc-osr-loop (root n) \
          (let ((sum 1152921504606845000)) \
            (dotimes (i n root) (setq sum (+ sum 1))))) \
        (let ((root (list (quote anchor) (cons 1 2) \"payload\"))) \
          (gc-osr-loop root 1) (gc-osr-loop root 2) \
          (format t \"~a~%\" (egcl-ext:function-tier (quote gc-osr-loop))) \
          (let ((before (egcl-ext:deopt-count)) \
                (r (gc-osr-loop root 5000))) \
            (format t \"~a ~a ~a ~a ~a~%\" \
              (car r) (length r) \
              (egcl-ext:function-tier (quote gc-osr-loop)) \
              (egcl-ext:function-back-edge-count (quote gc-osr-loop)) \
              (- (egcl-ext:deopt-count) before))))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "1000"),
            ("EGCL_OSR_THRESHOLD", "100000000"),
            ("EGCL_T2_THREADS", "1"),
            ("EGCL_HEAP_MB", "4096"),
            ("EGCL_GC_STRESS", "8"),
            ("EGCL_GC_POISON", "1"),
        ],
    );
    assert!(ok, "GC-stress OSR/deopt failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("1"),
        "long call must start from T1: {out}"
    );
    let fields: Vec<_> = lines.get(1).unwrap_or(&"").split_whitespace().collect();
    assert_eq!(
        fields.first().copied(),
        Some("ANCHOR"),
        "heap list root must survive transition GC stress: {out}"
    );
    assert_eq!(
        fields.get(1).copied(),
        Some("3"),
        "root list structure must survive transition GC stress: {out}"
    );
    assert_eq!(
        fields.get(2).copied(),
        Some("2"),
        "T2 must publish during the call: {out}"
    );
    let back_edges: u32 = fields.get(3).unwrap_or(&"0").parse().unwrap_or(0);
    assert!(
        (1000..5000).contains(&back_edges),
        "T1 should hand off to T2 before completing the loop: {out}"
    );
    let deopts: u64 = fields.get(4).unwrap_or(&"0").parse().unwrap_or(0);
    assert!(deopts >= 1, "overflow guard must deopt under stress: {out}");
}

/// Multiple values produced after a T2 guard deopt must flow through the
/// reconstructed interpreter continuation. The hot loop OSRs from T1 to T2,
/// overflows in the native body, resumes in T0, completes the DOTIMES result
/// form, and returns both values to MULTIPLE-VALUE-BIND.
#[cfg(target_arch = "x86_64")]
#[test]
fn t1_to_t2_osr_deopt_preserves_multiple_values() {
    let program = "\
        (defun mv-osr-loop (n) \
          (let ((sum 1152921504601846975)) \
            (dotimes (i n (values sum (+ sum 1))) \
              (setq sum (+ sum 2))))) \
        (mv-osr-loop 1) (mv-osr-loop 2) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote mv-osr-loop))) \
        (multiple-value-bind (primary secondary) (mv-osr-loop 4000000) \
          (format t \"~a ~a ~a ~a~%\" \
            primary secondary \
            (egcl-ext:function-tier (quote mv-osr-loop)) \
            (egcl-ext:function-back-edge-count (quote mv-osr-loop))))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "100000000"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "1000"),
            ("EGCL_OSR_THRESHOLD", "100000000"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(ok, "multiple-value OSR/deopt failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("1"),
        "long call must start from T1: {out}"
    );
    let fields: Vec<_> = lines.get(1).unwrap_or(&"").split_whitespace().collect();
    assert_eq!(
        fields.first().copied(),
        Some("1152921504609846975"),
        "primary value must be the deopt-completed sum: {out}"
    );
    assert_eq!(
        fields.get(1).copied(),
        Some("1152921504609846976"),
        "secondary value must survive the reconstructed continuation: {out}"
    );
    assert_eq!(
        fields.get(2).copied(),
        Some("2"),
        "T2 must publish during the call: {out}"
    );
    let back_edges: u32 = fields.get(3).unwrap_or(&"0").parse().unwrap_or(0);
    assert!(
        (1000..4_000_000).contains(&back_edges),
        "T1 should hand off to T2 before completing the loop: {out}"
    );
}

/// Warm straight-line helpers should already run natively in T1 without paying
/// for optimizing compilation that a short initial library load cannot amortize.
#[cfg(target_arch = "x86_64")]
#[test]
fn default_tiering_keeps_warm_helpers_native_without_requesting_t2() {
    let out = run_output(
        "(defun warm-helper (x) (* x 5)) \
         (dotimes (i 768) (warm-helper i)) \
         (format t \"~a ~a~%\" \
           (egcl-ext:function-tier 'warm-helper) (warm-helper 7))",
        &[("EGCL_T2_LOG", "-")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "warm helper failed:\n{stderr}");
    assert!(
        !stderr.contains("WARM-HELPER: queued for background T2 compilation"),
        "warm helpers must not pay for T2 compilation:\n{stderr}"
    );
    assert_eq!(stdout.lines().next(), Some("1 35"), "{stdout}");
}

/// With all tier-control variables absent, the default thresholds eventually
/// install T2. This is the regression test for removing the old opt-in gate.
#[cfg(target_arch = "x86_64")]
#[test]
fn default_tiering_reaches_t2_without_any_t2_environment_variable() {
    let program = "\
        (defun default-tier (x) (* x 5)) \
        (dotimes (i 5010) (default-tier i)) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote default-tier)) (default-tier 7))";
    let (out, ok) = run(program, &[]);
    assert!(ok, "default automatic tiering failed: {out}");
    assert_eq!(
        out.lines().next(),
        Some("2 35"),
        "default settings must reach T2 without EGCL_T2=1: {out}"
    );
}

/// A sustained numeric phase change invalidates only the stale speculative T2
/// version. Dispatch keeps a generic native T1 fallback installed while the
/// profile compiles the opposite specialization; it must not permanently drop
/// the function to T0. Exercise both Fixnum→SingleFloat and the reverse.
#[cfg(target_arch = "x86_64")]
#[test]
fn numeric_phase_changes_recompile_t2_instead_of_blacklisting() {
    let program = "\
        (defun phase-number (x) (* x 5)) \
        (dotimes (i 20) (phase-number i)) \
        (format t \"initial=~a~%\" \
          (egcl-ext:function-tier (quote phase-number))) \
        (dotimes (i 20) (phase-number 2.5)) \
        (format t \"float=~a tier=~a~%\" (phase-number 2.5) \
          (egcl-ext:function-tier (quote phase-number))) \
        (disassemble (quote phase-number)) \
        (dotimes (i 20) (phase-number 7)) \
        (format t \"fixnum=~a tier=~a~%\" (phase-number 7) \
          (egcl-ext:function-tier (quote phase-number))) \
        (disassemble (quote phase-number))";

    let (out, ok) = run(
        program,
        &[
            ("EGCL_T2", "1"),
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_DEOPT_BLACKLIST_THRESHOLD", "3"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(ok, "numeric phase-change program failed: {out}");
    assert!(out.lines().any(|line| line == "initial=2"), "{out}");
    assert!(out.lines().any(|line| line == "float=12.5 tier=2"), "{out}");
    assert!(out.lines().any(|line| line == "fixnum=35 tier=2"), "{out}");
    assert!(
        out.contains("mulss xmm0,xmm1"),
        "missing float T2 version:\n{out}"
    );
    assert!(
        out.lines().any(|line| line.contains("imul ")),
        "missing fixnum T2 version:\n{out}"
    );
}

/// Bignum operands need generic arithmetic, but must not blacklist the native
/// function after its initial fixnum specialization stops matching.
#[cfg(target_arch = "x86_64")]
#[test]
fn bignum_phase_recompiles_t2_instead_of_blacklisting() {
    let program = r#"
        (defun integer-phase (x) (+ x 1))
        (dotimes (i 30) (assert (= (integer-phase i) (+ i 1))))
        (format t "initial=~a~%" (egcl-ext:function-tier 'integer-phase))
        (let ((large (expt 2 61)))
          (dotimes (i 30) (assert (= (integer-phase large) (+ large 1)))))
        (format t "bignum=~a~%" (egcl-ext:function-tier 'integer-phase))
        (dotimes (i 30) (assert (= (integer-phase i) (+ i 1))))
        (format t "returned=~a~%" (egcl-ext:function-tier 'integer-phase))
    "#;
    let (out, ok) = run(program, &[("EGCL_FORCE_TIER", "t2")]);
    assert!(ok, "bignum phase produced incorrect results: {out}");
    for expected in ["initial=2", "bignum=2", "returned=2"] {
        assert!(out.lines().any(|line| line == expected), "missing {expected}: {out}");
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn mixed_bignum_loop_stops_deoptimizing_after_recovery() {
    let program = r#"
        (defun mixed-integers (big)
          (let ((result 0))
            (dotimes (i 200 result)
              (setq result (+ (if (= i 0) big i) 1)))))
        (dotimes (i 30) (assert (= 200 (mixed-integers 0))))
        (dotimes (i 80)
          (assert (= 200 (mixed-integers 2305843009213693952))))
        (let ((before (egcl-ext:deopt-count)))
          (dotimes (i 40)
            (assert (= 200 (mixed-integers 2305843009213693952))))
          (format t "steady=~D tier=~D~%" (- (egcl-ext:deopt-count) before)
                  (egcl-ext:function-tier 'mixed-integers)))
    "#;
    let (out, ok) = run(program, &[("EGCL_FORCE_TIER", "t2")]);
    assert!(ok, "mixed integer loop returned incorrect results: {out}");
    assert!(out.lines().any(|line| line == "steady=0 tier=2"), "{out}");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn bignum_with_single_float_recovers_to_generic_t2() {
    let program = r#"
        (defun float-with-big (x) (+ x 1.0))
        (dotimes (i 30) (float-with-big 2.0))
        (dotimes (i 30)
          (assert (= (float-with-big 2305843009213693952) 2305843009213693952.0)))
        (format t "float-bignum=~D~%" (egcl-ext:function-tier 'float-with-big))
    "#;
    let (out, ok) = run(program, &[("EGCL_FORCE_TIER", "t2")]);
    assert!(ok, "mixed bignum/float result was incorrect: {out}");
    assert!(out.lines().any(|line| line == "float-bignum=2"), "{out}");
}

/// The shipping dispatcher has two real transitions. With T2 enabled by
/// default, the function is first observable at T1 and only later crosses the
/// independent T1→T2 invocation threshold; no `EGCL_T2=1` opt-in is used.
#[cfg(target_arch = "x86_64")]
#[test]
fn automatic_tiering_observably_progresses_t0_to_t1_to_t2() {
    let program = "\
        (defun auto-tier (x) (* x 5)) \
        (auto-tier 1) (auto-tier 2) (auto-tier 3) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote auto-tier))) \
        (auto-tier 4) (auto-tier 5) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote auto-tier)) (auto-tier 7))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
        ],
    );
    assert!(ok, "automatic tiering run failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("1"),
        "T1 must be observable first: {out}"
    );
    assert_eq!(lines.get(1).copied(), Some("2 35"), "T2 tier/result: {out}");
}

/// Reaching the T2 threshold is a request, not permission to discard working
/// code. A CAPTURING variadic body (its &rest is boxed for a closure) is
/// unsupported by T2's slot-only entry, so it keeps its T1 entry (bliss-32l;
/// non-capturing variadic functions do reach T2).
#[test]
fn automatic_t2_decline_retains_t1() {
    let program = "\
        (defun auto-rest (&rest xs) (funcall (lambda () (length xs)))) \
        (auto-rest 1) (auto-rest 1 2) (auto-rest 1 2 3) \
        (auto-rest 1) (auto-rest 1 2) (auto-rest 1 2 3) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote auto-rest)) (auto-rest 1 2 3 4))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
        ],
    );
    assert!(ok, "T2-decline run failed: {out}");
    assert_eq!(
        out.lines().next(),
        Some("1 4"),
        "declined T2 must retain T1: {out}"
    );
}

/// A movable literal remains rooted from the background snapshot through the
/// installed code's lifetime. T2 loads it indirectly from that GC-rewritten
/// constant slot, so a moving collection never leaves a stale pointer in code.
#[cfg(target_arch = "x86_64")]
#[test]
fn background_t2_loads_a_rooted_movable_literal_from_its_constant_slot() {
    let program = "\
        (defun rooted-literal (x) (if x (quote (11 22)) nil)) \
        (dotimes (i 20) (rooted-literal t)) \
        (dotimes (i 2000) \
          (egcl-ext:function-tier (quote rooted-literal))) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote rooted-literal)) (rooted-literal t))";
    let out = run_output(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_T2_THREADS", "1"),
            ("EGCL_T2_LOG", "-"),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "rooted-literal run failed:\n{stderr}");
    assert_eq!(stdout.lines().next(), Some("2 (11 22)"), "{stdout}");
    assert!(
        stderr.contains("ROOTED-LITERAL: queued for background T2 compilation"),
        "movable snapshot never reached the background compiler:\n{stderr}"
    );
    assert!(
        stderr.contains("ROOTED-LITERAL: background T2 result published"),
        "heap literal did not reach installed T2 code:\n{stderr}"
    );
    assert!(
        !stderr.contains("movable literal cannot be rooted"),
        "obsolete admission guard rejected the rooted snapshot:\n{stderr}"
    );
}

/// T2 can be disabled for differential/debug runs without restoring the old
/// opt-in model: normal operation is automatic, while the explicit switch pins
/// an otherwise-hot function at its working T1 entry.
#[test]
fn explicit_t2_disable_pins_hot_function_at_t1() {
    let program = "\
        (defun no-t2 (x) (* x 5)) \
        (dotimes (i 10) (no-t2 i)) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote no-t2)) (no-t2 7))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_DISABLE_T2", "1"),
        ],
    );
    assert!(ok, "T2-disabled run failed: {out}");
    assert_eq!(
        out.lines().next(),
        Some("1 35"),
        "disable switch must retain T1: {out}"
    );
}

/// Once a caller becomes native, its c2i calls still count and promote the
/// callee. The CAPTURING variadic caller deliberately stays T1 (its &rest is
/// boxed for the closure, so it is declined from T2 — bliss-32l) so the
/// fixed-arity leaf is reached through the native adapter long enough to become T2.
#[cfg(target_arch = "x86_64")]
#[test]
fn native_caller_continues_warming_callee_to_t2() {
    let program = "\
        (defun warm-leaf (x) (* x 5)) \
        (defun warm-driver (&rest xs) (funcall (lambda () (warm-leaf (car xs))))) \
        (dotimes (i 10) (warm-driver i)) \
        (format t \"~a ~a ~a~%\" \
          (egcl-ext:function-tier (quote warm-driver)) \
          (egcl-ext:function-tier (quote warm-leaf)) \
          (warm-driver 7))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
        ],
    );
    assert!(ok, "native-caller warmup failed: {out}");
    assert_eq!(
        out.lines().next(),
        Some("1 2 35"),
        "callee must reach T2: {out}"
    );
}

/// Once a total callee reaches T2, a subsequently compiled T1 caller can bake
/// its frame-ABI entry directly. The compile trace makes the selected path
/// observable; result/tier checks ensure this is the real installed T2 code.
#[cfg(target_arch = "x86_64")]
#[test]
fn t1_direct_calls_non_deopting_t2_callee() {
    let program = "\
        (defun t2-direct-leaf (x) x) \
        (dotimes (i 20) (t2-direct-leaf i)) \
        (dotimes (i 2000) \
          (when (eql (egcl-ext:function-tier (quote t2-direct-leaf)) 2) (return)) \
          (t2-direct-leaf i)) \
        (defun t2-direct-driver (x) (t2-direct-leaf x)) \
        (t2-direct-driver 1) (t2-direct-driver 2) \
        (format t \"~a ~a ~a~%\" \
          (egcl-ext:function-tier (quote t2-direct-leaf)) \
          (egcl-ext:function-tier (quote t2-direct-driver)) \
          (t2-direct-driver 9))";
    let out = run_output(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_LOG", "compile=trace"),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "T2 direct-call run failed:\n{stderr}");
    assert_eq!(
        stdout.lines().next(),
        Some("2 1 9"),
        "wrong tiers/result: {stdout}"
    );
    assert!(
        stderr.contains("[T1] T2-DIRECT-DRIVER: direct call to T2-DIRECT-LEAF [T2]"),
        "caller did not select the T2 direct-call path:\n{stderr}"
    );
}

/// A speculative T2 callee must retain the c2i path: its deopt resumes inside
/// `run_native`, a transition the direct caller deliberately cannot process.
#[cfg(target_arch = "x86_64")]
#[test]
fn t1_does_not_direct_call_deopting_t2_callee() {
    let program = "\
        (defun t2-deopt-leaf (x) (* x 5)) \
        (dotimes (i 20) (t2-deopt-leaf i)) \
        (dotimes (i 2000) \
          (when (eql (egcl-ext:function-tier (quote t2-deopt-leaf)) 2) (return)) \
          (t2-deopt-leaf i)) \
        (defun t2-deopt-driver (x) (t2-deopt-leaf x)) \
        (t2-deopt-driver 1) (t2-deopt-driver 2) \
        (format t \"~a ~a ~a~%\" \
          (egcl-ext:function-tier (quote t2-deopt-leaf)) \
          (egcl-ext:function-tier (quote t2-deopt-driver)) \
          (= (t2-deopt-driver 2.0) 10.0))";
    let out = run_output(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_LOG", "compile=trace"),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "T2 deopt boundary run failed:\n{stderr}"
    );
    assert_eq!(
        stdout.lines().next(),
        Some("2 1 T"),
        "wrong tiers/result: {stdout}"
    );
    assert!(
        !stderr.contains("[T1] T2-DEOPT-DRIVER: direct call to T2-DEOPT-LEAF [T2]"),
        "deopt-capable T2 callee was unsafely direct-called:\n{stderr}"
    );
}

/// Loop heat accumulated in T0 is an independent T1→T2 trigger. The high
/// invocation threshold proves the third call promotes because of back-edges.
#[cfg(target_arch = "x86_64")]
#[test]
fn automatic_t2_promotion_uses_backedge_threshold() {
    let program = "\
        (defun auto-loop (n) \
          (let ((sum 0)) \
            (dotimes (i n sum) (setq sum (+ sum i))))) \
        (auto-loop 20) (auto-loop 2) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote auto-loop))) \
        (auto-loop 3) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote auto-loop)) (auto-loop 10))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "1000"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "5"),
        ],
    );
    assert!(ok, "back-edge tiering run failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("1"),
        "loop reaches T1 first: {out}"
    );
    assert_eq!(
        lines.get(1).copied(),
        Some("2 45"),
        "loop reaches T2 from heat: {out}"
    );
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
                (egcl-ext:function-tier (quote spin)) \
                (egcl-ext:function-invoke-count (quote spin)) \
                (egcl-ext:function-back-edge-count (quote spin)))";
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
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote sq)) (sq 9))";

    // Under a low T1 threshold, sq promotes to tier 1 and still returns 81.
    let (t1_out, t1_ok) = run(program, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(t1_ok, "T1 run must succeed; got:\n{t1_out}");
    let t1_line = t1_out.lines().next().unwrap_or("");
    let t1_fields: Vec<&str> = t1_line.split_whitespace().collect();
    assert_eq!(
        t1_fields.first().copied(),
        Some("1"),
        "sq must reach T1: {t1_line:?}"
    );
    assert_eq!(
        t1_fields.get(1).copied(),
        Some("81"),
        "T1 result must be 81: {t1_line:?}"
    );

    // Under the pure tree-walker, the result is identical (tier is 0 there).
    let (tw_out, tw_ok) = run(program, &[("EGCL_BACKEND", "tree-walker")]);
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
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote sumto)) (sumto 100))";

    let (t1_out, t1_ok) = run(program, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(t1_ok, "T1 run must succeed; got:\n{t1_out}");
    let fields: Vec<&str> = t1_out
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    assert_eq!(
        fields.first().copied(),
        Some("1"),
        "the loop must reach T1: {t1_out:?}"
    );
    assert_eq!(
        fields.get(1).copied(),
        Some("5050"),
        "T1 loop result must be 5050"
    );

    // Identical under pure interpretation.
    let (tw_out, tw_ok) = run(program, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run must succeed; got:\n{tw_out}");
    let tw_result = tw_out
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(str::to_string);
    assert_eq!(
        tw_result.as_deref(),
        Some("5050"),
        "interpreter result must also be 5050"
    );
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
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote tri)) (tri 100))";
    let (out, ok) = run(dt, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "dotimes run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        f.first().copied(),
        Some("1"),
        "dotimes loop must reach T1: {out:?}"
    );
    assert_eq!(f.get(1).copied(), Some("4950"), "dotimes result");

    // DOLIST sum.
    let dl = "\
        (defun sm (lst) (let ((s 0)) (dolist (x lst s) (setq s (+ s x))))) \
        (sm (list 1 2 3)) (sm (list 1 2 3)) (sm (list 1 2 3)) \
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote sm)) (sm (list 10 20 30 40)))";
    let (out, ok) = run(dl, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "dolist run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        f.first().copied(),
        Some("1"),
        "dolist loop must reach T1: {out:?}"
    );
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
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote g)) (g 100))";
    let (out, ok) = run(simple, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "simple loop run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        f.first().copied(),
        Some("1"),
        "simple loop must reach T1: {out:?}"
    );
    assert_eq!(f.get(1).copied(), Some("4950"), "simple loop result");

    // Extended LOOP with an ascending numeric `for` now lowers to the same
    // block/let/tagbody/go shape as DOTIMES and promotes to T1 (bliss-x5y.3),
    // with the same value. (Non-numeric-for extended loops still bail to T0.)
    let extended = "\
        (defun h (n) (let ((s 0)) (loop for i from 1 to n do (setq s (+ s i))) s)) \
        (h 5) (h 5) (h 5) \
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote h)) (h 100))";
    let (out, ok) = run(extended, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "extended loop run failed: {out}");
    let out = out.lines().next().unwrap_or("").to_string();
    let f: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        f.first().copied(),
        Some("1"),
        "numeric-for loop reaches T1: {out:?}"
    );
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
/// `EGCL_T2=1` is a no-op and the assertion below would still hold at T1, so we
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
    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
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
    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let twl = tw.lines().next().unwrap_or("").to_string();
    assert_eq!(
        twl, line,
        "T2 result must match the tree-walker: {line:?} vs {twl:?}"
    );
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
        (let ((before (egcl-ext:deopt-count)) \
              (value (inline-deopt 1.5))) \
          (format t \"~a ~a ~a~%\" before value (egcl-ext:deopt-count)))";

    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "T2 intrinsic/deopt run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        fields.get(1).copied(),
        Some("3.5"),
        "forced-deopt result: {line:?}"
    );
    let before: u64 = fields
        .first()
        .expect("before count")
        .parse()
        .expect("count");
    let after: u64 = fields.get(2).expect("after count").parse().expect("count");
    assert_eq!(
        after,
        before + 1,
        "the cold float path must deopt exactly once: {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let tw_fields: Vec<&str> = tw.lines().next().unwrap_or("").split_whitespace().collect();
    assert_eq!(
        tw_fields.get(1).copied(),
        Some("3.5"),
        "tree-walker oracle: {tw:?}"
    );
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
    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "T2 STRINGP run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(line, "10 20 10");

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker STRINGP run failed: {tw}");
    assert_eq!(tw.lines().next().unwrap_or(""), line);
}

/// UIOP/UTILITY:FIRST-CHAR is selected by package-aware inline metadata.  Its
/// ASCII/non-empty path runs entirely through string layout IR; empty and
/// wrong-type inputs deopt at the original call and evaluate the Lisp body,
/// returning NIL exactly as the tree-walker does.
#[test]
fn metadata_first_char_fast_path_and_deopts_match_the_lisp_body() {
    let prog = "\
        (defpackage :uiop/utility (:use :cl) (:export :first-char)) \
        (in-package :uiop/utility) \
        (defun first-char (s) \
          (and (stringp s) (plusp (length s)) (char s 0))) \
        (in-package :cl-user) \
        (defun first-char-probe (x) (uiop/utility:first-char x)) \
        (dotimes (k 60) (first-char-probe \"warm\")) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"~a ~a ~a ~a ~a~%\" \
            (first-char-probe \"abc\") \
            (first-char-probe \"\") \
            (first-char-probe 7) \
            (first-char-probe \"éclair\") \
            (- (egcl-ext:deopt-count) before)))";

    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "T2 FIRST-CHAR run failed: {out}");
    let line = out.lines().next().unwrap_or("");
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields.len(), 5, "unexpected FIRST-CHAR output: {line:?}");
    assert_eq!(&fields[..4], &["a", "NIL", "NIL", "é"]);
    assert_eq!(
        fields.get(4).copied(),
        Some("3"),
        "empty, wrong-type, and Unicode cases deopt: {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker FIRST-CHAR run failed: {tw}");
    let tw_line = tw.lines().next().unwrap_or("");
    let tw_fields: Vec<&str> = tw_line.split_whitespace().collect();
    assert_eq!(
        tw_fields.len(),
        5,
        "unexpected tree-walker output: {tw_line:?}"
    );
    assert_eq!(&tw_fields[..4], &fields[..4]);
}

/// A guard inside a cloned saved body reconstructs both the inlined callee and
/// its caller. The caller increments `*inline-hits*` before the call; resuming
/// or re-running the wrong frame would increment it twice. The empty-string
/// guard must deopt once, return through the reconstructed callee, and continue
/// the suspended caller with exactly one committed increment.
#[test]
fn body_inline_deopt_reconstructs_callee_and_caller() {
    let prog = "\
        (defpackage :uiop/utility (:use :cl) (:export :first-char)) \
        (in-package :uiop/utility) \
        (defun first-char (s) \
          (and (stringp s) (plusp (length s)) (char s 0))) \
        (in-package :cl-user) \
        (defvar *inline-hits* 0) \
        (defun inline-leaf (s) (uiop/utility:first-char s)) \
        (defun inline-caller (s) \
          (setq *inline-hits* (+ *inline-hits* 1)) \
          (inline-leaf s)) \
        (defun inline-pure-caller (s) (inline-leaf s)) \
        (dotimes (k 60) (inline-caller \"warm\")) \
        (dotimes (k 60) (inline-pure-caller \"warm\")) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"~a ~a ~a ~a ~a ~a~%\" \
            (egcl-ext:function-tier (quote inline-caller)) \
            (inline-caller \"\") \
            (egcl-ext:function-tier (quote inline-pure-caller)) \
            (inline-pure-caller \"\") \
            *inline-hits* \
            (- (egcl-ext:deopt-count) before)))";

    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "inlined multi-scope deopt failed: {out}");
    let line = out.lines().next().unwrap_or("");
    assert_eq!(
        line, "2 NIL 2 NIL 61 2",
        "effectful/pure tiers and results, side effects, deopts: {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker oracle failed: {tw}");
    let fields: Vec<_> = tw.lines().next().unwrap_or("").split_whitespace().collect();
    assert_eq!(&fields[1..5], &["NIL", "0", "NIL", "61"]);
}

/// Two independently built callee bodies each carry a simple-string layout
/// proof.  Once both bodies are cloned into the caller, the production T2
/// GuardElim pass must retain only the first dominating proof.  This exercises
/// the real binary/pipeline rather than a FIRST-CHAR-specific template shortcut.
#[cfg(target_arch = "x86_64")]
#[test]
fn body_inlining_eliminates_redundant_layout_guards() {
    let prog = "\
        (defpackage :uiop/utility (:use :cl) (:export :first-char)) \
        (in-package :uiop/utility) \
        (defun first-char (s) \
          (and (stringp s) (plusp (length s)) (char s 0))) \
        (in-package :cl-user) \
        (defun guarded-leaf (s) (uiop/utility:first-char s)) \
        (defun guarded-pair (s) (guarded-leaf s) (guarded-leaf s)) \
        (dotimes (k 60) (guarded-pair \"warm\")) \
        (format t \"~a ~a~%\" (guarded-pair \"abc\") (guarded-pair \"\")) \
        (disassemble (quote guarded-pair))";

    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "general post-inline guard elimination failed: {out}");
    assert_eq!(out.lines().next().unwrap_or(""), "a NIL");
    assert!(out.contains("; T2 — profile-guided native"), "{out}");
    assert_eq!(
        out.matches("cmp byte [rdx+7],5").count(),
        1,
        "two inlined guarded bodies must share one base-string layout proof:\n{out}"
    );
    assert_eq!(
        out.matches("cmp byte [rdx+7],6").count(),
        1,
        "two inlined guarded bodies must share one character-string layout proof:\n{out}"
    );
}

/// A saved callee deliberately larger than the ordinary 30-bytecode threshold
/// is nevertheless cloned because its call site executes on every interpreted
/// caller invocation.  The caller itself contains no string operation, so the
/// layout fast path in its T2 code is direct evidence that production profile
/// counters granted the hot-site allowance.
#[cfg(target_arch = "x86_64")]
#[test]
fn hot_call_site_inlines_body_above_small_threshold() {
    let prog = "\
        (defpackage :uiop/utility (:use :cl) (:export :first-char)) \
        (in-package :uiop/utility) \
        (defun first-char (s) \
          (and (stringp s) (plusp (length s)) (char s 0))) \
        (in-package :cl-user) \
        (defun large-leaf (s) \
          s s s s s s s s s s s s s s s s s s s s \
          (uiop/utility:first-char s)) \
        (defun hot-caller (s) (large-leaf s)) \
        (dotimes (k 60) (hot-caller \"warm\")) \
        (format t \"~a~%\" (hot-caller \"EGCL\")) \
        (disassemble (quote hot-caller))";

    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "profile-guided large-body inline failed: {out}");
    assert_eq!(out.lines().next().unwrap_or(""), "E");
    assert!(out.contains("; T2 — profile-guided native"), "{out}");
    assert!(
        out.contains("cmp byte [rdx+7],5") && out.contains("cmp byte [rdx+7],6"),
        "HOT-CALLER has no string operation of its own; its native layout checks prove LARGE-LEAF was inlined:\n{out}"
    );
}

/// A checked parameter declaration becomes an entry proof for T2. The call is
/// still safe at `(safety 1)`: a wrong argument signals TYPE-ERROR, while the
/// native multiplication consumes the proof and therefore carries no redundant
/// fixnum tag guard. Overflow remains guarded because FIXNUM input alone does
/// not prove that the mathematical result fits a fixnum.
#[cfg(target_arch = "x86_64")]
#[test]
fn declared_fixnum_parameter_removes_arithmetic_type_guard() {
    let program = "\
        (defun declared-mul5 (x) \
          (declare (type fixnum x) (optimize (speed 3) (safety 1))) \
          (* x 5)) \
        (dotimes (k 60) (declared-mul5 k)) \
        (format t \"~a~%\" (declared-mul5 9)) \
        (disassemble (quote declared-mul5)) \
        (format t \"~a~%\" \
          (handler-case \
            (progn (declared-mul5 1.5) (quote missed-type-error)) \
            (type-error () (quote type-error)))) \
        (format t \"~a~%\" (declared-mul5 1152921504606846975))";

    let (out, ok) = run(program, &[("EGCL_T2", "1")]);
    assert!(ok, "declared T2 function failed: {out}");
    assert_eq!(out.lines().next().unwrap_or(""), "45");
    assert!(out.contains("; T2 — profile-guided native"), "{out}");
    assert!(
        out.contains("checked parameter declarations: X: FIXNUM"),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|line| line.contains("imul ") && line.ends_with(",5")),
        "expected direct declared fixnum multiply in allocated registers:\n{out}"
    );
    assert!(
        !out.contains("test cl,7"),
        "declaration proof should remove the operation guard:\n{out}"
    );
    assert!(
        out.contains("jo near"),
        "fixnum overflow must remain guarded:\n{out}"
    );
    assert!(
        out.lines().any(|line| line == "TYPE-ERROR"),
        "wrong declared argument must be caught as TYPE-ERROR:\n{out}"
    );
    assert!(
        out.lines().any(|line| line == "5764607523034234875"),
        "overflow must deopt to exact bignum multiplication:\n{out}"
    );
}

#[test]
fn declared_parameter_validation_is_shared_by_t0_and_t1() {
    let program = "\
        (defun declared-entry (x) (declare (type fixnum x)) x) \
        (declared-entry 1) (declared-entry 2) (declared-entry 3) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote declared-entry)) \
          (handler-case \
            (progn (declared-entry 1.5) (quote missed-type-error)) \
            (type-error () (quote type-error))))";

    let (t0, t0_ok) = run(program, &[("EGCL_T1_THRESHOLD", "1000")]);
    assert!(t0_ok, "T0 declaration validation failed: {t0}");
    assert_eq!(t0.lines().next().unwrap_or(""), "0 TYPE-ERROR");

    let (t1, t1_ok) = run(program, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(t1_ok, "T1 declaration validation failed: {t1}");
    assert_eq!(t1.lines().next().unwrap_or(""), "1 TYPE-ERROR");
}

/// The live T2 path uses regalloc2 rather than declining when more values are
/// simultaneously live than fit in its GPR set. This loop keeps seven arguments,
/// its induction variable, and its accumulator live; at least one range must be
/// split to a native spill slot and the result must remain exact.
#[cfg(target_arch = "x86_64")]
#[test]
fn t2_regalloc_spills_high_pressure_loop() {
    let program = "\
        (defun spill-pressure (n a b c) \
          (declare (type fixnum n a b c)) \
          (let ((d (+ a 1)) (e (+ b 2)) (f (+ c 3)) (g (+ a b)) (sum 0)) \
            (dotimes (i n sum) \
              (setq sum (+ sum i)) (setq sum (+ sum a)) \
              (setq sum (+ sum b)) (setq sum (+ sum c)) \
              (setq sum (+ sum d)) (setq sum (+ sum e)) \
              (setq sum (+ sum f)) (setq sum (+ sum g))))) \
        (dotimes (warm 80) (spill-pressure 10 1 2 3)) \
        (format t \"~a~%\" (spill-pressure 10 1 2 3)) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote spill-pressure))) \
        (format t \"~a~%\" (spill-pressure 1 1152921504606846975 0 0)) \
        (disassemble (quote spill-pressure))";

    let (out, ok) = run(program, &[("EGCL_T2", "1")]);
    assert!(ok, "spill-pressure program failed: {out}");
    assert_eq!(out.lines().next().unwrap_or(""), "255", "{out}");
    assert!(
        out.lines().any(|line| line == "2"),
        "function did not reach T2: {out}"
    );
    assert!(
        out.lines().any(|line| line == "3458764513820540931"),
        "overflow deopt did not reconstruct spilled state: {out}"
    );
    assert!(out.contains("; T2 — profile-guided native"), "{out}");
    assert!(
        out.lines().any(|line| line.contains("[rsp")),
        "expected native spill/reload addressing in T2 output:\n{out}"
    );
}

/// Deopt frame states name only the locals the interpreter can still read
/// (bliss-dfilp, bliss-enisp). A `LET*` chain inside a loop therefore keeps
/// nothing but the loop-carried values alive, and a guard that fails
/// mid-chain reconstructs a frame whose dead slots are UNBOUND. The resumed
/// interpreter must still finish the call with the exact (bignum) result.
#[test]
fn t2_dead_let_locals_are_not_reconstructed_on_deopt() {
    let program = "\
        (defun chain5 (a n) \
          (let ((x a)) \
            (dotimes (i n x) \
              (let* ((y (- (+ x 7919) 7918)) \
                     (y (- (+ y 7932) 7931)) \
                     (y (- (+ y 7945) 7944)) \
                     (y (- (+ y 7958) 7957)) \
                     (y (- (+ y 7971) 7970))) \
                (setq x y))))) \
        (dotimes (warm 80) (chain5 1 10)) \
        (dotimes (warm 2000) (egcl-ext:function-tier (quote chain5))) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote chain5))) \
        (format t \"~a~%\" (chain5 1 10)) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"~a~%\" (chain5 1152921504606839975 3)) \
          (format t \"~a~%\" (if (> (egcl-ext:deopt-count) before) :deopted :no-deopt)))";
    let out = run_output(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_T2_THREADS", "1"),
            ("EGCL_T2_LOG", "-"),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "chain5 run failed:\n{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("2"),
        "chain5 did not reach T2:\n{stdout}\n{stderr}"
    );
    assert_eq!(lines.get(1).copied(), Some("51"), "{stdout}");
    // 2^60 - 7001 + 3 iterations x 5 net increments: the first add overflows
    // the fixnum range, so T2 deopts inside the chain and T0 finishes.
    assert_eq!(
        lines.get(2).copied(),
        Some("1152921504606839990"),
        "overflow deopt with dead LET* locals produced a wrong result:\n{stdout}"
    );
    assert_eq!(
        lines.get(3).copied(),
        Some("DEOPTED"),
        "the bignum call was expected to deoptimise:\n{stdout}\n{stderr}"
    );
}

/// regalloc2 may coalesce a binary result with either input. The framed x86
/// templates must preserve the RHS when it is also the destination, including
/// non-commutative subtraction and multiplication's destructive untag step.
#[cfg(target_arch = "x86_64")]
#[test]
fn t2_two_address_ops_preserve_a_coalesced_rhs() {
    let program = "\
        (defun rhs-add (x y) \
          (declare (type fixnum x y)) \
          (+ (+ (* x 5) (* y 9)) 7)) \
        (defun rhs-sub (x y) \
          (declare (type fixnum x y)) \
          (- (* x 5) (* y 9))) \
        (defun rhs-mul (x y) \
          (declare (type fixnum x y)) \
          (* (+ x 1) (+ y 2))) \
        (defun rhs-xor (x y) \
          (declare (type fixnum x y)) \
          (logxor (+ x 1) (+ y 2))) \
        (dotimes (i 80) \
          (rhs-add i 4) (rhs-sub i 4) (rhs-mul i 4) (rhs-xor i 4)) \
        (format t \"~a ~a ~a ~a~%\" \
          (rhs-add 12 4) (rhs-sub 12 4) (rhs-mul 12 4) (rhs-xor 12 4)) \
        (format t \"~a ~a ~a ~a~%\" \
          (egcl-ext:function-tier (quote rhs-add)) \
          (egcl-ext:function-tier (quote rhs-sub)) \
          (egcl-ext:function-tier (quote rhs-mul)) \
          (egcl-ext:function-tier (quote rhs-xor)))";

    let (out, ok) = run(program, &[("EGCL_T2", "1")]);
    assert!(ok, "two-address alias program failed: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.first().copied(), Some("103 24 78 11"), "{out}");
    assert_eq!(lines.get(1).copied(), Some("2 2 2 2"), "{out}");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn declared_single_float_parameter_removes_arithmetic_type_guard() {
    let program = "\
        (defun declared-float5 (x) \
          (declare (single-float x) (optimize (speed 3) (safety 1))) \
          (* x 5)) \
        (dotimes (k 60) (declared-float5 1.5)) \
        (format t \"~a~%\" (declared-float5 2.5)) \
        (disassemble (quote declared-float5)) \
        (format t \"~a~%\" \
          (handler-case \
            (progn (declared-float5 2) (quote missed-type-error)) \
            (type-error () (quote type-error))))";

    let (out, ok) = run(program, &[("EGCL_T2", "1")]);
    assert!(ok, "declared single-float T2 function failed: {out}");
    assert_eq!(out.lines().next().unwrap_or(""), "12.5");
    assert!(
        out.contains("checked parameter declarations: X: SINGLE-FLOAT"),
        "{out}"
    );
    assert!(
        out.contains("mulss xmm0,xmm1"),
        "expected direct declared float multiply:\n{out}"
    );
    assert!(
        !out.contains("cmp dl,4"),
        "declaration proof should remove the float tag guard:\n{out}"
    );
    assert!(out.lines().any(|line| line == "TYPE-ERROR"), "{out}");
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

    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "T2 run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(
        line, "10 15 7.5 7.5",
        "loop results incl. the mid-loop float deopt (a b c *s*): {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let twl = tw.lines().next().unwrap_or("").to_string();
    assert_eq!(
        twl, line,
        "T2 loop result must match the tree-walker: {line:?} vs {twl:?}"
    );
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

    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "T2 run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(
        line, "35 27.5 27.5",
        "all-fixnum sum, then the float-deopt sum and *s* (ok dp *s*): {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let twl = tw.lines().next().unwrap_or("").to_string();
    assert_eq!(
        twl, line,
        "T2 result must match the tree-walker: {line:?} vs {twl:?}"
    );
}

/// A hash-table-using function compiles to bytecode (does NOT bail to the
/// tree-walker) and promotes to T1 (bliss-x5y.2). make-hash-table (with a :test
/// keyword), gethash, (setf (gethash ...) ...), remhash, hash-table-count, and
/// MAPHASH iteration all lower to bytecode — the setf store goes to
/// the internal EGCL::PUT-GETHASH primitive. A bailed function stays at tier 0,
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
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote htf)) (htf))";

    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "hash function run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert!(
        line.starts_with("1 "),
        "hash function must compile and reach T1 (tier 1, not a bailed 0): {line:?}"
    );
    assert!(line.contains("((2 15 ABSENT) 45)"), "hash result: {line:?}");

    // Identical value under the tree-walker.
    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker run failed: {tw}");
    let tw_result = tw
        .lines()
        .next()
        .unwrap_or("")
        .split_once(' ')
        .map(|(_, r)| r.to_string());
    let t1_result = line.split_once(' ').map(|(_, r)| r.to_string());
    assert_eq!(
        tw_result, t1_result,
        "T1 hash result must match the tree-walker"
    );
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
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote hash-loop)) (hash-loop))";

    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "hash LOOP run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert_eq!(
        line, "1 (30 30)",
        "hash LOOP must reach T1 with the right sums"
    );

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker hash LOOP failed: {tw}");
    assert_eq!(tw.lines().next(), Some("0 (30 30)"));
}

/// T1 passes call arguments as a slice in the caller's EgclStack frame, so a
/// call-heavy function is not rejected merely because one callee has more than
/// three arguments.
#[test]
fn t1_c2i_supports_calls_with_many_arguments() {
    let prog = "\
        (defun add8 (a b c d e f g h) (+ a b c d e f g h)) \
        (defun call-add8 () (add8 1 2 3 4 5 6 7 8)) \
        (call-add8) (call-add8) (call-add8) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote call-add8)) (call-add8))";
    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "wide c2i call failed: {out}");
    assert_eq!(out.lines().next(), Some("1 36"));

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker wide call failed: {tw}");
    assert_eq!(tw.lines().next(), Some("0 36"));
}

/// Captured parameters/locals, host-evaluated closure construction, and
/// multiple-value binding share the activation's heap environment in T1.
#[test]
fn t1_executes_captured_environment_bytecodes() {
    let prog = "\
        (defun captured-native (&optional (x 4)) \
          (multiple-value-bind (q r) (floor x 3) \
            (let ((z (+ q r))) (funcall (lambda () (+ x z)))))) \
        (captured-native 10) (captured-native 10) (captured-native 10) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote captured-native)) \
          (captured-native 10))";
    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "captured-environment T1 run failed: {out}");
    assert_eq!(out.lines().next(), Some("1 14"));

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(tw_ok, "tree-walker captured-environment run failed: {tw}");
    assert_eq!(tw.lines().next(), Some("0 14"));
}

/// Calling a retained heap function object through FUNCALL must drive the same
/// tier transition as calling its global symbol directly.
#[test]
fn function_object_calls_participate_in_tiering() {
    let prog = "\
        (defun object-hot (x) (+ x 1)) \
        (let ((f (fdefinition (quote object-hot)))) \
          (funcall f 1) (funcall f 2) (funcall f 3)) \
        (format t \"~a ~a~%\" \
          (egcl-ext:function-tier (quote object-hot)) (object-hot 9))";
    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "function-object tiering failed: {out}");
    assert_eq!(out.lines().next(), Some("1 10"));
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
          (egcl-ext:function-tier (quote tree-called-leaf)) \
          (tree-called-leaf 41))";

    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "tree-called leaf run failed: {out}");
    assert_eq!(out.lines().next(), Some("1 42"));

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
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
          (egcl-ext:function-tier (quote f)) \
          (f 10 20) (g 5 :c 99) (h 1 2 3))";
    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "variadic run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert!(
        line.starts_with("1 "),
        "&optional fn must reach T1: {line:?}"
    );
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
    let (t1, _) = run(prog2, &[("EGCL_T1_THRESHOLD", "2")]);
    let (tw, _) = run(prog2, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(
        t1.lines().next(),
        tw.lines().next(),
        "supplied-p + arity error must match the tree-walker"
    );
}

/// A variadic (`&rest`) function must stay correct under `EGCL_T2=1`. T2's entry
/// sequence binds only the fixed positional parameters (locals `0..arity`) and
/// leaves the rest NIL, so a variadic function is DECLINED from T2 and runs at
/// the shared `bind_variadic` path (run_native), which collects `&rest` into a
/// frame slot BEFORE the compiled body runs. This guards the UIOP STRCAT
/// regression: a T2 `&rest` used to see an EMPTY list, so
/// `(make-string (loop :for s :in strings :sum …))` got NIL — "NIL is not of type
/// non-negative string size" — and `asdf` failed to load under T2. A NON-capturing
/// variadic function now DOES reach T2 (bliss-32l), and its `&rest` result must
/// still equal the tree-walker's.
#[test]
fn variadic_rest_stays_correct_under_t2() {
    let prog = "\
        (defun rs (strings) \
          (make-string (loop :for s :in strings :sum (if (characterp s) 1 (length s))))) \
        (defun sc (&rest strings) (rs strings)) \
        (dotimes (k 80) (sc \"ab\" \"cd\" \"ef\")) \
        (format t \"~a ~a~%\" (egcl-ext:function-tier (quote sc)) (length (sc \"ab\" \"cd\" \"ef\")))";
    let (out, ok) = run(prog, &[("EGCL_T2", "1")]);
    assert!(ok, "variadic under T2 failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    assert!(line.ends_with(" 6"), "hot &rest result must be 6: {line:?}");
    assert!(
        line.starts_with("2 "),
        "non-capturing variadic fn should now reach T2 (bliss-32l): {line:?}"
    );

    let (tw, tw_ok) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
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
          (egcl-ext:function-tier (quote compute)) (compute 10) (skipped 7))";
    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
    assert!(ok, "eval-when run failed: {out}");
    let line = out.lines().next().unwrap_or("").to_string();
    // compute reaches T1; (* i i) for 0..9 sums to 285; the :compile-toplevel-only
    // eval-when does NOT fire at execute time, so skipped returns its arg (7).
    assert_eq!(
        line, "1 285 7",
        "eval-when compile result (tier, compute, skipped): {line:?}"
    );

    let (tw, _) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    let tw_rest = tw
        .lines()
        .next()
        .unwrap_or("")
        .split_once(' ')
        .map(|(_, r)| r.to_string());
    let t1_rest = line.split_once(' ').map(|(_, r)| r.to_string());
    assert_eq!(
        t1_rest, tw_rest,
        "eval-when result must match the tree-walker"
    );
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
          (egcl-ext:function-tier (quote pa)) (egcl-ext:function-tier (quote qa)) \
          (pa 3) (pb 3) (qa 10) (qb 10) (pb 0))";
    let (out, ok) = run(prog, &[("EGCL_T1_THRESHOLD", "2")]);
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
    let (tw, _) = run(prog, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(
        values(&line),
        values(tw.lines().next().unwrap_or("")),
        "nested-def results must match the tree-walker"
    );
}

/// A fixnum comparison whose result is used as a VALUE — not fused into a
/// branch — must still reach T2 (bliss-n6d1).
///
/// The emitter originally lowered a comparison only when a `Brif` consumed it
/// directly, so `(not (< x y))` or simply returning `(< x y)` failed the whole
/// function out of T2 and left it at T1. That silently knocked TAK — a
/// canonical Gabriel benchmark — off T2 entirely, costing ~5x on that workload
/// with no error and no test failure, which is exactly why this asserts the
/// TIER and not just the result.
#[test]
fn comparison_used_as_a_value_still_reaches_t2() {
    // Warming queues compilation; it does not guarantee publication before the
    // next form. Poll each function with a bound, then require the same T2 tier.
    let program = "\
        (defun cmp-fused (x y) (if (< x y) 1 0)) \
        (defun cmp-negated (x y) (if (not (< x y)) 1 0)) \
        (defun cmp-value (x y) (< x y)) \
        (defun cmp-const (x) (< x 5)) \
        (dotimes (k 40) (cmp-fused k 7) (cmp-negated k 7) (cmp-value k 7) (cmp-const k)) \
        (dolist (fn '(cmp-fused cmp-negated cmp-value cmp-const)) \
          (loop repeat 30 until (= 2 (egcl-ext:function-tier fn)) do (sleep 0.01))) \
        (format t \"~a ~a ~a ~a | ~a ~a ~a ~a~%\" \
          (egcl-ext:function-tier (quote cmp-fused)) \
          (egcl-ext:function-tier (quote cmp-negated)) \
          (egcl-ext:function-tier (quote cmp-value)) \
          (egcl-ext:function-tier (quote cmp-const)) \
          (cmp-fused 1 2) (cmp-negated 1 2) (cmp-value 1 2) (cmp-const 1))";
    let envs = [
        ("EGCL_T0_T1_THRESHOLD", "2"),
        ("EGCL_T1_T2_INVOKE_THRESHOLD", "2"),
        ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "2"),
    ];
    let (out, ok) = run(program, &envs);
    assert!(ok, "run failed: {out}");
    // The last line is the interpreter's value echo; take the format line.
    let line = out
        .lines()
        .find(|l| l.contains('|'))
        .unwrap_or_default()
        .trim()
        .to_string();
    let (tiers, results) = line.split_once('|').unwrap_or(("", ""));
    for (name, tier) in ["cmp-fused", "cmp-negated", "cmp-value", "cmp-const"]
        .iter()
        .zip(tiers.split_whitespace())
    {
        assert_eq!(
            tier, "2",
            "{name} should reach T2, got tier {tier} (full output: {out:?})"
        );
    }
    // The values must be what interpretation gives, not merely self-consistent.
    assert_eq!(
        results.split_whitespace().collect::<Vec<_>>(),
        ["1", "0", "T", "T"],
        "T2 results differ from the expected interpreted values ({out:?})"
    );
}

// ── T2 coverage ratchet (bliss-jpd0.1) ───────────────────────────
//
// A tier loss is SILENT: the function returns the right answer and merely runs
// at T1 forever. Nothing fails, so gaps are found by accident — two were, on
// 2026-09-03 (bliss-n6d1 comparison-as-value, bliss-8tlo ReturnFrom), each
// having quietly cost real time. This asserts the EXACT set of shapes that do
// not reach T2, so a regression fails here, and so does an unrecorded fix
// (which should tighten the list).

/// `(name, definition, one call)` for each shape. Names are the Lisp function
/// names, upcased by the reader, which is how `function-tier` sees them.
const T2_SHAPES: &[(&str, &str, &str)] = &[
    (
        "COV-ARITH",
        "(defun cov-arith (x y) (+ (* x 2) (- y 3)))",
        "(cov-arith k 7)",
    ),
    (
        "COV-CMP-FUSED",
        "(defun cov-cmp-fused (x y) (if (< x y) 1 0))",
        "(cov-cmp-fused k 7)",
    ),
    (
        "COV-CMP-VALUE",
        "(defun cov-cmp-value (x y) (< x y))",
        "(cov-cmp-value k 7)",
    ),
    (
        "COV-CMP-NOT",
        "(defun cov-cmp-not (x y) (if (not (< x y)) 1 0))",
        "(cov-cmp-not k 7)",
    ),
    (
        "COV-CMP-AND",
        "(defun cov-cmp-and (x y) (and (< x y) (> y 0)))",
        "(cov-cmp-and k 7)",
    ),
    (
        "COV-CMP-OR",
        "(defun cov-cmp-or (x y) (or (< x y) (= x 0)))",
        "(cov-cmp-or k 7)",
    ),
    (
        "COV-CARCDR",
        "(defun cov-carcdr (l) (car (cdr l)))",
        "(cov-carcdr (list 1 2 3))",
    ),
    (
        "COV-CONS",
        "(defun cov-cons (x) (cons x x))",
        "(cov-cons k)",
    ),
    (
        "COV-NULL",
        "(defun cov-null (x) (if (null x) 1 0))",
        "(cov-null nil)",
    ),
    ("COV-EQ", "(defun cov-eq (x y) (eq x y))", "(cov-eq k k)"),
    (
        "COV-NESTED-IF",
        "(defun cov-nested-if (x) (if (< x 0) -1 (if (> x 10) 1 0)))",
        "(cov-nested-if k)",
    ),
    (
        "COV-LOOP",
        "(defun cov-loop (n) (let ((s 0)) (dotimes (i n) (setq s (+ s i))) s))",
        "(cov-loop 5)",
    ),
    (
        "COV-STRLEN",
        "(defun cov-strlen (s) (length s))",
        "(cov-strlen \"abc\")",
    ),
    (
        "COV-AREF",
        "(defun cov-aref (v i) (aref v i))",
        "(cov-aref (vector 1 2 3) 1)",
    ),
    (
        "COV-ZEROP",
        "(defun cov-zerop (x) (if (zerop x) 1 0))",
        "(cov-zerop k)",
    ),
    ("COV-MOD", "(defun cov-mod (x) (mod x 7))", "(cov-mod k)"),
    (
        "COV-RECUR",
        "(defun cov-recur (n) (if (< n 2) n (+ (cov-recur (- n 1)) (cov-recur (- n 2)))))",
        "(cov-recur 6)",
    ),
    (
        "COV-MULTI",
        "(defun cov-multi (x) (values x (+ x 1)))",
        "(cov-multi k)",
    ),
    // Known gap: an explicit RETURN-FROM is not modelled by the T2 builder
    // (bliss-8tlo). Listed so the ratchet records it rather than ignoring it.
    (
        "COV-RETURN-FROM",
        "(defun cov-return-from (x) (block b (when (< x 0) (return-from b :neg)) :pos))",
        "(cov-return-from k)",
    ),
];

/// Shapes known NOT to reach T2 today, each with the bead that tracks it.
/// Tightening this list is the point; every entry is a bug, not a policy.
const T2_KNOWN_GAPS: &[&str] = &[
    // Empty. COV-RETURN-FROM was the last entry; the builder learned
    // Instr::ReturnFrom in bliss-8tlo. Keep it that way.
];

#[test]
fn common_shapes_reach_t2() {
    let mut program = String::new();
    for (_, defn, _) in T2_SHAPES {
        program.push_str(defn);
        program.push(' ');
    }
    program.push_str("(dotimes (k 40) ");
    for (_, _, call) in T2_SHAPES {
        program.push_str(call);
        program.push(' ');
    }
    program.push_str(") ");
    // FUNCTION-TIER is also the owner-thread publication safepoint for
    // background T2 results. Drain every queued shape once before recording
    // the ratchet: otherwise the first line can observe T1 while its result is
    // still behind another shape in the shared completion channel (bliss-wiwb).
    program.push_str("(progn ");
    for (name, _, _) in T2_SHAPES {
        program.push_str(&format!("(egcl-ext:function-tier (quote {name})) "));
    }
    program.push_str(") ");
    // One line per shape: "NAME tier".
    for (name, _, _) in T2_SHAPES {
        program.push_str(&format!(
            "(format t \"{name} ~a~%\" (egcl-ext:function-tier (quote {name}))) "
        ));
    }
    let (out, ok) = run(
        &program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "2"),
            ("EGCL_T1_T2_BACKEDGE_THRESHOLD", "2"),
        ],
    );
    assert!(ok, "run failed: {out}");

    let mut not_t2: Vec<&str> = Vec::new();
    for (name, _, _) in T2_SHAPES {
        let line = out
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{name} ")))
            .unwrap_or_else(|| panic!("no tier line for {name} in {out:?}"));
        if line.split_whitespace().nth(1) != Some("2") {
            not_t2.push(name);
        }
    }

    let expected: Vec<&str> = T2_KNOWN_GAPS.to_vec();
    assert_eq!(
        not_t2, expected,
        "T2 coverage changed.\n  not reaching T2: {not_t2:?}\n  expected gaps:   {expected:?}\n\
         If a shape REGRESSED, that is a silent perf loss — fix it.\n\
         If a shape was FIXED, tighten T2_KNOWN_GAPS and close its bead.\n\
         Full output: {out:?}"
    );
}

/// Regression for bliss-r8pt: a T2 deopt reconstructed a corrupt frame because
/// the safepoint GC-root set omitted a value that is live ACROSS a call but not
/// USED at it (a "live-through" value). `(reduce #'logand list)` keeps the loop
/// list in a callee-saved register across the reducer call; with the roots bug a
/// moving GC during that call left the register stale, so the next iteration's
/// cons-guard saw a fixnum and deopted to `(car <fixnum>)` -> "not of type list".
///
/// #[ignore]d: reproducing needs REDUCE to reach T2 AND a (background-compiler-
/// triggered) minor GC to land during a T2 reduce call, which only happens after
/// a few thousand default-tier warm-up calls (~1 min in a debug build) and is
/// timing-dependent — unsuitable for the routine suite, but a faithful on-demand
/// reproducer. The fix (ProgPoint-correct safepoint liveness) makes the result
/// correct regardless of GC timing, so this never spuriously fails once fixed.
#[test]
#[ignore = "slow (~1min) timing-dependent T2+GC reproducer; run on demand"]
fn r8pt_reduce_logand_live_through_list_survives_gc_across_reducer_call() {
    // Default tiering (no threshold envs) — the trigger depends on the real
    // promotion/background-GC cadence.
    let program = "\
        (defun rr (l) (reduce (function logand) l)) \
        (dotimes (k 6000) (rr (list 255 254 253 15))) \
        (format t \"result=~a\" (rr (list 255 254 253 15)))";
    let (out, ok) = run(program, &[]);
    // The bug deopts to `(car <fixnum>)`, raising a type-error and exiting
    // non-zero — so success + the right fold value is the signal.
    assert!(
        ok && !out.contains("not of type"),
        "reduce repro deopted to a corrupt frame: {out:?}"
    );
    assert!(
        out.contains("result=12"),
        "logand-reduce must fold to 12; got: {out:?}"
    );
}

/// bliss-lwws: `emit_call` marshalled call arguments into the fixed argument
/// registers with sequential moves, so a later argument whose home register was
/// an earlier argument's DESTINATION was read after being clobbered — REDUCE's
/// `:from-end` funcall (`mov rcx,r9; mov r8,rcx`) called `fn(x, x)` instead of
/// `fn(x, acc)`, silently corrupting every right-fold once REDUCE reached T2.
/// In-context this broke ASDF's `nest` macro (a `:from-end` reduce running at
/// macroexpansion time) and surfaced as the bliss-kfhp "compute-action-stamp
/// ... NIL" failure on every fresh `(asdf:load-system :babel)`. Warm REDUCE to
/// T2 and check the right-fold results are the interpreter's.
#[test]
fn t2_reduce_from_end_folds_correctly() {
    let program = "\
        (dotimes (i 200) (reduce (quote cons) (list 1 2 3 4) :from-end t :initial-value nil)) \
        (dotimes (i 2000) \
          (when (eql (egcl-ext:function-tier (quote reduce)) 2) (return)) \
          (reduce (quote cons) (list 1 2) :from-end t :initial-value nil)) \
        (format t \"~a ~a ~a~%\" \
          (egcl-ext:function-tier (quote reduce)) \
          (reduce (quote cons) (list 1 2 3 4) :from-end t :initial-value nil) \
          (reduce (quote +) (list 1 2 3) :from-end t :initial-value 0))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "5"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "20"),
        ],
    );
    assert!(ok, "run failed: {out}");
    assert_eq!(
        out.lines().next().unwrap_or_default().trim(),
        "2 (1 2 3 4) 6",
        "T2 REDUCE right-fold diverged (tier fold-cons fold-plus): {out}"
    );
}

/// A T1 decline is a property of the compiled bytecode — an unsupported opcode,
/// a non-local GO/RETURN-FROM target, an over-wide arity — so it cannot change
/// until the function is redefined. T1 had no memo (unlike T2), so every
/// dispatch past the threshold re-ran the whole T1 front end just to fail at the
/// same instruction: CTAK re-declined 127,213 times in a single benchmark run
/// (bliss-yy9m). The decline must now be recorded once.
#[cfg(target_arch = "x86_64")]
#[test]
fn structural_t1_decline_is_attempted_only_once() {
    // THROW is not supported by the native emitter, so this declines at T1 and
    // stays in the interpreter — where it must still compute the right answer.
    let program = "\
        (defun thrower (n) (catch 'tag (if (> n 3) (throw 'tag (* n 2)) n))) \
        (dotimes (i 300) (thrower i)) \
        (format t \"~a ~a~%\" (thrower 10) (egcl-ext:function-tier (quote thrower)))";
    let out = run_output(
        program,
        &[("EGCL_T0_T1_THRESHOLD", "2"), ("EGCL_LOG", "compile=trace")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "decline-memo run failed:\n{stderr}");
    // Declining must not change the answer, and the function stays at T0.
    assert_eq!(
        stdout.lines().next(),
        Some("20 0"),
        "wrong result or tier for a T1-declined function: {stdout}"
    );
    let declines = stderr
        .lines()
        .filter(|l| l.contains("THROWER: declined"))
        .count();
    assert_eq!(
        declines, 1,
        "a structural T1 decline must be recorded once, not re-attempted on \
         every dispatch (saw {declines}):\n{stderr}"
    );
}

/// ...but the memo is keyed to the bytecode, so REDEFINING the function must
/// clear it — otherwise a function that is fixed to be compilable would stay
/// pinned at T0 for the life of the process.
#[cfg(target_arch = "x86_64")]
#[test]
fn redefining_a_declined_function_allows_promotion_again() {
    let program = "\
        (defun redef-target (n) (catch 'tag (throw 'tag n))) \
        (dotimes (i 50) (redef-target i)) \
        (defun redef-target (n) (+ n 1)) \
        (dotimes (i 300) (redef-target i)) \
        (format t \"~a ~a~%\" (redef-target 5) \
          (if (> (egcl-ext:function-tier (quote redef-target)) 0) :promoted :t0))";
    let out = run_output(
        program,
        &[("EGCL_T0_T1_THRESHOLD", "2"), ("EGCL_LOG", "compile=trace")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "redefinition run failed:\n{stderr}");
    assert_eq!(
        stdout.lines().next(),
        Some("6 PROMOTED"),
        "a redefined, now-compilable function stayed stuck at T0: {stdout}\n{stderr}"
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn six_argument_forwarder_reaches_t2_without_allocator_panic() {
    let (out, ok) = run(
        "(defun consume-six (a b c d e f) (list a b c d e f))
         (defun forward-six (a b c d e f) (consume-six a b c d e f))
         (dotimes (i 20)
           (assert (equal (forward-six 1 2 3 4 5 6) '(1 2 3 4 5 6))))
         (format t \"forward-tier=~A result=~S~%\"
           (egcl-ext:function-tier 'forward-six)
           (forward-six 6 5 4 3 2 1))",
        &[
            ("EGCL_T2", "1"),
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(ok, "{out}");
    assert!(!out.contains("panicked"), "{out}");
    assert!(out.contains("forward-tier=2 result=(6 5 4 3 2 1)"), "{out}");
}

/// A `SETQ` in a call-free loop must not cost a runtime call per assignment
/// (bliss-y7wdk). The bytecode lowering emits `ClearMv` after every `SETQ`;
/// T2 keeps only the resets that can be observed, which here is the one
/// before the function returns. The remaining resets must still truncate a
/// stale multiple-value state left by the caller.
#[test]
fn t2_elides_unobservable_multiple_value_resets() {
    let program = "\
        (defun mv-loop (x n) (dotimes (i n x) (setq x (+ x 1)))) \
        (dotimes (warm 80) (mv-loop 1 10)) \
        (dotimes (warm 2000) (egcl-ext:function-tier (quote mv-loop))) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote mv-loop))) \
        (format t \"~a~%\" (multiple-value-list (mv-loop 1 3))) \
        (format t \"~a~%\" (multiple-value-list (progn (values 1 2 3) (mv-loop 1 3))))";
    let out = run_output(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_T2_THREADS", "1"),
            ("EGCL_T2_LOG", "-"),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "mv-loop run failed:\n{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("2"),
        "mv-loop did not reach T2:\n{stdout}\n{stderr}"
    );
    assert_eq!(lines.get(1).copied(), Some("(4)"), "{stdout}");
    assert_eq!(
        lines.get(2).copied(),
        Some("(4)"),
        "a stale multiple-value state leaked through the T2 return:\n{stdout}"
    );
    let installed = stderr
        .lines()
        .find(|l| l.contains("MV-LOOP: T2 INSTALLED"))
        .unwrap_or_else(|| panic!("no T2 install record for MV-LOOP:\n{stderr}"));
    // Both loop-body resets (after `(setq x ...)` and after the DOTIMES step)
    // are dead: every path from them reaches another reset before a reader.
    // The only safepoint-carrying instructions left are outside the loop.
    assert!(
        installed.contains("safepoints=1,"),
        "expected exactly the pre-return reset to survive, got: {installed}"
    );
}

/// A loop-carried fixnum is guarded once on loop entry (and once at the OSR
/// entry), never per iteration (bliss-5yz5h). The hot path of a counted loop
/// over a `LET`-bound accumulator must contain no tag checks beyond the two
/// loop-entry ones; a per-iteration check on `i`, `n` or `x` would add more.
#[cfg(target_arch = "x86_64")]
#[test]
fn t2_loop_carried_fixnums_are_checked_on_entry_not_per_iteration() {
    let program = "\
        (defun acc-loop (a n) \
          (let ((x a)) \
            (dotimes (i n x) \
              (let* ((y (- (+ x 7919) 7918)) \
                     (y (- (+ y 7932) 7931))) \
                (setq x y))))) \
        (dotimes (warm 80) (acc-loop 1 10)) \
        (dotimes (warm 2000) (egcl-ext:function-tier (quote acc-loop))) \
        (format t \"~a~%\" (egcl-ext:function-tier (quote acc-loop))) \
        (format t \"~a~%\" (acc-loop 1 10)) \
        (format t \"~a~%\" (acc-loop 1.5 0)) \
        (format t \"~a~%\" (acc-loop 1.5 2)) \
        (disassemble (quote acc-loop))";
    let (out, ok) = run(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
            ("EGCL_T2_THREADS", "1"),
        ],
    );
    assert!(ok, "acc-loop program failed: {out}");
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("2"), "acc-loop did not reach T2: {out}");
    assert_eq!(lines.next(), Some("21"), "{out}");
    // A non-fixnum entry value fails the loop-entry guard and deoptimises:
    // zero iterations return it unchanged, two iterations use float arithmetic.
    assert_eq!(lines.next(), Some("1.5"), "{out}");
    assert_eq!(lines.next(), Some("5.5"), "{out}");
    assert!(out.contains("; T2 — profile-guided native"), "{out}");
    let hot_path: Vec<&str> = out
        .lines()
        .take_while(|l| !l.contains("deoptimization stubs"))
        .collect();
    let tag_checks = hot_path.iter().filter(|l| l.contains("tag check")).count();
    assert!(
        tag_checks <= 2,
        "expected at most the two loop-entry tag checks on the hot path, found {tag_checks}:\n{out}"
    );
}
