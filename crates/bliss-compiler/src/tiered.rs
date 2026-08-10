//! Tiered compilation — T0 interpreter, T1 baseline, T2 optimising.
//!
//! See spec §4.4.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

/// Compilation tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    /// T0 — Tree-walk interpreter.
    Interpreter = 0,
    /// T1 — Baseline compiler (single-pass, no IR).
    Baseline = 1,
    /// T2 — Optimising compiler (sea-of-nodes IR, full passes).
    Optimising = 2,
}

/// Tiered compilation configuration.
#[derive(Clone, Debug)]
pub struct TierConfig {
    /// Invocation count to trigger T0→T1 promotion (default: 10).
    pub t1_threshold: u32,
    /// Invocation count to trigger T1→T2 promotion (default: 5000).
    pub t2_threshold: u32,
    /// Back-edge count to trigger OSR entry (default: 10000).
    pub osr_threshold: u32,
    /// Number of background compilation threads.
    pub compile_threads: u32,
}

// ── T0 interpreter ─────────────────────────────────────────────────

/// T0 tree-walk interpreter.
pub struct Interpreter {
    _private: (),
}

impl Interpreter {
    /// Create a new interpreter instance.
    pub fn new() -> Self {
        unimplemented!("Interpreter::new")
    }

    /// Evaluate a CL form in the given environment.
    pub fn eval(&mut self, form: BlissVal) -> Result<BlissVal, BlissError> {
        unimplemented!("Interpreter::eval")
    }

    /// Apply a function to arguments.
    pub fn apply(&mut self, function: BlissVal, args: BlissVal) -> Result<BlissVal, BlissError> {
        unimplemented!("Interpreter::apply")
    }
}

// ── T1 baseline compiler ──────────────────────────────────────────

/// T1 baseline compiler — single-pass AST→native code.
pub struct BaselineCompiler {
    _private: (),
}

impl BaselineCompiler {
    /// Create a new baseline compiler.
    pub fn new() -> Self {
        unimplemented!("BaselineCompiler::new")
    }

    /// Compile a function to baseline native code.
    /// Target latency: < 1 ms for ≤200 AST nodes.
    pub fn compile(&mut self, function: BlissVal) -> Result<CompiledCode, BlissError> {
        unimplemented!("BaselineCompiler::compile")
    }
}

// ── T2 optimising compiler ─────────────────────────────────────────

/// T2 optimising compiler — builds IR, runs passes, emits optimised code.
pub struct OptimisingCompiler {
    _private: (),
}

impl OptimisingCompiler {
    /// Create a new optimising compiler.
    pub fn new() -> Self {
        unimplemented!("OptimisingCompiler::new")
    }

    /// Compile a function with full optimisation.
    /// Runs on a background thread (R4.29).
    pub fn compile(&mut self, function: BlissVal) -> Result<CompiledCode, BlissError> {
        unimplemented!("OptimisingCompiler::compile")
    }
}

// ── Compiled code handle ───────────────────────────────────────────

/// Handle to compiled native code ready for installation.
pub struct CompiledCode {
    _private: (),
}

impl CompiledCode {
    /// Entry point address of the compiled code.
    pub fn entry_point(&self) -> *const u8 {
        unimplemented!("CompiledCode::entry_point")
    }

    /// Size of the native code in bytes.
    pub fn code_size(&self) -> usize {
        unimplemented!("CompiledCode::code_size")
    }

    /// The tier this code was compiled at.
    pub fn tier(&self) -> Tier {
        unimplemented!("CompiledCode::tier")
    }

    /// Install this compiled code into a function object atomically.
    pub fn install(self, function: BlissVal) -> Result<(), BlissError> {
        unimplemented!("CompiledCode::install")
    }
}

// ── Tier promotion ─────────────────────────────────────────────────

/// Check if a function should be promoted to a higher tier.
pub fn check_promotion(function: BlissVal, config: &TierConfig) -> Option<Tier> {
    unimplemented!("check_promotion")
}

/// Request background compilation of a function at the given tier.
pub fn request_compilation(function: BlissVal, target_tier: Tier) -> Result<(), BlissError> {
    unimplemented!("request_compilation")
}
