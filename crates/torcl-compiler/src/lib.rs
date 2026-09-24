//! `torcl-compiler` — TorCL Common Lisp compiler pipeline.
//!
//! Reader → macro-expansion → sea-of-nodes IR → tiered compilation
//! (T0 interpreter, T1 baseline, T2 optimising) → register allocation
//! → code emission → inline caches → profiling → OSR/deopt.

// ── Front-end ─────────────────────────────────────────────────────
pub mod macroexpand;
pub mod reader;

// ── Intermediate representation ───────────────────────────────────
pub mod ir;

// ── T2 optimising compiler (block-based SSA, spec §4.3–§4.10) ──────
// New pipeline; supersedes the sea-of-nodes ir/opt/codegen/osr modules above.
pub mod t2;

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
pub use codegen::{
    Aarch64Backend, CodeBuffer, CodegenBackend, LinearScanAllocator, RegisterAllocation, RelocKind,
    Relocation, StackMap, TargetArch, X86_64Backend, native_arch,
};
pub use error::CompilerError;
pub use ic::{IcEntry, IcState, InlineCache, ic_generation, init_ic_registry, reset_all_caches};
pub use ir::{Edge, EdgeKind, IrBuilder, IrGraph, IrSourceInfo, NodeId, NodeKind, verify};
pub use macroexpand::{
    CompilerMacroFn, DeclInfo, Environment, FunctionInfo, InlinePolicy, MacroexpandHook,
    OptimizeQualities, VariableInfo, define_compiler_macro, define_global_macro, macroexpand,
    macroexpand_1, macroexpand_all, set_macroexpand_hook, set_macroexpand_limit,
    undefine_compiler_macro, undefine_global_macro,
};
pub use opt::{
    ConstantFolding, DeadCodeElimination, EscapeAnalysis, FunctionRegistry, Inlining,
    InliningConfig, Licm, NullCheckElimination, Pass, PassManager, StrengthReduction,
    TypePropagation,
};
pub use osr::{
    ConversionKind, DeoptConfig, DeoptEntry, DeoptLog, DeoptReason, DeoptResult, LocalMapping,
    Location, OsrEntryMap, OsrEntryResult, OsrSlotDesc, TypeGuard, clear_global_deopt_logs,
    deoptimize, is_function_blacklisted, is_function_in_backoff, osr_entry,
};
pub use profiling::{BackEdgeCounter, FunctionProfile, InvocationCounter, TypeProfile};
pub use reader::{
    ReaderState, SourcePos, SyntaxType, copy_readtable, get_dispatch_macro_character,
    get_macro_character, intern_symbol, make_dispatch_macro_character, make_readtable, read,
    read_from_string, read_from_string_with_base, register_package, set_dispatch_macro_character,
    set_macro_character, set_read_eval_hook, symbol_name,
};
pub use tiered::{
    BaselineCompiler, CompiledCode, Interpreter, OptimisingCompiler, Tier, TierConfig,
    check_promotion, pop_compilation_request, process_compilation_request, request_compilation,
};
