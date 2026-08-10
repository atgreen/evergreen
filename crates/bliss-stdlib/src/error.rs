//! Standard library error types.
//!
//! Distinct from bliss-rt::error — these cover standard library specific
//! errors like package conflicts, stream I/O errors, pathname resolution
//! errors, FORMAT directive errors, and CLOS protocol errors.

use bliss_rt::value::BlissVal;

/// Standard library error type.
#[derive(Debug)]
pub enum StdlibError {
    /// Package name conflict (e.g., symbol already accessible).
    PackageConflict {
        package: String,
        symbol: String,
        message: String,
    },

    /// Package not found.
    PackageNotFound(String),

    /// Stream I/O error.
    StreamIoError {
        stream: String,
        message: String,
    },

    /// End of file reached unexpectedly.
    EndOfFile {
        stream: String,
    },

    /// Pathname resolution error.
    PathnameError {
        pathname: String,
        message: String,
    },

    /// Logical pathname translation error.
    LogicalPathnameError {
        host: String,
        message: String,
    },

    /// FORMAT directive error (invalid control string).
    FormatError {
        control_string: String,
        position: usize,
        message: String,
    },

    /// CLOS protocol error (e.g., slot not found, invalid class).
    ClosError {
        message: String,
    },

    /// Unbound slot access.
    UnboundSlot {
        instance: String,
        slot_name: String,
    },

    /// Method combination error.
    MethodCombinationError {
        generic_function: String,
        message: String,
    },

    /// Sequence bounds error.
    SequenceBoundsError {
        sequence_length: usize,
        index: usize,
    },

    /// Hash table error.
    HashTableError {
        message: String,
    },

    /// Sort comparison error (predicate returned invalid result).
    SortError {
        message: String,
    },

    /// Print-not-readable error.
    PrintNotReadable {
        object: String,
    },
}

impl core::fmt::Display for StdlibError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        unimplemented!("StdlibError::Display")
    }
}

impl std::error::Error for StdlibError {}

/// Convert a StdlibError into a CL condition BlissVal.
pub fn to_condition(error: &StdlibError) -> BlissVal {
    unimplemented!("to_condition")
}
