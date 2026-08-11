//! `bliss-rt` — Bliss Common Lisp runtime core.
//!
//! Provides the object model (tagged values, heap object layouts),
//! memory management (generational region-based GC), green-thread
//! scheduler, stack/frame layout, safepoint infrastructure, FFI
//! bridge, image persistence, and security sandbox.

// ── Object model ──────────────────────────────────────────────────
pub mod object;
pub mod types;
pub mod value;

// ── Memory / GC ───────────────────────────────────────────────────
pub mod gc;

// ── Thread runtime ────────────────────────────────────────────────
pub mod safepoint;
pub mod scheduler;
pub mod stack;
pub mod thread;

// ── FFI ───────────────────────────────────────────────────────────
pub mod ffi;

// ── Image persistence ─────────────────────────────────────────────
pub mod image;

// ── Security sandbox ──────────────────────────────────────────────
pub mod sandbox;

// ── Error types ───────────────────────────────────────────────────
pub mod error;

// ── Top-level entry ───────────────────────────────────────────────
pub mod runtime;

// ── Re-exports for convenience ────────────────────────────────────
pub use error::BlissError;
pub use object::ObjectHeader;
pub use value::BlissVal;
