//! Tests for torcl-rt error types: TorclError enum.
//!
//! Covers every variant's construction and Display output,
//! std::error::Error trait implementation, Debug output,
//! Send/Sync bounds, and non_exhaustive match routing.

use torcl_rt::error::TorclError;
use torcl_rt::thread::FiberId;
use torcl_rt::value::TorclVal;

// ══════════════════════════════════════════════════════════════════
// TorclError — Construction of every variant
// ══════════════════════════════════════════════════════════════════

#[test]
fn construct_all_variants() {
    // Verify every variant can be constructed without panic
    let _errs: Vec<TorclError> = vec![
        TorclError::Oom,
        TorclError::StackOverflow(FiberId(42)),
        TorclError::InvalidImage("corrupt header".into()),
        TorclError::FfiError("dlopen failed".into()),
        TorclError::SignalError(11),
        TorclError::Shutdown,
        TorclError::Internal("invariant broken".into()),
        TorclError::TypeError {
            datum: TorclVal(0),
            expected: "INTEGER".into(),
        },
        TorclError::UnboundVariable(TorclVal(0)),
        TorclError::UndefinedFunction(TorclVal(0)),
        TorclError::ArithmeticError("division by zero".into()),
        TorclError::PackageError("package not found".into()),
        TorclError::StreamError("stream closed".into()),
        TorclError::FileError("/no/such/file".into()),
        TorclError::SandboxViolation("network access denied".into()),
    ];
    assert_eq!(_errs.len(), 15);
}

// ══════════════════════════════════════════════════════════════════
// TorclError — Display output for every variant
// ══════════════════════════════════════════════════════════════════

#[test]
fn display_oom() {
    let err = TorclError::Oom;
    assert_eq!(err.to_string(), "out of memory");
}

#[test]
fn display_stack_overflow() {
    let err = TorclError::StackOverflow(FiberId(7));
    assert_eq!(err.to_string(), "stack overflow in thread 7");
}

#[test]
fn display_stack_overflow_edge_ids() {
    assert_eq!(
        TorclError::StackOverflow(FiberId(0)).to_string(),
        "stack overflow in thread 0"
    );
    let large = TorclError::StackOverflow(FiberId(u64::MAX)).to_string();
    assert!(large.contains(&u64::MAX.to_string()));
}

#[test]
fn display_invalid_image() {
    assert_eq!(
        TorclError::InvalidImage("bad magic".into()).to_string(),
        "invalid image: bad magic"
    );
    assert_eq!(
        TorclError::InvalidImage(String::new()).to_string(),
        "invalid image: "
    );
}

#[test]
fn display_ffi_error() {
    let err = TorclError::FfiError("symbol not found".to_string());
    assert_eq!(err.to_string(), "FFI error: symbol not found");
}

#[test]
fn display_signal_error() {
    let err = TorclError::SignalError(9);
    assert_eq!(err.to_string(), "signal error: signal 9");
}

#[test]
fn display_signal_error_negative() {
    let err = TorclError::SignalError(-1);
    assert_eq!(err.to_string(), "signal error: signal -1");
}

#[test]
fn display_shutdown() {
    let err = TorclError::Shutdown;
    assert_eq!(err.to_string(), "shutdown requested");
}

#[test]
fn display_internal() {
    let err = TorclError::Internal("null pointer".to_string());
    assert_eq!(err.to_string(), "internal error: null pointer");
}

#[test]
fn display_type_error() {
    let datum = TorclVal(42);
    let err = TorclError::TypeError {
        datum,
        expected: "STRING".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.starts_with("type error: "), "got: {}", msg);
    assert!(
        msg.contains("STRING"),
        "expected type name in message, got: {}",
        msg
    );
}

#[test]
fn display_unbound_variable() {
    let sym = TorclVal(100);
    let err = TorclError::UnboundVariable(sym);
    let msg = err.to_string();
    assert!(msg.starts_with("unbound variable: "), "got: {}", msg);
}

#[test]
fn display_undefined_function() {
    let sym = TorclVal(200);
    let err = TorclError::UndefinedFunction(sym);
    let msg = err.to_string();
    assert!(msg.starts_with("undefined function: "), "got: {}", msg);
}

#[test]
fn display_arithmetic_error() {
    let err = TorclError::ArithmeticError("division by zero".to_string());
    assert_eq!(err.to_string(), "arithmetic error: division by zero");
}

#[test]
fn display_package_error() {
    let err = TorclError::PackageError("CL-USER not found".to_string());
    assert_eq!(err.to_string(), "package error: CL-USER not found");
}

#[test]
fn display_stream_error() {
    let err = TorclError::StreamError("read past EOF".to_string());
    assert_eq!(err.to_string(), "stream error: read past EOF");
}

#[test]
fn display_file_error() {
    let err = TorclError::FileError("permission denied".to_string());
    assert_eq!(err.to_string(), "file error: permission denied");
}

#[test]
fn display_sandbox_violation() {
    let err = TorclError::SandboxViolation("filesystem write blocked".to_string());
    assert_eq!(
        err.to_string(),
        "sandbox violation: filesystem write blocked"
    );
}

// ══════════════════════════════════════════════════════════════════
// TorclError — std::error::Error trait
// ══════════════════════════════════════════════════════════════════

#[test]
fn torcl_error_implements_std_error() {
    // Verify that TorclError can be used as a dyn std::error::Error.
    let err: Box<dyn std::error::Error> = Box::new(TorclError::Oom);
    // Display should work through the trait object
    assert_eq!(err.to_string(), "out of memory");
}

#[test]
fn torcl_error_source_is_none() {
    // Default Error impl returns None for source()
    let err = TorclError::Internal("bug".to_string());
    let as_error: &dyn std::error::Error = &err;
    assert!(as_error.source().is_none());
}

#[test]
fn torcl_error_all_variants_as_dyn_error() {
    // Every variant must be usable as dyn Error
    let errors: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(TorclError::Oom),
        Box::new(TorclError::StackOverflow(FiberId(1))),
        Box::new(TorclError::InvalidImage("x".into())),
        Box::new(TorclError::FfiError("x".into())),
        Box::new(TorclError::SignalError(2)),
        Box::new(TorclError::Shutdown),
        Box::new(TorclError::Internal("x".into())),
        Box::new(TorclError::TypeError {
            datum: TorclVal(0),
            expected: "T".into(),
        }),
        Box::new(TorclError::UnboundVariable(TorclVal(0))),
        Box::new(TorclError::UndefinedFunction(TorclVal(0))),
        Box::new(TorclError::ArithmeticError("x".into())),
        Box::new(TorclError::PackageError("x".into())),
        Box::new(TorclError::StreamError("x".into())),
        Box::new(TorclError::FileError("x".into())),
        Box::new(TorclError::SandboxViolation("x".into())),
    ];
    for err in &errors {
        // Each should produce a non-empty Display string
        assert!(!err.to_string().is_empty());
        // source() defaults to None
        assert!(err.source().is_none());
    }
}

// ══════════════════════════════════════════════════════════════════
// TorclError — Debug output
// ══════════════════════════════════════════════════════════════════

#[test]
fn debug_unit_variants() {
    let dbg_oom = format!("{:?}", TorclError::Oom);
    assert!(dbg_oom.contains("Oom"), "got: {}", dbg_oom);

    let dbg_shutdown = format!("{:?}", TorclError::Shutdown);
    assert!(dbg_shutdown.contains("Shutdown"), "got: {}", dbg_shutdown);
}

#[test]
fn debug_stack_overflow_contains_thread_id() {
    let dbg = format!("{:?}", TorclError::StackOverflow(FiberId(99)));
    assert!(dbg.contains("StackOverflow"), "got: {}", dbg);
    assert!(dbg.contains("99"), "got: {}", dbg);
}

#[test]
fn debug_signal_error_contains_signal_number() {
    let dbg = format!("{:?}", TorclError::SignalError(15));
    assert!(dbg.contains("SignalError"), "got: {}", dbg);
    assert!(dbg.contains("15"), "got: {}", dbg);
}

#[test]
fn debug_type_error_contains_fields() {
    let dbg = format!(
        "{:?}",
        TorclError::TypeError {
            datum: TorclVal(5),
            expected: "NUMBER".to_string(),
        }
    );
    assert!(dbg.contains("TypeError"), "got: {}", dbg);
    assert!(dbg.contains("NUMBER"), "got: {}", dbg);
}

#[test]
fn debug_all_string_variants() {
    let cases: Vec<(TorclError, &str, &str)> = vec![
        (
            TorclError::ArithmeticError("overflow".into()),
            "ArithmeticError",
            "overflow",
        ),
        (
            TorclError::PackageError("conflict".into()),
            "PackageError",
            "conflict",
        ),
        (
            TorclError::StreamError("closed".into()),
            "StreamError",
            "closed",
        ),
        (
            TorclError::FileError("not found".into()),
            "FileError",
            "not found",
        ),
        (
            TorclError::SandboxViolation("blocked".into()),
            "SandboxViolation",
            "blocked",
        ),
        (
            TorclError::InvalidImage("truncated".into()),
            "InvalidImage",
            "truncated",
        ),
        (
            TorclError::FfiError("abi mismatch".into()),
            "FfiError",
            "abi mismatch",
        ),
        (
            TorclError::Internal("null ref".into()),
            "Internal",
            "null ref",
        ),
    ];
    for (err, variant, msg) in cases {
        let dbg = format!("{:?}", err);
        assert!(
            dbg.contains(variant),
            "expected '{}' in debug: {}",
            variant,
            dbg
        );
        assert!(dbg.contains(msg), "expected '{}' in debug: {}", msg, dbg);
    }
}

// ══════════════════════════════════════════════════════════════════
// TorclError — Send + Sync (required for cross-thread error propagation)
// ══════════════════════════════════════════════════════════════════

#[test]
fn torcl_error_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TorclError>();
}

// ══════════════════════════════════════════════════════════════════
// TorclError — match routing with wildcard arm
// ══════════════════════════════════════════════════════════════════
// NOTE: TorclError is #[non_exhaustive], which means external crates
// MUST include a wildcard arm in match expressions. A true compile-time
// test of that guarantee requires a compile-fail harness (e.g. trybuild).
// The tests below only verify that match routing with a wildcard arm
// works correctly for each variant — they do not verify that omitting
// the wildcard causes a compile error.

#[test]
fn match_routing_with_wildcard_arm() {
    // Verify each variant routes to the correct arm when a wildcard is present.
    let classify = |e: &TorclError| -> &str {
        match e {
            TorclError::Oom => "oom",
            TorclError::StackOverflow(_) => "stack",
            TorclError::InvalidImage(_) => "image",
            TorclError::FfiError(_) => "ffi",
            TorclError::SignalError(_) => "signal",
            TorclError::Shutdown => "shutdown",
            TorclError::Internal(_) => "internal",
            TorclError::TypeError { .. } => "type",
            TorclError::UnboundVariable(_) => "unbound",
            TorclError::UndefinedFunction(_) => "undef",
            TorclError::ArithmeticError(_) => "arith",
            TorclError::PackageError(_) => "pkg",
            TorclError::StreamError(_) => "stream",
            TorclError::FileError(_) => "file",
            TorclError::SandboxViolation(_) => "sandbox",
            _ => "unknown",
        }
    };
    assert_eq!(classify(&TorclError::Oom), "oom");
    assert_eq!(classify(&TorclError::StackOverflow(FiberId(1))), "stack");
    assert_eq!(classify(&TorclError::InvalidImage("x".into())), "image");
    assert_eq!(classify(&TorclError::FfiError("x".into())), "ffi");
    assert_eq!(classify(&TorclError::SignalError(1)), "signal");
    assert_eq!(classify(&TorclError::Shutdown), "shutdown");
    assert_eq!(classify(&TorclError::Internal("x".into())), "internal");
    assert_eq!(
        classify(&TorclError::TypeError {
            datum: TorclVal(0),
            expected: "T".into()
        }),
        "type"
    );
    assert_eq!(
        classify(&TorclError::UnboundVariable(TorclVal(0))),
        "unbound"
    );
    assert_eq!(
        classify(&TorclError::UndefinedFunction(TorclVal(0))),
        "undef"
    );
    assert_eq!(classify(&TorclError::ArithmeticError("x".into())), "arith");
    assert_eq!(classify(&TorclError::PackageError("x".into())), "pkg");
    assert_eq!(classify(&TorclError::StreamError("x".into())), "stream");
    assert_eq!(classify(&TorclError::FileError("x".into())), "file");
    assert_eq!(
        classify(&TorclError::SandboxViolation("x".into())),
        "sandbox"
    );
}

// ══════════════════════════════════════════════════════════════════
// TorclError — Result<T, TorclError> ergonomics
// ══════════════════════════════════════════════════════════════════

#[test]
fn torcl_error_in_result_ok() {
    let result: Result<i32, TorclError> = Ok(42);
    assert!(matches!(result, Ok(42)));
}

#[test]
fn torcl_error_in_result_err() {
    let result: Result<i32, TorclError> = Err(TorclError::Oom);
    assert!(matches!(result, Err(TorclError::Oom)));
}

#[test]
fn torcl_error_question_mark_propagation() {
    fn inner() -> Result<(), TorclError> {
        let _: () = Err(TorclError::Shutdown)?;
        Ok(())
    }
    let result = inner();
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "shutdown requested");
}
