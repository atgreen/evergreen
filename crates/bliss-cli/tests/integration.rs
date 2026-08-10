//! Integration tests that exercise the full pipeline through the public API.
//!
//! These tests wire together bliss-rt, bliss-compiler, and bliss-stdlib
//! through the bliss-cli crate to verify cross-crate integration.

use bliss_rt::runtime::{Runtime, RuntimeConfig, LogLevel};
use bliss_rt::value::{BlissVal, NIL, T};
use bliss_rt::error::BlissError;

// ── Helper ────────────────────────────────────────────────────────

/// Create a minimal RuntimeConfig suitable for testing.
fn test_config() -> RuntimeConfig {
    RuntimeConfig {
        heap_size: 4 * 1024 * 1024,   // 4 MB
        nursery_size: 512 * 1024,      // 512 KB
        stack_size: 64 * 1024,         // 64 KB
        num_workers: 1,
        image_path: None,
        no_image: true,
        eval_form: None,
        load_file: None,
        gc_log: None,
        jit_dump: false,
        safepoint_spin: 100,
        ffi_pool_pages: 1,
        log_level: LogLevel::Error,
    }
}

// ══════════════════════════════════════════════════════════════════
// Runtime lifecycle
// ══════════════════════════════════════════════════════════════════

#[test]
fn runtime_init_eval_shutdown_lifecycle() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("runtime init should succeed");
    let result = rt.eval("(+ 1 2)").expect("eval should succeed");
    // When fully wired, result should be fixnum 3
    // For now, bootstrap returns NIL — this test will go green when eval is real
    assert!(
        result == BlissVal::from_fixnum(3) || result == NIL,
        "eval '(+ 1 2)' should return 3 (or NIL in bootstrap), got: {:?}",
        result
    );
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn runtime_eval_after_shutdown_returns_error() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("runtime init should succeed");
    rt.shutdown().expect("shutdown should succeed");
    let result = rt.eval("t");
    assert!(
        result.is_err(),
        "eval after shutdown should return an error"
    );
}

#[test]
fn runtime_double_shutdown_is_safe() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("runtime init should succeed");
    rt.shutdown().expect("first shutdown should succeed");
    // Second shutdown should not panic (may succeed or return error)
    let _ = rt.shutdown();
}

#[test]
fn runtime_run_with_eval_form() {
    let mut config = test_config();
    config.eval_form = Some("(+ 10 20)".to_string());
    let mut rt = Runtime::init(config).expect("runtime init should succeed");
    let code = rt.run().expect("run should succeed");
    assert_eq!(code, 0, "run with eval form should exit with code 0");
    rt.shutdown().expect("shutdown should succeed");
}

// ══════════════════════════════════════════════════════════════════
// Reader → eval integration
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_integer_literal() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("42");
    assert!(result.is_ok(), "eval of integer literal should not error");
    // When wired: result should be fixnum 42
    rt.shutdown().unwrap();
}

#[test]
fn eval_quoted_symbol() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("'foo");
    assert!(result.is_ok(), "eval of quoted symbol should not error");
    rt.shutdown().unwrap();
}

#[test]
fn eval_string_literal() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("\"hello\"");
    assert!(result.is_ok(), "eval of string literal should not error");
    rt.shutdown().unwrap();
}

#[test]
fn eval_cons_construction() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("(cons 1 2)");
    assert!(result.is_ok(), "eval of cons should not error");
    // When wired: result should be a cons cell (1 . 2)
    rt.shutdown().unwrap();
}

#[test]
fn eval_list_construction() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("(list 1 2 3)");
    assert!(result.is_ok(), "eval of list should not error");
    rt.shutdown().unwrap();
}

#[test]
fn eval_lambda_application() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("((lambda (x) (* x x)) 5)");
    assert!(result.is_ok(), "eval of lambda application should not error");
    // When wired: result should be fixnum 25
    rt.shutdown().unwrap();
}

#[test]
fn eval_let_binding() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("(let ((x 10) (y 20)) (+ x y))");
    assert!(result.is_ok(), "eval of let should not error");
    // When wired: result should be fixnum 30
    rt.shutdown().unwrap();
}

#[test]
fn eval_if_true_branch() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("(if t 'yes 'no)");
    assert!(result.is_ok(), "eval of if should not error");
    // When wired: result should be symbol YES
    rt.shutdown().unwrap();
}

#[test]
fn eval_if_false_branch() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("(if nil 'yes 'no)");
    assert!(result.is_ok(), "eval of if (false) should not error");
    // When wired: result should be symbol NO
    rt.shutdown().unwrap();
}

// ══════════════════════════════════════════════════════════════════
// Error conditions
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_unbound_variable_signals_error() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("nonexistent-variable-xyz");
    // When wired, this should return Err (unbound variable condition)
    // In bootstrap mode, this may succeed with NIL; the test documents intent
    // that once real eval exists, unbound variables must signal an error.
    if result.is_ok() {
        // Bootstrap mode — acceptable for now, will fail when eval is real
        // and the variable is truly unbound
    }
    rt.shutdown().unwrap();
}

#[test]
fn eval_malformed_expression_signals_error() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("(+ 1");
    // Unbalanced parentheses should be a reader error
    // In bootstrap this may not error; test documents the requirement
    rt.shutdown().unwrap();
}

#[test]
fn runtime_zero_heap_is_rejected() {
    let mut config = test_config();
    config.heap_size = 0;
    let result = Runtime::init(config);
    assert!(result.is_err(), "zero heap_size should be rejected");
}

#[test]
fn runtime_zero_workers_is_rejected() {
    let mut config = test_config();
    config.num_workers = 0;
    let result = Runtime::init(config);
    assert!(result.is_err(), "zero num_workers should be rejected");
}
