//! Tests for bliss-rt error types: BlissError enum.
//!
//! Covers every variant's construction and Display output,
//! std::error::Error trait implementation, Debug output,
//! and non_exhaustive attribute behavior. Also includes
//! additional runtime module tests for LogLevel ordering,
//! RuntimeConfig Clone/Debug, and edge cases.

use bliss_rt::error::BlissError;
use bliss_rt::thread::GreenThreadId;
use bliss_rt::value::BlissVal;
use bliss_rt::runtime::{LogLevel, RuntimeConfig, Runtime};

// ══════════════════════════════════════════════════════════════════
// BlissError — Construction of every variant
// ══════════════════════════════════════════════════════════════════

#[test]
fn construct_all_variants() {
    // Verify every variant can be constructed without panic
    let _errs: Vec<BlissError> = vec![
        BlissError::Oom,
        BlissError::StackOverflow(GreenThreadId(42)),
        BlissError::InvalidImage("corrupt header".into()),
        BlissError::FfiError("dlopen failed".into()),
        BlissError::SignalError(11),
        BlissError::Shutdown,
        BlissError::Internal("invariant broken".into()),
        BlissError::TypeError { datum: BlissVal(0), expected: "INTEGER".into() },
        BlissError::UnboundVariable(BlissVal(0)),
        BlissError::UndefinedFunction(BlissVal(0)),
        BlissError::ArithmeticError("division by zero".into()),
        BlissError::PackageError("package not found".into()),
        BlissError::StreamError("stream closed".into()),
        BlissError::FileError("/no/such/file".into()),
        BlissError::SandboxViolation("network access denied".into()),
    ];
    assert_eq!(_errs.len(), 15);
}

// ══════════════════════════════════════════════════════════════════
// BlissError — Display output for every variant
// ══════════════════════════════════════════════════════════════════

#[test]
fn display_oom() {
    let err = BlissError::Oom;
    assert_eq!(err.to_string(), "out of memory");
}

#[test]
fn display_stack_overflow() {
    let err = BlissError::StackOverflow(GreenThreadId(7));
    assert_eq!(err.to_string(), "stack overflow in thread 7");
}

#[test]
fn display_stack_overflow_edge_ids() {
    assert_eq!(
        BlissError::StackOverflow(GreenThreadId(0)).to_string(),
        "stack overflow in thread 0"
    );
    let large = BlissError::StackOverflow(GreenThreadId(u64::MAX)).to_string();
    assert!(large.contains(&u64::MAX.to_string()));
}

#[test]
fn display_invalid_image() {
    assert_eq!(BlissError::InvalidImage("bad magic".into()).to_string(), "invalid image: bad magic");
    assert_eq!(BlissError::InvalidImage(String::new()).to_string(), "invalid image: ");
}

#[test]
fn display_ffi_error() {
    let err = BlissError::FfiError("symbol not found".to_string());
    assert_eq!(err.to_string(), "FFI error: symbol not found");
}

#[test]
fn display_signal_error() {
    let err = BlissError::SignalError(9);
    assert_eq!(err.to_string(), "signal error: signal 9");
}

#[test]
fn display_signal_error_negative() {
    let err = BlissError::SignalError(-1);
    assert_eq!(err.to_string(), "signal error: signal -1");
}

#[test]
fn display_shutdown() {
    let err = BlissError::Shutdown;
    assert_eq!(err.to_string(), "shutdown requested");
}

#[test]
fn display_internal() {
    let err = BlissError::Internal("null pointer".to_string());
    assert_eq!(err.to_string(), "internal error: null pointer");
}

#[test]
fn display_type_error() {
    let datum = BlissVal(42);
    let err = BlissError::TypeError {
        datum,
        expected: "STRING".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.starts_with("type error: "), "got: {}", msg);
    assert!(msg.contains("STRING"), "expected type name in message, got: {}", msg);
}

#[test]
fn display_unbound_variable() {
    let sym = BlissVal(100);
    let err = BlissError::UnboundVariable(sym);
    let msg = err.to_string();
    assert!(msg.starts_with("unbound variable: "), "got: {}", msg);
}

#[test]
fn display_undefined_function() {
    let sym = BlissVal(200);
    let err = BlissError::UndefinedFunction(sym);
    let msg = err.to_string();
    assert!(msg.starts_with("undefined function: "), "got: {}", msg);
}

#[test]
fn display_arithmetic_error() {
    let err = BlissError::ArithmeticError("division by zero".to_string());
    assert_eq!(err.to_string(), "arithmetic error: division by zero");
}

#[test]
fn display_package_error() {
    let err = BlissError::PackageError("CL-USER not found".to_string());
    assert_eq!(err.to_string(), "package error: CL-USER not found");
}

#[test]
fn display_stream_error() {
    let err = BlissError::StreamError("read past EOF".to_string());
    assert_eq!(err.to_string(), "stream error: read past EOF");
}

#[test]
fn display_file_error() {
    let err = BlissError::FileError("permission denied".to_string());
    assert_eq!(err.to_string(), "file error: permission denied");
}

#[test]
fn display_sandbox_violation() {
    let err = BlissError::SandboxViolation("filesystem write blocked".to_string());
    assert_eq!(err.to_string(), "sandbox violation: filesystem write blocked");
}

// ══════════════════════════════════════════════════════════════════
// BlissError — std::error::Error trait
// ══════════════════════════════════════════════════════════════════

#[test]
fn bliss_error_implements_std_error() {
    // Verify that BlissError can be used as a dyn std::error::Error.
    let err: Box<dyn std::error::Error> = Box::new(BlissError::Oom);
    // Display should work through the trait object
    assert_eq!(err.to_string(), "out of memory");
}

#[test]
fn bliss_error_source_is_none() {
    // Default Error impl returns None for source()
    let err = BlissError::Internal("bug".to_string());
    let as_error: &dyn std::error::Error = &err;
    assert!(as_error.source().is_none());
}

#[test]
fn bliss_error_all_variants_as_dyn_error() {
    // Every variant must be usable as dyn Error
    let errors: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(BlissError::Oom),
        Box::new(BlissError::StackOverflow(GreenThreadId(1))),
        Box::new(BlissError::InvalidImage("x".into())),
        Box::new(BlissError::FfiError("x".into())),
        Box::new(BlissError::SignalError(2)),
        Box::new(BlissError::Shutdown),
        Box::new(BlissError::Internal("x".into())),
        Box::new(BlissError::TypeError {
            datum: BlissVal(0),
            expected: "T".into(),
        }),
        Box::new(BlissError::UnboundVariable(BlissVal(0))),
        Box::new(BlissError::UndefinedFunction(BlissVal(0))),
        Box::new(BlissError::ArithmeticError("x".into())),
        Box::new(BlissError::PackageError("x".into())),
        Box::new(BlissError::StreamError("x".into())),
        Box::new(BlissError::FileError("x".into())),
        Box::new(BlissError::SandboxViolation("x".into())),
    ];
    for err in &errors {
        // Each should produce a non-empty Display string
        assert!(!err.to_string().is_empty());
        // source() defaults to None
        assert!(err.source().is_none());
    }
}

// ══════════════════════════════════════════════════════════════════
// BlissError — Debug output
// ══════════════════════════════════════════════════════════════════

#[test]
fn debug_unit_variants() {
    let dbg_oom = format!("{:?}", BlissError::Oom);
    assert!(dbg_oom.contains("Oom"), "got: {}", dbg_oom);

    let dbg_shutdown = format!("{:?}", BlissError::Shutdown);
    assert!(dbg_shutdown.contains("Shutdown"), "got: {}", dbg_shutdown);
}

#[test]
fn debug_stack_overflow_contains_thread_id() {
    let dbg = format!("{:?}", BlissError::StackOverflow(GreenThreadId(99)));
    assert!(dbg.contains("StackOverflow"), "got: {}", dbg);
    assert!(dbg.contains("99"), "got: {}", dbg);
}

#[test]
fn debug_signal_error_contains_signal_number() {
    let dbg = format!("{:?}", BlissError::SignalError(15));
    assert!(dbg.contains("SignalError"), "got: {}", dbg);
    assert!(dbg.contains("15"), "got: {}", dbg);
}

#[test]
fn debug_type_error_contains_fields() {
    let dbg = format!("{:?}", BlissError::TypeError {
        datum: BlissVal(5),
        expected: "NUMBER".to_string(),
    });
    assert!(dbg.contains("TypeError"), "got: {}", dbg);
    assert!(dbg.contains("NUMBER"), "got: {}", dbg);
}

#[test]
fn debug_all_string_variants() {
    let cases: Vec<(BlissError, &str, &str)> = vec![
        (BlissError::ArithmeticError("overflow".into()), "ArithmeticError", "overflow"),
        (BlissError::PackageError("conflict".into()), "PackageError", "conflict"),
        (BlissError::StreamError("closed".into()), "StreamError", "closed"),
        (BlissError::FileError("not found".into()), "FileError", "not found"),
        (BlissError::SandboxViolation("blocked".into()), "SandboxViolation", "blocked"),
        (BlissError::InvalidImage("truncated".into()), "InvalidImage", "truncated"),
        (BlissError::FfiError("abi mismatch".into()), "FfiError", "abi mismatch"),
        (BlissError::Internal("null ref".into()), "Internal", "null ref"),
    ];
    for (err, variant, msg) in cases {
        let dbg = format!("{:?}", err);
        assert!(dbg.contains(variant), "expected '{}' in debug: {}", variant, dbg);
        assert!(dbg.contains(msg), "expected '{}' in debug: {}", msg, dbg);
    }
}

// ══════════════════════════════════════════════════════════════════
// BlissError — #[non_exhaustive] behavior
// ══════════════════════════════════════════════════════════════════

#[test]
fn non_exhaustive_requires_wildcard_arm() {
    // Because BlissError is #[non_exhaustive], external crates must
    // use a wildcard arm in match. This test verifies we can match
    // known variants and must have a catch-all.
    let err = BlissError::Oom;
    let msg = match err {
        BlissError::Oom => "oom",
        BlissError::StackOverflow(_) => "stack",
        BlissError::InvalidImage(_) => "image",
        BlissError::FfiError(_) => "ffi",
        BlissError::SignalError(_) => "signal",
        BlissError::Shutdown => "shutdown",
        BlissError::Internal(_) => "internal",
        BlissError::TypeError { .. } => "type",
        BlissError::UnboundVariable(_) => "unbound",
        BlissError::UndefinedFunction(_) => "undef",
        BlissError::ArithmeticError(_) => "arith",
        BlissError::PackageError(_) => "pkg",
        BlissError::StreamError(_) => "stream",
        BlissError::FileError(_) => "file",
        BlissError::SandboxViolation(_) => "sandbox",
        // Wildcard is REQUIRED by #[non_exhaustive]
        _ => "unknown",
    };
    assert_eq!(msg, "oom");
}

#[test]
fn non_exhaustive_match_each_variant() {
    // Spot-check that several distinct variants route correctly
    let classify = |e: &BlissError| -> &str {
        match e {
            BlissError::Oom => "oom",
            BlissError::StackOverflow(_) => "stack",
            BlissError::InvalidImage(_) => "image",
            BlissError::FfiError(_) => "ffi",
            BlissError::SignalError(_) => "signal",
            BlissError::Shutdown => "shutdown",
            BlissError::Internal(_) => "internal",
            BlissError::TypeError { .. } => "type",
            BlissError::UnboundVariable(_) => "unbound",
            BlissError::UndefinedFunction(_) => "undef",
            BlissError::ArithmeticError(_) => "arith",
            BlissError::PackageError(_) => "pkg",
            BlissError::StreamError(_) => "stream",
            BlissError::FileError(_) => "file",
            BlissError::SandboxViolation(_) => "sandbox",
            _ => "unknown",
        }
    };
    assert_eq!(classify(&BlissError::Shutdown), "shutdown");
    assert_eq!(classify(&BlissError::FfiError("x".into())), "ffi");
    assert_eq!(classify(&BlissError::SandboxViolation("x".into())), "sandbox");
}

// ══════════════════════════════════════════════════════════════════
// BlissError — Result<T, BlissError> ergonomics
// ══════════════════════════════════════════════════════════════════

#[test]
fn bliss_error_in_result_ok() {
    let result: Result<i32, BlissError> = Ok(42);
    assert_eq!(result.unwrap(), 42);
}

#[test]
fn bliss_error_in_result_err() {
    let result: Result<i32, BlissError> = Err(BlissError::Oom);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "out of memory");
}

#[test]
fn bliss_error_question_mark_propagation() {
    fn inner() -> Result<(), BlissError> {
        let _: () = Err(BlissError::Shutdown)?;
        Ok(())
    }
    let result = inner();
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "shutdown requested");
}

// ══════════════════════════════════════════════════════════════════
// LogLevel — ordering tests
// ══════════════════════════════════════════════════════════════════

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
    let cloned = level.clone();
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

// ══════════════════════════════════════════════════════════════════
// RuntimeConfig — Clone and Debug
// ══════════════════════════════════════════════════════════════════

#[test]
fn runtime_config_clone_preserves_all_fields() {
    let cfg = RuntimeConfig::from_env();
    let cloned = cfg.clone();
    assert_eq!(cfg.heap_size, cloned.heap_size);
    assert_eq!(cfg.nursery_size, cloned.nursery_size);
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
    let mut cfg = RuntimeConfig::from_env();
    let cloned = cfg.clone();
    cfg.heap_size = 999_999;
    // The clone should retain its original value
    assert_ne!(cloned.heap_size, 999_999);
}

#[test]
fn runtime_config_debug_contains_field_names() {
    let cfg = RuntimeConfig::from_env();
    let dbg = format!("{:?}", cfg);
    assert!(dbg.contains("RuntimeConfig"), "got: {}", dbg);
    assert!(dbg.contains("heap_size"), "got: {}", dbg);
    assert!(dbg.contains("nursery_size"), "got: {}", dbg);
    assert!(dbg.contains("stack_size"), "got: {}", dbg);
    assert!(dbg.contains("num_workers"), "got: {}", dbg);
    assert!(dbg.contains("log_level"), "got: {}", dbg);
}

// ══════════════════════════════════════════════════════════════════
// Runtime::init — edge cases
// ══════════════════════════════════════════════════════════════════

#[test]
fn runtime_init_zero_heap_size_is_error() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.heap_size = 0;
    // A zero-byte heap is nonsensical; init should fail
    let result = Runtime::init(cfg);
    assert!(result.is_err(), "Runtime::init with heap_size=0 should return Err");
}

#[test]
fn runtime_init_zero_nursery_size_is_error() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.nursery_size = 0;
    let result = Runtime::init(cfg);
    assert!(result.is_err(), "Runtime::init with nursery_size=0 should return Err");
}

#[test]
fn runtime_init_zero_stack_size_is_error() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.stack_size = 0;
    let result = Runtime::init(cfg);
    assert!(result.is_err(), "Runtime::init with stack_size=0 should return Err");
}

#[test]
fn runtime_init_zero_workers_is_error() {
    let mut cfg = RuntimeConfig::from_env();
    cfg.num_workers = 0;
    let result = Runtime::init(cfg);
    assert!(result.is_err(), "Runtime::init with num_workers=0 should return Err");
}
