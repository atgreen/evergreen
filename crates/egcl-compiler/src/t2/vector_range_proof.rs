// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Structural proofs for versioned, counted simple-vector stores.
//!
//! Feedback is deliberately absent from this module. A runtime range predicate
//! establishes the entry interval, and the checked CFG/induction establishes
//! every subsequent index. Callers run SSA verification before this checker.

use super::ir::{
    AuxData, Block, Function, Inst, InstFlags, Opcode, TypeBits, Value, ValueDef,
    ValueRepresentation,
};
use super::opt_licm::{NaturalLoop, detect_loops};

#[derive(Clone, Debug)]
pub(crate) struct CountedVectorLoop {
    pub(crate) header: Block,
    pub(crate) body: Vec<Block>,
    pub(crate) preheader: Block,
    pub(crate) index: Value,
    pub(crate) end: Value,
    pub(crate) vector: Value,
    pub(crate) stores: Vec<Inst>,
    pub(crate) entry_arguments: Vec<Value>,
    pub(crate) index_parameter: usize,
}

fn tagged(f: &Function, value: Value) -> bool {
    f.value(value).repr == ValueRepresentation::Tagged
}

fn defining_block(f: &Function, value: Value) -> Option<Block> {
    match f.value(value).def {
        ValueDef::Param { block, .. } => Some(block),
        ValueDef::Result { inst, .. } => f
            .block_order()
            .iter()
            .copied()
            .find(|&block| f.block(block).insts.contains(&inst)),
    }
}

fn result_inst(f: &Function, value: Value) -> Option<Inst> {
    match f.value(value).def {
        ValueDef::Result { inst, num: 0 } => Some(inst),
        _ => None,
    }
}

fn one(f: &Function, value: Value) -> bool {
    tagged(f, value)
        && result_inst(f, value).is_some_and(|inst| {
            f.inst(inst).opcode == Opcode::ConstFixnum
                && matches!(f.inst(inst).aux, AuxData::FixnumImm(1))
        })
}

fn unsupported_aux(aux: &AuxData) -> bool {
    matches!(
        aux,
        AuxData::CatchScope { .. }
            | AuxData::HandlerScope { .. }
            | AuxData::HandlerBindScope { .. }
            | AuxData::RestartCaseScope { .. }
            | AuxData::HandlerDestination { .. }
            | AuxData::CatchDestination { .. }
            | AuxData::CleanupContinuation { .. }
            | AuxData::TransferSite { .. }
    )
}

/// The same analysis handles checked source loops and witness-bearing clones.
/// `proved` changes only the entry/store shape, never the induction proof.
fn analyze(f: &Function, lp: &NaturalLoop, proved: bool) -> Option<CountedVectorLoop> {
    let header = lp.header;
    let body = &lp.body;
    if header == f.entry()
        || body.iter().map(|&b| f.block(b).insts.len()).sum::<usize>() > 64
        || body.iter().any(|&block| {
            (block != header && f.preds(block).iter().any(|pred| !body.contains(pred)))
                || f.block(block).insts.iter().any(|&inst| {
                    let d = f.inst(inst);
                    d.flags.call
                        || d.flags.safepoint
                        || matches!(d.opcode, Opcode::Call | Opcode::Invoke | Opcode::ClearMv)
                        || unsupported_aux(&d.aux)
                })
        })
    {
        return None;
    }
    let outside: Vec<_> = f
        .preds(header)
        .into_iter()
        .filter(|b| !body.contains(b))
        .collect();
    if outside.len() != 1 {
        return None;
    }
    let preheader = outside[0];
    let entry = f.inst(f.terminator(preheader)?);
    if (!proved && (entry.opcode != Opcode::Jump || entry.targets.len() != 1))
        || (proved && (entry.opcode != Opcode::Brif || entry.targets.len() != 2))
    {
        return None;
    }
    let entry_edge = entry.targets.first()?;
    if entry_edge.block != header || entry_edge.args.len() != f.block(header).params.len() {
        return None;
    }
    let term = f.inst(f.terminator(header)?);
    if term.opcode != Opcode::Brif
        || term.args.len() != 1
        || term.targets.len() != 2
        || body.contains(&term.targets[0].block)
        || !body.contains(&term.targets[1].block)
        || f.preds(term.targets[1].block) != vec![header]
    {
        return None;
    }
    let comparison = f.inst(result_inst(f, term.args[0])?);
    if comparison.opcode != Opcode::FixnumCmpGe
        || comparison.args.len() != 2
        || comparison.results.len() != 1
        || !tagged(f, comparison.results[0])
        || !matches!(comparison.aux, AuxData::None | AuxData::CallTarget(_))
    {
        return None;
    }
    let index = comparison.args[0];
    let end = comparison.args[1];
    let ValueDef::Param { block, num } = f.value(index).def else {
        return None;
    };
    if block != header
        || !tagged(f, index)
        || !tagged(f, end)
        || body.contains(&defining_block(f, end)?)
    {
        return None;
    }
    let backedges: Vec<_> = body
        .iter()
        .flat_map(|&b| {
            f.terminator(b).into_iter().flat_map(move |inst| {
                f.inst(inst)
                    .targets
                    .iter()
                    .filter(move |edge| edge.block == header)
                    .map(move |edge| (b, inst, edge))
            })
        })
        .collect();
    if backedges.len() != 1 {
        return None;
    }
    let (latch, latch_inst, latch_edge) = backedges[0];
    let latch_term = f.inst(latch_inst);
    if latch_term.opcode != Opcode::Jump || latch_term.targets.len() != 1 {
        return None;
    }
    let next = *latch_edge.args.get(num as usize)?;
    let increment = result_inst(f, next)?;
    let add = f.inst(increment);
    if !f.block(latch).insts.contains(&increment)
        || add.opcode != Opcode::FixnumAdd
        || add.args.len() != 2
        || add.results.len() != 1
        || !matches!(add.aux, AuxData::None | AuxData::CallTarget(_))
        || !tagged(f, next)
        || !((add.args[0] == index && one(f, add.args[1]))
            || (add.args[1] == index && one(f, add.args[0])))
    {
        return None;
    }
    let dom = f.dominators();
    let body_entry = term.targets[1].block;
    if !dom.dominates(body_entry, latch) {
        return None;
    }
    let mut vector = None;
    let mut stores = Vec::new();
    for &block in body {
        for &inst in &f.block(block).insts {
            let d = f.inst(inst);
            if !matches!(d.opcode, Opcode::VecSet | Opcode::VecSetInRange) {
                continue;
            }
            if d.opcode
                != if proved {
                    Opcode::VecSetInRange
                } else {
                    Opcode::VecSet
                }
                || d.args.len() != if proved { 4 } else { 3 }
                || d.args[1] != index
                || d.results.len() != 1
                || d.args.iter().any(|&v| !tagged(f, v))
                || !tagged(f, d.results[0])
                || !dom.dominates(body_entry, block)
                || body.contains(&defining_block(f, d.args[0])?)
                || vector.is_some_and(|v| v != d.args[0])
                || d.flags
                    != (InstFlags {
                        guard: true,
                        effectful: true,
                        ..Default::default()
                    })
                || !(matches!(d.aux, AuxData::None)
                    || !proved && matches!(d.aux, AuxData::SimpleVectorStore))
                || d.frame_state.is_none()
            {
                return None;
            }
            vector = Some(d.args[0]);
            stores.push(inst);
        }
    }
    Some(CountedVectorLoop {
        header,
        body: body.clone(),
        preheader,
        index,
        end,
        vector: vector?,
        stores,
        entry_arguments: entry_edge.args.clone(),
        index_parameter: num as usize,
    })
}

fn innermost(loops: &[NaturalLoop], lp: &NaturalLoop) -> bool {
    !loops
        .iter()
        .any(|inner| inner.header != lp.header && lp.body.contains(&inner.header))
}

/// Discover checked loops with a single ordinary Jump entry. Original OSR
/// records stay on their checked version; this function creates no proof.
pub(crate) fn candidates(f: &Function) -> Vec<CountedVectorLoop> {
    let loops = detect_loops(f, &f.dominators());
    loops
        .iter()
        .filter(|lp| innermost(&loops, lp))
        .filter_map(|lp| analyze(f, lp, false))
        .collect()
}

/// Validate proof consumers after ordinary structural SSA verification. A valid
/// SSA Boolean is insufficient: its producing predicate and successful edge
/// must establish this exact loop's vector and entry interval.
pub(crate) fn verify_accesses(f: &Function) -> Result<(), String> {
    if !f.block_order().iter().any(|&block| {
        f.block(block)
            .insts
            .iter()
            .any(|&inst| f.inst(inst).opcode == Opcode::VecSetInRange)
    }) {
        return Ok(());
    }
    let loops = detect_loops(f, &f.dominators());
    let mut covered = std::collections::HashSet::new();
    for lp in &loops {
        if !innermost(&loops, lp) {
            continue;
        }
        let Some(candidate) = analyze(f, lp, true) else {
            continue;
        };
        if f.osr_entries
            .iter()
            .any(|entry| candidate.body.contains(&entry.block))
        {
            continue;
        }
        let branch = f.inst(f.terminator(candidate.preheader).unwrap());
        if branch.args.len() != 1 || candidate.body.contains(&branch.targets[1].block) {
            continue;
        }
        let witness = branch.args[0];
        let Some(predicate) = result_inst(f, witness) else {
            continue;
        };
        let instructions = &f.block(candidate.preheader).insts;
        if instructions.iter().rev().nth(1) != Some(&predicate) {
            continue;
        }
        let d = f.inst(predicate);
        if d.opcode != Opcode::SmallVectorRangeP
            || d.args.len() != 3
            || d.results.len() != 1
            || d.results[0] != witness
            || d.flags != InstFlags::default()
            || d.frame_state.is_some()
            || !d.targets.is_empty()
            || !matches!(d.aux, AuxData::None)
            || d.args.iter().any(|&v| !tagged(f, v))
            || !tagged(f, witness)
            || f.value(witness).ty.bits != TypeBits::SYMBOL.join(TypeBits::NULL)
            || d.args[0] != candidate.vector
            || d.args[1] != candidate.entry_arguments[candidate.index_parameter]
            || d.args[2] != candidate.end
            || candidate
                .stores
                .iter()
                .any(|&inst| f.inst(inst).args[3] != witness)
        {
            continue;
        }
        covered.extend(candidate.stores);
    }
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            if f.inst(inst).opcode == Opcode::VecSetInRange && !covered.contains(&inst) {
                return Err(format!("unproved VecSetInRange {:?} in {:?}", inst, block));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{BlockCall, IRType, InstData, OsrEntry};
    use ValueRepresentation::Tagged;

    fn data(opcode: Opcode, args: Vec<Value>) -> InstData {
        InstData {
            opcode,
            args,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }
    fn jump(f: &mut Function, from: Block, to: Block, args: Vec<Value>) {
        let mut d = data(Opcode::Jump, vec![]);
        d.targets = vec![BlockCall { block: to, args }];
        f.set_terminator(from, d);
    }
    struct Fixture {
        f: Function,
        entry: Block,
        header: Block,
        body: Block,
        exit: Block,
        vector: Value,
        start: Value,
        end: Value,
        index: Value,
        store: Inst,
        add: Inst,
        state: super::super::frame_state::FrameStateId,
    }
    fn fixture() -> Fixture {
        let mut f = Function::new("vector-range-proof");
        let entry = f.entry();
        let vector = f.add_block_param(entry, IRType::TOP, Tagged);
        let start = f.add_block_param(entry, IRType::TOP, Tagged);
        let end = f.add_block_param(entry, IRType::TOP, Tagged);
        let mut d = data(Opcode::ConstFixnum, vec![]);
        d.aux = AuxData::FixnumImm(1);
        let (_, one) = f.push_inst(entry, d, &[(IRType::of(TypeBits::FIXNUM), Tagged)]);
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();
        let index = f.add_block_param(header, IRType::TOP, Tagged);
        jump(&mut f, entry, header, vec![start]);
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: index,
                    repr: Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let mut cmp = data(Opcode::FixnumCmpGe, vec![index, end]);
        cmp.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        cmp.frame_state = Some(state);
        let (_, condition) = f.push_inst(
            header,
            cmp,
            &[(IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)), Tagged)],
        );
        let mut branch = data(Opcode::Brif, condition);
        branch.targets = vec![
            BlockCall {
                block: exit,
                args: vec![],
            },
            BlockCall {
                block: body,
                args: vec![],
            },
        ];
        f.set_terminator(header, branch);
        let mut store = data(Opcode::VecSet, vec![vector, index, one[0]]);
        store.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        store.frame_state = Some(state);
        let (store, _) = f.push_inst(body, store, &[(IRType::TOP, Tagged)]);
        let mut add = data(Opcode::FixnumAdd, vec![index, one[0]]);
        add.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        add.frame_state = Some(state);
        let (add, next) = f.push_inst(body, add, &[(IRType::of(TypeBits::FIXNUM), Tagged)]);
        jump(&mut f, body, header, next);
        f.set_terminator(exit, data(Opcode::Return, vec![vector]));
        Fixture {
            f,
            entry,
            header,
            body,
            exit,
            vector,
            start,
            end,
            index,
            store,
            add,
            state,
        }
    }
    fn version(x: &mut Fixture) -> (Inst, Value) {
        let old = x.f.terminator(x.entry).unwrap();
        let (predicate, result) = x.f.push_inst(
            x.entry,
            data(Opcode::SmallVectorRangeP, vec![x.vector, x.start, x.end]),
            &[(IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)), Tagged)],
        );
        // push_inst appends after the old terminator; replace the old Jump
        // while retaining predicate directly before the new branch.
        x.f.block_mut(x.entry).insts.retain(|&i| i != old);
        let mut branch = data(Opcode::Brif, result.clone());
        branch.targets = vec![
            BlockCall {
                block: x.header,
                args: vec![x.start],
            },
            BlockCall {
                block: x.exit,
                args: vec![],
            },
        ];
        x.f.set_terminator(x.entry, branch);
        x.f.inst_mut(x.store).opcode = Opcode::VecSetInRange;
        x.f.inst_mut(x.store).args.push(result[0]);
        (predicate, result[0])
    }
    #[test]
    fn checked_counted_loop_is_discovered_and_witnessed_clone_is_verified() {
        let mut x = fixture();
        let found = candidates(&x.f);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].index, x.index);
        assert_eq!(found[0].entry_arguments, vec![x.start]);
        assert_eq!(found[0].stores, vec![x.store]);
        version(&mut x);
        assert!(verify_accesses(&x.f).is_ok());
        assert!(candidates(&x.f).is_empty());
    }
    #[test]
    fn mismatched_witness_or_entry_interval_is_rejected() {
        for mismatch in 0..4 {
            let mut x = fixture();
            let (predicate, _) = version(&mut x);
            match mismatch {
                0 => x.f.inst_mut(x.store).args[3] = x.start,
                1 => x.f.inst_mut(predicate).args[1] = x.end,
                2 => x.f.inst_mut(predicate).args[2] = x.start,
                _ => x.f.inst_mut(predicate).args[0] = x.start,
            }
            assert!(verify_accesses(&x.f).is_err(), "mismatch {mismatch}");
        }
    }
    #[test]
    fn osr_and_external_entry_cannot_bypass_the_witness() {
        for osr in [false, true] {
            let mut x = fixture();
            version(&mut x);
            if osr {
                x.f.osr_entries.push(OsrEntry {
                    block: x.header,
                    control_scopes: vec![],
                    bcp: 0,
                    frame_state: x.state,
                    checks: vec![],
                });
            } else {
                let other = x.f.make_block();
                jump(&mut x.f, other, x.header, vec![x.start]);
                let branch = x.f.terminator(x.entry).unwrap();
                x.f.inst_mut(branch).targets[1].block = other;
            }
            assert!(verify_accesses(&x.f).is_err());
        }
    }
    #[test]
    fn transformation_keeps_vector_only_fallback_and_proves_fast_store() {
        let mut x = fixture();
        x.f.inst_mut(x.store).aux = AuxData::SimpleVectorStore;
        let stats = super::super::vector_ranges::version_counted_stores(&mut x.f);
        assert_eq!((stats.loops, stats.stores), (1, 1));
        assert!(matches!(x.f.inst(x.store).aux, AuxData::SimpleVectorStore));
        assert_eq!(x.f.inst(x.store).opcode, Opcode::VecSet);
        assert!(x.f.block_order().iter().any(|&block| {
            x.f.block(block).insts.iter().any(|&inst| {
                x.f.inst(inst).opcode == Opcode::VecSetInRange
                    && matches!(x.f.inst(inst).aux, AuxData::None)
            })
        }));
        super::super::verify::verify(&x.f).unwrap();
    }

    #[test]
    fn predicate_must_be_immediately_before_success_branch() {
        let mut x = fixture();
        version(&mut x);
        let term = x.f.terminator(x.entry).unwrap();
        x.f.block_mut(x.entry).insts.retain(|&i| i != term);
        x.f.push_inst(
            x.entry,
            data(Opcode::ConstNil, vec![]),
            &[(IRType::of(TypeBits::NULL), Tagged)],
        );
        x.f.block_mut(x.entry).insts.push(term);
        assert!(verify_accesses(&x.f).is_err());
    }

    #[test]
    fn changed_induction_or_variant_vector_is_rejected() {
        for variant_vector in [false, true] {
            let mut x = fixture();
            version(&mut x);
            if variant_vector {
                x.f.inst_mut(x.store).args[0] = x.index;
            } else {
                x.f.inst_mut(x.add).opcode = Opcode::FixnumSub;
            }
            assert!(verify_accesses(&x.f).is_err());
        }
    }
    #[test]
    fn calls_safepoints_and_early_accesses_decline_discovery() {
        for kind in 0..3 {
            let mut x = fixture();
            match kind {
                0 => x.f.inst_mut(x.store).flags.call = true,
                1 => x.f.inst_mut(x.add).flags.safepoint = true,
                _ => {
                    x.f.block_mut(x.body).insts.retain(|&i| i != x.store);
                    x.f.block_mut(x.header).insts.insert(0, x.store);
                }
            }
            assert!(candidates(&x.f).is_empty());
        }
    }
    #[test]
    fn transformation_retains_checked_loop_and_versions_only_once() {
        let mut x = fixture();
        crate::t2::verify::verify(&x.f).unwrap();
        let stats = crate::t2::vector_ranges::version_counted_stores(&mut x.f);
        assert_eq!(stats.loops, 1, "{stats:?}");
        assert_eq!(stats.stores, 1);
        let count = |opcode| {
            x.f.block_order()
                .iter()
                .flat_map(|&b| &x.f.block(b).insts)
                .filter(|&&i| x.f.inst(i).opcode == opcode)
                .count()
        };
        assert_eq!(count(Opcode::VecSet), 1);
        assert_eq!(count(Opcode::VecSetInRange), 1);
        assert_eq!(x.f.inst(x.store).opcode, Opcode::VecSet);
        crate::t2::verify::verify(&x.f).unwrap();
        verify_accesses(&x.f).unwrap();
        let second = crate::t2::vector_ranges::version_counted_stores(&mut x.f);
        assert_eq!(second.loops, 0, "{second:?}");
    }

    #[test]
    fn transformation_accepts_retained_numeric_call_targets() {
        let mut x = fixture();
        let branch = x.f.terminator(x.header).unwrap();
        let condition = x.f.inst(branch).args[0];
        let comparison = result_inst(&x.f, condition).unwrap();
        x.f.inst_mut(comparison).aux = AuxData::CallTarget(egcl_rt::symbols::intern(">="));
        x.f.inst_mut(x.add).aux = AuxData::CallTarget(egcl_rt::symbols::intern("+"));
        crate::t2::verify::verify(&x.f).unwrap();
        let stats = crate::t2::vector_ranges::version_counted_stores(&mut x.f);
        assert_eq!(stats.loops, 1, "{stats:?}");
        assert_eq!(stats.stores, 1);
        assert_eq!(x.f.inst(comparison).opcode, Opcode::FixnumCmpGe);
        assert_eq!(x.f.inst(x.add).opcode, Opcode::FixnumAdd);
        assert!(matches!(x.f.inst(comparison).aux, AuxData::CallTarget(_)));
        assert!(matches!(x.f.inst(x.add).aux, AuxData::CallTarget(_)));
        crate::t2::verify::verify(&x.f).unwrap();
        verify_accesses(&x.f).unwrap();
    }

    #[test]
    fn transformation_repairs_exit_value_and_frame_state_but_preserves_osr() {
        let mut x = fixture();
        let exit_return = x.f.block_mut(x.exit).insts.pop().unwrap();
        assert_eq!(x.f.inst(exit_return).opcode, Opcode::Return);
        let exit_state = x.f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 1,
                locals: vec![ValueSource::Value {
                    value: x.index,
                    repr: Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let mut guard = data(Opcode::Guard, vec![x.index]);
        guard.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
        guard.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        guard.frame_state = Some(exit_state);
        let (guard, _) = x.f.push_inst(x.exit, guard, &[]);
        let returned =
            x.f.set_terminator(x.exit, data(Opcode::Return, vec![x.index]));
        x.f.osr_entries.push(OsrEntry {
            control_scopes: vec![],
            bcp: 0,
            block: x.header,
            frame_state: x.state,
            checks: vec![],
        });
        let original_osr = format!("{:?}", x.f.osr_entries);
        crate::t2::verify::verify(&x.f).unwrap();
        let stats = crate::t2::vector_ranges::version_counted_stores(&mut x.f);
        assert_eq!(stats.loops, 1, "{stats:?}");
        crate::t2::verify::verify(&x.f).unwrap();
        verify_accesses(&x.f).unwrap();
        assert_eq!(format!("{:?}", x.f.osr_entries), original_osr);
        assert_eq!(x.f.osr_entries.len(), 1);
        assert_eq!(x.f.osr_entries[0].block, x.header);
        let merged = x.f.inst(returned).args[0];
        assert_ne!(merged, x.index);
        assert!(matches!(x.f.value(merged).def, ValueDef::Param { block, .. } if block == x.exit));
        assert_eq!(x.f.inst(guard).args, vec![merged]);
        let state = x.f.frame_states.get(x.f.inst(guard).frame_state.unwrap());
        assert!(matches!(state.scopes[0].locals[0],
            ValueSource::Value { value, repr: Tagged } if value == merged));
        // Both checked and fast exits must supply the merged index.
        assert_eq!(x.f.preds(x.exit).len(), 2);
        for pred in x.f.preds(x.exit) {
            let term = x.f.inst(x.f.terminator(pred).unwrap());
            let edge = term
                .targets
                .iter()
                .find(|edge| edge.block == x.exit)
                .unwrap();
            assert_eq!(edge.args.len(), x.f.block(x.exit).params.len());
            assert!(edge.args.contains(&x.f.block(pred).params[0]));
        }
    }

    #[test]
    fn transformation_declines_unsupported_or_malformed_input_without_changes() {
        for malformed in [false, true] {
            let mut x = fixture();
            if malformed {
                let backedge = x.f.terminator(x.body).unwrap();
                x.f.inst_mut(backedge).targets[0].args.clear();
            } else {
                x.f.inst_mut(x.add).opcode = Opcode::FixnumSub;
                crate::t2::verify::verify(&x.f).unwrap();
            }
            let before = format!("{:?}", x.f);
            let stats = crate::t2::vector_ranges::version_counted_stores(&mut x.f);
            assert_eq!(stats.loops, 0, "{stats:?}");
            assert_eq!(format!("{:?}", x.f), before);
        }
    }
}
