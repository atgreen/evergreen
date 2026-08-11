//! `bliss-compiler` — Bliss Common Lisp compiler pipeline.
//!
//! Reader → macro-expansion → sea-of-nodes IR → tiered compilation
//! (T0 interpreter, T1 baseline, T2 optimising) → register allocation
//! → code emission → inline caches → profiling → OSR/deopt.

// ── Front-end ─────────────────────────────────────────────────────
pub mod macroexpand;
pub mod reader;

// ── Intermediate representation ───────────────────────────────────
pub mod ir;

// ── Tiered compilation ────────────────────────────────────────────
pub mod tiered;

// ── Optimisation passes ───────────────────────────────────────────
pub mod opt;

// ── On-stack replacement / deoptimisation ─────────────────────────
pub mod osr;

// ── Code generation ───────────────────────────────────────────────
pub mod codegen;

// ── Inline caches ─────────────────────────────────────────────────
pub mod ic;

// ── Profiling infrastructure ──────────────────────────────────────
pub mod profiling;

// ── Error types ──────────────────────────────────────────────────
pub mod error;

// ── Re-exports for convenience ────────────────────────────────────
pub use error::CompilerError;
pub use ir::IrGraph;
pub use reader::ReaderState;
pub use tiered::{CompiledCode, Tier};
