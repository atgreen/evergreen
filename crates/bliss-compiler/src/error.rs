//! Compiler error types.
//!
//! Distinct from bliss-rt::error — these cover reader errors,
//! macro expansion errors, IR validation errors, and codegen errors.
//! See spec §4.10.

use crate::reader::SourcePos;

/// Compiler error type.
#[derive(Debug)]
pub enum CompilerError {
    /// Reader error — invalid syntax in source. Maps to CL `READER-ERROR`.
    ReaderError {
        message: String,
        position: Option<SourcePos>,
    },

    /// Macro expansion error. Maps to CL `PROGRAM-ERROR`.
    MacroExpansionError {
        message: String,
        /// Expansion backtrace (forms being expanded).
        backtrace: Vec<String>,
    },

    /// Circular macro expansion detected.
    CircularExpansion {
        macro_name: String,
    },

    /// IR construction or validation error. Internal — fall back to T0.
    IrError {
        message: String,
    },

    /// Optimisation pass error. Internal — abort T2, retain T1.
    OptimisationError {
        pass_name: String,
        message: String,
    },

    /// Code emission error. Internal — abort T2, retain T1.
    CodegenError {
        message: String,
    },

    /// Register allocation failure (e.g. too many live values).
    RegisterAllocationError {
        message: String,
    },
}

impl core::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        unimplemented!("CompilerError::Display")
    }
}

impl std::error::Error for CompilerError {}
