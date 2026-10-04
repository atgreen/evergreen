// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tests for egcl-rt error types: EgclError enum.
//!
//! Covers every variant's construction and Display output,
//! std::error::Error trait implementation, Debug output,
//! Send/Sync bounds, and non_exhaustive match routing.

use egcl_rt::error::EgclError;
use egcl_rt::thread::FiberId;
use egcl_rt::value::EgclVal;

// ══════════════════════════════════════════════════════════════════
// EgclError — Construction of every variant
// ══════════════════════════════════════════════════════════════════

#[test]
fn construct_all_variants() {
    // Verify every variant can be constructed without panic
    let _errs: Vec<EgclError> = vec![
        EgclError::Oom,
        EgclError::StackOverflow(FiberId(42)),
        EgclError::InvalidImage("corrupt header".into()),
        EgclError::FfiError("dlopen failed".into()),
        EgclError::SignalError(11),
        EgclError::Shutdown,
        EgclError::Internal("invariant broken".into()),
        EgclError::TypeError {
            datum: EgclVal(0),
            expected: "INTEGER".into(),
        },
        EgclError::UnboundVariable(EgclVal(0)),
        EgclError::UndefinedFunction(EgclVal(0)),
        EgclError::ArithmeticError("division by zero".into()),
        EgclError::PackageError("package not found".into()),
        EgclError::StreamError("stream closed".into()),
        EgclError::FileError("/no/such/file".into()),
        EgclError::SandboxViolation("network access denied".into()),
    ];
    assert_eq!(_errs.len(), 15);
}

// ══════════════════════════════════════════════════════════════════
// EgclError — Display output for every variant
// ══════════════════════════════════════════════════════════════════

#[test]
fn display_oom() {
    let err = EgclError::Oom;
    assert_eq!(err.to_string(), "out of memory");
}

#[test]
fn display_stack_overflow() {
    let err = EgclError::StackOverflow(FiberId(7));
    assert_eq!(err.to_string(), "stack overflow in thread 7");
}

#[test]
fn display_stack_overflow_edge_ids() {
    assert_eq!(
        EgclError::StackOverflow(FiberId(0)).to_string(),
        "stack overflow in thread 0"
    );
    let large = EgclError::StackOverflow(FiberId(u64::MAX)).to_string();
    assert!(large.contains(&u64::MAX.to_string()));
}

#[test]
fn display_invalid_image() {
    assert_eq!(
        EgclError::InvalidImage("bad magic".into()).to_string(),
        "invalid image: bad magic"
    );
    assert_eq!(
        EgclError::InvalidImage(String::new()).to_string(),
        "invalid image: "
    );
}

#[test]
fn display_ffi_error() {
    let err = EgclError::FfiError("symbol not found".to_string());
    assert_eq!(err.to_string(), "FFI error: symbol not found");
}

#[test]
fn display_signal_error() {
    let err = EgclError::SignalError(9);
    assert_eq!(err.to_string(), "signal error: signal 9");
}

#[test]
fn display_signal_error_negative() {
    let err = EgclError::SignalError(-1);
    assert_eq!(err.to_string(), "signal error: signal -1");
}

#[test]
fn display_shutdown() {
    let err = EgclError::Shutdown;
    assert_eq!(err.to_string(), "shutdown requested");
}

#[test]
fn display_internal() {
    let err = EgclError::Internal("null pointer".to_string());
    assert_eq!(err.to_string(), "internal error: null pointer");
}

#[test]
fn display_type_error() {
    let datum = EgclVal(42);
    let err = EgclError::TypeError {
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
    let sym = EgclVal(100);
    let err = EgclError::UnboundVariable(sym);
    let msg = err.to_string();
    assert!(msg.starts_with("unbound variable: "), "got: {}", msg);
}

#[test]
fn display_undefined_function() {
    let sym = EgclVal(200);
    let err = EgclError::UndefinedFunction(sym);
    let msg = err.to_string();
    assert!(msg.starts_with("undefined function: "), "got: {}", msg);
}

#[test]
fn display_arithmetic_error() {
    let err = EgclError::ArithmeticError("division by zero".to_string());
    assert_eq!(err.to_string(), "arithmetic error: division by zero");
}

#[test]
fn display_package_error() {
    let err = EgclError::PackageError("CL-USER not found".to_string());
    assert_eq!(err.to_string(), "package error: CL-USER not found");
}

#[test]
fn display_stream_error() {
    let err = EgclError::StreamError("read past EOF".to_string());
    assert_eq!(err.to_string(), "stream error: read past EOF");
}

#[test]
fn display_file_error() {
    let err = EgclError::FileError("permission denied".to_string());
    assert_eq!(err.to_string(), "file error: permission denied");
}

#[test]
fn display_sandbox_violation() {
    let err = EgclError::SandboxViolation("filesystem write blocked".to_string());
    assert_eq!(
        err.to_string(),
        "sandbox violation: filesystem write blocked"
    );
}

// ══════════════════════════════════════════════════════════════════
// EgclError — std::error::Error trait
// ══════════════════════════════════════════════════════════════════

#[test]
fn egcl_error_implements_std_error() {
    // Verify that EgclError can be used as a dyn std::error::Error.
    let err: Box<dyn std::error::Error> = Box::new(EgclError::Oom);
    // Display should work through the trait object
    assert_eq!(err.to_string(), "out of memory");
}

#[test]
fn egcl_error_source_is_none() {
    // Default Error impl returns None for source()
    let err = EgclError::Internal("bug".to_string());
    let as_error: &dyn std::error::Error = &err;
    assert!(as_error.source().is_none());
}

#[test]
fn egcl_error_all_variants_as_dyn_error() {
    // Every variant must be usable as dyn Error
    let errors: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(EgclError::Oom),
        Box::new(EgclError::StackOverflow(FiberId(1))),
        Box::new(EgclError::InvalidImage("x".into())),
        Box::new(EgclError::FfiError("x".into())),
        Box::new(EgclError::SignalError(2)),
        Box::new(EgclError::Shutdown),
        Box::new(EgclError::Internal("x".into())),
        Box::new(EgclError::TypeError {
            datum: EgclVal(0),
            expected: "T".into(),
        }),
        Box::new(EgclError::UnboundVariable(EgclVal(0))),
        Box::new(EgclError::UndefinedFunction(EgclVal(0))),
        Box::new(EgclError::ArithmeticError("x".into())),
        Box::new(EgclError::PackageError("x".into())),
        Box::new(EgclError::StreamError("x".into())),
        Box::new(EgclError::FileError("x".into())),
        Box::new(EgclError::SandboxViolation("x".into())),
    ];
    for err in &errors {
        // Each should produce a non-empty Display string
        assert!(!err.to_string().is_empty());
        // source() defaults to None
        assert!(err.source().is_none());
    }
}

// ══════════════════════════════════════════════════════════════════
// EgclError — Debug output
// ══════════════════════════════════════════════════════════════════

#[test]
fn debug_unit_variants() {
    let dbg_oom = format!("{:?}", EgclError::Oom);
    assert!(dbg_oom.contains("Oom"), "got: {}", dbg_oom);

    let dbg_shutdown = format!("{:?}", EgclError::Shutdown);
    assert!(dbg_shutdown.contains("Shutdown"), "got: {}", dbg_shutdown);
}

#[test]
fn debug_stack_overflow_contains_thread_id() {
    let dbg = format!("{:?}", EgclError::StackOverflow(FiberId(99)));
    assert!(dbg.contains("StackOverflow"), "got: {}", dbg);
    assert!(dbg.contains("99"), "got: {}", dbg);
}

#[test]
fn debug_signal_error_contains_signal_number() {
    let dbg = format!("{:?}", EgclError::SignalError(15));
    assert!(dbg.contains("SignalError"), "got: {}", dbg);
    assert!(dbg.contains("15"), "got: {}", dbg);
}

#[test]
fn debug_type_error_contains_fields() {
    let dbg = format!(
        "{:?}",
        EgclError::TypeError {
            datum: EgclVal(5),
            expected: "NUMBER".to_string(),
        }
    );
    assert!(dbg.contains("TypeError"), "got: {}", dbg);
    assert!(dbg.contains("NUMBER"), "got: {}", dbg);
}

#[test]
fn debug_all_string_variants() {
    let cases: Vec<(EgclError, &str, &str)> = vec![
        (
            EgclError::ArithmeticError("overflow".into()),
            "ArithmeticError",
            "overflow",
        ),
        (
            EgclError::PackageError("conflict".into()),
            "PackageError",
            "conflict",
        ),
        (
            EgclError::StreamError("closed".into()),
            "StreamError",
            "closed",
        ),
        (
            EgclError::FileError("not found".into()),
            "FileError",
            "not found",
        ),
        (
            EgclError::SandboxViolation("blocked".into()),
            "SandboxViolation",
            "blocked",
        ),
        (
            EgclError::InvalidImage("truncated".into()),
            "InvalidImage",
            "truncated",
        ),
        (
            EgclError::FfiError("abi mismatch".into()),
            "FfiError",
            "abi mismatch",
        ),
        (
            EgclError::Internal("null ref".into()),
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
// EgclError — Send + Sync (required for cross-thread error propagation)
// ══════════════════════════════════════════════════════════════════

#[test]
fn egcl_error_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EgclError>();
}

// ══════════════════════════════════════════════════════════════════
// EgclError — match routing with wildcard arm
// ══════════════════════════════════════════════════════════════════
// NOTE: EgclError is #[non_exhaustive], which means external crates
// MUST include a wildcard arm in match expressions. A true compile-time
// test of that guarantee requires a compile-fail harness (e.g. trybuild).
// The tests below only verify that match routing with a wildcard arm
// works correctly for each variant — they do not verify that omitting
// the wildcard causes a compile error.

#[test]
fn match_routing_with_wildcard_arm() {
    // Verify each variant routes to the correct arm when a wildcard is present.
    let classify = |e: &EgclError| -> &str {
        match e {
            EgclError::Oom => "oom",
            EgclError::StackOverflow(_) => "stack",
            EgclError::InvalidImage(_) => "image",
            EgclError::FfiError(_) => "ffi",
            EgclError::SignalError(_) => "signal",
            EgclError::Shutdown => "shutdown",
            EgclError::Internal(_) => "internal",
            EgclError::TypeError { .. } => "type",
            EgclError::UnboundVariable(_) => "unbound",
            EgclError::UndefinedFunction(_) => "undef",
            EgclError::ArithmeticError(_) => "arith",
            EgclError::PackageError(_) => "pkg",
            EgclError::StreamError(_) => "stream",
            EgclError::FileError(_) => "file",
            EgclError::SandboxViolation(_) => "sandbox",
            _ => "unknown",
        }
    };
    assert_eq!(classify(&EgclError::Oom), "oom");
    assert_eq!(classify(&EgclError::StackOverflow(FiberId(1))), "stack");
    assert_eq!(classify(&EgclError::InvalidImage("x".into())), "image");
    assert_eq!(classify(&EgclError::FfiError("x".into())), "ffi");
    assert_eq!(classify(&EgclError::SignalError(1)), "signal");
    assert_eq!(classify(&EgclError::Shutdown), "shutdown");
    assert_eq!(classify(&EgclError::Internal("x".into())), "internal");
    assert_eq!(
        classify(&EgclError::TypeError {
            datum: EgclVal(0),
            expected: "T".into()
        }),
        "type"
    );
    assert_eq!(
        classify(&EgclError::UnboundVariable(EgclVal(0))),
        "unbound"
    );
    assert_eq!(
        classify(&EgclError::UndefinedFunction(EgclVal(0))),
        "undef"
    );
    assert_eq!(classify(&EgclError::ArithmeticError("x".into())), "arith");
    assert_eq!(classify(&EgclError::PackageError("x".into())), "pkg");
    assert_eq!(classify(&EgclError::StreamError("x".into())), "stream");
    assert_eq!(classify(&EgclError::FileError("x".into())), "file");
    assert_eq!(
        classify(&EgclError::SandboxViolation("x".into())),
        "sandbox"
    );
}

// ══════════════════════════════════════════════════════════════════
// EgclError — Result<T, EgclError> ergonomics
// ══════════════════════════════════════════════════════════════════

#[test]
fn egcl_error_in_result_ok() {
    let result: Result<i32, EgclError> = Ok(42);
    assert!(matches!(result, Ok(42)));
}

#[test]
fn egcl_error_in_result_err() {
    let result: Result<i32, EgclError> = Err(EgclError::Oom);
    assert!(matches!(result, Err(EgclError::Oom)));
}

#[test]
fn egcl_error_question_mark_propagation() {
    fn inner() -> Result<(), EgclError> {
        let _: () = Err(EgclError::Shutdown)?;
        Ok(())
    }
    let result = inner();
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "shutdown requested");
}

#[test]
#[cfg(target_pointer_width = "64")]
fn historical_error_details_do_not_enlarge_ordinary_results() {
    // The pre-backtrace error/result representation occupied five words.
    // Cold diagnostic payloads must not widen every successful runtime return.
    assert!(std::mem::size_of::<EgclError>() <= 40);
    assert!(std::mem::size_of::<Result<EgclVal, EgclError>>() <= 40);
}
