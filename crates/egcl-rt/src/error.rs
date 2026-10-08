// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Runtime error types.
//!
//! All runtime-internal functions return `Result<T, EgclError>`.
//! See §2.10 of the spec.

use crate::thread::FiberId;
use crate::value::EgclVal;

/// Runtime error type. D2.04.
///
/// MUST NOT be converted to `panic!` (R2.18). At the FFI boundary,
/// `EgclError` is translated into the appropriate CL condition class.
#[derive(Debug)]
#[non_exhaustive]
pub enum EgclError {
    /// Nursery + old-gen exhausted. Maps to CL `STORAGE-CONDITION`.
    Oom,

    /// CL stack overflow. Maps to CL `STORAGE-CONDITION`.
    StackOverflow(FiberId),

    /// Invalid or corrupt image file. Maps to `EGCL-EXT:IMAGE-ERROR`.
    InvalidImage(String),

    /// FFI call error. Maps to `EGCL-FFI:FFI-ERROR`.
    FfiError(String),

    /// Signal handling error. Maps to `EGCL-EXT:SIGNAL-ERROR`.
    SignalError(i32),

    /// User interrupt (SIGINT / Ctrl-C). Maps to CL `INTERRUPT-CONDITION`.
    Interrupt,

    /// Sandbox CPU deadline exceeded. Maps to `EGCL-EXT:TIMEOUT-CONDITION`.
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
    TypeError { datum: EgclVal, expected: String },

    /// Unbound variable. Maps to CL `UNBOUND-VARIABLE`.
    UnboundVariable(EgclVal),

    /// Undefined function. Maps to CL `UNDEFINED-FUNCTION`.
    UndefinedFunction(EgclVal),

    /// Arithmetic error (division by zero, overflow, etc.).
    ArithmeticError(String),

    /// Package error (conflict, not found, etc.).
    PackageError(String),

    /// Stream / I/O error. Maps to CL `STREAM-ERROR`.
    StreamError(String),

    /// A stream I/O deadline expired. Maps to `EGCL-EXT:IO-TIMEOUT`.
    IoTimeout { stream: EgclVal, message: String },

    /// File error. Maps to CL `FILE-ERROR`.
    FileError(String),

    /// Sandbox policy violation.
    SandboxViolation(String),

    /// A control-flow error in the CL sense: a THROW to a tag with no matching
    /// CATCH, a GO/RETURN-FROM to a no-longer-active tag/block, etc. Maps to CL
    /// `CONTROL-ERROR`, so it is catchable — unlike `Internal`, which also
    /// carries the evaluator's live control-flow tokens and must keep propagating.
    ControlError(String),

    /// Python raised. Maps to `PY:ERROR`, carrying the exception's type, message,
    /// Python frames and the exception object itself.
    ///
    /// A variant rather than an `FfiError` string because a Python failure is a
    /// first-class condition with structure a caller can inspect — `(py:error-kind
    /// e)`, `(py:error-frames e)` — and because the mixed-language backtrace needs
    /// the frames, which a formatted string has already thrown away. Gated on the
    /// feature that provides the payload; the enum is `#[non_exhaustive]`, so a
    /// build without it simply has one fewer variant.
    #[cfg(feature = "python")]
    PythonRaised(Box<crate::python::Raise>),

    /// A condition already signalled through the live handler stack and declined
    /// by every handler. Box the cold details so historical snapshots do not
    /// enlarge ordinary runtime Result values (bliss-cf672.7.1).
    Signalled(Box<SignalledError>),

    /// A raw failure with a historical call chain, not yet signalled. Native
    /// helpers can capture this without allocating a Lisp condition at an
    /// unpublished safepoint. Keep the cold payload boxed to preserve Result size.
    Traced(Box<TracedError>),
}

#[derive(Debug)]
pub struct TracedError {
    pub error: EgclError,
    pub backtrace: Vec<crate::debug_stack::LogicalFrame>,
}

/// Owned details for an already-signalled condition. Condition conversion skips
/// these errors so enclosing handlers do not run a second time. Construction
/// allocates on the Rust heap only; it cannot trigger Lisp collection.
#[derive(Debug)]
pub struct SignalledError {
    pub condition: EgclVal,
    pub report: String,
    /// Historical calls captured before signalling/unwinding. Values are traced
    /// with the error; these are not inspectable live frames.
    pub backtrace: Vec<crate::debug_stack::LogicalFrame>,
}

impl EgclError {
    /// Retain the first failure's current call chain using Rust allocation only.
    /// Control transfers and storage exhaustion keep their no-capture paths.
    /// Root the returned error across any subsequent Lisp allocation/safepoint.
    pub fn capture_backtrace(self) -> Self {
        if matches!(
            self,
            Self::Oom
                | Self::StackOverflow(_)
                | Self::Shutdown
                | Self::Internal(_)
                | Self::Signalled(_)
                | Self::Traced(_)
        ) {
            return self;
        }
        let backtrace = crate::debug_stack::capture_current(usize::MAX);
        Self::Traced(Box::new(TracedError {
            error: self,
            backtrace,
        }))
    }

    pub fn without_backtrace(&self) -> &Self {
        match self {
            Self::Traced(details) => details.error.without_backtrace(),
            _ => self,
        }
    }

    pub fn backtrace(&self) -> Option<&[crate::debug_stack::LogicalFrame]> {
        match self {
            Self::Traced(details) => Some(&details.backtrace),
            Self::Signalled(details) => Some(&details.backtrace),
            _ => None,
        }
    }

    pub fn signalled(
        condition: EgclVal,
        report: String,
        backtrace: Vec<crate::debug_stack::LogicalFrame>,
    ) -> Self {
        Self::Signalled(Box::new(SignalledError {
            condition,
            report,
            backtrace,
        }))
    }
}

impl core::fmt::Display for EgclError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EgclError::Oom => write!(f, "out of memory"),
            EgclError::StackOverflow(tid) => write!(f, "stack overflow in thread {}", tid.0),
            EgclError::InvalidImage(msg) => write!(f, "invalid image: {}", msg),
            EgclError::FfiError(msg) => write!(f, "FFI error: {}", msg),
            EgclError::SignalError(sig) => write!(f, "signal error: signal {}", sig),
            EgclError::Interrupt => write!(f, "interrupt"),
            EgclError::Timeout => write!(f, "sandbox CPU timeout"),
            EgclError::Shutdown => write!(f, "shutdown requested"),
            EgclError::Internal(msg) => write!(f, "internal error: {}", msg),
            EgclError::ProgramError(msg) => write!(f, "program error: {}", msg),
            EgclError::TypeError { datum, expected } => {
                // A symbol datum prints by NAME. The Debug form is
                // "Symbol(4941)", which told a user reporting a failed
                // (asdf:load-system :babel) nothing at all about which type
                // specifier was rejected; the answer was BABEL::UNICODE-STRING.
                match crate::symbols::symbol_name_of(*datum) {
                    Some(name) => write!(f, "type error: {} is not of type {}", name, expected),
                    None => write!(f, "type error: {:?} is not of type {}", datum, expected),
                }
            }
            EgclError::UnboundVariable(sym) => {
                write!(f, "unbound variable: {}", describe_symbol(*sym))
            }
            EgclError::UndefinedFunction(sym) => {
                write!(f, "undefined function: {}", describe_symbol(*sym))
            }
            EgclError::ArithmeticError(msg) => write!(f, "arithmetic error: {}", msg),
            EgclError::PackageError(msg) => write!(f, "package error: {}", msg),
            EgclError::StreamError(msg) => write!(f, "stream error: {}", msg),
            EgclError::IoTimeout { message, .. } => write!(f, "I/O timeout: {message}"),
            EgclError::FileError(msg) => write!(f, "file error: {}", msg),
            EgclError::SandboxViolation(msg) => write!(f, "sandbox violation: {}", msg),
            EgclError::ControlError(msg) => write!(f, "control error: {}", msg),
            #[cfg(feature = "python")]
            EgclError::PythonRaised(raise) => {
                // The frames belong here too: this is the form an UNCAUGHT Python
                // error takes on the way out, which is exactly where a reader wants
                // to see where in Python it happened.
                write!(f, "{}: {}", raise.kind, raise.message)?;
                let frames = raise.render_frames();
                if !frames.is_empty() {
                    write!(f, "\n{}", frames.trim_end())?;
                }
                Ok(())
            }
            EgclError::Signalled(details) => write!(f, "{}", details.report),
            EgclError::Traced(details) => details.error.fmt(f),
        }
    }
}

impl std::error::Error for EgclError {}

/// Render a symbol value by its name (e.g. `FIND-IF`) rather than the opaque
/// `Symbol(194)` debug form, for user-facing error messages. Falls back to the
/// debug form for a non-symbol datum or an unnamed index.
fn describe_symbol(sym: crate::value::EgclVal) -> String {
    if sym.is_symbol() {
        if let Some(name) = crate::symbols::symbol_name(sym.as_symbol_index()) {
            return name;
        }
    }
    format!("{:?}", sym)
}
