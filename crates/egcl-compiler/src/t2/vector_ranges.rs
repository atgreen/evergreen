// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Version small counted loops after numeric representation selection. The
//! original checked loop remains the slow and OSR entry. A total runtime
//! predicate selects a clone whose store witnesses are checked structurally
//! by the verifier. Neither profiles nor constructor provenance prove bounds.

use super::ir::{
    AuxData, BlockCall, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueDef,
    ValueRepresentation,
};
use super::region_clone::{clone_region, repair_outside_uses};
use super::vector_range_proof::{CountedVectorLoop, candidates};

#[derive(Default, Debug)]
pub struct VectorRangeStats {
    pub loops: usize,
    pub stores: usize,
    pub declined: Vec<String>,
}

/// Bounded, transactional specialization. Call after representation selection:
/// a tagged induction variable is part of the proof and address contract.
pub fn version_counted_stores(f: &mut Function) -> VectorRangeStats {
    let mut stats = VectorRangeStats::default();
    if !f.block_order().iter().any(|&block| {
        f.block(block)
            .insts
            .iter()
            .any(|&inst| f.inst(inst).opcode == Opcode::VecSet)
    }) {
        return stats;
    }
    if super::verify::verify(f).is_err() {
        stats.declined.push("input IR is malformed".into());
        return stats;
    }
    let headers: Vec<_> = candidates(f).into_iter().map(|lp| lp.header).collect();
    for header in headers {
        if stats.loops == 2 {
            break;
        }
        // Exit repair for an earlier loop can change a later loop's operands.
        let Some(lp) = candidates(f).into_iter().find(|lp| lp.header == header) else {
            continue;
        };
        let mut trial = f.clone();
        if let Err(error) = version(&mut trial, &lp) {
            stats.declined.push(format!("header {}: {error}", header.0));
            continue;
        }
        if let Err(errors) = super::verify::verify(&trial) {
            stats
                .declined
                .push(format!("header {}: {errors:?}", header.0));
            continue;
        }
        *f = trial;
        stats.loops += 1;
        stats.stores += lp.stores.len();
    }
    stats
}

fn version(f: &mut Function, lp: &CountedVectorLoop) -> Result<(), &'static str> {
    if f.block(lp.header).params.get(lp.index_parameter) != Some(&lp.index) {
        return Err("induction parameter changed after analysis");
    }
    let fast = clone_region(f, &lp.body)?;
    let old_jump = f.terminator(lp.preheader).ok_or("missing entry jump")?;
    let position = f.inst(old_jump).source_pos;
    if f.block_mut(lp.preheader).insts.pop() != Some(old_jump) {
        return Err("entry jump is not last");
    }
    let (_, witness) = f.push_inst(
        lp.preheader,
        InstData {
            opcode: Opcode::SmallVectorRangeP,
            args: vec![lp.vector, lp.entry_arguments[lp.index_parameter], lp.end],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: position,
        },
        &[(
            IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
            ValueRepresentation::Tagged,
        )],
    );
    f.set_terminator(
        lp.preheader,
        InstData {
            opcode: Opcode::Brif,
            args: witness.clone(),
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![
                BlockCall {
                    block: fast.block_map[&lp.header],
                    args: lp.entry_arguments.clone(),
                },
                BlockCall {
                    block: lp.header,
                    args: lp.entry_arguments.clone(),
                },
            ],
            frame_state: None,
            source_pos: position,
        },
    );
    for &old in &lp.stores {
        let old_result = *f.inst(old).results.first().ok_or("store has no result")?;
        let result = *fast
            .value_map
            .get(&old_result)
            .ok_or("store was not cloned")?;
        let ValueDef::Result { inst, .. } = f.value(result).def else {
            return Err("cloned store result is not an instruction");
        };
        let store = f.inst_mut(inst);
        store.opcode = Opcode::VecSetInRange;
        // The successful predicate already establishes SIMPLE_VECTOR. Keep
        // a vector-only source store's contract on its checked fallback.
        store.aux = AuxData::None;
        store.args.push(witness[0]);
    }
    // This also repairs FrameState-only uses and merges distinct cloned loop
    // values at exits. OSR records still reference the original checked loop.
    repair_outside_uses(f, &lp.body, &[fast])
}
