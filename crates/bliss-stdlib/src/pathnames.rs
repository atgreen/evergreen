//! Pathnames and logical pathnames.
//!
//! See spec §5.8.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── Pathname operations ────────────────────────────────────────────

/// Parse a namestring into a pathname object. R5.36.
pub fn parse_namestring(
    thing: BlissVal,
    host: Option<BlissVal>,
    default_pathname: Option<BlissVal>,
) -> Result<(BlissVal, usize), BlissError> {
    unimplemented!("parse_namestring")
}

/// Construct a pathname from components.
pub fn make_pathname(
    host: BlissVal,
    device: BlissVal,
    directory: BlissVal,
    name: BlissVal,
    type_field: BlissVal,
    version: BlissVal,
) -> Result<BlissVal, BlissError> {
    unimplemented!("make_pathname")
}

/// Merge two pathnames (CL `MERGE-PATHNAMES`). R5.38.
pub fn merge_pathnames(
    pathname: BlissVal,
    default: BlissVal,
    default_version: BlissVal,
) -> Result<BlissVal, BlissError> {
    unimplemented!("merge_pathnames")
}

/// Convert a pathname to a namestring.
pub fn namestring(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("namestring")
}

// ── Pathname component accessors ───────────────────────────────────

/// Get the host component.
pub fn pathname_host(pathname: BlissVal) -> BlissVal {
    unimplemented!("pathname_host")
}

/// Get the device component.
pub fn pathname_device(pathname: BlissVal) -> BlissVal {
    unimplemented!("pathname_device")
}

/// Get the directory component.
pub fn pathname_directory(pathname: BlissVal) -> BlissVal {
    unimplemented!("pathname_directory")
}

/// Get the name component.
pub fn pathname_name(pathname: BlissVal) -> BlissVal {
    unimplemented!("pathname_name")
}

/// Get the type component.
pub fn pathname_type(pathname: BlissVal) -> BlissVal {
    unimplemented!("pathname_type")
}

/// Get the version component.
pub fn pathname_version(pathname: BlissVal) -> BlissVal {
    unimplemented!("pathname_version")
}

// ── Pathname predicates ────────────────────────────────────────────

/// Check if two pathnames are equal.
pub fn pathname_match_p(pathname: BlissVal, wildcard: BlissVal) -> Result<bool, BlissError> {
    unimplemented!("pathname_match_p")
}

/// Check if a pathname is a wild pathname.
pub fn wild_pathname_p(pathname: BlissVal, field: Option<BlissVal>) -> bool {
    unimplemented!("wild_pathname_p")
}

// ── Logical pathnames ──────────────────────────────────────────────

/// Translate a logical pathname to a physical pathname. R5.37, R5.38.
pub fn translate_logical_pathname(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("translate_logical_pathname")
}

/// Set logical pathname translations.
pub fn set_logical_pathname_translations(
    host: &str,
    translations: BlissVal,
) -> Result<(), BlissError> {
    unimplemented!("set_logical_pathname_translations")
}

/// Get logical pathname translations.
pub fn logical_pathname_translations(host: &str) -> Result<BlissVal, BlissError> {
    unimplemented!("logical_pathname_translations")
}

// ── Filesystem operations ──────────────────────────────────────────

/// Probe whether a file exists (CL `PROBE-FILE`).
pub fn probe_file(pathname: BlissVal) -> Result<Option<BlissVal>, BlissError> {
    unimplemented!("probe_file")
}

/// Get the truename of a pathname (CL `TRUENAME`).
pub fn truename(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("truename")
}

/// List directory contents (CL `DIRECTORY`).
pub fn directory(pathname: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    unimplemented!("directory")
}

/// Ensure directories exist (CL `ENSURE-DIRECTORIES-EXIST`).
pub fn ensure_directories_exist(pathname: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    unimplemented!("ensure_directories_exist")
}

/// Delete a file (CL `DELETE-FILE`).
pub fn delete_file(pathname: BlissVal) -> Result<(), BlissError> {
    unimplemented!("delete_file")
}

/// Rename a file (CL `RENAME-FILE`).
pub fn rename_file(
    filespec: BlissVal,
    new_name: BlissVal,
) -> Result<(BlissVal, BlissVal, BlissVal), BlissError> {
    unimplemented!("rename_file")
}
