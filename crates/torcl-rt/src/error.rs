//! Runtime error types.
//!
//! All runtime-internal functions return `Result<T, TorclError>`.
//! See §2.10 of the spec.

use crate::thread::FiberId;
use crate::value::TorclVal;

/// Runtime error type. D2.04.
///
/// MUST NOT be converted to `panic!` (R2.18). At the FFI boundary,
/// `TorclError` is translated into the appropriate CL condition class.
#[derive(Debug)]
#[non_exhaustive]
pub enum TorclError {
    /// Nursery + old-gen exhausted. Maps to CL `STORAGE-CONDITION`.
    Oom,

    /// CL stack overflow. Maps to CL `STORAGE-CONDITION`.
    StackOverflow(FiberId),

    /// Invalid or corrupt image file. Maps to `TORCL-EXT:IMAGE-ERROR`.
    InvalidImage(String),

    /// FFI call error. Maps to `TORCL-FFI:FFI-ERROR`.
    FfiError(String),

    /// Signal handling error. Maps to `TORCL-EXT:SIGNAL-ERROR`.
    SignalError(i32),

    /// User interrupt (SIGINT / Ctrl-C). Maps to CL `INTERRUPT-CONDITION`.
    Interrupt,

    /// Sandbox CPU deadline exceeded. Maps to `TORCL-EXT:TIMEOUT-CONDITION`.
    Timeout,

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
    TypeError { datum: TorclVal, expected: String },

    /// Unbound variable. Maps to CL `UNBOUND-VARIABLE`.
    UnboundVariable(TorclVal),

    /// Undefined function. Maps to CL `UNDEFINED-FUNCTION`.
    UndefinedFunction(TorclVal),

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

    /// A control-flow error in the CL sense: a THROW to a tag with no matching
    /// CATCH, a GO/RETURN-FROM to a no-longer-active tag/block, etc. Maps to CL
    /// `CONTROL-ERROR`, so it is catchable — unlike `Internal`, which also
    /// carries the evaluator's live control-flow tokens and must keep propagating.
    ControlError(String),

    /// A condition-denoting raw error that has ALREADY been signalled through the
    /// live handler stack (bliss-9kc) and declined by every handler. Carries the
    /// built condition (so it stays reachable/traced) plus a pre-rendered report
    /// for display. Condition-conversion returns `None` for this variant, so an
    /// enclosing handler frame that already had its in-context turn does not run
    /// its handlers a second time. Constructed only from an already-allocated
    /// condition, so producing it allocates nothing new.
    Signalled { condition: TorclVal, report: String },
}

impl core::fmt::Display for TorclError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TorclError::Oom => write!(f, "out of memory"),
            TorclError::StackOverflow(tid) => write!(f, "stack overflow in thread {}", tid.0),
            TorclError::InvalidImage(msg) => write!(f, "invalid image: {}", msg),
            TorclError::FfiError(msg) => write!(f, "FFI error: {}", msg),
            TorclError::SignalError(sig) => write!(f, "signal error: signal {}", sig),
            TorclError::Interrupt => write!(f, "interrupt"),
            TorclError::Timeout => write!(f, "sandbox CPU timeout"),
            TorclError::Shutdown => write!(f, "shutdown requested"),
            TorclError::Internal(msg) => write!(f, "internal error: {}", msg),
            TorclError::ProgramError(msg) => write!(f, "program error: {}", msg),
            TorclError::TypeError { datum, expected } => {
                // A symbol datum prints by NAME. The Debug form is
                // "Symbol(4941)", which told a user reporting a failed
                // (asdf:load-system :babel) nothing at all about which type
                // specifier was rejected; the answer was BABEL::UNICODE-STRING.
                match crate::symbols::symbol_name_of(*datum) {
                    Some(name) => write!(f, "type error: {} is not of type {}", name, expected),
                    None => write!(f, "type error: {:?} is not of type {}", datum, expected),
                }
            }
            TorclError::UnboundVariable(sym) => {
                write!(f, "unbound variable: {}", describe_symbol(*sym))
            }
            TorclError::UndefinedFunction(sym) => {
                write!(f, "undefined function: {}", describe_symbol(*sym))
            }
            TorclError::ArithmeticError(msg) => write!(f, "arithmetic error: {}", msg),
            TorclError::PackageError(msg) => write!(f, "package error: {}", msg),
            TorclError::StreamError(msg) => write!(f, "stream error: {}", msg),
            TorclError::FileError(msg) => write!(f, "file error: {}", msg),
            TorclError::SandboxViolation(msg) => write!(f, "sandbox violation: {}", msg),
            TorclError::ControlError(msg) => write!(f, "control error: {}", msg),
            TorclError::Signalled { report, .. } => write!(f, "{}", report),
        }
    }
}

impl std::error::Error for TorclError {}

/// Render a symbol value by its name (e.g. `FIND-IF`) rather than the opaque
/// `Symbol(194)` debug form, for user-facing error messages. Falls back to the
/// debug form for a non-symbol datum or an unnamed index.
fn describe_symbol(sym: crate::value::TorclVal) -> String {
    if sym.is_symbol() {
        if let Some(name) = crate::symbols::symbol_name(sym.as_symbol_index()) {
            return name;
        }
    }
    format!("{:?}", sym)
}
