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
    TierConfig { t1_threshold: 10, t2_threshold: 5000, osr_threshold: 10000, compile_threads: 2 }
}

#[test]
fn tier_config_fields() {
    let c = TierConfig { t1_threshold: 20, t2_threshold: 100, osr_threshold: 500, compile_threads: 4 };
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

// ── CompiledCode ──────────────────────────────────────────────────

#[test]
fn compiled_code_baseline_tier() {
    let mut bc = BaselineCompiler::new();
    if let Ok(code) = bc.compile(NIL) {
        assert!(!code.entry_point().is_null());
        assert!(code.code_size() > 0);
        assert_eq!(code.tier(), Tier::Baseline);
    }
}

#[test]
fn compiled_code_optimising_tier() {
    let mut oc = OptimisingCompiler::new();
    if let Ok(code) = oc.compile(NIL) {
        assert_eq!(code.tier(), Tier::Optimising);
    }
}

#[test]
fn compiled_code_install_on_non_function_errors() {
    let mut bc = BaselineCompiler::new();
    if let Ok(code) = bc.compile(NIL) {
        assert!(code.install(NIL).is_err());
    }
}

// ── check_promotion / request_compilation ─────────────────────────

#[test]
fn check_promotion_cold_returns_none() {
    assert!(check_promotion(NIL, &default_config()).is_none());
}

#[test]
fn request_compilation_does_not_panic() {
    let _ = request_compilation(NIL, Tier::Baseline);
    let _ = request_compilation(NIL, Tier::Optimising);
}
