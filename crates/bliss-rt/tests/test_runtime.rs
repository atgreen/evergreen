//! Tests for bliss-rt runtime module: RuntimeConfig, Runtime lifecycle,
//! parse_cli, and install_signal_handlers.

use bliss_rt::runtime::LogLevel;
use bliss_rt::runtime::*;

// ── RuntimeConfig::from_env ───────────────────────────────────────

#[test]
fn from_env_returns_sane_defaults() {
    let cfg = RuntimeConfig::from_env().expect("from_env");
    assert!(cfg.heap_size > 0);
    assert!(cfg.nursery_size > 0);
    assert!(cfg.tlab_size > 0);
    assert!(cfg.stack_size > 0);
    assert!(cfg.num_workers > 0);
    assert!(!cfg.no_image);
    assert!(cfg.eval_form.is_none());
    assert!(cfg.load_file.is_none());
    // Additional fields that should have sane defaults
    assert!(
        cfg.image_path.is_some(),
        "image_path should default to Some(\"bliss.bimg\") or similar"
    );
    assert!(cfg.gc_log.is_none(), "gc_log should default to None");
    assert!(!cfg.jit_dump, "jit_dump should default to false");
    assert!(
        cfg.safepoint_spin > 0,
        "safepoint_spin should have a non-zero default"
    );
    assert!(
        cfg.ffi_pool_pages > 0,
        "ffi_pool_pages should have a non-zero default"
    );
    assert_eq!(
        cfg.log_level,
        LogLevel::Info,
        "default log level should be Info"
    );
}

// ── RuntimeConfig::apply_cli_args ─────────────────────────────────

#[test]
fn apply_cli_args_eval() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.apply_cli_args(&["--eval".into(), "(+ 1 2)".into()])
        .expect("apply_cli_args");
    assert_eq!(cfg.eval_form.as_deref(), Some("(+ 1 2)"));
}

#[test]
fn apply_cli_args_heap_size() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.apply_cli_args(&["--heap-size".into(), "1073741824".into()])
        .expect("apply_cli_args");
    assert_eq!(cfg.heap_size, 1073741824);
}

#[test]
fn apply_cli_args_no_image() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.apply_cli_args(&["--no-image".into()])
        .expect("apply_cli_args");
    assert!(cfg.no_image);
}

#[test]
fn apply_cli_args_load() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.apply_cli_args(&["--load".into(), "boot.lisp".into()])
        .expect("apply_cli_args");
    assert_eq!(cfg.load_file.as_deref(), Some("boot.lisp"));
}

#[test]
fn apply_cli_args_gc_log() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.apply_cli_args(&["--gc-log".into(), "/tmp/gc.log".into()])
        .expect("apply_cli_args");
    assert_eq!(cfg.gc_log.as_deref(), Some("/tmp/gc.log"));
}

#[test]
fn apply_cli_args_jit_dump() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.apply_cli_args(&["--jit-dump".into()])
        .expect("apply_cli_args");
    assert!(cfg.jit_dump);
}

#[test]
fn apply_cli_args_log_level() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.apply_cli_args(&["--log-level".into(), "debug".into()])
        .expect("apply_cli_args");
    assert_eq!(cfg.log_level, LogLevel::Debug);
}

#[test]
fn apply_cli_args_unknown_flag_errors() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    assert!(cfg.apply_cli_args(&["--unknown-flag-xyz".into()]).is_err());
}

#[test]
fn apply_cli_args_eval_missing_value_errors() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    assert!(cfg.apply_cli_args(&["--eval".into()]).is_err());
}

// ── gc_config / scheduler_config ──────────────────────────────────

#[test]
fn gc_config_reflects_heap_size() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.heap_size = 256 << 20;
    assert_eq!(cfg.gc_config().heap_size, 256 << 20);
}

#[test]
fn scheduler_config_reflects_num_workers() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.num_workers = 8;
    assert_eq!(cfg.scheduler_config().num_workers, 8);
}

// ── parse_cli ─────────────────────────────────────────────────────

#[test]
fn parse_cli_separates_at_double_dash() {
    let args: Vec<String> = ["--heap-size", "1024", "--", "u1", "u2"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (cfg, cl) = parse_cli(&args).expect("parse_cli");
    assert_eq!(cfg.heap_size, 1024);
    assert_eq!(cl, vec!["u1", "u2"]);
}

#[test]
fn parse_cli_empty_args_gives_defaults() {
    let (cfg, cl) = parse_cli(&[]).expect("parse_cli");
    assert!(cl.is_empty());
    assert!(cfg.heap_size > 0);
}

#[test]
fn parse_cli_eval_flag() {
    let args: Vec<String> = ["--eval", "(print 42)"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (cfg, _) = parse_cli(&args).expect("parse_cli");
    assert_eq!(cfg.eval_form.as_deref(), Some("(print 42)"));
}

// ── Runtime lifecycle ─────────────────────────────────────────────

#[test]
fn runtime_init_and_config() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.heap_size = 999;
    let rt = Runtime::init(cfg).expect("init");
    assert_eq!(rt.config().heap_size, 999);
}

#[test]
fn runtime_eval_simple_form() {
    let mut rt = Runtime::init(RuntimeConfig::from_env().expect("from_env")).expect("init");
    assert!(rt.eval("(+ 1 2)").is_ok());
}

#[test]
fn runtime_run_returns_exit_code() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.eval_form = Some("(+ 1 2)".into());
    let mut rt = Runtime::init(cfg).expect("init");
    let result = rt.run();
    assert!(result.is_ok(), "Runtime::run should return Ok(exit_code)");
    // Conventionally, successful exit returns 0
    assert_eq!(result.unwrap(), 0);
}

#[test]
fn runtime_shutdown_idempotent() {
    let mut rt = Runtime::init(RuntimeConfig::from_env().expect("from_env")).expect("init");
    assert!(rt.shutdown().is_ok());
    assert!(rt.shutdown().is_ok());
}

// ── install_signal_handlers ───────────────────────────────────────

#[test]
fn install_signal_handlers_succeeds() {
    assert!(install_signal_handlers().is_ok());
}

// ── LogLevel — ordering ─────────────────────────────────────────

#[test]
fn log_level_error_is_least() {
    assert!(LogLevel::Error < LogLevel::Warn);
    assert!(LogLevel::Error < LogLevel::Info);
    assert!(LogLevel::Error < LogLevel::Debug);
    assert!(LogLevel::Error < LogLevel::Trace);
}

#[test]
fn log_level_trace_is_greatest() {
    assert!(LogLevel::Trace > LogLevel::Debug);
    assert!(LogLevel::Trace > LogLevel::Info);
    assert!(LogLevel::Trace > LogLevel::Warn);
    assert!(LogLevel::Trace > LogLevel::Error);
}

#[test]
fn log_level_total_ordering() {
    assert!(LogLevel::Error < LogLevel::Warn);
    assert!(LogLevel::Warn < LogLevel::Info);
    assert!(LogLevel::Info < LogLevel::Debug);
    assert!(LogLevel::Debug < LogLevel::Trace);
}

#[test]
fn log_level_equality() {
    assert_eq!(LogLevel::Error, LogLevel::Error);
    assert_eq!(LogLevel::Warn, LogLevel::Warn);
    assert_eq!(LogLevel::Info, LogLevel::Info);
    assert_eq!(LogLevel::Debug, LogLevel::Debug);
    assert_eq!(LogLevel::Trace, LogLevel::Trace);
}

#[test]
fn log_level_not_equal_across_variants() {
    assert_ne!(LogLevel::Error, LogLevel::Warn);
    assert_ne!(LogLevel::Warn, LogLevel::Info);
    assert_ne!(LogLevel::Info, LogLevel::Debug);
    assert_ne!(LogLevel::Debug, LogLevel::Trace);
}

#[test]
fn log_level_clone_and_copy() {
    let level = LogLevel::Debug;
    let cloned = level;
    assert_eq!(level, cloned);
    // Copy: level is still usable after move
    let copied = level;
    let _also = level;
    assert_eq!(copied, LogLevel::Debug);
}

#[test]
fn log_level_debug_output() {
    assert_eq!(format!("{:?}", LogLevel::Error), "Error");
    assert_eq!(format!("{:?}", LogLevel::Warn), "Warn");
    assert_eq!(format!("{:?}", LogLevel::Info), "Info");
    assert_eq!(format!("{:?}", LogLevel::Debug), "Debug");
    assert_eq!(format!("{:?}", LogLevel::Trace), "Trace");
}

#[test]
fn log_level_min_max() {
    let levels = [
        LogLevel::Trace,
        LogLevel::Error,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Debug,
    ];
    assert_eq!(*levels.iter().min().unwrap(), LogLevel::Error);
    assert_eq!(*levels.iter().max().unwrap(), LogLevel::Trace);
}

// ── RuntimeConfig — Clone and Debug ─────────────────────────────

#[test]
fn runtime_config_clone_preserves_all_fields() {
    let cfg = RuntimeConfig::from_env().expect("from_env");
    let cloned = cfg.clone();
    assert_eq!(cfg.heap_size, cloned.heap_size);
    assert_eq!(cfg.nursery_size, cloned.nursery_size);
    assert_eq!(cfg.tlab_size, cloned.tlab_size);
    assert_eq!(cfg.stack_size, cloned.stack_size);
    assert_eq!(cfg.num_workers, cloned.num_workers);
    assert_eq!(cfg.image_path, cloned.image_path);
    assert_eq!(cfg.no_image, cloned.no_image);
    assert_eq!(cfg.eval_form, cloned.eval_form);
    assert_eq!(cfg.load_file, cloned.load_file);
    assert_eq!(cfg.gc_log, cloned.gc_log);
    assert_eq!(cfg.jit_dump, cloned.jit_dump);
    assert_eq!(cfg.safepoint_spin, cloned.safepoint_spin);
    assert_eq!(cfg.ffi_pool_pages, cloned.ffi_pool_pages);
    assert_eq!(cfg.log_level, cloned.log_level);
}

#[test]
fn runtime_config_clone_is_independent() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    let cloned = cfg.clone();
    cfg.heap_size = 999_999;
    // The clone should retain its original value
    assert_ne!(cloned.heap_size, 999_999);
}

#[test]
fn runtime_config_debug_contains_field_names() {
    let cfg = RuntimeConfig::from_env().expect("from_env");
    let dbg = format!("{:?}", cfg);
    assert!(dbg.contains("RuntimeConfig"), "got: {}", dbg);
    assert!(dbg.contains("heap_size"), "got: {}", dbg);
    assert!(dbg.contains("nursery_size"), "got: {}", dbg);
    assert!(dbg.contains("stack_size"), "got: {}", dbg);
    assert!(dbg.contains("num_workers"), "got: {}", dbg);
    assert!(dbg.contains("log_level"), "got: {}", dbg);
}

// ── Runtime::init — edge cases ──────────────────────────────────

#[test]
fn runtime_init_zero_heap_size_is_error() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.heap_size = 0;
    let result = Runtime::init(cfg);
    assert!(
        result.is_err(),
        "Runtime::init with heap_size=0 should return Err"
    );
}

#[test]
fn runtime_init_zero_nursery_size_is_error() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.nursery_size = 0;
    let result = Runtime::init(cfg);
    assert!(
        result.is_err(),
        "Runtime::init with nursery_size=0 should return Err"
    );
}

#[test]
fn runtime_init_zero_stack_size_is_error() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.stack_size = 0;
    let result = Runtime::init(cfg);
    assert!(
        result.is_err(),
        "Runtime::init with stack_size=0 should return Err"
    );
}

#[test]
fn runtime_init_zero_workers_is_error() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.num_workers = 0;
    let result = Runtime::init(cfg);
    assert!(
        result.is_err(),
        "Runtime::init with num_workers=0 should return Err"
    );
}
