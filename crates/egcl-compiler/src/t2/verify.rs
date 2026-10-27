// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! IR verifier: well-formedness checks for a T2 [`Function`].
//!
//! # Purpose
//!
//! [`verify`] reports every violation it can find as a [`VerifyError`] naming
//! the check and the offending block or value. It runs after the builder and
//! again after the mid-end; the promotion path declines T2 on any error rather
//! than emit from IR a pass has broken. Hand-built fixtures in tests use it as
//! the contract for what an IR producer must satisfy.
//!
//! # Structure
//!
//! 1. **Live-set reconstruction.** Walk every block, collecting block
//!    parameters and instruction results into the set of live `Value`
//!    handles and mapping each `Inst` to its (block, position). An
//!    instruction not listed in any block is therefore dead, and a value
//!    defined only by such an instruction is dangling.
//! 2. **Structural pass**, which never computes dominators and so is safe on
//!    IR with out-of-range handles. It also records whether every successor
//!    handle is in range.
//! 3. **Dominator pass**, run only when the CFG is sound.
//!
//! # Checks
//!
//! * **V1 dangling-value** — every `args` and `BlockCall.args` entry is a live
//!   value.
//! * **V3 dominance** — every value operand's definition dominates its use,
//!   refined to program order within a block; a block-call argument's
//!   definition dominates the terminator. An `Invoke` result exists only on
//!   its normal edge, so a use must go through that successor's parameter.
//! * **V4 block-param arity / type / repr** — every `BlockCall` to block B
//!   passes exactly `B.params.len()` arguments, each with an `IRType` that
//!   fits and a `ValueRepresentation` equal to the corresponding parameter's.
//! * **V6 terminator / entry-preds** — exactly one terminator per block, last;
//!   no non-terminator carries `targets`; the entry has no predecessors.
//! * **V8 guard-framestate / framestate-handle / framestate-dominance /
//!   remat-recipe** — every `flags.guard` instruction carries a `frame_state`;
//!   the id is in range; every SSA value the state names (in scopes and in
//!   rematerialisation inputs) is live and dominates the instruction; recipes
//!   are acyclic.
//! * **V8 layout-proof / layout-guard-contract** — `StringByteLength` and
//!   `StringAsciiCharAt` consume the result of a `Guard` carrying
//!   `StringLayout`; the former is a pure load (no guard, effect, or frame
//!   state), the latter remains an ordered deopt guard. `StringCharOrNil`
//!   takes any operands, answers NIL for every case it does not handle, and
//!   so is an effectful two-operand load with no guard or frame state.
//! * **V10 succ-exists / reducibility** — every successor block exists, and
//!   every retreating edge targets a block that dominates its source.
//! * **V11 invoke-shape / invoke-result** — an `Invoke` has two distinct
//!   edges, the call/effect/safepoint flags, a transfer-capable `AuxData`, and
//!   its results are not used on the exceptional edge or directly elsewhere.
//! * **V12 transfer-shape** — an `NlxTransfer` carries `TransferSite`
//!   metadata whose origin matches its frame state, the call/effect flags, and
//!   no normal successor.
//! * **V13 cleanup, V14 catch, V15 handler, V16 handler-bind, V17
//!   restart-case** — the native-transfer landing and scope instructions have
//!   the operand, result, flag, and position shape their emitters assume, and
//!   each scope enter/exit identity agrees with its frame state or its cold
//!   `TransferSite`. `verify_cleanup_continuations` additionally checks that
//!   every join agrees on the complete ordered continuation stack, not merely
//!   its depth.
//!
//! The V-numbers are the stable labels in `VerifyError::check`; gaps are
//! checks that are not implemented here:
//!
//! * **V2** use-list/pred-list consistency — `preds` is derived from
//!   terminators, so there is no independent structure to cross-check.
//! * **V5** per-opcode input-type schema — no such schema is defined.
//! * **V7** effect ordering against an alias model — no alias model exists.
//! * The slot-count-versus-bcp and representation-producibility clauses of
//!   the frame-state check need the source `BytecodeFunction`'s abstract
//!   frame, which the verifier does not have.
//! * **V9** source-position presence — block parameters carry none and it
//!   would only be a warning; skipped so hand-built fixtures stay clean.

use std::collections::{HashMap, HashSet};

use crate::t2::frame_state::{FrameState, ValueSource};
use crate::t2::ir::{AuxData, Block, Function, Opcode, Value, ValueDef};

/// A single verification failure.
#[derive(Clone, Debug)]
pub struct VerifyError {
    /// Which check failed, e.g. "V3 dominance".
    pub check: &'static str,
    pub detail: String,
}

impl VerifyError {
    fn new(check: &'static str, detail: impl Into<String>) -> VerifyError {
        VerifyError {
            check,
            detail: detail.into(),
        }
    }
}

/// Where a value is defined, resolved to a CFG position.
///
/// `pos = None` is a block parameter (defined at block entry, before every
/// instruction of the block); `pos = Some(i)` is the result of the instruction
/// at index `i` in the block's instruction list.
struct DefLoc {
    block: Block,
    pos: Option<usize>,
}

/// Verify a function; `Ok(())` iff it is well-formed.
pub fn verify(f: &Function) -> Result<(), Vec<VerifyError>> {
    let mut errors: Vec<VerifyError> = Vec::new();
    let n_blocks = f.num_blocks();

    // ── Reconstruct the live-value set and the inst → (block, position) map.
    //
    // Every SSA value is either a block parameter or an instruction result, and
    // every instruction is listed in exactly one block's `insts`. Walking the
    // blocks therefore reconstructs both the set of live `Value` handles (for
    // V1) and each instruction's CFG location (for V3 program-order checks),
    // without needing an arena-length accessor the contract does not expose.
    let mut valid_values: HashSet<u32> = HashSet::new();
    let mut inst_loc: HashMap<u32, (Block, usize)> = HashMap::new();
    for bi in 0..n_blocks {
        let b = Block(bi as u32);
        let bd = f.block(b);
        for &p in &bd.params {
            valid_values.insert(p.0);
        }
        for (pos, &inst) in bd.insts.iter().enumerate() {
            inst_loc.insert(inst.0, (b, pos));
            for &r in &f.inst(inst).results {
                valid_values.insert(r.0);
            }
        }
    }

    // ── Structural pass: V6, V4 (arity + type/repr), V1/V10 handle existence.
    //
    // This pass never calls `dominators()`/`reverse_postorder()`, which would
    // panic on an out-of-range successor block handle. It also records whether
    // every successor block is in range, so we only run the dominator-based
    // checks when doing so is safe.
    let mut cfg_sound = true;

    for bi in 0..n_blocks {
        let b = Block(bi as u32);
        let bd = f.block(b);

        // V6 — terminator well-formedness within the block.
        let mut term_positions: Vec<usize> = Vec::new();
        for (pos, &inst) in bd.insts.iter().enumerate() {
            let data = f.inst(inst);
            let is_term = data.opcode.is_terminator();
            if matches!(data.aux, AuxData::OpaqueCallTarget(_))
                && (!matches!(data.opcode, Opcode::Call | Opcode::Invoke)
                    || !data.flags.call || !data.flags.effectful || !data.flags.safepoint
                    || data.frame_state.is_none()) {
                errors.push(VerifyError::new("V8 opaque-call-contract",
                    format!("block{bi} NOTINLINE dispatch requires an effectful safepoint Call/Invoke with recovery state")));
            }

            if data.opcode == Opcode::Invoke
                && (data.targets.len() != 2
                    || data.targets[0].block == data.targets[1].block
                    || !data.flags.call
                    || !data.flags.effectful
                    || !data.flags.safepoint
                    || !matches!(
                        data.aux,
                        AuxData::CallTarget(_) | AuxData::OpaqueCallTarget(_)
                            | AuxData::HostEval(_)
                            | AuxData::FunctionLookup(_)
                            | AuxData::ValuesToList
                            | AuxData::NestedClosure(_)
                            | AuxData::LexicalEnvironment { .. }
                            | AuxData::TransferThrow
                            | AuxData::CatchScope { .. }
                            | AuxData::HandlerScope { .. }
                            | AuxData::HandlerBindScope { .. }
                            | AuxData::RestartCaseScope { .. }
                            | AuxData::SpecialBinding { .. }
                            | AuxData::GlobalValue { .. }
                            | AuxData::CleanupContinuation { .. }
                    ))
            {
                errors.push(VerifyError::new("V11 invoke-shape",
                    format!("block{bi} Invoke requires distinct normal/exceptional edges and call effects")));
            }
            if matches!(data.aux, AuxData::TransferThrow)
                && (data.opcode != Opcode::Invoke || data.args.len() != 2)
            {
                errors.push(VerifyError::new(
                    "V11 invoke-shape",
                    "THROW requires an Invoke with tag and primary",
                ));
            }
            if matches!(data.aux, AuxData::HostEval(_) | AuxData::FunctionLookup(_))
                && (data.opcode != Opcode::Invoke || !data.args.is_empty())
            {
                errors.push(VerifyError::new(
                    "V11 invoke-shape",
                    "host evaluation and function lookup require argument-free Invoke",
                ));
            }
            // Closure creation reaches a checked-ABI target as a target-free
            // `Call` to a plain adapter rather than a mapped `Invoke`, so it
            // admits either opcode. Argument-free either way: the nested body
            // is named by the aux index, never passed on the operand stack.
            if matches!(data.aux, AuxData::NestedClosure(_))
                && (!matches!(data.opcode, Opcode::Invoke | Opcode::Call)
                    || !data.args.is_empty())
            {
                errors.push(VerifyError::new(
                    "V11 invoke-shape",
                    "closure creation requires an argument-free Invoke or Call",
                ));
            }
            if matches!(data.aux, AuxData::ValuesToList)
                && (!matches!(data.opcode, Opcode::Call | Opcode::Invoke)
                    || data.args.len() != 1
                    || data.results.len() != 1)
            {
                errors.push(VerifyError::new(
                    "V11 invoke-shape",
                    "multiple-value list collection requires one primary and one result",
                ));
            }
            if let AuxData::SpecialBinding { bcp, enter } = data.aux {
                let identity_matches = data.frame_state
                    .filter(|id| (id.0 as usize) < f.frame_states.len())
                    .and_then(|id| f.frame_states.get(id).scopes.last())
                    .is_some_and(|scope| scope.bcp == bcp);
                if data.opcode != Opcode::Invoke || data.args.len() != usize::from(enter)
                    || !identity_matches {
                    errors.push(VerifyError::new("V11 invoke-shape", "special binding requires exact bytecode identity and entry value"));
                }
            }
            if let AuxData::LexicalEnvironment { value, result, .. } = data.aux {
                // No captured-variable operation both consumes and produces.
                if data.opcode != Opcode::Invoke
                    || data.args.len() != usize::from(value)
                    || (value && result)
                {
                    errors.push(VerifyError::new(
                        "V11 invoke-shape",
                        "lexical environment access has invalid operands",
                    ));
                }
            }
            if let AuxData::GlobalValue { store, .. } = data.aux {
                if data.opcode != Opcode::Invoke || data.args.len() != usize::from(store) {
                    errors.push(VerifyError::new("V11 invoke-shape", "global value access has invalid operands"));
                }
            }
            if let AuxData::HandlerScope { push_bcp, enter } = data.aux {
                let identity_matches = if enter {
                    data.frame_state
                        .filter(|id| (id.0 as usize) < f.frame_states.len())
                        .and_then(|id| f.frame_states.get(id).scopes.last())
                        .is_some_and(|scope| scope.bcp == push_bcp)
                } else {
                    data.targets.get(1).and_then(|edge| {
                        ((edge.block.0 as usize) < n_blocks).then_some(edge.block)
                    }).and_then(|block| f.terminator(block)).is_some_and(|cold| {
                        matches!(&f.inst(cold).aux, AuxData::TransferSite { scopes, .. }
                            if scopes.last().is_some_and(|scope|
                                scope.push_bcp == push_bcp
                                && scope.ownership == crate::control_scope::Ownership::Local
                                && matches!(scope.kind, crate::control_scope::ScopeKind::HandlerCase { .. })))
                    })
                };
                if data.opcode != Opcode::Invoke || !data.args.is_empty() || !identity_matches {
                    errors.push(VerifyError::new(
                        "V15 handler",
                        "invalid handler registration identity or shape",
                    ));
                }
            }
            if let AuxData::CatchScope { push_bcp, enter } = data.aux {
                let identity_matches = if enter {
                    data.frame_state
                        .filter(|id| (id.0 as usize) < f.frame_states.len())
                        .and_then(|id| f.frame_states.get(id).scopes.last())
                        .is_some_and(|scope| scope.bcp == push_bcp)
                } else {
                    data.targets.get(1).and_then(|edge| {
                        ((edge.block.0 as usize) < n_blocks).then_some(edge.block)
                    }).and_then(|block| f.terminator(block)).is_some_and(|cold| {
                        matches!(&f.inst(cold).aux, AuxData::TransferSite { scopes, .. }
                            if scopes.last().is_some_and(|scope|
                                scope.push_bcp == push_bcp
                                && scope.ownership == crate::control_scope::Ownership::Local
                                && matches!(scope.kind, crate::control_scope::ScopeKind::Catch { .. })))
                    })
                };
                if data.opcode != Opcode::Invoke
                    || data.args.len() != usize::from(enter)
                    || !identity_matches
                {
                    errors.push(VerifyError::new("V11 invoke-shape", "CATCH registration requires Invoke with one tag on entry and no arguments on exit"));
                }
            }
            if let AuxData::HandlerBindScope { push_bcp, enter } = data.aux {
                let identity_matches = if enter {
                    data.frame_state
                        .filter(|id| (id.0 as usize) < f.frame_states.len())
                        .and_then(|id| f.frame_states.get(id).scopes.last())
                        .is_some_and(|scope| scope.bcp == push_bcp)
                } else {
                    data.targets.get(1).and_then(|edge| {
                        ((edge.block.0 as usize) < n_blocks).then_some(edge.block)
                    }).and_then(|block| f.terminator(block)).is_some_and(|cold| {
                        matches!(&f.inst(cold).aux, AuxData::TransferSite { scopes, .. }
                            if scopes.last().is_some_and(|scope|
                                scope.push_bcp == push_bcp
                                && scope.ownership == crate::control_scope::Ownership::Local
                                && matches!(scope.kind, crate::control_scope::ScopeKind::HandlerBind { .. })))
                    })
                };
                if data.opcode != Opcode::Invoke
                    || (!enter && !data.args.is_empty())
                    || !identity_matches
                {
                    errors.push(VerifyError::new(
                        "V16 handler-bind",
                        "invalid handler-bind registration identity or shape",
                    ));
                }
            }
            if let AuxData::RestartCaseScope { push_bcp, enter } = data.aux {
                let identity_matches = if enter {
                    data.frame_state
                        .filter(|id| (id.0 as usize) < f.frame_states.len())
                        .and_then(|id| f.frame_states.get(id).scopes.last())
                        .is_some_and(|scope| scope.bcp == push_bcp)
                } else {
                    data.targets.get(1).and_then(|edge| {
                        ((edge.block.0 as usize) < n_blocks).then_some(edge.block)
                    }).and_then(|block| f.terminator(block)).is_some_and(|cold| {
                        matches!(&f.inst(cold).aux, AuxData::TransferSite { scopes, .. }
                            if scopes.last().is_some_and(|scope|
                                scope.push_bcp == push_bcp
                                && scope.ownership == crate::control_scope::Ownership::Local
                                && matches!(scope.kind, crate::control_scope::ScopeKind::RestartCase { .. })))
                    })
                };
                if data.opcode != Opcode::Invoke || !data.args.is_empty() || !identity_matches {
                    errors.push(VerifyError::new(
                        "V17 restart-case",
                        "invalid restart-case registration identity or shape",
                    ));
                }
            }
            if data.opcode == Opcode::NlxTransfer {
                let origin_matches = match (&data.aux, data.frame_state) {
                    (AuxData::TransferSite { origin_bcp, .. }, Some(id))
                        if (id.0 as usize) < f.frame_states.len() =>
                    {
                        f.frame_states
                            .get(id)
                            .scopes
                            .last()
                            .is_some_and(|scope| scope.bcp == *origin_bcp)
                    }
                    _ => false,
                };
                if !origin_matches
                    || !data.results.is_empty()
                    || !data.flags.effectful
                    || !data.flags.call
                    || !data.flags.safepoint
                    || !data.flags.terminator
                {
                    errors.push(VerifyError::new("V12 transfer-shape",
                        format!("block{bi} transfer requires matching capture/scope metadata, effects and no normal successor")));
                }
            }
            if is_term {
                term_positions.push(pos);
            } else if !data.targets.is_empty() {
                errors.push(VerifyError::new(
                    "V6 terminator",
                    format!(
                        "non-terminator {:?} in block{} carries targets",
                        data.opcode, bi
                    ),
                ));
            }
        }
        match term_positions.as_slice() {
            [] => errors.push(VerifyError::new(
                "V6 terminator",
                format!("block{bi} has no terminator"),
            )),
            [only] => {
                if *only != bd.insts.len() - 1 {
                    errors.push(VerifyError::new(
                        "V6 terminator",
                        format!("block{bi} terminator is not the last instruction"),
                    ));
                }
            }
            many => errors.push(VerifyError::new(
                "V6 terminator",
                format!("block{bi} has {} terminators (expected 1)", many.len()),
            )),
        }

        // V4 — block-parameter agreement on every terminator BlockCall.
        // Also fold in successor-block existence (V1/V10 handle liveness).
        if let Some(term) = f.terminator(b) {
            for (ti, call) in f.inst(term).targets.iter().enumerate() {
                if call.block.index() >= n_blocks {
                    cfg_sound = false;
                    errors.push(VerifyError::new(
                        "V10 succ-exists",
                        format!("block{bi} target #{ti} → nonexistent block{}", call.block.0),
                    ));
                    continue;
                }
                let params = &f.block(call.block).params;
                if call.args.len() != params.len() {
                    errors.push(VerifyError::new(
                        "V4 block-param arity",
                        format!(
                            "block{bi} → block{} passes {} args, block has {} params",
                            call.block.0,
                            call.args.len(),
                            params.len()
                        ),
                    ));
                    continue;
                }
                for (i, (&arg, &param)) in call.args.iter().zip(params.iter()).enumerate() {
                    if !valid_values.contains(&arg.0) {
                        errors.push(VerifyError::new(
                            "V1 dangling-value",
                            format!(
                                "block{bi} → block{} arg #{i} v{} is not live",
                                call.block.0, arg.0
                            ),
                        ));
                        continue;
                    }
                    let av = f.value(arg);
                    let pv = f.value(param);
                    if !pv.ty.bits.contains(av.ty.bits) {
                        errors.push(VerifyError::new(
                            "V4 block-param type",
                            format!(
                                "block{bi} → block{} arg #{i}: type {:?} not a subtype of param {:?}",
                                call.block.0, av.ty.bits, pv.ty.bits
                            ),
                        ));
                    }
                    if av.repr != pv.repr {
                        errors.push(VerifyError::new(
                            "V4 block-param repr",
                            format!(
                                "block{bi} → block{} arg #{i}: repr {:?} != param repr {:?}",
                                call.block.0, av.repr, pv.repr
                            ),
                        ));
                    }
                }
            }
        }
    }

    // V6 — the entry block has no predecessors. `preds` scans terminator targets
    // only; it is safe even when a target block is out of range (it compares by
    // handle equality and never indexes with a bad handle).
    if !f.preds(f.entry()).is_empty() {
        errors.push(VerifyError::new(
            "V6 entry-preds",
            "entry block has predecessors".to_string(),
        ));
    }

    // ── Dominator-based pass: V3, V8 dominance, V10 reducibility.
    // Only safe once every successor block handle is in range.
    if cfg_sound {
        let dom = f.dominators();

        // Resolve a value's definition to a CFG position (None ⇒ dangling).
        let def_loc = |v: Value| -> Option<DefLoc> {
            match f.value(v).def {
                ValueDef::Param { block, .. } => Some(DefLoc { block, pos: None }),
                // An Invoke result exists only on its successful edge. Users
                // must name the normal successor's block parameter instead.
                ValueDef::Result { inst, .. } if f.inst(inst).opcode == Opcode::Invoke => None,
                ValueDef::Result { inst, .. } => {
                    inst_loc.get(&inst.0).map(|&(block, pos)| DefLoc {
                        block,
                        pos: Some(pos),
                    })
                }
            }
        };

        // Does the definition of `v` dominate a use at (use_block, use_pos)?
        let dominates_use = |v: Value, use_block: Block, use_pos: usize| -> bool {
            match def_loc(v) {
                None => false,
                Some(d) if d.block == use_block => match d.pos {
                    None => true,           // block parameter: precedes all insts
                    Some(p) => p < use_pos, // result: must appear earlier
                },
                Some(d) => dom.dominates(d.block, use_block),
            }
        };

        for bi in 0..n_blocks {
            let b = Block(bi as u32);
            let bd = f.block(b);
            for (pos, &inst) in bd.insts.iter().enumerate() {
                let data = f.inst(inst);

                // V3 — ordinary value operands.
                for (ai, &arg) in data.args.iter().enumerate() {
                    if !valid_values.contains(&arg.0) {
                        errors.push(VerifyError::new(
                            "V1 dangling-value",
                            format!(
                                "block{bi} {:?} arg #{ai} v{} is not live",
                                data.opcode, arg.0
                            ),
                        ));
                        continue;
                    }
                    if !dominates_use(arg, b, pos) {
                        errors.push(VerifyError::new(
                            "V3 dominance",
                            format!(
                                "block{bi} {:?} arg #{ai} v{}: definition does not dominate use",
                                data.opcode, arg.0
                            ),
                        ));
                    }
                }

                // V3 — block-call arguments (their use site is the terminator).
                for (edge_index, call) in data.targets.iter().enumerate() {
                    if call.block.index() >= n_blocks {
                        continue; // already reported as V10 above
                    }
                    for (ai, &arg) in call.args.iter().enumerate() {
                        if !valid_values.contains(&arg.0) {
                            continue; // already reported as V1 above
                        }
                        let own_invoke_result = data.opcode == Opcode::Invoke
                            && matches!(f.value(arg).def, ValueDef::Result { inst: definition, .. } if definition == inst);
                        if own_invoke_result && edge_index != 0 {
                            errors.push(VerifyError::new(
                                "V11 invoke-result",
                                format!(
                                    "block{bi} Invoke result v{} used on exceptional edge",
                                    arg.0
                                ),
                            ));
                        }
                        let available =
                            (own_invoke_result && edge_index == 0) || dominates_use(arg, b, pos);
                        if !available {
                            errors.push(VerifyError::new(
                                "V3 dominance",
                                format!(
                                    "block{bi} → block{} arg #{ai} v{}: definition does not dominate terminator",
                                    call.block.0, arg.0
                                ),
                            ));
                        }
                    }
                }

                // ArrayRef is a semantic memory-layout contract, never call
                // provenance. Clones retain it; unknown metadata cannot widen
                // SIMPLE_VECTOR-only SVREF into an AREF byte-array operation.
                let array_read = matches!(data.opcode, Opcode::VecRef | Opcode::ArrayReadDiscarded);
                if matches!(data.aux, AuxData::ArrayRef) && !array_read
                    || array_read && !matches!(data.aux, AuxData::None | AuxData::ArrayRef)
                    || array_read && (data.args.len() != 2
                        || data.results.len() != usize::from(data.opcode == Opcode::VecRef)
                        || data.flags != (crate::t2::ir::InstFlags { guard: true, effectful: true, ..Default::default() })
                        || data.frame_state.is_none()
                        || data.args.iter().chain(&data.results).any(|v| valid_values.contains(&v.0)
                            && f.value(*v).repr != super::ir::ValueRepresentation::Tagged))
                {
                    errors.push(VerifyError::new("V8 array-read-contract",
                        format!("block{bi} malformed {:?} array-read contract", data.opcode)));
                }

                if matches!(data.aux, AuxData::SimpleVectorStore) && data.opcode != Opcode::VecSet
                    || data.opcode == Opcode::VecSet && (
                        !matches!(data.aux, AuxData::None | AuxData::SimpleVectorStore)
                        || data.args.len() != 3 || data.results.len() != 1
                        || data.flags != (crate::t2::ir::InstFlags {
                            guard: true, effectful: true, ..Default::default() })
                        || data.frame_state.is_none()
                        || data.args.iter().chain(&data.results).any(|v| valid_values.contains(&v.0)
                            && f.value(*v).repr != super::ir::ValueRepresentation::Tagged))
                {
                    errors.push(VerifyError::new("V8 array-store-contract",
                        format!("block{bi} malformed {:?} array-store contract", data.opcode)));
                }

                // V8 — StringByteLength consumes an explicitly layout-refined
                // SSA value and is a pure load. StringAsciiCharAt consumes the
                // same refinement but can still fail its bounds/ASCII assumptions,
                // so it remains an ordered deopt guard.
                if data.opcode == Opcode::StringByteLength
                    && (data.flags.guard || data.flags.effectful || data.frame_state.is_some())
                {
                    errors.push(VerifyError::new(
                        "V8 layout-guard-contract",
                        format!(
                            "block{bi} layout load {:?} is not a pure refined-value load",
                            data.opcode
                        ),
                    ));
                }
                if data.opcode == Opcode::StringAsciiCharAt
                    && (!data.flags.guard || !data.flags.effectful)
                {
                    errors.push(VerifyError::new(
                        "V8 layout-guard-contract",
                        format!(
                            "block{bi} character load {:?} is not an effectful guard",
                            data.opcode
                        ),
                    ));
                }

                // V8 — StringCharOrNil answers NIL instead of failing, so it
                // must not carry guard or deopt state, and stays ordered after
                // stores (effectful) without being a call or safepoint.
                if data.opcode == Opcode::StringCharOrNil
                    && (data.args.len() != 2
                        || data.results.len() != 1
                        || data.flags.guard
                        || !data.flags.effectful
                        || data.flags.call
                        || data.flags.safepoint
                        || data.frame_state.is_some())
                {
                    errors.push(VerifyError::new(
                        "V8 layout-guard-contract",
                        format!(
                            "block{bi} {:?} is not an effectful, unguarded two-operand load",
                            data.opcode
                        ),
                    ));
                }

                if data.opcode == Opcode::SmallVectorRangeP {
                    use crate::t2::ir::{TypeBits, ValueRepresentation as Repr};
                    let valid = data.args.iter().chain(&data.results).all(|v| valid_values.contains(&v.0));
                    if !valid || data.args.len() != 3 || data.results.len() != 1
                        || data.flags != crate::t2::ir::InstFlags::default()
                        || data.frame_state.is_some() || !matches!(data.aux, AuxData::None)
                        || (valid && data.results.len() == 1 &&
                            (data.args.iter().chain(&data.results).any(|&v| f.value(v).repr != Repr::Tagged)
                                || f.value(data.results[0]).ty.bits != TypeBits::SYMBOL.join(TypeBits::NULL))) {
                        errors.push(VerifyError::new("V8 vector-range-predicate-contract",
                            format!("block{bi} requires three tagged operands and one pure Boolean result")));
                    }
                }
                if data.opcode == Opcode::VecSetInRange {
                    use crate::t2::ir::ValueRepresentation as Repr;
                    let valid = data.args.iter().chain(&data.results).all(|v| valid_values.contains(&v.0));
                    if !valid || data.args.len() != 4 || data.results.len() != 1
                        || data.flags != (crate::t2::ir::InstFlags {
                            effectful: true, guard: true, ..Default::default() })
                        || data.frame_state.is_none() || !matches!(data.aux, AuxData::None)
                        || data.args.iter().chain(&data.results).any(|&v| f.value(v).repr != Repr::Tagged) {
                        errors.push(VerifyError::new("V8 vector-range-access-contract",
                            format!("block{bi} requires a guarded tagged store and its SSA witness")));
                    }
                }
                if data.opcode == Opcode::PackedU8Layout {
                    use crate::t2::ir::{TypeBits, ValueRepresentation as Repr};
                    let valid = data.args.iter().chain(&data.results).all(|v| valid_values.contains(&v.0));
                    if !valid || data.args.len() != 1 || data.results.len() != 1
                        || data.flags != crate::t2::ir::InstFlags::default()
                        || data.frame_state.is_some() || !data.targets.is_empty() || !matches!(data.aux, AuxData::None)
                        || (valid && data.results.len() == 1 &&
                            (data.args.iter().chain(&data.results).any(|&v| f.value(v).repr != Repr::Tagged)
                                || f.value(data.results[0]).ty.bits != TypeBits::FIXNUM.join(TypeBits::NULL))) {
                        errors.push(VerifyError::new("V8 packed-u8-layout-contract", format!("block{bi} requires one tagged array and a pure tagged width/NIL result")));
                    }
                }
                if matches!(data.opcode, Opcode::TryStoreIntegerOctets8 | Opcode::TryStoreIntegerOctets1To8 | Opcode::TryStoreIntegerOctets1To8Layout) {
                    use crate::t2::ir::{TypeBits, ValueRepresentation as Repr};
                    let valid = data.args.iter().chain(&data.results)
                        .all(|value| valid_values.contains(&value.0));
                    if !valid || data.args.len() != if data.opcode == Opcode::TryStoreIntegerOctets1To8Layout { 5 } else { 4 }
                        || data.results.len() != 2
                        || data.flags != (crate::t2::ir::InstFlags { effectful: true, ..Default::default() })
                        || data.frame_state.is_some() || !data.targets.is_empty()
                        || !matches!(data.aux, AuxData::None)
                        || (valid && data.results.len() == 2 &&
                            (data.args.iter().chain(&data.results).any(|&v| f.value(v).repr != Repr::Tagged)
                                || f.value(data.results[0]).ty.bits != TypeBits::SYMBOL.join(TypeBits::NULL)
                                || f.value(data.results[1]).ty.bits != TypeBits::FIXNUM)) {
                        errors.push(VerifyError::new("V8 octet-word-store-contract",
                            format!("block{bi} requires four tagged operands and effectful-only (boolean, fixnum) results")));
                    }
                }
                if data.opcode == Opcode::NativePoll {
                    // Source-backed scope metadata survives ordinary lowering,
                    // which legitimately removes the OSR import records.
                    let scope_proven = data.frame_state.filter(|id| (id.0 as usize) < f.frame_states.len())
                        .is_some_and(|id| {
                            let [frame] = f.frame_states.get(id).scopes.as_slice() else { return false; };
                            let AuxData::BoundedPoll { scope, .. } = &data.aux else { return false; };
                            scope.function == frame.function && scope.bcp == frame.bcp
                                && crate::t2::octet_word_fusion::bounded_poll_scopes_supported(&scope.control_scopes)
                        });
                    if !scope_proven || pos != 0 || !data.args.is_empty() || !data.results.is_empty()
                        || !data.targets.is_empty() || data.frame_state.is_none()
                        || data.flags != (crate::t2::ir::InstFlags {
                            effectful: true, safepoint: true, ..Default::default() })
                        || !matches!(data.aux, AuxData::BoundedPoll { .. }) {
                        errors.push(VerifyError::new("V8 bounded-native-poll-contract",
                            format!("block{bi} requires a first-in-block effectful safepoint with an exact FrameState")));
                    }
                }

                if data.opcode == Opcode::TryIntegerShr {
                    use crate::t2::ir::{TypeBits, ValueRepresentation as Repr};
                    let valid = data.args.iter().chain(&data.results).all(|v| valid_values.contains(&v.0));
                    if !valid || data.args.len() != 1 || data.results.len() != 2
                        || data.flags != crate::t2::ir::InstFlags::default()
                        || data.frame_state.is_some() || !matches!(data.aux, AuxData::FixnumImm(n) if n > 0)
                        || (valid && data.results.len() == 2 &&
                            (data.args.iter().chain(&data.results).any(|&v| f.value(v).repr != Repr::Tagged)
                                || f.value(data.results[0]).ty.bits != TypeBits::SYMBOL.join(TypeBits::NULL)
                                || f.value(data.results[1]).ty.bits != TypeBits::FIXNUM)) {
                        errors.push(VerifyError::new("V8 integer-shift-probe-contract",
                            format!("block{bi} requires tagged integer input and (boolean, fixnum) outputs")));
                    }
                }

                if data.opcode == Opcode::TryPackedF64Set {
                    use crate::t2::ir::{TypeBits, ValueRepresentation as Repr};
                    let valid = data.args.iter().chain(&data.results)
                        .all(|value| valid_values.contains(&value.0));
                    if !valid || data.args.len() != 3 || data.results.len() != 1
                        || data.flags != (crate::t2::ir::InstFlags {
                            effectful: true, ..crate::t2::ir::InstFlags::default() })
                        || data.frame_state.is_some() || !matches!(data.aux, AuxData::None)
                        || (valid && data.args.len() == 3 && data.results.len() == 1
                            && (data.args[..2].iter().any(|&v| f.value(v).repr != Repr::Tagged)
                                || f.value(data.args[2]).repr != Repr::UnboxedF64
                                || f.value(data.args[2]).ty.bits != TypeBits::DOUBLE_FLOAT
                                || f.value(data.results[0]).repr != Repr::Tagged)) {
                        errors.push(VerifyError::new("V8 packed-store-probe-contract",
                            format!("block{bi} requires tagged array/index, raw double and tagged success flag")));
                    }
                }

                if matches!(data.opcode, Opcode::PackedF64Ref | Opcode::VecF64Ref | Opcode::PackedF64Set) {
                    use crate::t2::ir::{TypeBits, ValueRepresentation as Repr};
                    let load = matches!(data.opcode, Opcode::PackedF64Ref | Opcode::VecF64Ref);
                    let valid = data.args.iter().chain(&data.results)
                        .all(|value| valid_values.contains(&value.0));
                    let shape = data.args.len() == (if load { 2 } else { 3 })
                        && data.results.len() == usize::from(load);
                    let raw_double = |value| f.value(value).repr == Repr::UnboxedF64
                        && f.value(value).ty.bits == TypeBits::DOUBLE_FLOAT;
                    if !valid || !shape || !data.flags.guard || !data.flags.effectful
                        || data.flags.call || data.flags.safepoint || data.flags.terminator
                        || data.frame_state.is_none() || !matches!(data.aux, AuxData::None)
                        || (valid && shape && (data.args[..2].iter().any(|&value|
                            f.value(value).repr != Repr::Tagged)
                            || !raw_double(if load { data.results[0] } else { data.args[2] })))
                    {
                        errors.push(VerifyError::new("V8 packed-f64-contract",
                            format!("block{bi} {:?} requires tagged array/index, raw double and ordered recovery", data.opcode)));
                    }
                }

                // V8 — guard/FrameState well-formedness.
                if data.flags.guard || matches!(data.opcode, Opcode::Invoke | Opcode::NlxTransfer) {
                    match data.frame_state {
                        None => errors.push(VerifyError::new(
                            "V8 guard-framestate",
                            format!("block{bi} guard {:?} has no frame_state", data.opcode),
                        )),
                        Some(id) if id.0 as usize >= f.frame_states.len() => {
                            errors.push(VerifyError::new(
                                "V8 framestate-handle",
                                format!("block{bi} has nonexistent frame_state {}", id.0),
                            ));
                        }
                        Some(id) => {
                            let fs = f.frame_states.get(id);
                            check_frame_state(
                                fs,
                                b,
                                pos,
                                &valid_values,
                                &dominates_use,
                                bi,
                                &mut errors,
                            );
                        }
                    }
                }
            }
        }

        // V10 — reducibility: every retreating edge (a successor earlier in RPO)
        // must target a block that dominates its source, or the CFG is
        // irreducible.
        for bi in 0..n_blocks {
            let u = Block(bi as u32);
            let u_rpo = dom.rpo_num(u);
            if u_rpo == usize::MAX {
                continue; // unreachable source: no natural-loop obligation
            }
            for v in f.succs(u) {
                let v_rpo = dom.rpo_num(v);
                if v_rpo == usize::MAX {
                    continue;
                }
                if v_rpo <= u_rpo && !dom.dominates(v, u) {
                    errors.push(VerifyError::new(
                        "V10 reducibility",
                        format!(
                            "irreducible: retreating edge block{} → block{} whose target does not dominate its source",
                            u.0, v.0
                        ),
                    ));
                }
            }
        }
    }

    // Structural queries run only after SSA and metadata validation: malformed
    // value/edge indices must produce diagnostics rather than analysis panics.
    if errors.is_empty() {
        let facts = super::value_facts::StructuralFacts::analyze(f);
        for &block in f.block_order() {
            for &inst in &f.block(block).insts {
                let d = f.inst(inst);
                if matches!(d.opcode, Opcode::StringByteLength | Opcode::StringAsciiCharAt)
                    && !d.args.first().is_some_and(|&subject|
                        facts.refined_layout_before(subject, super::value_facts::LayoutKind::SimpleString, inst)
                            .is_some_and(|proof| proof.encoding == super::value_facts::LayoutEvidence::RefinedValue))
                {
                    errors.push(VerifyError::new("V8 layout-proof",
                        format!("{inst:?} lacks a dominating successful simple-string layout proof")));
                }
            }
        }
    }
    if errors.is_empty() {
        verify_cleanup_continuations(f, &mut errors);
    }
    if errors.is_empty() {
        if let Err(error) = crate::t2::vector_range_proof::verify_accesses(f) {
            errors.push(VerifyError::new("V8 vector-range-proof", error));
        }
    }
    if errors.is_empty() {
        if let Err(error) = crate::t2::array_layout::verify_accesses(f) {
            errors.push(VerifyError::new("V8 packed-u8-layout-proof", error));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// A normal cleanup owns an execution-local saved value tuple. Joins must agree
/// on the complete ordered continuation stack, not merely its depth. Exceptional
/// exits retain that stack in their scope metadata for the unwind cursor.
fn verify_cleanup_continuations(f: &Function, errors: &mut Vec<VerifyError>) {
    use crate::control_scope::ScopeKind;
    use crate::t2::ir::ValueRepresentation;
    if !f.block_order().iter().any(|&b| {
        f.block(b).insts.iter().any(|&i| {
            matches!(&f.inst(i).aux, AuxData::CleanupContinuation { .. })
                || (f.inst(i).opcode == Opcode::NlxTransfer && !f.inst(i).targets.is_empty())
                || matches!(
                    f.inst(i).opcode,
                    Opcode::CleanupSave | Opcode::CleanupRestore | Opcode::CleanupLanding
                )
        })
    }) {
        return;
    }
    let mut incoming = HashMap::from([(f.entry(), Vec::<(u32, u32)>::new())]);
    let mut work = vec![f.entry()];
    while let Some(block) = work.pop() {
        let mut stack = incoming[&block].clone();
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            if data.opcode == Opcode::HandlerLanding
                && (!matches!(data.aux, AuxData::HandlerDestination { .. })
                    || !data.args.is_empty()
                    || data.results.len() != 1
                    || !data.flags.effectful
                    || !data.flags.call
                    || data.flags.safepoint
                    || data.flags.terminator
                    || !data.targets.is_empty()
                    || f.block(block).insts.first() != Some(&inst))
            {
                errors.push(VerifyError::new(
                    "V15 handler",
                    "invalid handler landing shape",
                ));
            }
            if data.opcode == Opcode::CatchLanding
                && (!matches!(data.aux, AuxData::CatchDestination { .. })
                    || !data.args.is_empty()
                    || data.results.len() != 1
                    || !data.flags.effectful
                    || !data.flags.call
                    || data.flags.safepoint
                    || data.flags.terminator
                    || !data.targets.is_empty()
                    || f.block(block).insts.first() != Some(&inst))
            {
                errors.push(VerifyError::new("V14 catch", "invalid catch landing shape"));
            }
            if matches!(&data.aux, AuxData::CleanupContinuation { .. })
                && !matches!(
                    data.opcode,
                    Opcode::CleanupSave
                        | Opcode::CleanupRestore
                        | Opcode::CleanupLanding
                        | Opcode::Invoke
                )
            {
                errors.push(VerifyError::new(
                    "V13 cleanup",
                    "continuation metadata on a non-cleanup operation",
                ));
            }
            if matches!(data.opcode, Opcode::CleanupSave | Opcode::CleanupRestore) {
                let save = data.opcode == Opcode::CleanupSave;
                let valid_shape = data.args.len() == usize::from(save)
                    && data.results.len() == usize::from(!save)
                    && data
                        .args
                        .iter()
                        .chain(&data.results)
                        .all(|&v| f.value(v).repr == ValueRepresentation::Tagged)
                    && data.flags.effectful
                    && data.flags.call
                    && data.flags.safepoint
                    && !data.flags.terminator
                    && data.frame_state.is_some();
                let AuxData::CleanupContinuation {
                    cleanup_bcp,
                    resume_bcp,
                } = data.aux
                else {
                    errors.push(VerifyError::new(
                        "V13 cleanup",
                        "missing continuation identity",
                    ));
                    continue;
                };
                if !valid_shape {
                    errors.push(VerifyError::new(
                        "V13 cleanup",
                        "invalid cleanup effects or value shape",
                    ));
                }
                if save {
                    stack.push((cleanup_bcp, resume_bcp));
                } else if stack.pop() != Some((cleanup_bcp, resume_bcp)) {
                    errors.push(VerifyError::new(
                        "V13 cleanup",
                        "restore does not match active continuation",
                    ));
                }
            }
            if matches!(data.opcode, Opcode::CleanupLanding | Opcode::Invoke)
                && matches!(data.aux, AuxData::CleanupContinuation { .. })
            {
                let AuxData::CleanupContinuation {
                    cleanup_bcp,
                    resume_bcp,
                } = data.aux
                else {
                    unreachable!()
                };
                let landing = data.opcode == Opcode::CleanupLanding;
                let valid = data.args.is_empty()
                    && data.results.len() == usize::from(!landing)
                    && data
                        .results
                        .iter()
                        .all(|&v| f.value(v).repr == ValueRepresentation::Tagged)
                    && data.flags.effectful
                    && data.frame_state.is_some()
                    && (!landing
                        || data.frame_state.is_some_and(|id| {
                            f.frame_states
                                .get(id)
                                .scopes
                                .last()
                                .is_some_and(|frame| frame.bcp == cleanup_bcp)
                        }))
                    && if landing {
                        !data.flags.call && !data.flags.terminator && data.targets.is_empty()
                    } else {
                        data.flags.call && data.flags.safepoint && data.flags.terminator
                    };
                if !valid || stack.last() != Some(&(cleanup_bcp, resume_bcp)) {
                    errors.push(VerifyError::new(
                        "V13 cleanup",
                        "invalid landing or cleanup dispatch state",
                    ));
                }
                if landing && f.block(block).insts.first() != Some(&inst) {
                    errors.push(VerifyError::new(
                        "V13 cleanup",
                        "cleanup landing must start its block",
                    ));
                }
            }
            if data.opcode == Opcode::CleanupLanding
                && !matches!(data.aux, AuxData::CleanupContinuation { .. })
            {
                errors.push(VerifyError::new("V13 cleanup", "missing landing identity"));
            }
            if matches!(data.opcode, Opcode::Return | Opcode::TailCall) && !stack.is_empty() {
                errors.push(VerifyError::new(
                    "V13 cleanup",
                    "normal return abandons saved cleanup values",
                ));
            }
            if let AuxData::TransferSite { scopes, .. } = &data.aux {
                let captured: Vec<_> = scopes
                    .iter()
                    .filter_map(|scope| match scope.kind {
                        ScopeKind::Cleanup { cleanup_bcp } => Some(cleanup_bcp),
                        _ => None,
                    })
                    .collect();
                if !captured
                    .iter()
                    .copied()
                    .eq(stack.iter().map(|&(cleanup, _)| cleanup))
                {
                    errors.push(VerifyError::new(
                        "V13 cleanup",
                        "transfer lost active cleanup continuation",
                    ));
                }
            }
            for (edge_index, target) in data.targets.iter().enumerate() {
                let mut outgoing = stack.clone();
                let handler_landing = f
                    .block(target.block)
                    .insts
                    .first()
                    .map(|&i| f.inst(i))
                    .filter(|landing| landing.opcode == Opcode::HandlerLanding);
                if handler_landing.is_some() && data.opcode != Opcode::NlxTransfer {
                    errors.push(VerifyError::new(
                        "V15 handler",
                        "handler landing requires an exceptional edge",
                    ));
                }
                let catch_landing = f
                    .block(target.block)
                    .insts
                    .first()
                    .map(|&i| f.inst(i))
                    .filter(|landing| landing.opcode == Opcode::CatchLanding);
                if catch_landing.is_some() && data.opcode != Opcode::NlxTransfer {
                    errors.push(VerifyError::new(
                        "V14 catch",
                        "catch landing requires an exceptional edge",
                    ));
                }
                if data.opcode == Opcode::Invoke
                    && edge_index == 0
                    && matches!(data.aux, AuxData::CleanupContinuation { .. })
                {
                    outgoing.pop();
                }
                if data.opcode == Opcode::NlxTransfer {
                    let AuxData::TransferSite { scopes, .. } = &data.aux else {
                        unreachable!()
                    };
                    if let Some(landing) = handler_landing {
                        let valid = match landing.aux {
                            AuxData::HandlerDestination {
                                push_bcp,
                                table_index,
                                clause_index,
                            } => {
                                let selected = scopes
                                    .iter()
                                    .enumerate()
                                    .find(|(_, scope)| scope.push_bcp == push_bcp);
                                let clause = f
                                    .handler_cases
                                    .get(table_index as usize)
                                    .and_then(|info| info.clauses.get(clause_index as usize));
                                selected
                                    .zip(clause)
                                    .filter(|((index, scope), clause)| {
                                        scope.ownership == crate::control_scope::Ownership::Local
                                            && scope.kind == ScopeKind::HandlerCase { table_index }
                                            && !scopes[index + 1..].iter().any(|s| {
                                                s.ownership
                                                    != crate::control_scope::Ownership::Local
                                                    || matches!(s.kind, ScopeKind::Unwind { .. })
                                            })
                                            && landing
                                                .frame_state
                                                .and_then(|id| f.frame_states.get(id).scopes.last())
                                                .is_some_and(|frame| {
                                                    frame.bcp == clause.body_bcp
                                                        && frame.stack.len()
                                                            == usize::from(scope.sp_restore)
                                                        && clause.var_slot.is_none_or(|slot| {
                                                            usize::from(slot) < frame.locals.len()
                                                        })
                                                })
                                    })
                                    .map(|((index, _), _)| {
                                        let retained: Vec<_> = scopes[..index]
                                            .iter()
                                            .filter_map(|scope| {
                                                if let ScopeKind::Cleanup { cleanup_bcp } =
                                                    scope.kind
                                                {
                                                    Some(cleanup_bcp)
                                                } else {
                                                    None
                                                }
                                            })
                                            .collect();
                                        outgoing.retain(|(cleanup, _)| retained.contains(cleanup));
                                    })
                                    .is_some()
                            }
                            _ => false,
                        };
                        if !valid {
                            errors.push(VerifyError::new(
                                "V15 handler",
                                "handler landing does not match its live clause",
                            ));
                        }
                    } else if let Some(landing) = catch_landing {
                        let valid = match landing.aux {
                            AuxData::CatchDestination {
                                push_bcp,
                                resume_bcp,
                            } => scopes
                                .iter()
                                .enumerate()
                                .find(|(_, scope)| scope.push_bcp == push_bcp)
                                .filter(|(index, scope)| {
                                    scope.ownership == crate::control_scope::Ownership::Local
                                        && scope.kind == (ScopeKind::Catch { resume_bcp })
                                        && !scopes[index + 1..]
                                            .iter()
                                            .any(|s| matches!(s.kind, ScopeKind::Unwind { .. }))
                                })
                                .filter(|(_, scope)| {
                                    landing
                                        .frame_state
                                        .and_then(|id| f.frame_states.get(id).scopes.last())
                                        .is_some_and(|frame| {
                                            frame.bcp == resume_bcp
                                                && frame.stack.len()
                                                    == usize::from(scope.sp_restore)
                                        })
                                })
                                .map(|(index, _)| {
                                    let retained: Vec<_> = scopes[..index]
                                        .iter()
                                        .filter_map(|s| match s.kind {
                                            ScopeKind::Cleanup { cleanup_bcp } => Some(cleanup_bcp),
                                            _ => None,
                                        })
                                        .collect();
                                    outgoing.retain(|(cleanup, _)| retained.contains(cleanup));
                                })
                                .is_some(),
                            _ => false,
                        };
                        if !valid {
                            errors.push(VerifyError::new(
                                "V14 catch",
                                "catch edge skips cleanup or names an invalid target",
                            ));
                        }
                    } else {
                        let selected = scopes
                            .iter()
                            .rposition(|scope| matches!(scope.kind, ScopeKind::Unwind { .. }));
                        let landing = f.block(target.block).insts.first().map(|&i| f.inst(i));
                        let valid = selected
                            .zip(landing)
                            .and_then(|(index, landing)| {
                                let ScopeKind::Unwind { cleanup_bcp } = scopes[index].kind else {
                                    return None;
                                };
                                let AuxData::CleanupContinuation {
                                    cleanup_bcp: destination,
                                    resume_bcp,
                                } = landing.aux
                                else {
                                    return None;
                                };
                                let fs = landing
                                    .frame_state
                                    .and_then(|id| f.frame_states.get(id).scopes.last());
                                if landing.opcode != Opcode::CleanupLanding
                                    || destination != cleanup_bcp
                                    || !fs.is_some_and(|frame| {
                                        frame.bcp == cleanup_bcp
                                            && frame.stack.len()
                                                == usize::from(scopes[index].sp_restore)
                                    })
                                {
                                    return None;
                                }
                                let retained: Vec<_> = scopes[..index]
                                    .iter()
                                    .filter_map(|scope| {
                                        if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
                                            Some(cleanup_bcp)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                outgoing.retain(|(cleanup, _)| retained.contains(cleanup));
                                outgoing.push((cleanup_bcp, resume_bcp));
                                Some(())
                            })
                            .is_some();
                        if !valid {
                            errors.push(VerifyError::new(
                                "V13 cleanup",
                                "transfer target is not the selected cleanup landing",
                            ));
                        }
                    }
                }
                if let Some(prior) = incoming.get(&target.block) {
                    if prior != &outgoing {
                        errors.push(VerifyError::new(
                            "V13 cleanup",
                            "join disagrees on active continuations",
                        ));
                    }
                } else {
                    incoming.insert(target.block, outgoing);
                    work.push(target.block);
                }
            }
        }
    }
}

/// V8 (decidable portion): every SSA value named by a
/// `FrameState` must dominate the deoptimising instruction, and the
/// rematerialisation recipes must be acyclic.
fn check_frame_state(
    fs: &FrameState,
    use_block: Block,
    use_pos: usize,
    valid_values: &HashSet<u32>,
    dominates_use: &dyn Fn(Value, Block, usize) -> bool,
    bi: usize,
    errors: &mut Vec<VerifyError>,
) {
    // Named SSA values (in scopes and in remat recipe inputs) must dominate.
    let mut check_source = |src: &ValueSource| {
        if let ValueSource::Value { value, .. } = src {
            if !valid_values.contains(&value.0) {
                errors.push(VerifyError::new(
                    "V8 framestate-dominance",
                    format!("block{bi} frame_state names non-live v{}", value.0),
                ));
            } else if !dominates_use(*value, use_block, use_pos) {
                errors.push(VerifyError::new(
                    "V8 framestate-dominance",
                    format!(
                        "block{bi} frame_state value v{} does not dominate the guard",
                        value.0
                    ),
                ));
            }
        }
    };
    for scope in &fs.scopes {
        for src in scope.locals.iter().chain(scope.stack.iter()) {
            check_source(src);
        }
    }
    for recipe in &fs.remat {
        for src in &recipe.inputs {
            check_source(src);
        }
    }

    // Rematerialisation recipes must be acyclic: recipe i depends on recipe j
    // whenever i has an input `ValueSource::Remat(j)`.
    let n = fs.remat.len();
    // 0 = unvisited, 1 = on stack, 2 = done.
    let mut state = vec![0u8; n];
    let mut cyclic = false;
    // Iterative DFS to avoid unbounded native recursion.
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        state[start] = 1;
        while let Some(&mut (node, ref mut edge)) = stack.last_mut() {
            let inputs = &fs.remat[node].inputs;
            // Advance to the next Remat edge from this node.
            let mut next: Option<usize> = None;
            while *edge < inputs.len() {
                let idx = *edge;
                *edge += 1;
                if let ValueSource::Remat(rid) = inputs[idx] {
                    let j = rid.0 as usize;
                    if j >= n {
                        errors.push(VerifyError::new(
                            "V8 remat-recipe",
                            format!("block{bi} remat recipe references out-of-range recipe {j}"),
                        ));
                        continue;
                    }
                    match state[j] {
                        1 => {
                            cyclic = true; // back edge
                        }
                        0 => {
                            next = Some(j);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            match next {
                Some(j) => {
                    state[j] = 1;
                    stack.push((j, 0));
                }
                None => {
                    state[node] = 2;
                    stack.pop();
                }
            }
        }
    }
    if cyclic {
        errors.push(VerifyError::new(
            "V8 remat-recipe",
            format!("block{bi} frame_state has a cyclic rematerialisation recipe"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, FrameStateId, ValueSource};
    use crate::t2::ir::{
        AuxData, BlockCall, IRType, InstData, InstFlags, Opcode, TypeBits, Value,
        ValueRepresentation,
    };

    // ── Small hand-built-IR helpers ─────────────────────────────────────

    fn fixnum() -> IRType {
        IRType::of(TypeBits::FIXNUM)
    }

    fn inst(opcode: Opcode) -> InstData {
        InstData {
            opcode,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 1,
        }
    }

    fn jump(to: Block, args: Vec<Value>) -> InstData {
        InstData {
            targets: vec![BlockCall { block: to, args }],
            ..inst(Opcode::Jump)
        }
    }

    fn ret(vals: Vec<Value>) -> InstData {
        InstData {
            args: vals,
            ..inst(Opcode::Return)
        }
    }

    fn const_fixnum(f: &mut Function, block: Block, imm: i64) -> Value {
        let (_, r) = f.push_inst(
            block,
            InstData {
                aux: AuxData::FixnumImm(imm),
                ..inst(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        r[0]
    }

    fn invoke_graph() -> (Function, crate::t2::ir::Inst, Value) {
        let mut f = Function::new("invoke");
        let entry = f.entry();
        let normal = f.make_block();
        let exceptional = f.make_block();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let returned = f.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let saved = f.add_block_param(exceptional, IRType::TOP, ValueRepresentation::Tagged);
        let frame = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let (invoke, values) = f.set_terminator_with_results(
            entry,
            InstData {
                args: vec![input],
                aux: AuxData::CallTarget(1),
                flags: InstFlags {
                    call: true,
                    effectful: true,
                    safepoint: true,
                    ..Default::default()
                },
                targets: vec![
                    BlockCall {
                        block: normal,
                        args: vec![],
                    },
                    BlockCall {
                        block: exceptional,
                        args: vec![input],
                    },
                ],
                frame_state: Some(frame),
                ..inst(Opcode::Invoke)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.inst_mut(invoke).targets[0].args.push(values[0]);
        f.set_terminator(normal, ret(vec![returned]));
        f.set_terminator(exceptional, ret(vec![saved]));
        (f, invoke, values[0])
    }

    #[test]
    fn invoke_edges_expose_exception_only_live_values() {
        let (f, _, _) = invoke_graph();
        assert!(verify(&f).is_ok(), "{:?}", verify(&f));
        assert_eq!(f.succs(f.entry()), vec![Block(1), Block(2)]);
        assert_eq!(f.preds(Block(2)), vec![f.entry()]);
    }

    #[test]
    fn invoke_result_cannot_flow_to_exceptional_successor() {
        let (mut f, invoke, result) = invoke_graph();
        f.inst_mut(invoke).targets[1].args[0] = result;
        assert!(verify(&f)
            .unwrap_err()
            .iter()
            .any(|e| e.check == "V11 invoke-result"));
    }

    #[test]
    fn invoke_result_must_be_projected_through_normal_block_parameter() {
        let (mut f, _, result) = invoke_graph();
        let ret = f.terminator(Block(1)).unwrap();
        f.inst_mut(ret).args[0] = result;
        assert!(verify(&f)
            .unwrap_err()
            .iter()
            .any(|e| e.check == "V3 dominance"));
    }

    #[test]
    fn invoke_requires_two_distinct_successors_and_runtime_effects() {
        for mutation in 0..5 {
            let (mut f, invoke, _) = invoke_graph();
            let d = f.inst_mut(invoke);
            match mutation {
                0 => {
                    d.targets.pop();
                }
                1 => d.targets[1].block = d.targets[0].block,
                2 => d.flags.call = false,
                3 => d.flags.effectful = false,
                _ => d.flags.safepoint = false,
            }
            assert!(verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V11 invoke-shape"));
        }
    }

    #[test]
    fn invoke_requires_frame_state_before_the_call() {
        let (mut f, invoke, _) = invoke_graph();
        f.inst_mut(invoke).frame_state = None;
        assert!(verify(&f)
            .unwrap_err()
            .iter()
            .any(|e| e.check == "V8 guard-framestate"));
    }

    #[test]
    fn invoke_result_is_not_available_in_pre_call_frame_state() {
        let (mut f, invoke, result) = invoke_graph();
        let frame = f.inst(invoke).frame_state.unwrap();
        f.frame_states.get_mut(frame).scopes.push(FrameScope {
            function: 1,
            bcp: 0,
            locals: vec![ValueSource::Value {
                value: result,
                repr: ValueRepresentation::Tagged,
            }],
            stack: vec![],
        });
        assert!(verify(&f)
            .unwrap_err()
            .iter()
            .any(|e| e.check == "V8 framestate-dominance"));
    }

    #[test]
    fn invoke_invalid_frame_handle_is_reported_without_panicking() {
        let (mut f, invoke, _) = invoke_graph();
        f.inst_mut(invoke).frame_state = Some(FrameStateId(u32::MAX));
        assert!(verify(&f)
            .unwrap_err()
            .iter()
            .any(|e| e.check == "V8 framestate-handle"));
    }

    #[test]
    fn invoke_exception_only_value_survives_dead_code_elimination() {
        use crate::t2::pass::Pass;
        let (mut f, invoke, _) = invoke_graph();
        let entry = f.entry();
        assert_eq!(f.block_mut(entry).insts.pop(), Some(invoke));
        let (constant, values) = f.push_inst(
            entry,
            inst(Opcode::ConstNil),
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.block_mut(entry).insts.push(invoke);
        f.inst_mut(invoke).targets[1].args = values;
        crate::t2::opt_dce::Dce.run(&mut f, &mut crate::t2::pass::Analyses::new());
        assert!(f.block(entry).insts.contains(&constant));
        assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    }

    #[test]
    fn invoke_machine_emission_is_explicitly_unsupported_until_landing_pads_exist() {
        use crate::t2::emit::{emit, EmitError};
        use crate::t2::lower::op;
        let (f, _, _) = invoke_graph();
        let machine = crate::t2::lower::lower(&f);
        assert!(matches!(
            emit(&machine),
            Err(EmitError::UnsupportedOp(op::INVOKE))
        ));
    }

    #[test]
    fn invoke_inlining_preserves_edge_local_results() {
        let (mut callee, _, _) = invoke_graph();
        for index in 0..callee.num_insts() {
            callee
                .inst_mut(crate::t2::ir::Inst(index as u32))
                .source_pos = 0;
        }
        let mut caller = Function::new("invoke-caller");
        let entry = caller.entry();
        let input = caller.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (call, result) = caller.push_inst(
            entry,
            InstData {
                args: vec![input],
                aux: AuxData::CallTarget(7),
                source_pos: 0,
                ..inst(Opcode::Call)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        caller.set_terminator(entry, ret(result));
        caller.inline_call(call, &callee, &[]).unwrap();
        assert!(verify(&caller).is_ok(), "{:?}", verify(&caller));
        assert!(caller.block_order().iter().any(|&block| {
            caller
                .terminator(block)
                .is_some_and(|i| caller.inst(i).opcode == Opcode::Invoke)
        }));
    }

    /// `block_order` is the code LAYOUT, and a pass may leave a block ahead of
    /// one that dominates it -- `admit_nil_list_loads` does exactly that when
    /// it splits a block twice, which is how a ppc64le T2 worker came to abort
    /// on a missing value-map entry. Cloning an inlined callee must follow
    /// dominance, not layout, or an edge argument gets translated before the
    /// definition it names.
    ///
    /// The callee here is valid SSA: `test` dominates both arms and the join,
    /// and both arms pass a value `test` defines. Only its layout disagrees.
    #[test]
    fn inlining_follows_dominance_when_the_layout_disagrees() {
        let mut callee = Function::new("nil-armed-callee");
        let entry = callee.entry();
        let operand = callee.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        // Allocated -- and so laid out -- before the block that dominates them.
        let nil_arm = callee.make_block();
        let cons_arm = callee.make_block();
        let join = callee.make_block();
        let test = callee.make_block();
        assert_eq!(
            callee.block_order(),
            [entry, nil_arm, cons_arm, join, test],
            "the regression needs the dominator laid out last"
        );

        callee.set_terminator(entry, jump(test, vec![]));
        let (_, nil) = callee.push_inst(
            test,
            inst(Opcode::ConstNil),
            &[(
                IRType::of(TypeBits::NULL),
                ValueRepresentation::Tagged,
            )],
        );
        let (_, is_nil) = callee.push_inst(
            test,
            InstData {
                args: vec![operand, nil[0]],
                ..inst(Opcode::GenericEq)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        callee.set_terminator(
            test,
            InstData {
                args: vec![is_nil[0]],
                targets: vec![
                    BlockCall {
                        block: nil_arm,
                        args: vec![],
                    },
                    BlockCall {
                        block: cons_arm,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        let merged = callee.add_block_param(join, IRType::TOP, ValueRepresentation::Tagged);
        // Both arms name `nil`, which `test` defines: the forward reference
        // that layout-order cloning could not resolve.
        callee.set_terminator(nil_arm, jump(join, vec![nil[0]]));
        callee.set_terminator(cons_arm, jump(join, vec![nil[0]]));
        callee.set_terminator(join, ret(vec![merged]));
        for index in 0..callee.num_insts() {
            callee
                .inst_mut(crate::t2::ir::Inst(index as u32))
                .source_pos = 0;
        }
        assert!(verify(&callee).is_ok(), "callee: {:?}", verify(&callee));

        let mut caller = Function::new("nil-armed-caller");
        let caller_entry = caller.entry();
        let input = caller.add_block_param(caller_entry, IRType::TOP, ValueRepresentation::Tagged);
        let (call, result) = caller.push_inst(
            caller_entry,
            InstData {
                args: vec![input],
                aux: AuxData::CallTarget(11),
                source_pos: 0,
                ..inst(Opcode::Call)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        caller.set_terminator(caller_entry, ret(result));
        caller
            .inline_call(call, &callee, &[])
            .expect("a callee whose layout is not reverse postorder still inlines");
        assert!(verify(&caller).is_ok(), "caller: {:?}", verify(&caller));
    }

    // ── Positive: a valid single-block function verifies clean ──────────

    #[test]
    fn valid_straight_line_ok() {
        let mut f = Function::new("ok");
        let e = f.entry();
        let a = const_fixnum(&mut f, e, 1);
        let b = const_fixnum(&mut f, e, 2);
        let (_, sum) = f.push_inst(
            e,
            InstData {
                args: vec![a, b],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(e, ret(vec![sum[0]]));
        assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    }

    // ── Positive: a valid diamond with a merge parameter ────────────────

    #[test]
    fn valid_diamond_with_param_ok() {
        let mut f = Function::new("diamond");
        let e = f.entry();
        let b1 = f.make_block();
        let b2 = f.make_block();
        let merge = f.make_block();
        let mp = f.add_block_param(merge, fixnum(), ValueRepresentation::UnboxedFixnum);

        let cond = const_fixnum(&mut f, e, 1);
        f.set_terminator(
            e,
            InstData {
                args: vec![cond],
                targets: vec![
                    BlockCall {
                        block: b1,
                        args: vec![],
                    },
                    BlockCall {
                        block: b2,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        let v1 = const_fixnum(&mut f, b1, 10);
        f.set_terminator(b1, jump(merge, vec![v1]));
        let v2 = const_fixnum(&mut f, b2, 20);
        f.set_terminator(b2, jump(merge, vec![v2]));
        f.set_terminator(merge, ret(vec![mp]));

        assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    }

    // ── Positive: a reducible loop verifies clean ───────────────────────

    #[test]
    fn valid_loop_ok() {
        let mut f = Function::new("loop");
        let e = f.entry();
        let header = f.make_block();
        let hp = f.add_block_param(header, fixnum(), ValueRepresentation::UnboxedFixnum);
        let exit = f.make_block();

        let init = const_fixnum(&mut f, e, 0);
        f.set_terminator(e, jump(header, vec![init]));
        // header: next = hp + 1; loop back, and also exit.
        let (_, next) = f.push_inst(
            header,
            InstData {
                args: vec![hp, hp],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        let cond = const_fixnum(&mut f, header, 1);
        f.set_terminator(
            header,
            InstData {
                args: vec![cond],
                targets: vec![
                    BlockCall {
                        block: header,
                        args: vec![next[0]],
                    }, // back-edge
                    BlockCall {
                        block: exit,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        f.set_terminator(exit, ret(vec![hp]));
        assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    }

    // ── V6: block with no terminator ────────────────────────────────────

    #[test]
    fn v6_missing_terminator() {
        let mut f = Function::new("no_term");
        let e = f.entry();
        const_fixnum(&mut f, e, 1); // no terminator appended
        let errs = verify(&f).unwrap_err();
        assert!(errs.iter().any(|e| e.check == "V6 terminator"), "{errs:?}");
    }

    // ── V6: a non-terminator instruction carrying targets ───────────────

    #[test]
    fn v6_nonterminator_with_targets() {
        let mut f = Function::new("bad_targets");
        let e = f.entry();
        let other = f.make_block();
        // A ConstNil (non-terminator) that illegally carries a successor edge.
        f.push_inst(
            e,
            InstData {
                targets: vec![BlockCall {
                    block: other,
                    args: vec![],
                }],
                ..inst(Opcode::ConstNil)
            },
            &[(IRType::of(TypeBits::NULL), ValueRepresentation::Tagged)],
        );
        f.set_terminator(e, ret(vec![]));
        f.set_terminator(other, ret(vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(errs.iter().any(|e| e.check == "V6 terminator"), "{errs:?}");
    }

    // ── V4: wrong block-parameter arity ─────────────────────────────────

    #[test]
    fn v4_arity_mismatch() {
        let mut f = Function::new("arity");
        let e = f.entry();
        let target = f.make_block();
        f.add_block_param(target, fixnum(), ValueRepresentation::UnboxedFixnum);
        // Jump passes 0 args but `target` has 1 param.
        f.set_terminator(e, jump(target, vec![]));
        f.set_terminator(target, ret(vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V4 block-param arity"),
            "{errs:?}"
        );
    }

    // ── V4: type/repr disagreement on a block argument ──────────────────

    // spec-covers: R4.61, R4.64
    // R4.61: representation MUST never change implicitly along an edge — an
    // UnboxedFixnum argument reaching a Tagged block param is rejected rather
    // than silently coerced. R4.64: the "representation a slot's lowering
    // cannot produce" clause of the verifier's FrameState rejection set.
    #[test]
    fn v4_repr_mismatch() {
        let mut f = Function::new("repr");
        let e = f.entry();
        let target = f.make_block();
        // Param expects a Tagged value.
        f.add_block_param(target, fixnum(), ValueRepresentation::Tagged);
        // But we pass an UnboxedFixnum const.
        let v = const_fixnum(&mut f, e, 7);
        f.set_terminator(e, jump(target, vec![v]));
        f.set_terminator(target, ret(vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V4 block-param repr"),
            "{errs:?}"
        );
    }

    // ── V3: a value used where its definition does not dominate ─────────

    #[test]
    fn v3_dominance_violation() {
        let mut f = Function::new("dom");
        let e = f.entry();
        let b1 = f.make_block();
        let b2 = f.make_block();
        let merge = f.make_block();

        let cond = const_fixnum(&mut f, e, 1);
        f.set_terminator(
            e,
            InstData {
                args: vec![cond],
                targets: vec![
                    BlockCall {
                        block: b1,
                        args: vec![],
                    },
                    BlockCall {
                        block: b2,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        // v is defined only on the b1 arm...
        let v = const_fixnum(&mut f, b1, 99);
        f.set_terminator(b1, jump(merge, vec![]));
        f.set_terminator(b2, jump(merge, vec![]));
        // ...but used at the merge, which b1 does not dominate.
        f.set_terminator(merge, ret(vec![v]));

        let errs = verify(&f).unwrap_err();
        assert!(errs.iter().any(|e| e.check == "V3 dominance"), "{errs:?}");
    }

    // ── V3: use-before-def within a single block ────────────────────────

    #[test]
    fn v3_use_before_def_same_block() {
        let mut f = Function::new("ubd");
        let e = f.entry();
        // Define a first, then b; but wire an add that consumes b before b is
        // defined by putting the add first.
        let a = const_fixnum(&mut f, e, 1); // a == Value(0)
                                            // The add is pushed at position 0 (after the const) and its own single
                                            // result takes the next id, Value(a.0 + 1). The const `b` that follows
                                            // then takes Value(a.0 + 2). We wire the add to read `b` — a value
                                            // defined *later* in the same block — to trip the intra-block order
                                            // check.
        let (_, addr) = f.push_inst(
            e,
            InstData {
                args: vec![a, Value(a.0 + 2)],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        let b = const_fixnum(&mut f, e, 2);
        assert_eq!(b, Value(a.0 + 2), "value numbering assumption");
        f.set_terminator(e, ret(vec![addr[0]]));

        let errs = verify(&f).unwrap_err();
        assert!(errs.iter().any(|e| e.check == "V3 dominance"), "{errs:?}");
    }

    // ── V10: successor block does not exist ─────────────────────────────

    #[test]
    fn v10_missing_successor() {
        let mut f = Function::new("nosucc");
        let e = f.entry();
        // Jump to a block index that was never allocated.
        f.set_terminator(e, jump(Block(42), vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V10 succ-exists"),
            "{errs:?}"
        );
    }

    // ── V1: a dangling value operand ────────────────────────────────────

    #[test]
    fn v1_dangling_value() {
        let mut f = Function::new("dangling");
        let e = f.entry();
        let a = const_fixnum(&mut f, e, 1);
        // Reference a value id far beyond anything defined.
        f.push_inst(
            e,
            InstData {
                args: vec![a, Value(9999)],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(e, ret(vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V1 dangling-value"),
            "{errs:?}"
        );
    }

    // ── V8: a guard with no frame_state ─────────────────────────────────

    #[test]
    fn v8_guard_without_framestate() {
        let mut f = Function::new("guard");
        let e = f.entry();
        let a = const_fixnum(&mut f, e, 1);
        f.push_inst(
            e,
            InstData {
                args: vec![a],
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: None,
                ..inst(Opcode::Guard)
            },
            &[],
        );
        f.set_terminator(e, ret(vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V8 guard-framestate"),
            "{errs:?}"
        );
    }

    #[test]
    fn v8_refined_string_byte_length_must_be_pure() {
        let mut f = Function::new("overguarded_string_load");
        let e = f.entry();
        let string = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        f.push_inst(
            e,
            InstData {
                args: vec![string],
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                ..inst(Opcode::StringByteLength)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(e, ret(vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V8 layout-guard-contract"),
            "{errs:?}"
        );
    }

    #[test]
    fn v8_string_layout_flows_through_phi_but_not_raw_input_or_mixed_join() {
        let mut f = Function::new("forwarded-string-layout");
        let entry = f.entry();
        let raw = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: raw,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let (_, checked) = f.push_inst(
            entry,
            InstData {
                args: vec![raw],
                aux: AuxData::StringLayout,
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: Some(state),
                ..inst(Opcode::Guard)
            },
            &[(IRType::of(TypeBits::STRING), ValueRepresentation::Tagged)],
        );
        let body = f.make_block();
        let phi = f.add_block_param(body, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(entry, jump(body, vec![checked[0]]));
        let (load, result) = f.push_inst(
            body,
            InstData {
                args: vec![phi],
                ..inst(Opcode::StringByteLength)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(body, ret(result));
        verify(&f).expect("a forwarded refined dependency carries its successful layout proof");
        f.inst_mut(load).args[0] = raw;
        assert!(
            verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V8 layout-proof"),
            "the unchecked original input does not carry the proof dependency"
        );
        f.inst_mut(load).args[0] = phi;
        let term = f.terminator(entry).unwrap();
        f.inst_mut(term).targets[0].args[0] = raw;
        assert!(
            verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V8 layout-proof")
        );
    }

    // ── V8 positive: a guard with a valid, dominating frame_state ───────

    #[test]
    fn v8_guard_with_valid_framestate_ok() {
        let mut f = Function::new("guard_ok");
        let e = f.entry();
        let a = const_fixnum(&mut f, e, 1);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: a,
                    repr: ValueRepresentation::UnboxedFixnum,
                }],
                stack: vec![],
            }],
            remat: vec![],
        };
        let id: FrameStateId = f.frame_states.add(fs);
        f.push_inst(
            e,
            InstData {
                args: vec![a],
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: Some(id),
                ..inst(Opcode::Guard)
            },
            &[],
        );
        f.set_terminator(e, ret(vec![]));
        assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    }

    // ── V8: a frame_state naming a non-dominating value ─────────────────

    // spec-covers: R4.64
    // The "references a non-dominating SSA value" clause.
    #[test]
    fn v8_framestate_non_dominating_value() {
        let mut f = Function::new("guard_bad_fs");
        let e = f.entry();
        let b1 = f.make_block();
        let b2 = f.make_block();

        let cond = const_fixnum(&mut f, e, 1);
        f.set_terminator(
            e,
            InstData {
                args: vec![cond],
                targets: vec![
                    BlockCall {
                        block: b1,
                        args: vec![],
                    },
                    BlockCall {
                        block: b2,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        // `only_in_b1` is defined only on the b1 arm.
        let only_in_b1 = const_fixnum(&mut f, b1, 7);
        f.set_terminator(b1, jump(b2, vec![]));
        // A guard in b2 whose frame_state names a value b1 defines — b1 does not
        // dominate b2.
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: only_in_b1,
                    repr: ValueRepresentation::UnboxedFixnum,
                }],
                stack: vec![],
            }],
            remat: vec![],
        };
        let id = f.frame_states.add(fs);
        let cond2 = const_fixnum(&mut f, b2, 1);
        f.push_inst(
            b2,
            InstData {
                args: vec![cond2],
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: Some(id),
                ..inst(Opcode::Guard)
            },
            &[],
        );
        f.set_terminator(b2, ret(vec![]));

        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V8 framestate-dominance"),
            "{errs:?}"
        );
    }

    // ── V8: cyclic rematerialisation recipe ─────────────────────────────

    // spec-covers: R4.64
    // The "rematerialisation recipe is cyclic" clause. NOTE: R4.64's remaining
    // clause — slot counts disagreeing with the source bytecode's abstract frame
    // — is NOT covered here and cannot be: this verifier runs without the source
    // BytecodeFunction (see the module header). Tracked, not silently claimed.
    #[test]
    fn v8_cyclic_remat() {
        use crate::t2::frame_state::{RematOp, RematRecipe, RematRecipeId};
        let mut f = Function::new("remat_cycle");
        let e = f.entry();
        let a = const_fixnum(&mut f, e, 1);
        // recipe 0 depends on recipe 1, recipe 1 depends on recipe 0 → cycle.
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![],
                stack: vec![],
            }],
            remat: vec![
                RematRecipe {
                    op: RematOp::FixnumAdd,
                    inputs: vec![ValueSource::Remat(RematRecipeId(1))],
                    result_repr: ValueRepresentation::UnboxedFixnum,
                },
                RematRecipe {
                    op: RematOp::FixnumAdd,
                    inputs: vec![ValueSource::Remat(RematRecipeId(0))],
                    result_repr: ValueRepresentation::UnboxedFixnum,
                },
            ],
        };
        let id = f.frame_states.add(fs);
        f.push_inst(
            e,
            InstData {
                args: vec![a],
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: Some(id),
                ..inst(Opcode::Guard)
            },
            &[],
        );
        f.set_terminator(e, ret(vec![]));
        let errs = verify(&f).unwrap_err();
        assert!(
            errs.iter().any(|e| e.check == "V8 remat-recipe"),
            "{errs:?}"
        );
    }
    #[test]
    fn octet_word_store_requires_tagged_outputs_and_effectful_only_flags() {
        for opcode in [
            Opcode::TryStoreIntegerOctets8,
            Opcode::TryStoreIntegerOctets1To8,
        ] {
            for malformed in 0..5 {
                let mut f = Function::new("octet-word-contract");
                let entry = f.entry();
                let args: Vec<_> = (0..4)
                    .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
                    .collect();
                let mut d = inst(opcode);
                d.args = args.clone();
                d.flags.effectful = true;
                let (i, values) = f.push_inst(
                    entry,
                    d,
                    &[
                        (
                            IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                            ValueRepresentation::Tagged,
                        ),
                        (fixnum(), ValueRepresentation::Tagged),
                    ],
                );
                f.set_terminator(entry, ret(vec![values[1]]));
                match malformed {
                    0 => {
                        verify(&f).unwrap();
                        continue;
                    }
                    1 => f.inst_mut(i).flags.effectful = false,
                    2 => f.inst_mut(i).flags.call = true,
                    3 => f.set_repr(args[0], ValueRepresentation::UnboxedFixnum),
                    _ => {
                        f.inst_mut(i).args.pop();
                    }
                }
                let errors = verify(&f).unwrap_err();
                assert!(
                    errors
                        .iter()
                        .any(|e| e.check == "V8 octet-word-store-contract"),
                    "{errors:?}"
                );
            }
        }
    }

    #[test]
    fn bounded_native_poll_requires_exact_state_and_first_block_position() {
        for malformed in 0..8 {
            let mut f = Function::new("bounded-poll-contract");
            let entry = f.entry();
            let fs = f.frame_states.add(FrameState { scopes: vec![FrameScope {
                function: 0, bcp: 0, locals: vec![], stack: vec![],
            }], remat: vec![] });
            if malformed == 4 { const_fixnum(&mut f, entry, 1); }
            let mut d = inst(Opcode::NativePoll);
            d.aux = AuxData::BoundedPoll { exit: false, scope: crate::t2::ir::BoundedPollScope {
                function: 0, bcp: 0, control_scopes: vec![],
            } };
            d.flags = InstFlags { effectful: true, safepoint: true, ..Default::default() };
            d.frame_state = Some(fs);
            match malformed {
                1 => d.frame_state = None,
                2 => d.flags.call = true,
                3 => d.aux = AuxData::None,
                5 => {
                    let AuxData::BoundedPoll { scope, .. } = &mut d.aux else { unreachable!(); };
                    scope.function = 1;
                }
                7 => {
                    use crate::control_scope::{ControlScope, Ownership, ScopeKind};
                    let AuxData::BoundedPoll { scope, .. } = &mut d.aux else { unreachable!(); };
                    scope.control_scopes.push(ControlScope { push_bcp: 0, ownership: Ownership::Inherited,
                        sp_restore: 0, kind: ScopeKind::Tagbody { id: 0 } });
                }
                6 => {
                    let mut different = f.frame_states.get(fs).clone();
                    different.scopes[0].bcp = 1;
                    d.frame_state = Some(f.frame_states.add(different));
                }
                _ => {}
            }
            f.push_inst(entry, d, &[]);
            f.set_terminator(entry, ret(vec![]));
            if malformed == 0 { assert!(f.osr_entries.is_empty()); verify(&f).unwrap(); }
            else {
                let errors = verify(&f).unwrap_err();
                assert!(errors.iter().any(|e| e.check == "V8 bounded-native-poll-contract"), "{errors:?}");
            }
        }
    }

}
