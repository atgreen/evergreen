//! FORMAT and pretty-printer.
//!
//! See spec §5.9.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── FORMAT ─────────────────────────────────────────────────────────

/// Execute a FORMAT directive string. R5.40.
///
/// `destination`:
/// - NIL → returns formatted string
/// - T → writes to *standard-output*, returns NIL
/// - A stream → writes to that stream, returns NIL
/// - A string with fill pointer → destructively appends, returns NIL
pub fn format(
    destination: BlissVal,
    control_string: &str,
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    unimplemented!("format")
}

/// Compile a FORMAT control string for repeated use.
/// Returns a compiled formatter function.
pub fn formatter(control_string: &str) -> Result<BlissVal, BlissError> {
    unimplemented!("formatter")
}

// ── Pretty-printer ─────────────────────────────────────────────────

/// Begin a logical block for pretty-printing (PPRINT-LOGICAL-BLOCK). R5.41.
pub fn pprint_logical_block(
    stream: BlissVal,
    list: BlissVal,
    prefix: Option<&str>,
    per_line_prefix: Option<&str>,
    suffix: Option<&str>,
    body: BlissVal,
) -> Result<(), BlissError> {
    unimplemented!("pprint_logical_block")
}

/// Insert a conditional newline (PPRINT-NEWLINE). R5.41.
pub fn pprint_newline(kind: NewlineKind, stream: BlissVal) -> Result<(), BlissError> {
    unimplemented!("pprint_newline")
}

/// Kind of pretty-printer newline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewlineKind {
    Linear,
    Fill,
    Miser,
    Mandatory,
}

/// Adjust indentation (PPRINT-INDENT). R5.41.
pub fn pprint_indent(relative: bool, n: i32, stream: BlissVal) -> Result<(), BlissError> {
    unimplemented!("pprint_indent")
}

/// Tab (PPRINT-TAB). R5.41.
pub fn pprint_tab(
    kind: TabKind,
    colnum: u32,
    colinc: u32,
    stream: BlissVal,
) -> Result<(), BlissError> {
    unimplemented!("pprint_tab")
}

/// Kind of tab for pprint-tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Line,
    Section,
    LineRelative,
    SectionRelative,
}

// ── Pprint dispatch ────────────────────────────────────────────────

/// Get the pprint dispatch function for a type.
pub fn pprint_dispatch(object: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    unimplemented!("pprint_dispatch")
}

/// Set a pprint dispatch entry.
pub fn set_pprint_dispatch(
    type_specifier: BlissVal,
    function: Option<BlissVal>,
    priority: f64,
    table: BlissVal,
) -> Result<(), BlissError> {
    unimplemented!("set_pprint_dispatch")
}

/// Copy a pprint dispatch table.
pub fn copy_pprint_dispatch(table: Option<BlissVal>) -> Result<BlissVal, BlissError> {
    unimplemented!("copy_pprint_dispatch")
}
