//! Tests for bliss-rt runtime module: RuntimeConfig, Runtime lifecycle,
//! parse_cli, and install_signal_handlers.

use bliss_rt::runtime::*;
use bliss_rt::runtime::LogLevel;

// ── RuntimeConfig::from_env ───────────────────────────────────────

#[test]
fn from_env_returns_sane_defaults() {
    let cfg = RuntimeConfig::from_env();
    assert!(cfg.heap_size > 0);
    assert!(cfg.nursery_size > 0);
    assert!(cfg.stack_size > 0);
    assert!(cfg.num_workers > 0);
    assert!(!cfg.no_image);
    assert!(cfg.eval_form.is_none());
    assert!(cfg.load_file.is_none());
    // Additional fields that should have sane defaults
    assert!(cfg.image_path.is_some(), "image_path should default to Some(\"bliss.bimg\") or similar");
    assert!(cfg.gc_log.is_none(), "gc_log should default to None");
    assert!(!cfg.jit_dump, "jit_dump should default to false");
    assert!(cfg.safepoint_spin > 0, "safepoint_spin should have a non-zero default");
    assert!(cfg.ffi_pool_pages > 0, "ffi_pool_pages should have a non-zero default");
    assert_eq!(cfg.log_level, LogLevel::Info, "default log level should be Info");
}

// ── RuntimeConfig::apply_cli_args ─────────────────────────────────

#[test]
fn apply_cli_args_eval() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.apply_cli_args(&["--eval".into(), "(+ 1 2)".into()]);
    assert_eq!(cfg.eval_form.as_deref(), Some("(+ 1 2)"));
}

#[test]
fn apply_cli_args_heap_size() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.apply_cli_args(&["--heap-size".into(), "1073741824".into()]);
    assert_eq!(cfg.heap_size, 1073741824);
}

#[test]
fn apply_cli_args_no_image() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.apply_cli_args(&["--no-image".into()]);
    assert!(cfg.no_image);
}

#[test]
fn apply_cli_args_load() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.apply_cli_args(&["--load".into(), "boot.lisp".into()]);
    assert_eq!(cfg.load_file.as_deref(), Some("boot.lisp"));
}

#[test]
fn apply_cli_args_gc_log() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.apply_cli_args(&["--gc-log".into(), "/tmp/gc.log".into()]);
    assert_eq!(cfg.gc_log.as_deref(), Some("/tmp/gc.log"));
}

#[test]
fn apply_cli_args_jit_dump() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.apply_cli_args(&["--jit-dump".into()]);
    assert!(cfg.jit_dump);
}

#[test]
fn apply_cli_args_log_level() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.apply_cli_args(&["--log-level".into(), "debug".into()]);
    assert_eq!(cfg.log_level, LogLevel::Debug);
}

#[test]
#[should_panic]
fn apply_cli_args_unknown_flag_errors() {
    let mut cfg = RuntimeConfig::from_env();
    // Unknown flags should cause an error (panic or Result::Err)
    cfg.apply_cli_args(&["--unknown-flag-xyz".into()]);
}

#[test]
#[should_panic]
fn apply_cli_args_eval_missing_value_errors() {
    let mut cfg = RuntimeConfig::from_env();
    // --eval with no following argument should error
    cfg.apply_cli_args(&["--eval".into()]);
}

// ── gc_config / scheduler_config ──────────────────────────────────

#[test]
fn gc_config_reflects_heap_size() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.heap_size = 256 << 20;
    assert_eq!(cfg.gc_config().heap_size, 256 << 20);
}

#[test]
fn scheduler_config_reflects_num_workers() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.num_workers = 8;
    assert_eq!(cfg.scheduler_config().num_workers, 8);
}

// ── parse_cli ─────────────────────────────────────────────────────

#[test]
fn parse_cli_separates_at_double_dash() {
    let args: Vec<String> = ["--heap-size", "1024", "--", "u1", "u2"]
        .iter().map(|s| s.to_string()).collect();
    let (cfg, cl) = parse_cli(&args);
    assert_eq!(cfg.heap_size, 1024);
    assert_eq!(cl, vec!["u1", "u2"]);
}

#[test]
fn parse_cli_empty_args_gives_defaults() {
    let (cfg, cl) = parse_cli(&[]);
    assert!(cl.is_empty());
    assert!(cfg.heap_size > 0);
}

#[test]
fn parse_cli_eval_flag() {
    let args: Vec<String> = ["--eval", "(print 42)"].iter().map(|s| s.to_string()).collect();
    let (cfg, _) = parse_cli(&args);
    assert_eq!(cfg.eval_form.as_deref(), Some("(print 42)"));
}

// ── Runtime lifecycle ─────────────────────────────────────────────

#[test]
fn runtime_init_and_config() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.heap_size = 999;
    let rt = Runtime::init(cfg).expect("init");
    assert_eq!(rt.config().heap_size, 999);
}

#[test]
fn runtime_eval_simple_form() {
    let mut rt = Runtime::init(RuntimeConfig::from_env()).expect("init");
    assert!(rt.eval("(+ 1 2)").is_ok());
}

#[test]
fn runtime_run_returns_exit_code() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.eval_form = Some("(+ 1 2)".into());
    let mut rt = Runtime::init(cfg).expect("init");
    let result = rt.run();
    assert!(result.is_ok(), "Runtime::run should return Ok(exit_code)");
    // Conventionally, successful exit returns 0
    assert_eq!(result.unwrap(), 0);
}

#[test]
fn runtime_shutdown_idempotent() {
    let mut rt = Runtime::init(RuntimeConfig::from_env()).expect("init");
    assert!(rt.shutdown().is_ok());
    assert!(rt.shutdown().is_ok());
}

// ── install_signal_handlers ───────────────────────────────────────

#[test]
fn install_signal_handlers_succeeds() {
    assert!(install_signal_handlers().is_ok());
}
