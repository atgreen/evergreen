// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Who may have a register entry, and which values need a shadow root.
//!
//! # Purpose
//!
//! A *register entry* is a second entry point to a framed body: the same
//! prologue, but arguments arrive in the ABI's argument registers instead of
//! activation slots, and the activation pointer is left as the caller's. A
//! self-call can then move its arguments into those registers and branch
//! straight there, skipping the generic dispatch that otherwise costs more
//! than the loop it surrounds.
//!
//! Deciding *whether* a body may have one is pure IR analysis: it asks about
//! the lambda list, the calls the body makes, and whether anything needs a
//! shadow root. None of that is machine-specific, and System Z paid for the
//! answer once already. This module is that analysis, so a port adds the
//! machine half — a prologue and an argument move — rather than
//! re-deriving the contract and its sharp edges.
//!
//! What stays with each backend: the entry prologue, the argument moves, the
//! stack-limit compare that keeps deep recursion a `STORAGE-CONDITION`, and
//! the frame-header stores. Those are the parts a register name appears in.
//!
//! # The contract, and why each clause is here
//!
//! A register-entry body runs WITHOUT OWNING AN ACTIVATION: the activation
//! pointer still belongs to its caller. Everything below follows from that.
//!
//! * **No shadow roots** (`root_slots == 0`). A shadow root lives in the
//!   activation, and this body has no activation of its own to put one in.
//!   This is the clause that makes [`proven_immediate`] load-bearing rather
//!   than merely an optimisation: without narrowing roots to values the
//!   collector can actually move, a fixnum-recursive function never reaches
//!   zero and never qualifies.
//! * **A fixed lambda list, no more than `args_max` parameters, none of them
//!   entry-checked.** A variadic list is bound by the variadic binder before
//!   the body runs, and a checked parameter needs the activation the check
//!   reports against.
//! * **Every call small enough to stage.** A self-call passes at most
//!   `args_max` arguments, in registers. Any other call stages its arguments
//!   in this frame and goes through the rooting slice adapter, which copies
//!   at most `call_args_max` of them into storage the collector can see
//!   before the callee can allocate (bliss-of8kz) — so a body that makes such
//!   a call needs that adapter installed.
//! * **No `TakeValuesToLocals`.** It writes through the activation.
//! * **Either a self-caller, or a body that makes no calls at all.**
//!   Publishing the entry makes the body use the in-frame staging form for
//!   its own calls, which costs the direct-builtin shortcut. A self-caller
//!   has already paid for that; for a body the runtime merely resolves, take
//!   the trade only when it is free (bliss-y1rx3). Leaf accessors and
//!   predicates are the hot case there anyway.
//!
//! Install-time validation re-checks the activation-less contract before any
//! of the emitted bytes run; this analysis decides, it does not enforce.

use crate::t2::infer::InferenceResult;
use crate::t2::ir::{AuxData, Function, InstData, Opcode, TypeBits, Value};

/// A value whose inferred type is confined to the immediate tags, and which
/// the collector therefore can neither move nor need to see.
///
/// The inference map gives each SSA value the type it has from its definition
/// on, so the fact holds wherever the value is live. `BOTTOM` is unreached
/// rather than proven, and stays rooted.
///
/// Every framed backend narrows its shadow roots with this. It is also what
/// lets a fixnum-recursive function reach `root_slots == 0`, and so the
/// register entry above.
pub fn proven_immediate(inference: &InferenceResult, value: Value) -> bool {
    const IMMEDIATE: TypeBits = TypeBits(
        TypeBits::FIXNUM.0
            | TypeBits::SINGLE_FLOAT.0
            | TypeBits::CHARACTER.0
            | TypeBits::SYMBOL.0
            | TypeBits::NULL.0,
    );
    let bits = inference.ty(value).bits;
    !bits.is_bottom() && bits.meet(IMMEDIATE) == bits
}

/// Whether `data` is a call to the function being compiled.
pub fn is_self_call(data: &InstData, self_sym: Option<u32>) -> bool {
    matches!(data.aux, AuxData::CallTarget(symbol) if Some(symbol) == self_sym)
}

/// What a backend knows that the IR does not.
pub struct RegisterEntryQuery<'a> {
    pub function: &'a Function,
    /// The entry block's parameters, in order.
    pub entry_params: &'a [Value],
    /// The symbol being compiled, when the runtime resolved one.
    pub self_sym: Option<u32>,
    /// Address of the rooting slice adapter, or zero when it is absent.
    pub call_slice_rooted: u64,
    /// Shadow roots this body needs, after narrowing.
    pub root_slots: u16,
    /// Arguments the entry prologue can move home, one per ABI argument
    /// register it uses.
    pub args_max: usize,
    /// The most arguments a non-self call may pass, being what the rooting
    /// slice adapter copies.
    pub call_args_max: usize,
    /// Whether this backend can stage a NON-self call's arguments in the
    /// body's own frame.
    ///
    /// A register-entry body owns no activation, so it cannot stage call
    /// arguments in one. A backend that has not implemented the in-frame
    /// form must therefore decline any body that makes a call to something
    /// other than itself -- otherwise those arguments are written to an
    /// activation region that eligibility has just sized to zero, which
    /// corrupts the caller's slots rather than failing cleanly.
    pub stages_other_calls_in_frame: bool,
}

/// The analysis result.
pub struct RegisterEntryPlan {
    /// Whether to publish a register entry at all.
    pub eligible: bool,
    /// Widest non-self call in the body, in arguments — the staging area a
    /// register-entry body must reserve.
    pub other_call_words: usize,
}

/// Decide whether `query`'s body may carry a register entry.
///
/// `EGCL_NO_DIRECT_SELF_CALL` disables it, which is how a suspected
/// miscompile is bisected without rebuilding.
pub fn plan(query: &RegisterEntryQuery<'_>) -> RegisterEntryPlan {
    let function = query.function;
    let instructions = || {
        function
            .block_order()
            .iter()
            .flat_map(|&block| &function.block(block).insts)
            .map(|&inst| function.inst(inst))
    };

    let other_call_words = instructions()
        .filter(|data| data.opcode == Opcode::Call && !is_self_call(data, query.self_sym))
        .map(|data| data.args.len())
        .max()
        .unwrap_or(0);

    let calls_fit = instructions().all(|data| {
        data.opcode != Opcode::Call
            || (is_self_call(data, query.self_sym) && data.args.len() <= query.args_max)
            || (matches!(data.aux, AuxData::CallTarget(_))
                && data.args.len() <= query.call_args_max)
    });

    let makes_calls = instructions().any(|data| data.opcode == Opcode::Call);

    // Without in-frame staging, a non-self call has nowhere to put its
    // arguments: this body owns no activation and the backend has no other
    // place to stage them.
    let calls_are_stageable = query.stages_other_calls_in_frame || other_call_words == 0;

    let eligible = (query.self_sym.is_some() || !makes_calls)
        && calls_are_stageable
        && std::env::var_os("EGCL_NO_DIRECT_SELF_CALL").is_none()
        && !function.is_variadic()
        && query.entry_params.len() <= query.args_max
        && !query
            .entry_params
            .iter()
            .any(|&param| function.is_entry_param_checked(param))
        && query.root_slots == 0
        && calls_fit
        && (other_call_words == 0 || query.call_slice_rooted != 0)
        && instructions().all(|data| data.opcode != Opcode::TakeValuesToLocals);

    RegisterEntryPlan {
        eligible,
        other_call_words,
    }
}
