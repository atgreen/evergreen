// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 optimising compiler: block-based SSA from bytecode to native code.
//!
//! # Purpose
//!
//! This module tree is the whole T2 tier: bytecode → SSA → mid-end passes →
//! lowering → register allocation → per-target machine code, plus the
//! deoptimisation and OSR metadata that lets T2 code hand an activation back to
//! the interpreter and accept one from it. The interpreter's promotion path in
//! `crates/egcl/src/cli/bytecode.rs` drives this tree exclusively.
//!
//! # Pipeline
//!
//! 1. [`build`] — abstract interpretation of a `BytecodeFunction` into an
//!    [`ir::Function`]: Braun-style SSA construction with block parameters as
//!    φ, a `FrameState` on every deoptimising site, OSR entry states at
//!    empty-stack loop headers, intrinsic expansion, and inlining of saved
//!    callee bodies under the policy in [`inlining`].
//! 2. [`verify`] — structural, dominance, and metadata checks. Run after the
//!    builder and again after the mid-end; a failure declines T2.
//! 3. [`speculate`] — profile-guided single-type speculation that rewrites
//!    generic operations to typed ones behind guards; [`infer`] supplies the
//!    type, range, and representation facts passes consult.
//! 4. Mid-end passes under [`pass::PassManager`]. The promotion path runs
//!    [`opt_fold`] → [`opt_gvn`] → [`opt_guard`] → [`opt_clear_mv`] → [`opt_dce`]; [`opt_licm`]
//!    and [`opt_escape`] are implemented and tested as passes but not yet
//!    scheduled there.
//! 5. [`lower`] — instruction selection from SSA to a [`mach::MachFunc`] over
//!    virtual registers.
//! 6. [`regalloc`] — the `regalloc2` adapter: per-operand locations, spill and
//!    reload edits, per-safepoint stack maps.
//! 7. Emission — [`emit`] for x86-64 (the flat leaf emitter `emit` and the
//!    production `emit_framed`), [`emit_a64`], [`emit_ppc64le`],
//!    [`emit_s390x`]. [`x64_frame`] chooses the final value homes the framed
//!    x86 emitter and the transfer metadata share.
//! 8. Deopt and OSR metadata — [`frame_state`] defines `FrameState`;
//!    [`deopt`] lowers each one to resolved slot descriptors and implements the
//!    runtime reconstruction; [`slot_map`] is the single producer of
//!    per-safepoint slot liveness and representation that the deopt export and
//!    the OSR import both read (bliss-ht4). OSR entries themselves are the
//!    `ir::OsrEntry` records the builder captures at empty-stack loop headers.
//! 9. Native non-local transfer (opt-in, x86-64 Linux and ppc64le) —
//!    [`transfer_capture`], [`transfer_map`], [`transfer_sites`],
//!    [`native_transfer`], `native_transfer_ppc64le`,
//!    [`native_transfer_s390x`]: capturing a throw at the throwing call,
//!    binding frame maps to emitted return PCs, and the machine stubs for the
//!    transfer ABI. System Z has the three stubs and the capture image; the
//!    emitter and installer that would use them are still being built, so the
//!    target stays on the checked ABI.
//!
//! # Cross-module invariants
//!
//! * **Deopt preservation.** A value named by any `FrameState` must stay
//!   defined and dominating at that instruction, or be replaced by a
//!   rematerialisation recipe. Every pass honours this; `opt_dce` is the only
//!   pass that may remove such a value, and only by rewriting its sources.
//! * **Decline, never partially install.** Every stage returns an error rather
//!   than panicking on input it cannot handle, and the function stays at its
//!   current tier. No stage installs code or metadata for a function another
//!   stage then rejects.
//! * **Tier differential.** Observable results are identical at T0, T1, and T2
//!   for the same program, and a forced deopt at any guard resumes with the
//!   interpreted result. The integration tests under `tests/` gate every
//!   change on both.

pub mod constants;
pub mod frame_state;
pub mod ir;
pub mod mach;
pub mod pass;
pub mod slot_map; // per-safepoint slot liveness + representation producer (bliss-ht4)
pub mod transfer_capture;
pub mod transfer_map;
pub mod x64_calls;
pub mod x64_frame;
pub mod x64_unwind;
pub mod x64_value_maps;

// ── Front end ─────────────────────────────────────────────────────
pub mod build; // bytecode → SSA
pub mod infer; // type/range/representation inference
pub mod builtin_binding; // exact binding guards for builtin specialization
pub mod inlining; // call-site policy + compiler-known inline metadata
pub mod verify; // IR verifier

// ── Mid-end and back end ──────────────────────────────────────────
pub mod deopt; // FrameState → stack-map lowering + runtime reconstruction
pub mod lower; // IR → MachFunc instruction selection
pub mod opt_clear_mv; // dead multiple-value-reset elimination
pub mod opt_dce; // deopt-aware DCE + rematerialisation
pub mod opt_guard; // guard elimination + hoisting
pub mod opt_gvn; // global value numbering / CSE
pub mod opt_loop_peel; // profitable type-transition peeling with generic fallback
pub mod vector_ranges;
pub(crate) mod vector_range_proof;
mod region_clone;
pub mod opt_licm; // loop-invariant code motion
pub mod regalloc; // regalloc2 adapter + stack maps
// Shared by the framed backends: who may carry a register entry, and which
// values need a shadow root. Pure IR analysis, so every port reads one
// answer instead of re-deriving the contract.
pub mod register_entry;
pub mod speculate; // profile-guided single-type speculative lowering
mod string_char; // inline CHAR/SCHAR/ELT fast path on simple strings

// ── Machine-code emission ─────────────────────────────────────────
pub mod emit; // MachFunc → executable x86-64 bytes
pub mod emit_a64;
pub mod emit_ppc64le;
pub mod emit_riscv64;
pub mod emit_s390x;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub mod native_transfer;
pub mod native_return;
#[cfg(all(
    target_arch = "powerpc64",
    target_endian = "little",
    target_os = "linux"
))]
pub mod native_transfer_ppc64le;
// Compiled on every host, like emit_s390x: the stubs are pure byte emission,
// so building them everywhere keeps x86 CI type-checking them. Only the tests
// that execute the bytes are target-gated.
pub mod native_transfer_s390x;
// Compiled on every host, like native_transfer_s390x: pure byte emission, so
// building it everywhere keeps x86 CI type-checking it. Only the tests that
// execute the bytes are target-gated.
pub mod native_transfer_aarch64;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub mod transfer_sites;

// ── Further passes ────────────────────────────────────────────────
pub mod opt_escape; // escape analysis + scalar replacement
pub mod opt_fold; // constant folding + strength reduction

/// Guarded packed-double closed-expression specialization.
pub mod packed_f64;
/// Raw double-float representation cleanup after double speculation.
pub mod unbox_f64;

/// Guarded general array element accesses.
pub mod array_access;

/// Native constant byte extraction.
pub mod integer_extract;
/// Signed machine integer representation selection.
pub mod unbox_fixnum;

/// Success-only integer shift fast paths with retained generic calls.
pub mod integer_shift;

/// Guarded whole-word replacement of bounded integer-to-octet loops.
pub mod octet_word_fusion;
pub mod array_layout;
pub mod value_facts;

/// Guarded return-format expectations, deliberately separate from inference.
pub mod return_hints;
