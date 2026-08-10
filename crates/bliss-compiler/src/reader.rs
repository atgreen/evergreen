//! CL Reader — converts character streams into Lisp objects.
//!
//! Implements the CLHS §2.2 reader algorithm. See spec §4.1.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

/// Reader state bundle. Holds all per-read configuration.
pub struct ReaderState {
    _private: (),
}

impl ReaderState {
    /// Create a new reader state with defaults.
    pub fn new() -> Self {
        unimplemented!("ReaderState::new")
    }

    /// Set the input stream.
    pub fn set_input(&mut self, stream: BlissVal) {
        unimplemented!("ReaderState::set_input")
    }

    /// Set the readtable.
    pub fn set_readtable(&mut self, readtable: BlissVal) {
        unimplemented!("ReaderState::set_readtable")
    }

    /// Set *read-base* (default 10).
    pub fn set_read_base(&mut self, base: u32) {
        unimplemented!("ReaderState::set_read_base")
    }

    /// Set *read-suppress*.
    pub fn set_read_suppress(&mut self, suppress: bool) {
        unimplemented!("ReaderState::set_read_suppress")
    }

    /// Set *read-eval*.
    pub fn set_read_eval(&mut self, eval: bool) {
        unimplemented!("ReaderState::set_read_eval")
    }
}

// ── Source location ────────────────────────────────────────────────

/// Source position for error reporting.
#[derive(Clone, Debug)]
pub struct SourcePos {
    pub file: Option<String>,
    pub line: u32,
    pub column: u32,
}

// ── Reader interface ───────────────────────────────────────────────

/// Read one Lisp object from the given stream using `state`.
/// Returns `EOF` marker on end-of-input.
pub fn read(state: &mut ReaderState) -> Result<BlissVal, BlissError> {
    unimplemented!("read")
}

/// Read one Lisp object from a string.
pub fn read_from_string(s: &str) -> Result<(BlissVal, usize), BlissError> {
    unimplemented!("read_from_string")
}

// ── Readtable operations ───────────────────────────────────────────

/// Character syntax type in a readtable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxType {
    Constituent,
    Whitespace,
    TerminatingMacro,
    NonTerminatingMacro,
    SingleEscape,
    MultipleEscape,
    Invalid,
}

/// Create a new readtable (default or copy of existing).
pub fn make_readtable(from: Option<BlissVal>) -> Result<BlissVal, BlissError> {
    unimplemented!("make_readtable")
}

/// Copy a readtable.
pub fn copy_readtable(from: BlissVal, to: Option<BlissVal>) -> Result<BlissVal, BlissError> {
    unimplemented!("copy_readtable")
}

/// Set a macro character in the readtable.
pub fn set_macro_character(
    readtable: BlissVal,
    ch: char,
    function: BlissVal,
    non_terminating: bool,
) -> Result<(), BlissError> {
    unimplemented!("set_macro_character")
}

/// Get the macro character function for a character.
pub fn get_macro_character(
    readtable: BlissVal,
    ch: char,
) -> Result<(Option<BlissVal>, bool), BlissError> {
    unimplemented!("get_macro_character")
}

/// Set a dispatch macro character sub-function.
pub fn set_dispatch_macro_character(
    readtable: BlissVal,
    disp_char: char,
    sub_char: char,
    function: BlissVal,
) -> Result<(), BlissError> {
    unimplemented!("set_dispatch_macro_character")
}

/// Get a dispatch macro character sub-function.
pub fn get_dispatch_macro_character(
    readtable: BlissVal,
    disp_char: char,
    sub_char: char,
) -> Result<Option<BlissVal>, BlissError> {
    unimplemented!("get_dispatch_macro_character")
}

/// Make a character a dispatch macro character.
pub fn make_dispatch_macro_character(
    readtable: BlissVal,
    ch: char,
    non_terminating: bool,
) -> Result<(), BlissError> {
    unimplemented!("make_dispatch_macro_character")
}
