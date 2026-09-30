//! Integration tests that exercise the full pipeline through the public API.
//!
//! These tests wire together egcl-rt, egcl-compiler, and egcl-stdlib
//! through the egcl crate to verify cross-crate integration.

use egcl_rt::runtime::{LogLevel, Runtime, RuntimeConfig};
use egcl_rt::value::EgclVal;

// ── Helper ────────────────────────────────────────────────────────

/// Create a minimal RuntimeConfig suitable for testing.
fn test_config() -> RuntimeConfig {
    let mut config = RuntimeConfig::from_env().expect("from_env");
    config.heap_size = 4 * 1024 * 1024; // 4 MB
    config.nursery_size = 512 * 1024; // 512 KB
    config.tlab_size = 128 * 1024; // 128 KB
    config.stack_size = 64 * 1024; // 64 KB
    config.num_workers = 1;
    config.image_path = None;
    config.no_image = true;
    config.eval_form = None;
    config.load_file = None;
    config.gc_log = None;
    config.jit_dump = false;
    config.safepoint_spin = 100;
    config.ffi_pool_pages = 1;
    config.log_level = LogLevel::Error;
    config
}

// ══════════════════════════════════════════════════════════════════
// Runtime lifecycle
// ══════════════════════════════════════════════════════════════════

#[test]
fn runtime_init_eval_shutdown_lifecycle() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("runtime init should succeed");
    let result = rt.eval("(+ 1 2)").expect("eval should succeed");
    assert_eq!(
        result,
        EgclVal::from_fixnum(3),
        "eval '(+ 1 2)' should return 3, got: {:?}",
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
    let result = rt
        .eval("42")
        .expect("eval of integer literal should not error");
    assert_eq!(
        result,
        EgclVal::from_fixnum(42),
        "eval of '42' should return fixnum 42, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_quoted_symbol() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("'foo")
        .expect("eval of quoted symbol should not error");
    // Result should be a symbol named FOO (CL upcases by default)
    assert!(
        result.is_symbol(),
        "eval of 'foo should return a symbol, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_string_literal() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("\"hello\"")
        .expect("eval of string literal should not error");
    assert!(
        result.is_string(),
        "eval of '\"hello\"' should return a string, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_cons_construction() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("(cons 1 2)")
        .expect("eval of cons should not error");
    // Result should be a cons cell (1 . 2)
    assert!(
        result.is_cons(),
        "eval of '(cons 1 2)' should return a cons cell, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_list_construction() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("(list 1 2 3)")
        .expect("eval of list should not error");
    // Result should be a cons cell (a proper list)
    assert!(
        result.is_cons(),
        "eval of '(list 1 2 3)' should return a cons (list), got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_lambda_application() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("((lambda (x) (* x x)) 5)")
        .expect("eval of lambda application should not error");
    assert_eq!(
        result,
        EgclVal::from_fixnum(25),
        "eval of '((lambda (x) (* x x)) 5)' should return 25, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_let_binding() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("(let ((x 10) (y 20)) (+ x y))")
        .expect("eval of let should not error");
    assert_eq!(
        result,
        EgclVal::from_fixnum(30),
        "eval of '(let ((x 10) (y 20)) (+ x y))' should return 30, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_if_true_branch() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("(if t 'yes 'no)")
        .expect("eval of if should not error");
    // Result should be the symbol YES
    assert!(
        result.is_symbol(),
        "eval of '(if t 'yes 'no)' should return a symbol, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_if_false_branch() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt
        .eval("(if nil 'yes 'no)")
        .expect("eval of if (false) should not error");
    // Result should be the symbol NO
    assert!(
        result.is_symbol(),
        "eval of '(if nil 'yes 'no)' should return a symbol, got: {:?}",
        result
    );
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
    assert!(
        result.is_err(),
        "eval of unbound variable should return an error, got: {:?}",
        result
    );
    rt.shutdown().unwrap();
}

#[test]
fn eval_malformed_expression_signals_error() {
    let config = test_config();
    let mut rt = Runtime::init(config).expect("init");
    let result = rt.eval("(+ 1");
    assert!(
        result.is_err(),
        "unbalanced parens should be a reader error, got: {:?}",
        result
    );
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
