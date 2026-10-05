// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `egcl-compiler`: the Evergreen Common Lisp front end and the T2 optimising
//! compiler.
//!
//! # Crate map
//!
//! * [`reader`] — the Common Lisp reader: character slices or live streams to
//!   Lisp objects. Used by the `egcl` evaluator, `egcl-stdlib`, and the REPL.
//! * [`mod@macroexpand`] — lexical `Environment`, global/compiler/local macro
//!   tables, `macroexpand-1` / `macroexpand` / `macroexpand-all`, and the
//!   special-operator code walker. Used by the evaluator and the bytecode
//!   compiler.
//! * [`t2`] — the block-based SSA optimising compiler: bytecode → SSA
//!   (`t2::build`), verification, inference, optimisation passes, lowering,
//!   `regalloc2` allocation, per-target emission, deopt metadata, and the
//!   native exception-transfer tables.
//! * [`control_scope`] and [`native_unwind`] — logical handler-scope analysis
//!   over bytecode and the next-unwind-action selector, shared by the bytecode
//!   tier's native-transfer entry code and the T2 transfer tables.
//! * [`tiered`] — a standalone tree-walking evaluator over Lisp forms, kept
//!   only because `egcl-stdlib`'s debugger evaluates forms in a frame with it.
//!   It is not the production T0 and should be replaced by the real evaluator.
//!
//! The bytecode compiler, the T0 bytecode interpreter, the T1 baseline JIT,
//! the tier thresholds (`EGCL_T0_T1_THRESHOLD`, `EGCL_T1_T2_THRESHOLD`,
//! `EGCL_OSR_THRESHOLD`) and the production type profiles are **not** in this
//! crate: they live in `crates/egcl/src/cli/bytecode.rs`, with the bytecode
//! types and the executable buffer in `egcl-rt`. That code calls into this
//! crate for everything above.
//!
//! # Re-exports
//!
//! The flat `pub use` list below exists for the older tests and tools that
//! import `egcl_compiler::X` directly. New code should name the module.

// ── Front-end ─────────────────────────────────────────────────────
pub mod macroexpand;
pub mod reader;

// ── Bytecode control-scope analysis ───────────────────────────────
pub mod control_scope;
pub mod native_unwind;

// ── T2 optimising compiler (block-based SSA) ──────────────────────
pub mod t2;

// ── Standalone tree-walking evaluator (debugger only) ─────────────
pub mod tiered;

// ── Re-exports for convenience ────────────────────────────────────
pub use macroexpand::{
    CompilerMacroFn, DeclInfo, Environment, FunctionInfo, InlinePolicy, MacroexpandHook,
    OptimizeQualities, VariableInfo, define_compiler_macro, define_global_macro, macroexpand,
    macroexpand_1, macroexpand_all, set_macroexpand_hook, set_macroexpand_limit,
    undefine_compiler_macro, undefine_global_macro,
};
pub use reader::{
    ReaderState, SourcePos, SyntaxType, copy_readtable, get_dispatch_macro_character,
    get_macro_character, intern_symbol, make_dispatch_macro_character, make_readtable, read,
    read_from_string, read_from_string_with_base, register_package, set_dispatch_macro_character,
    set_macro_character, set_read_eval_hook, symbol_name,
};
