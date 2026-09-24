//! Standard library error types.
//!
//! Distinct from torcl-rt::error — these cover standard library specific
//! errors like package conflicts, stream I/O errors, pathname resolution
//! errors, FORMAT directive errors, and CLOS protocol errors.

use torcl_rt::value::TorclVal;

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
    StreamIoError { stream: String, message: String },

    /// End of file reached unexpectedly.
    EndOfFile { stream: String },

    /// Pathname resolution error.
    PathnameError { pathname: String, message: String },

    /// Logical pathname translation error.
    LogicalPathnameError { host: String, message: String },

    /// FORMAT directive error (invalid control string).
    FormatError {
        control_string: String,
        position: usize,
        message: String,
    },

    /// CLOS protocol error (e.g., slot not found, invalid class).
    ClosError { message: String },

    /// Unbound slot access.
    UnboundSlot { instance: String, slot_name: String },

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
    HashTableError { message: String },

    /// Sort comparison error (predicate returned invalid result).
    SortError { message: String },

    /// Print-not-readable error.
    PrintNotReadable { object: String },
}

impl core::fmt::Display for StdlibError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StdlibError::PackageConflict {
                package,
                symbol,
                message,
            } => {
                write!(
                    f,
                    "Package conflict in {}: symbol {} — {}",
                    package, symbol, message
                )
            }
            StdlibError::PackageNotFound(name) => {
                write!(f, "Package not found: {}", name)
            }
            StdlibError::StreamIoError { stream, message } => {
                write!(f, "Stream I/O error on {}: {}", stream, message)
            }
            StdlibError::EndOfFile { stream } => {
                write!(f, "End of file on stream {}", stream)
            }
            StdlibError::PathnameError { pathname, message } => {
                write!(f, "Pathname error for {}: {}", pathname, message)
            }
            StdlibError::LogicalPathnameError { host, message } => {
                write!(f, "Logical pathname error for host {}: {}", host, message)
            }
            StdlibError::FormatError {
                control_string,
                position,
                message,
            } => {
                write!(
                    f,
                    "FORMAT error in {:?} at position {}: {}",
                    control_string, position, message
                )
            }
            StdlibError::ClosError { message } => {
                write!(f, "CLOS error: {}", message)
            }
            StdlibError::UnboundSlot {
                instance,
                slot_name,
            } => {
                write!(f, "Unbound slot {} in instance {}", slot_name, instance)
            }
            StdlibError::MethodCombinationError {
                generic_function,
                message,
            } => {
                write!(
                    f,
                    "Method combination error in {}: {}",
                    generic_function, message
                )
            }
            StdlibError::SequenceBoundsError {
                sequence_length,
                index,
            } => {
                write!(
                    f,
                    "Sequence index {} out of bounds for length {}",
                    index, sequence_length
                )
            }
            StdlibError::HashTableError { message } => {
                write!(f, "Hash table error: {}", message)
            }
            StdlibError::SortError { message } => {
                write!(f, "Sort error: {}", message)
            }
            StdlibError::PrintNotReadable { object } => {
                write!(f, "Object not readable: {}", object)
            }
        }
    }
}

impl std::error::Error for StdlibError {}

/// Convert a StdlibError into a CL condition TorclVal.
///
/// Each variant is mapped to a unique fixnum discriminant so that the
/// condition system can dispatch on it. The discriminants are:
///   1  PackageConflict
///   2  PackageNotFound
///   3  StreamIoError
///   4  EndOfFile
///   5  PathnameError
///   6  LogicalPathnameError
///   7  FormatError
///   8  ClosError
///   9  UnboundSlot
///  10  MethodCombinationError
///  11  SequenceBoundsError
///  12  HashTableError
///  13  SortError
///  14  PrintNotReadable
pub fn to_condition(error: &StdlibError) -> TorclVal {
    let discriminant: i64 = match error {
        StdlibError::PackageConflict { .. } => 1,
        StdlibError::PackageNotFound(_) => 2,
        StdlibError::StreamIoError { .. } => 3,
        StdlibError::EndOfFile { .. } => 4,
        StdlibError::PathnameError { .. } => 5,
        StdlibError::LogicalPathnameError { .. } => 6,
        StdlibError::FormatError { .. } => 7,
        StdlibError::ClosError { .. } => 8,
        StdlibError::UnboundSlot { .. } => 9,
        StdlibError::MethodCombinationError { .. } => 10,
        StdlibError::SequenceBoundsError { .. } => 11,
        StdlibError::HashTableError { .. } => 12,
        StdlibError::SortError { .. } => 13,
        StdlibError::PrintNotReadable { .. } => 14,
    };
    TorclVal::from_fixnum(discriminant)
}
