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
        match self {
            CompilerError::ReaderError { message, position } => {
                if let Some(pos) = position {
                    if let Some(ref file) = pos.file {
                        write!(f, "reader error at {}:{}:{}: {}", file, pos.line, pos.column, message)
                    } else {
                        write!(f, "reader error at {}:{}: {}", pos.line, pos.column, message)
                    }
                } else {
                    write!(f, "reader error: {}", message)
                }
            }
            CompilerError::MacroExpansionError { message, backtrace } => {
                write!(f, "macro expansion error: {}", message)?;
                for frame in backtrace {
                    write!(f, "\n  in: {}", frame)?;
                }
                Ok(())
            }
            CompilerError::CircularExpansion { macro_name } => {
                write!(f, "circular macro expansion detected for {}", macro_name)
            }
            CompilerError::IrError { message } => {
                write!(f, "IR error: {}", message)
            }
            CompilerError::OptimisationError { pass_name, message } => {
                write!(f, "optimisation error in pass '{}': {}", pass_name, message)
            }
            CompilerError::CodegenError { message } => {
                write!(f, "codegen error: {}", message)
            }
            CompilerError::RegisterAllocationError { message } => {
                write!(f, "register allocation error: {}", message)
            }
        }
    }
}

impl std::error::Error for CompilerError {}
