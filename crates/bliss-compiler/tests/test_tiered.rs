//! Tests for bliss-compiler tiered compilation.

use bliss_compiler::tiered::*;
use bliss_rt::value::{NIL, T};

// ── Tier ordering ─────────────────────────────────────────────────

#[test]
fn tier_ordering() {
    assert!(Tier::Interpreter < Tier::Baseline);
    assert!(Tier::Baseline < Tier::Optimising);
    assert!(Tier::Interpreter < Tier::Optimising);
    assert_eq!(Tier::Baseline, Tier::Baseline);
}

#[test]
fn tier_is_copy_and_clone() {
    let t = Tier::Baseline;
    let t2 = t;
    assert_eq!(t, t2);
    assert_eq!(t, t.clone());
}

// ── TierConfig ────────────────────────────────────────────────────

fn default_config() -> TierConfig {
    TierConfig {
        t1_threshold: 10,
        t2_threshold: 5000,
        osr_threshold: 10000,
        compile_threads: 2,
    }
}

#[test]
fn tier_config_fields() {
    let c = TierConfig {
        t1_threshold: 20,
        t2_threshold: 100,
        osr_threshold: 500,
        compile_threads: 4,
    };
    assert_eq!(c.t1_threshold, 20);
    assert_eq!(c.t2_threshold, 100);
    assert_eq!(c.osr_threshold, 500);
    assert_eq!(c.compile_threads, 4);
}

#[test]
fn tier_config_is_cloneable() {
    let c = default_config();
    let c2 = c.clone();
    assert_eq!(c2.t1_threshold, c.t1_threshold);
}

// ── Interpreter ───────────────────────────────────────────────────

#[test]
fn interpreter_eval_nil_is_self_evaluating() {
    let mut interp = Interpreter::new();
    let result = interp.eval(NIL);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), NIL);
}

#[test]
fn interpreter_eval_t_is_self_evaluating() {
    let mut interp = Interpreter::new();
    assert_eq!(interp.eval(T).unwrap(), T);
}

#[test]
fn interpreter_apply_non_function_errors() {
    let mut interp = Interpreter::new();
    assert!(interp.apply(NIL, NIL).is_err());
}

// ── BaselineCompiler ──────────────────────────────────────────────

#[test]
fn baseline_compiler_compile_returns_result() {
    let mut bc = BaselineCompiler::new();
    let _ = bc.compile(NIL); // must not panic
}

// ── OptimisingCompiler ────────────────────────────────────────────

#[test]
fn optimising_compiler_compile_returns_result() {
    let mut oc = OptimisingCompiler::new();
    let _ = oc.compile(NIL);
}

// ── CompiledCode — Issue #2: use expect instead of if-let ────────

#[test]
fn compiled_code_baseline_tier() {
    let mut bc = BaselineCompiler::new();
    let code = bc
        .compile(NIL)
        .expect("baseline compile of NIL should succeed");
    assert!(!code.entry_point().is_null());
    assert!(code.code_size() > 0);
    assert_eq!(code.tier(), Tier::Baseline);
}

#[test]
fn compiled_code_optimising_tier() {
    let mut oc = OptimisingCompiler::new();
    let code = oc
        .compile(NIL)
        .expect("optimising compile of NIL should succeed");
    assert_eq!(code.tier(), Tier::Optimising);
}

#[test]
fn compiled_code_install_on_non_function_errors() {
    let mut bc = BaselineCompiler::new();
    let code = bc
        .compile(NIL)
        .expect("baseline compile of NIL should succeed");
    // Installing compiled code onto NIL (not a function) should error.
    assert!(code.install(NIL).is_err());
}

// ── check_promotion / request_compilation — Issue #6 ─────────────

#[test]
fn check_promotion_cold_returns_none() {
    // A cold function (NIL, no invocations) should not be promoted.
    assert!(check_promotion(NIL, &default_config()).is_none());
}

#[test]
fn check_promotion_non_function_with_zero_threshold() {
    // NIL is not a function — check_promotion treats non-functions as T0 with
    // invoke_count=0. With t1_threshold=0, invoke_count(0) >= threshold(0) is
    // true, so this returns Some(Baseline). This tests the threshold=0 boundary
    // for non-function values specifically.
    let config = TierConfig {
        t1_threshold: 0,
        t2_threshold: 5000,
        osr_threshold: 10000,
        compile_threads: 2,
    };
    let result = check_promotion(NIL, &config);
    assert_eq!(
        result,
        Some(Tier::Baseline),
        "non-function at T0 with invoke_count=0 and t1_threshold=0 should promote to Baseline"
    );
}

#[test]
fn check_promotion_non_function_below_threshold_returns_none() {
    // NIL is not a function — check_promotion treats it as T0 with invoke_count=0.
    // With t1_threshold=10, invoke_count(0) < threshold(10), so no promotion.
    let config = default_config();
    assert!(
        check_promotion(NIL, &config).is_none(),
        "non-function at T0 with invoke_count=0 below t1_threshold should return None"
    );
}

#[test]
fn check_promotion_real_function_needs_function_tag() {
    // T is also not a function — verify that non-function values consistently
    // get the non-function code path (T0, invoke_count=0).
    let config = default_config();
    assert!(
        check_promotion(T, &config).is_none(),
        "T (non-function) should return None with default thresholds"
    );
}

#[test]
fn request_compilation_rejects_non_function() {
    // request_compilation on NIL (not a function) should return an error.
    let result_baseline = request_compilation(NIL, Tier::Baseline);
    assert!(
        result_baseline.is_err(),
        "requesting compilation of NIL (non-function) should error"
    );

    let result_optimising = request_compilation(NIL, Tier::Optimising);
    assert!(
        result_optimising.is_err(),
        "requesting compilation of NIL (non-function) should error"
    );
}
