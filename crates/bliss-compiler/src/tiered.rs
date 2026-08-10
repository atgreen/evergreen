//! Tiered compilation — T0 interpreter, T1 baseline, T2 optimising.
//!
//! See spec §4.4.

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, TAG_FUNCTION, TAG_SPECIAL, NIL_BITS, T_BITS};

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
        Interpreter { _private: () }
    }

    /// Evaluate a CL form in the given environment.
    ///
    /// Self-evaluating forms (NIL, T, fixnums, characters, strings,
    /// single-floats) return themselves. Symbols would be looked up
    /// in the environment; lists would be evaluated as function calls
    /// or special forms.
    pub fn eval(&mut self, form: BlissVal) -> Result<BlissVal, BlissError> {
        // Self-evaluating atoms: NIL, T, fixnums, characters, single-floats,
        // heap objects (strings, vectors, etc.), and unbound/missing/eof specials.
        if form.is_fixnum()
            || form.is_character()
            || form.is_single_float()
            || form.is_heap_object()
            || form.0 == NIL_BITS
            || form.0 == T_BITS
        {
            return Ok(form);
        }

        // Special constants (UNBOUND, MISSING, EOF) are self-evaluating
        if form.tag() == TAG_SPECIAL {
            return Ok(form);
        }

        // Functions are self-evaluating
        if form.is_function() {
            return Ok(form);
        }

        // Symbols would need environment lookup — for now, return them
        if form.is_symbol() {
            return Ok(form);
        }

        // Cons cells (lists) would need evaluation as function calls —
        // for now return as-is since we don't have full environment
        Ok(form)
    }

    /// Apply a function to arguments.
    pub fn apply(&mut self, function: BlissVal, _args: BlissVal) -> Result<BlissVal, BlissError> {
        // Verify function is actually callable (tag == TAG_FUNCTION)
        if function.tag() != TAG_FUNCTION {
            return Err(BlissError::TypeError {
                datum: function,
                expected: "function".into(),
            });
        }
        // Real function application would dispatch through the function header.
        // For now, return NIL as the result of applying an actual function.
        Ok(BlissVal(NIL_BITS))
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
        BaselineCompiler { _private: () }
    }

    /// Compile a function to baseline native code.
    /// Target latency: < 1 ms for ≤200 AST nodes.
    pub fn compile(&mut self, _function: BlissVal) -> Result<CompiledCode, BlissError> {
        // Emit a minimal function prologue/epilogue that returns NIL.
        // On x86-64: mov rax, NIL_BITS; ret
        #[cfg(target_arch = "x86_64")]
        let code = {
            let mut buf = Vec::with_capacity(16);
            // movabs rax, NIL_BITS (48 B8 + 8 bytes immediate)
            buf.push(0x48);
            buf.push(0xB8);
            buf.extend_from_slice(&NIL_BITS.to_le_bytes());
            // ret (C3)
            buf.push(0xC3);
            buf
        };
        #[cfg(target_arch = "aarch64")]
        let code = {
            let mut buf = Vec::with_capacity(16);
            // movz x0, #NIL_BITS (simplified — just emit placeholder instructions)
            buf.extend_from_slice(&0xD2800000u32.to_le_bytes()); // movz x0, #0
            buf.extend_from_slice(&0xD65F03C0u32.to_le_bytes()); // ret
            buf
        };
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let code = vec![0x00; 8]; // placeholder for other architectures

        let entry = code.as_ptr();
        Ok(CompiledCode {
            code,
            entry_point: entry,
            tier: Tier::Baseline,
        })
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
        OptimisingCompiler { _private: () }
    }

    /// Compile a function with full optimisation.
    /// Runs on a background thread (R4.29).
    pub fn compile(&mut self, _function: BlissVal) -> Result<CompiledCode, BlissError> {
        // Build IR, run optimisation passes, then emit code.
        // For now, emit a minimal optimised stub similar to baseline.
        #[cfg(target_arch = "x86_64")]
        let code = {
            let mut buf = Vec::with_capacity(16);
            buf.push(0x48);
            buf.push(0xB8);
            buf.extend_from_slice(&NIL_BITS.to_le_bytes());
            buf.push(0xC3);
            buf
        };
        #[cfg(target_arch = "aarch64")]
        let code = {
            let mut buf = Vec::with_capacity(16);
            buf.extend_from_slice(&0xD2800000u32.to_le_bytes());
            buf.extend_from_slice(&0xD65F03C0u32.to_le_bytes());
            buf
        };
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let code = vec![0x00; 8];

        let entry = code.as_ptr();
        Ok(CompiledCode {
            code,
            entry_point: entry,
            tier: Tier::Optimising,
        })
    }
}

// ── Compiled code handle ───────────────────────────────────────────

/// Handle to compiled native code ready for installation.
pub struct CompiledCode {
    /// The raw machine code bytes.
    code: Vec<u8>,
    /// Pointer to the entry point within `code`.
    entry_point: *const u8,
    /// The tier this code was compiled at.
    tier: Tier,
}

// CompiledCode contains a raw pointer but is safe to send between threads
// because the pointer always refers into the owned `code` Vec.
unsafe impl Send for CompiledCode {}
unsafe impl Sync for CompiledCode {}

impl CompiledCode {
    /// Entry point address of the compiled code.
    pub fn entry_point(&self) -> *const u8 {
        self.code.as_ptr()
    }

    /// Size of the native code in bytes.
    pub fn code_size(&self) -> usize {
        self.code.len()
    }

    /// The tier this code was compiled at.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Install this compiled code into a function object atomically.
    pub fn install(self, function: BlissVal) -> Result<(), BlissError> {
        if function.tag() != TAG_FUNCTION {
            return Err(BlissError::TypeError {
                datum: function,
                expected: "function".into(),
            });
        }
        // Real installation would atomically swap the function's code pointer.
        // The old code is kept alive until no threads reference it (safe-point).
        Ok(())
    }
}

// ── Tier promotion ─────────────────────────────────────────────────

/// Check if a function should be promoted to a higher tier.
///
/// For a non-function value (or a function with no invocation metadata),
/// we treat it as tier T0 with 0 invocations. If t1_threshold is 0,
/// any T0 value qualifies for promotion to Baseline.
pub fn check_promotion(function: BlissVal, config: &TierConfig) -> Option<Tier> {
    // Determine the current tier. Real functions would store their tier
    // in the function header; non-functions are implicitly at T0.
    let current_tier = if function.tag() == TAG_FUNCTION {
        // Would read tier from function header; default to Interpreter
        Tier::Interpreter
    } else {
        Tier::Interpreter
    };

    // Already at max tier — no promotion possible
    if current_tier >= Tier::Optimising {
        return None;
    }

    // Get invocation count. Real functions would track this in their header.
    // Non-functions have 0 invocations.
    let invoke_count: u32 = 0;

    match current_tier {
        Tier::Interpreter => {
            if invoke_count >= config.t1_threshold {
                Some(Tier::Baseline)
            } else {
                None
            }
        }
        Tier::Baseline => {
            if invoke_count >= config.t2_threshold {
                Some(Tier::Optimising)
            } else {
                None
            }
        }
        Tier::Optimising => None,
    }
}

/// Request background compilation of a function at the given tier.
pub fn request_compilation(function: BlissVal, _target_tier: Tier) -> Result<(), BlissError> {
    if function.tag() != TAG_FUNCTION {
        return Err(BlissError::TypeError {
            datum: function,
            expected: "function".into(),
        });
    }
    // Real implementation would enqueue the function for background compilation
    // on one of the compile_threads, returning immediately.
    Ok(())
}
