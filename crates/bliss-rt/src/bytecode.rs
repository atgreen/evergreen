//! Bliss bytecode — the portable instruction stream and function record.
//!
//! This is the **canonical coordinate system** shared by every execution tier
//! (spec §4.4, §4.10): T0 interprets it, T1 compiles it to baseline native code,
//! and T2 builds its optimising SSA IR from it. Deoptimisation and OSR resume the
//! T0 interpreter at a bytecode index (`bcp`), so the bytecode is also the anchor
//! for FrameState (spec §4.10 D4.15) and profiling.
//!
//! Only the *data types* live here. The lowering (CL forms → bytecode), the T0
//! interpreter, and the T1 native emitter live in the `bliss` crate, which
//! re-exports these types. `bliss-compiler` (T2) depends on `bliss-rt` and so can
//! read the bytecode without a dependency cycle.
//!
//! Fields are `pub` because the producer (the `bliss` bytecode compiler) and the
//! consumers (T0/T1 in `bliss`, T2 in `bliss-compiler`) live in other crates.

use crate::value::BlissVal;

/// A single bytecode instruction. Operand-stack based; the frame's value-slot
/// area holds `n_locals` lexical slots followed by `max_stack` operand slots
/// (spec D2.03).
#[derive(Clone, Debug)]
pub enum Instr {
    /// Push `constants[idx]`.
    Const(u16),
    /// Push the value of local slot `idx`.
    LoadLocal(u16),
    /// Pop and store into local slot `idx`.
    StoreLocal(u16),
    /// Push the dynamic/global value of symbol `sym` (spec `LOAD_SPECIAL`).
    LoadGlobal(u32),
    /// Pop and store into the dynamic/global value of `sym` (`STORE_SPECIAL`).
    StoreGlobal(u32),
    /// Pop `n` values, set them as the multiple values, and push the primary
    /// (`VALUES`). `n = 0` pushes NIL.
    SetValues(u16),
    /// Clear the pending multiple values (single-value context).
    ClearMv,
    /// Pop the primary; read the current multiple values; store the first
    /// `nvars` of them (primary, then secondaries, NIL-padded) into local slots
    /// `slot_base..slot_base+nvars` (`MULTIPLE-VALUE-BIND`).
    TakeValuesToLocals { nvars: u16, slot_base: u16 },
    /// Pop the primary; push a fresh list of the current multiple values
    /// (`MULTIPLE-VALUE-LIST`).
    ValuesToList,
    /// Evaluate a constant form via the tree-walker and push the result. Used
    /// for non-capturing `(lambda …)` / `(function …)`: the resulting closure
    /// is callable by both backends (`funcall`/`apply`/`mapcar` go host).
    EvalHost(u16),

    // ── Captured locals / closures (nmq.5) ─────────────────────────
    /// Push a boxed (closure-captured) variable from the heap `EnvFrame`.
    LoadEnvVar(u16),
    /// Pop and store into an existing boxed variable (`setq`).
    StoreEnvVar(u16),
    /// Pop and bind a boxed variable in the current heap `EnvFrame`.
    DefineEnvVar(u16),
    /// Enter a fresh child `EnvFrame` (a `let` that binds captured variables).
    PushEnvChild,
    /// Leave the current child `EnvFrame`.
    PopEnvChild,
    /// Evaluate a `(lambda …)` constant with `env.frame` bound to this
    /// activation's heap `EnvFrame`, so the closure captures it (shared, live).
    MakeClosureEnv(u16),
    /// Discard the top of the operand stack.
    Pop,
    /// Duplicate the top of the operand stack.
    Dup,
    /// Unconditional jump: set the bytecode pointer to `target`.
    Br(u32),
    /// Pop; if it is NIL, jump to `target`, else fall through.
    BrIfFalse(u32),
    /// Pop; if it is non-NIL, jump to `target`, else fall through.
    BrIfTrue(u32),
    /// Call `sym` with `nargs` operands. Resolves to a bytecode function
    /// (native frame push) or falls back to the tree-walker's `apply_function`.
    CallNamed { sym: u32, nargs: u16 },
    /// Return the top of the operand stack to the caller.
    Return,

    // ── Non-local control flow (nmq.4) — unwind-table opcodes ──────
    /// Pop a tag and establish a `CATCH` handler. Generates a control token
    /// (shared with the tree-walker via `env.catch_stack`), so a `THROW` from
    /// either backend lands here, resumes at `resume_bcp`, resets the operand
    /// stack to `sp_restore`, and pushes the thrown value.
    PushCatch { resume_bcp: u32, sp_restore: u16 },
    /// Establish a `BLOCK` exit handler keyed by the lexical `block_id`;
    /// `name_idx` names the block for `env.block_stack` (tree-walker interop).
    PushBlock {
        block_id: u32,
        name_idx: u16,
        resume_bcp: u32,
        sp_restore: u16,
    },
    /// Establish a `TAGBODY` handler keyed by the lexical `tagbody_id`.
    PushTag { tagbody_id: u32, sp_restore: u16 },
    /// Establish an `UNWIND-PROTECT` cleanup handler; on any unwind through it
    /// the cleanup at `cleanup_bcp` runs (operand stack reset to `sp_restore`).
    PushUnwind { cleanup_bcp: u32, sp_restore: u16 },
    /// Remove the most-recently established handler (normal completion).
    PopHandler,
    /// Pop a tag and a value and throw: unwind to the matching `CATCH`.
    Throw,
    /// Pop a value and return from the lexical block `block_id`.
    ReturnFrom { block_id: u32 },
    /// Transfer to tag `target_bcp` within tagbody `tagbody_id`, running any
    /// intervening `UNWIND-PROTECT` cleanups.
    Go { tagbody_id: u32, target_bcp: u32 },
    /// Normal-path `UNWIND-PROTECT`: save the protected value, run the cleanup
    /// at `cleanup_bcp`, then resume at `resume_bcp` with the value restored.
    EnterCleanupNormal { cleanup_bcp: u32, resume_bcp: u32 },
    /// End of a cleanup body: act on the saved continuation (resume normally,
    /// or continue an in-progress unwind).
    CleanupReturn,
    /// Establish a `HANDLER-CASE` cluster (registered in `env.handlers` so a
    /// host-signalled condition finds it) — `hc` indexes the static clause
    /// table. A matching condition unwinds into the selected clause body.
    PushHandlerCase { hc: u32, sp_restore: u16 },
    /// Normal completion of `HANDLER-CASE`: disestablish the cluster.
    PopHandlerCase,
    /// Establish a `HANDLER-BIND` cluster (`hb` indexes the static binding
    /// table). Handlers run in the signalling context via the shared machinery.
    PushHandlerBind { hb: u32 },
    /// Normal completion of `HANDLER-BIND`: disestablish the cluster.
    PopHandlerBind,
    /// Establish a `RESTART-CASE` (`rc` indexes the static restart table),
    /// registered in `env.restarts` for INVOKE-RESTART. `resume_bcp` is where a
    /// delivered restart result resumes.
    PushRestartCase {
        rc: u32,
        resume_bcp: u32,
        sp_restore: u16,
    },
    /// Normal completion of `RESTART-CASE`: disestablish the restarts.
    PopRestartCase,
}

/// A lowered CL function: a linear bytecode plus its constant pool and frame
/// shape. The frame's value-slot area holds `n_locals` lexical slots followed
/// by `max_stack` operand slots (spec D2.03).
#[derive(Debug)]
pub struct BytecodeFunction {
    pub code: Vec<Instr>,
    pub constants: Vec<BlissVal>,
    /// Static per-`handler-case` clause tables (indexed by `PushHandlerCase`).
    pub handler_cases: Vec<HandlerCaseInfo>,
    /// Static per-`handler-bind` binding tables (indexed by `PushHandlerBind`).
    pub handler_binds: Vec<HandlerBindInfo>,
    /// Interned block names (referenced by `PushBlock` for `env.block_stack`).
    pub names: Vec<String>,
    /// Static per-`restart-case` tables (indexed by `PushRestartCase`).
    pub restart_cases: Vec<RestartCaseInfo>,
    /// Per-parameter `(name, location)` for the entry sequence.
    pub param_layout: Vec<(String, VarLoc)>,
    /// Whether this function needs a heap `EnvFrame` (has captured locals).
    pub has_env: bool,
    /// Number of lexical local slots (params + `let` bindings).
    pub n_locals: u16,
    /// Maximum operand-stack depth.
    pub max_stack: u16,
    /// Fixed argument count (slice 1 lowers only fixed lambda lists).
    pub arity: u16,
    /// Function name, for debugging.
    pub name: String,
}

impl BytecodeFunction {
    /// Total value-slot count reserved in the frame (locals + operand stack).
    pub fn num_slots(&self) -> u16 {
        self.n_locals + self.max_stack
    }
}

/// Static description of one `handler-case` form: its clauses plus the PC to
/// resume at after the whole form.
#[derive(Debug, Clone)]
pub struct HandlerCaseInfo {
    pub clauses: Vec<ClauseInfo>,
}

/// Static description of one `handler-case` clause.
#[derive(Debug, Clone)]
pub struct ClauseInfo {
    /// Condition type name the clause handles (`T` = catch-all).
    pub type_name: String,
    /// Bytecode PC of the clause body.
    pub body_bcp: u32,
    /// Local slot the condition is bound to, if the clause has a variable.
    pub var_slot: Option<u16>,
}

/// Static description of one `handler-bind` form: `(type . handler-form)` pairs.
/// The handler form is stored raw (unevaluated) exactly as the tree-walker does,
/// so the shared signal machinery invokes it identically.
#[derive(Debug, Clone)]
pub struct HandlerBindInfo {
    pub bindings: Vec<(String, BlissVal)>,
}

/// Where a lexical variable lives: a fast frame slot, or boxed in the shared
/// heap `EnvFrame` because a closure captures it.
#[derive(Debug, Clone, Copy)]
pub enum VarLoc {
    Slot(u16),
    Boxed,
}

/// Static description of one `restart-case` form. Each restart's clause is a
/// `(lambda params . body)` form run by the shared INVOKE-RESTART machinery
/// (in `env.frame`); the bytecode only catches the restart-invoked transfer and
/// delivers the stored result.
#[derive(Debug, Clone)]
pub struct RestartCaseInfo {
    pub restarts: Vec<(String, BlissVal)>,
}
