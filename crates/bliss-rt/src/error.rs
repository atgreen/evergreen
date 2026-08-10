//! Runtime error types.
//!
//! All runtime-internal functions return `Result<T, BlissError>`.
//! See §2.10 of the spec.

use crate::thread::GreenThreadId;
use crate::value::BlissVal;

/// Runtime error type. D2.04.
///
/// MUST NOT be converted to `panic!` (R2.18). At the FFI boundary,
/// `BlissError` is translated into the appropriate CL condition class.
#[derive(Debug)]
#[non_exhaustive]
pub enum BlissError {
    /// Nursery + old-gen exhausted. Maps to CL `STORAGE-CONDITION`.
    Oom,

    /// CL stack overflow. Maps to CL `STORAGE-CONDITION`.
    StackOverflow(GreenThreadId),

    /// Invalid or corrupt image file. Maps to `BLISS-EXT:IMAGE-ERROR`.
    InvalidImage(String),

    /// FFI call error. Maps to `BLISS-FFI:FFI-ERROR`.
    FfiError(String),

    /// Signal handling error. Maps to `BLISS-EXT:SIGNAL-ERROR`.
    SignalError(i32),

    /// Clean shutdown requested. Not signalled as a CL condition.
    Shutdown,

    /// Invariant violation (runtime bug). Always a non-recoverable error.
    Internal(String),

    /// Type error (wrong argument type). Maps to CL `TYPE-ERROR`.
    TypeError {
        datum: BlissVal,
        expected: String,
    },

    /// Unbound variable. Maps to CL `UNBOUND-VARIABLE`.
    UnboundVariable(BlissVal),

    /// Undefined function. Maps to CL `UNDEFINED-FUNCTION`.
    UndefinedFunction(BlissVal),

    /// Arithmetic error (division by zero, overflow, etc.).
    ArithmeticError(String),

    /// Package error (conflict, not found, etc.).
    PackageError(String),

    /// Stream / I/O error. Maps to CL `STREAM-ERROR`.
    StreamError(String),

    /// File error. Maps to CL `FILE-ERROR`.
    FileError(String),

    /// Sandbox policy violation.
    SandboxViolation(String),
}

impl core::fmt::Display for BlissError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        unimplemented!("BlissError::Display")
    }
}

impl std::error::Error for BlissError {}
