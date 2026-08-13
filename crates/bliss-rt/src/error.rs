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

    /// A program error in the CL sense (malformed call: wrong argument count,
    /// destructuring mismatch, …). Maps to CL `PROGRAM-ERROR`, so it is a
    /// catchable condition — unlike `Internal`, which also carries the
    /// evaluator's control-flow tokens and must keep propagating.
    ProgramError(String),

    /// Type error (wrong argument type). Maps to CL `TYPE-ERROR`.
    TypeError { datum: BlissVal, expected: String },

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
        match self {
            BlissError::Oom => write!(f, "out of memory"),
            BlissError::StackOverflow(tid) => write!(f, "stack overflow in thread {}", tid.0),
            BlissError::InvalidImage(msg) => write!(f, "invalid image: {}", msg),
            BlissError::FfiError(msg) => write!(f, "FFI error: {}", msg),
            BlissError::SignalError(sig) => write!(f, "signal error: signal {}", sig),
            BlissError::Shutdown => write!(f, "shutdown requested"),
            BlissError::Internal(msg) => write!(f, "internal error: {}", msg),
            BlissError::ProgramError(msg) => write!(f, "program error: {}", msg),
            BlissError::TypeError { datum, expected } => {
                write!(f, "type error: {:?} is not of type {}", datum, expected)
            }
            BlissError::UnboundVariable(sym) => {
                write!(f, "unbound variable: {:?}", sym)
            }
            BlissError::UndefinedFunction(sym) => {
                write!(f, "undefined function: {:?}", sym)
            }
            BlissError::ArithmeticError(msg) => write!(f, "arithmetic error: {}", msg),
            BlissError::PackageError(msg) => write!(f, "package error: {}", msg),
            BlissError::StreamError(msg) => write!(f, "stream error: {}", msg),
            BlissError::FileError(msg) => write!(f, "file error: {}", msg),
            BlissError::SandboxViolation(msg) => write!(f, "sandbox violation: {}", msg),
        }
    }
}

impl std::error::Error for BlissError {}
