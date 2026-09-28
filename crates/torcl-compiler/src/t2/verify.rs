//! P2 — IR verifier, algorithm A4.07 (spec §4.3.8).
//!
//! **Parcel P2.** `verify` checks the well-formedness rules V1–V10 from spec
//! §4.3.8.1 against the frozen `ir` contract. The checks that the frozen
//! contract makes decidable are implemented here:
//!
//! - **V1** (partial) — no dangling handles: every `Value` referenced in an
//!   `args`/`BlockCall.args` slot must be a live SSA value, and every successor
//!   `Block` must exist. Inst-handle liveness and a full arena walk are not
//!   checkable because the contract exposes no `num_values`/`num_insts`
//!   accessor; block/value handle liveness is covered here (value liveness via
//!   the reconstructed def set, block liveness folded into V10).
//! - **V3** — SSA dominance: every value operand's definition dominates its use
//!   (block-level dominance, refined to program order within a block); a
//!   block-call argument's definition dominates the terminator.
//! - **V4** — block-parameter agreement: every terminator `BlockCall` to block
//!   B passes exactly `B.params.len()` args, each arg's `IRType` a subtype of /
//!   and `ValueRepresentation` equal to the corresponding parameter's.
//! - **V6** — terminator well-formedness: every block ends in exactly one
//!   terminator, no earlier instruction is a terminator, no non-terminator
//!   carries `targets`, and the entry block has no predecessors.
//! - **V8** — guard/FrameState well-formedness: every `flags.guard` instruction
//!   carries a `frame_state`; the referenced `FrameState`'s named SSA values
//!   dominate the guard and its rematerialisation recipes are acyclic (spec
//!   §4.10 R4.64, the portions decidable without the source bytecode frame).
//! - **V10** — all successor blocks exist and the CFG is reducible (every
//!   retreating edge targets a block that dominates its source).
//!
//! **Skipped** (missing contract surface, noted for the parcel owner):
//! - **V2** use-list/pred-list consistency — the contract exposes no reverse
//!   use map; `preds` is derived from terminators, so it is consistent by
//!   construction and there is nothing independent to cross-check.
//! - **V5** type consistency vs. per-opcode input schema — no opcode
//!   input-type schema is exposed.
//! - **V7** effect ordering vs. the alias model — no alias model is exposed.
//! - **V8** slot-count-vs-`bcp` and representation-producibility clauses of
//!   R4.64 — require the source `BytecodeFunction`'s abstract frame, not
//!   available here.
//! - **V9** source position present — instructions carry a `source_pos` id but
//!   block parameters carry none, and it is only a Warning; skipped so
//!   hand-built positive fixtures stay clean.

use std::collections::{HashMap, HashSet};

use crate::t2::frame_state::{FrameState, ValueSource};
use crate::t2::ir::{AuxData, Block, Function, Opcode, Value, ValueDef};

/// A single verification failure (spec §4.3.8.1 checks V1–V10).
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

/// Verify a function; `Ok(())` iff it is well-formed (spec R4.20).
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
            if data.opcode == Opcode::Invoke
                && (data.targets.len() != 2
                    || data.targets[0].block == data.targets[1].block
                    || !data.flags.call
                    || !data.flags.effectful
                    || !data.flags.safepoint
                    || !matches!(
                        data.aux,
                        AuxData::CallTarget(_)
                            | AuxData::TransferThrow
                            | AuxData::CatchScope { .. }
                            | AuxData::HandlerScope { .. }
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

                // V8 — StringByteLength consumes an explicitly layout-refined
                // SSA value and is a pure load. StringAsciiCharAt consumes the
                // same refinement but can still fail its bounds/ASCII assumptions,
                // so it remains an ordered deopt guard.
                if matches!(
                    data.opcode,
                    Opcode::StringByteLength | Opcode::StringAsciiCharAt
                ) {
                    let refined = data.args.first().is_some_and(|value| {
                        valid_values.contains(&value.0)
                            && matches!(
                                f.value(*value).def,
                                ValueDef::Result { inst, .. }
                                    if f.inst(inst).opcode == Opcode::Guard
                                        && matches!(&f.inst(inst).aux, AuxData::StringLayout)
                            )
                    });
                    if !refined {
                        errors.push(VerifyError::new(
                            "V8 layout-proof",
                            format!(
                                "block{bi} {:?} does not consume a StringLayout guard result",
                                data.opcode
                            ),
                        ));
                    }
                }
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

    if errors.is_empty() {
        verify_cleanup_continuations(f, &mut errors);
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

/// V8 / spec §4.10 R4.64 (decidable portion): every SSA value named by a
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
        assert!(
            verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V11 invoke-result")
        );
    }

    #[test]
    fn invoke_result_must_be_projected_through_normal_block_parameter() {
        let (mut f, _, result) = invoke_graph();
        let ret = f.terminator(Block(1)).unwrap();
        f.inst_mut(ret).args[0] = result;
        assert!(
            verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V3 dominance")
        );
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
            assert!(
                verify(&f)
                    .unwrap_err()
                    .iter()
                    .any(|e| e.check == "V11 invoke-shape")
            );
        }
    }

    #[test]
    fn invoke_requires_frame_state_before_the_call() {
        let (mut f, invoke, _) = invoke_graph();
        f.inst_mut(invoke).frame_state = None;
        assert!(
            verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V8 guard-framestate")
        );
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
        assert!(
            verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V8 framestate-dominance")
        );
    }

    #[test]
    fn invoke_invalid_frame_handle_is_reported_without_panicking() {
        let (mut f, invoke, _) = invoke_graph();
        f.inst_mut(invoke).frame_state = Some(FrameStateId(u32::MAX));
        assert!(
            verify(&f)
                .unwrap_err()
                .iter()
                .any(|e| e.check == "V8 framestate-handle")
        );
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
        use crate::t2::emit::{EmitError, emit};
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
}
