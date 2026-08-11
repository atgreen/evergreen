//! `bliss-stdlib` — Bliss Common Lisp standard library.
//!
//! Packages & bootstrap, CLOS, condition system, Gray streams,
//! generic sequences, hash tables, FORMAT / pretty-printer,
//! pathnames, and developer tools (REPL, debugger, profiler, SWANK).

// ── Package system & bootstrap ────────────────────────────────────
pub mod packages;

// ── CLOS ──────────────────────────────────────────────────────────
pub mod clos;

// ── Condition system ──────────────────────────────────────────────
pub mod conditions;

// ── Streams ───────────────────────────────────────────────────────
pub mod streams;

// ── Sequences & hash tables ───────────────────────────────────────
pub mod hashtable;
pub mod sequences;

// ── FORMAT & pretty-printer ───────────────────────────────────────
pub mod format;

// ── Pathnames ─────────────────────────────────────────────────────
pub mod pathnames;

// ── Developer tools ───────────────────────────────────────────────
pub mod devtools;

// ── Error types ──────────────────────────────────────────────────
pub mod error;

// ── Re-exports for convenience ────────────────────────────────────
pub use error::StdlibError;
pub use packages::PackageRegistry;
pub use streams::GrayStream;
