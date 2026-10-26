// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! CFG cloning without creating logical inline frames, and transactional SSA
//! repair after callers connect several versions of a region. Cloning itself
//! changes no incoming edges and duplicates no OSR entry records.

use std::collections::{HashMap, HashSet};

use super::frame_state::{FrameState, ValueSource};
use super::ir::{AuxData, Block, Function, Inst, Opcode, Value, ValueDef};

pub(crate) type RegionError = &'static str;

#[derive(Clone, Debug)]
pub(crate) struct RegionClone {
    pub(crate) block_map: HashMap<Block, Block>,
    pub(crate) value_map: HashMap<Value, Value>,
}

fn map_value(value: Value, values: &HashMap<Value, Value>) -> Value {
    values.get(&value).copied().unwrap_or(value)
}

fn map_source(source: &mut ValueSource, values: &HashMap<Value, Value>) {
    if let ValueSource::Value { value, .. } = source {
        *value = map_value(*value, values);
    }
}

fn map_state(state: &mut FrameState, values: &HashMap<Value, Value>) {
    for scope in &mut state.scopes {
        for source in scope.locals.iter_mut().chain(&mut scope.stack) {
            map_source(source, values);
        }
    }
    for recipe in &mut state.remat {
        for source in &mut recipe.inputs {
            map_source(source, values);
        }
    }
}

fn state_values(state: &FrameState) -> Vec<Value> {
    state
        .scopes
        .iter()
        .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
        .chain(state.remat.iter().flat_map(|recipe| recipe.inputs.iter()))
        .filter_map(|source| match source {
            ValueSource::Value { value, .. } => Some(*value),
            _ => None,
        })
        .collect()
}

fn region_values(f: &Function, blocks: &[Block]) -> Result<Vec<Value>, RegionError> {
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    for &block in blocks {
        if !f.is_valid_block(block) || !seen.insert(block) {
            return Err("invalid or duplicate region block");
        }
        if !f.block_has_terminator(block) {
            return Err("region block has no terminator");
        }
        values.extend_from_slice(&f.block(block).params);
        for &inst in &f.block(block).insts {
            values.extend_from_slice(&f.inst(inst).results);
        }
    }
    Ok(values)
}

/// Clone identities before translating uses, including Invoke results used by
/// their own normal edge. Non-region values and successors retain identity.
/// Scope-bearing native transfer metadata is deliberately unsupported: those
/// instructions require separate control-scope/landing registration repair.
pub(crate) fn clone_region(f: &mut Function, blocks: &[Block]) -> Result<RegionClone, RegionError> {
    region_values(f, blocks)?;
    for &block in blocks {
        for &inst in &f.block(block).insts {
            if matches!(
                f.inst(inst).aux,
                AuxData::CatchScope { .. }
                    | AuxData::HandlerScope { .. }
                    | AuxData::HandlerBindScope { .. }
                    | AuxData::RestartCaseScope { .. }
                    | AuxData::HandlerDestination { .. }
                    | AuxData::CatchDestination { .. }
                    | AuxData::CleanupContinuation { .. }
                    | AuxData::TransferSite { .. }
            ) {
                return Err("region contains native control-scope metadata");
            }
            if f.inst(inst).opcode.is_terminator()
                && f.inst(inst).opcode != Opcode::Invoke
                && !f.inst(inst).results.is_empty()
            {
                return Err("unsupported result-producing terminator");
            }
        }
    }
    let mut cloned = RegionClone {
        block_map: HashMap::new(),
        value_map: HashMap::new(),
    };
    for &block in blocks {
        cloned.block_map.insert(block, f.make_block());
    }
    for &block in blocks {
        let params = f.block(block).params.clone();
        for old in params {
            let data = f.value(old).clone();
            let new = f.add_block_param(cloned.block_map[&block], data.ty, data.repr);
            cloned.value_map.insert(old, new);
        }
    }
    let mut instructions = Vec::<(Inst, Inst)>::new();
    // Allocate all result identities with untranslated operands. No verifier or
    // analysis observes this temporary state; the second sweep fixes all uses.
    for &block in blocks {
        for old in f.block(block).insts.clone() {
            let data = f.inst(old).clone();
            let types: Vec<_> = data
                .results
                .iter()
                .map(|&value| {
                    let data = f.value(value);
                    (data.ty, data.repr)
                })
                .collect();
            let target = cloned.block_map[&block];
            let (new, results) = if data.opcode == Opcode::Invoke {
                f.set_terminator_with_results(target, data.clone(), &types)
            } else if data.opcode.is_terminator() {
                (f.set_terminator(target, data.clone()), Vec::new())
            } else {
                f.push_inst(target, data.clone(), &types)
            };
            for (&old, new) in data.results.iter().zip(results) {
                cloned.value_map.insert(old, new);
            }
            instructions.push((old, new));
        }
    }
    for (old, new) in instructions {
        let source = f.inst(old).clone();
        let frame_state = source.frame_state.map(|id| {
            let mut state = f.frame_states.get(id).clone();
            map_state(&mut state, &cloned.value_map);
            f.frame_states.add(state)
        });
        let target = f.inst_mut(new);
        for value in &mut target.args {
            *value = map_value(*value, &cloned.value_map);
        }
        for edge in &mut target.targets {
            edge.block = cloned
                .block_map
                .get(&edge.block)
                .copied()
                .unwrap_or(edge.block);
            for value in &mut edge.args {
                *value = map_value(*value, &cloned.value_map);
            }
        }
        target.frame_state = frame_state;
    }
    Ok(cloned)
}

fn defining_block(f: &Function, value: Value) -> Result<Block, RegionError> {
    match f.value(value).def {
        ValueDef::Param { block, .. } => Ok(block),
        ValueDef::Result { inst, .. } => f
            .block_order()
            .iter()
            .copied()
            .find(|&block| f.block(block).insts.contains(&inst))
            .ok_or("region value definition is no longer in the CFG"),
    }
}

struct Updater {
    original: Value,
    sources: HashMap<Block, Value>,
    entries: HashMap<Block, Value>,
    phis: Vec<Value>,
}

impl Updater {
    fn edge_value(
        &mut self,
        f: &mut Function,
        block: Block,
        edge: usize,
    ) -> Result<Value, RegionError> {
        if let Some(&value) = self.sources.get(&block) {
            if let ValueDef::Result { inst, .. } = f.value(value).def
                && f.inst(inst).opcode == Opcode::Invoke
                && edge != 0
            {
                return Err("Invoke result is unavailable on its exceptional edge");
            }
            return Ok(value);
        }
        self.entry_value(f, block)
    }

    fn entry_value(&mut self, f: &mut Function, block: Block) -> Result<Value, RegionError> {
        if let Some(&value) = self.entries.get(&block) {
            return Ok(value);
        }
        if f.osr_entries.iter().any(|entry| entry.block == block) || block == f.entry() {
            return Err("region value is unavailable at an alternate/function entry");
        }
        // Keep edge identities rather than distinct predecessors: a Brif can
        // have two edges to the same block, and both need the new argument.
        let mut incoming = Vec::new();
        for &pred in f.block_order() {
            if let Some(term) = f.terminator(pred) {
                for (index, target) in f.inst(term).targets.iter().enumerate() {
                    if target.block == block {
                        incoming.push((pred, term, index));
                    }
                }
            }
        }
        if incoming.is_empty() {
            return Err("region value has a bypass path without a definition");
        }
        let data = f.value(self.original).clone();
        if f.block(block).params.len() >= u16::MAX as usize {
            return Err("SSA repair exceeds block parameter limit");
        }
        // Publish the incomplete phi before following predecessors. This is
        // what makes backedges and downstream loops terminate correctly.
        let phi = f.add_block_param(block, data.ty, data.repr);
        self.entries.insert(block, phi);
        self.phis.push(phi);
        for (pred, term, edge) in incoming {
            let value = self.edge_value(f, pred, edge)?;
            f.inst_mut(term).targets[edge].args.push(value);
        }
        Ok(phi)
    }
}

/// Repair direct and deopt-only escaping uses after the caller connects clones.
/// Transactional: an unsupported bypass/OSR path leaves `f` unchanged. Phi
/// parameters can include harmless pass-through parameters on downstream loops;
/// every retained phi is fed on every exact CFG edge. Versions must preserve
/// value representation (type narrowing within that representation is allowed).
pub(crate) fn repair_outside_uses(
    f: &mut Function,
    original_region: &[Block],
    versions: &[RegionClone],
) -> Result<(), RegionError> {
    let mut candidate = f.clone();
    repair(&mut candidate, original_region, versions)?;
    *f = candidate;
    Ok(())
}

fn repair(f: &mut Function, region: &[Block], versions: &[RegionClone]) -> Result<(), RegionError> {
    let originals = region_values(f, region)?;
    let mut excluded: HashSet<_> = region.iter().copied().collect();
    for version in versions {
        excluded.extend(version.block_map.values().copied());
    }
    let outside: Vec<_> = f
        .block_order()
        .iter()
        .copied()
        .filter(|b| !excluded.contains(b))
        .collect();
    for original in originals {
        let mut uses = Vec::new();
        for &block in &outside {
            for &inst in &f.block(block).insts {
                let data = f.inst(inst);
                if data.args.contains(&original)
                    || data
                        .targets
                        .iter()
                        .any(|target| target.args.contains(&original))
                    || data
                        .frame_state
                        .is_some_and(|id| state_values(f.frame_states.get(id)).contains(&original))
                {
                    uses.push((block, inst));
                }
            }
        }
        for entry in &f.osr_entries {
            if !excluded.contains(&entry.block)
                && (state_values(f.frame_states.get(entry.frame_state)).contains(&original)
                    || entry.checks.iter().any(|&(value, _)| value == original))
            {
                return Err("external OSR state references a region definition");
            }
        }
        if let Some(id) = f.entry_frame_state
            && state_values(f.frame_states.get(id)).contains(&original)
        {
            return Err("function entry state references a region definition");
        }
        if uses.is_empty() {
            continue;
        }
        let mut sources = HashMap::from([(defining_block(f, original)?, original)]);
        for version in versions {
            let value = *version
                .value_map
                .get(&original)
                .ok_or("clone is missing an escaping value")?;
            if f.value(value).repr != f.value(original).repr {
                return Err("escaping clone values have incompatible representations");
            }
            if !f.value(original).ty.bits.contains(f.value(value).ty.bits) {
                return Err("escaping clone value widens the original type");
            }
            let block = defining_block(f, value)?;
            if sources.insert(block, value).is_some() {
                return Err("multiple version definitions occupy one block");
            }
        }
        let mut updater = Updater {
            original,
            sources,
            entries: HashMap::new(),
            phis: Vec::new(),
        };
        for (block, inst) in uses {
            let replacement = updater.entry_value(f, block)?;
            let map = HashMap::from([(original, replacement)]);
            let state = f.inst(inst).frame_state.map(|id| {
                let mut state = f.frame_states.get(id).clone();
                map_state(&mut state, &map);
                f.frame_states.add(state)
            });
            let data = f.inst_mut(inst);
            for value in &mut data.args {
                *value = map_value(*value, &map);
            }
            for target in &mut data.targets {
                for value in &mut target.args {
                    *value = map_value(*value, &map);
                }
            }
            data.frame_state = state;
        }
        // An unreachable cycle must not manufacture a value solely from its
        // own phis. Reject such components instead of inventing an UNBOUND or
        // selecting one version arbitrarily.
        let mut grounded: HashSet<Value> = updater.sources.values().copied().collect();
        loop {
            let before = grounded.len();
            for &phi in &updater.phis {
                let ValueDef::Param { block, num } = f.value(phi).def else {
                    unreachable!()
                };
                if f.block_order().iter().any(|&pred| {
                    f.terminator(pred).is_some_and(|term| {
                        f.inst(term).targets.iter().any(|edge| {
                            edge.block == block
                                && edge
                                    .args
                                    .get(num as usize)
                                    .is_some_and(|v| grounded.contains(v))
                        })
                    })
                }) {
                    grounded.insert(phi);
                }
            }
            if grounded.len() == before {
                break;
            }
        }
        if updater.phis.iter().any(|phi| !grounded.contains(phi)) {
            return Err("SSA repair encountered a cycle without a reaching definition");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, RematOp, RematRecipe, RematRecipeId};
    use crate::t2::ir::{BlockCall, IRType, InstData, InstFlags, TypeBits, ValueRepresentation};

    fn data(opcode: Opcode, args: Vec<Value>, targets: Vec<BlockCall>) -> InstData {
        InstData {
            opcode,
            args,
            results: Vec::new(),
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets,
            frame_state: None,
            source_pos: 0,
        }
    }
    fn constant(f: &mut Function, block: Block, n: i64) -> Value {
        let mut d = data(Opcode::ConstFixnum, vec![], vec![]);
        d.aux = AuxData::FixnumImm(n);
        f.push_inst(
            block,
            d,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0]
    }
    fn jump(f: &mut Function, from: Block, to: Block, args: Vec<Value>) {
        f.set_terminator(
            from,
            data(Opcode::Jump, vec![], vec![BlockCall { block: to, args }]),
        );
    }
    fn ret(f: &mut Function, block: Block, value: Value) {
        f.set_terminator(block, data(Opcode::Return, vec![value], vec![]));
    }
    fn choose(f: &mut Function, block: Block, value: Value, yes: Block, no: Block) {
        f.set_terminator(
            block,
            data(
                Opcode::Brif,
                vec![value],
                vec![
                    BlockCall {
                        block: yes,
                        args: vec![],
                    },
                    BlockCall {
                        block: no,
                        args: vec![],
                    },
                ],
            ),
        );
    }
    fn state(value: Value) -> FrameState {
        FrameState {
            scopes: vec![FrameScope {
                function: 123,
                bcp: 7,
                locals: vec![ValueSource::Value {
                    value,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![ValueSource::Remat(RematRecipeId(0))],
            }],
            remat: vec![RematRecipe {
                op: RematOp::BoxFixnum,
                inputs: vec![ValueSource::Value {
                    value,
                    repr: ValueRepresentation::Tagged,
                }],
                result_repr: ValueRepresentation::Tagged,
            }],
        }
    }

    #[test]
    fn clone_remaps_forward_definitions_states_and_external_edges() {
        let mut f = Function::new("clone");
        let entry = f.entry();
        let body = f.make_block();
        let exit = f.make_block();
        let external = constant(&mut f, entry, 1);
        jump(&mut f, entry, body, vec![]);
        let value = constant(&mut f, body, 2);
        let id = f.frame_states.add(state(value));
        let mut d = data(Opcode::GenericAdd, vec![external, value], vec![]);
        d.frame_state = Some(id);
        let (inst, _) = f.push_inst(body, d, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        jump(&mut f, body, exit, vec![]);
        ret(&mut f, exit, external);
        // Reverse layout order intentionally: definitions must be preallocated.
        let c = clone_region(&mut f, &[body, entry]).unwrap();
        let cloned_inst = f.block(c.block_map[&body]).insts[1];
        assert_eq!(
            f.inst(cloned_inst).args,
            vec![c.value_map[&external], c.value_map[&value]]
        );
        assert_ne!(f.inst(cloned_inst).frame_state, f.inst(inst).frame_state);
        let s = f.frame_states.get(f.inst(cloned_inst).frame_state.unwrap());
        assert_eq!(
            state_values(s),
            vec![c.value_map[&value], c.value_map[&value]]
        );
        assert_eq!(s.scopes[0].function, 123);
        assert_eq!(s.scopes[0].bcp, 7);
        assert_eq!(
            f.inst(f.terminator(c.block_map[&body]).unwrap()).targets[0].block,
            exit
        );
        assert!(f.osr_entries.is_empty());
    }

    #[test]
    fn clone_invoke_maps_its_own_normal_edge_result() {
        let mut f = Function::new("invoke");
        let block = f.entry();
        let normal = f.make_block();
        let exceptional = f.make_block();
        let param = f.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let fallback = constant(&mut f, exceptional, 0);
        ret(&mut f, normal, param);
        ret(&mut f, exceptional, fallback);
        let d = data(
            Opcode::Invoke,
            vec![],
            vec![
                BlockCall {
                    block: normal,
                    args: vec![],
                },
                BlockCall {
                    block: exceptional,
                    args: vec![],
                },
            ],
        );
        let (inst, values) =
            f.set_terminator_with_results(block, d, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        f.inst_mut(inst).targets[0].args = values.clone();
        let c = clone_region(&mut f, &[block, normal]).unwrap();
        let cloned = f.inst(f.terminator(c.block_map[&block]).unwrap());
        assert_eq!(cloned.targets[0].args, vec![c.value_map[&values[0]]]);
        assert_eq!(cloned.targets[0].block, c.block_map[&normal]);
        assert_eq!(cloned.targets[1].block, exceptional);
    }

    fn diamond() -> (Function, Block, Block, Block, Value, RegionClone) {
        let mut f = Function::new("repair");
        let entry = f.entry();
        let original = f.make_block();
        let exit = f.make_block();
        let condition = constant(&mut f, entry, 1);
        let value = constant(&mut f, original, 7);
        jump(&mut f, original, exit, vec![]);
        ret(&mut f, exit, value);
        let c = clone_region(&mut f, &[original]).unwrap();
        choose(&mut f, entry, condition, original, c.block_map[&original]);
        (f, entry, original, exit, value, c)
    }

    #[test]
    fn repair_merges_versions_and_frame_state_only_values() {
        let (mut f, _, original, exit, value, c) = diamond();
        let ret = f.terminator(exit).unwrap();
        f.inst_mut(ret).args.clear();
        f.inst_mut(ret).opcode = Opcode::Trap;
        f.inst_mut(ret).frame_state = Some(f.frame_states.add(state(value)));
        repair_outside_uses(&mut f, &[original], std::slice::from_ref(&c)).unwrap();
        let phi = f.block(exit).params[0];
        let s = f.frame_states.get(f.inst(ret).frame_state.unwrap());
        assert_eq!(state_values(s), vec![phi, phi]);
        assert_eq!(
            f.inst(f.terminator(original).unwrap()).targets[0].args,
            vec![value]
        );
        assert_eq!(
            f.inst(f.terminator(c.block_map[&original]).unwrap())
                .targets[0]
                .args,
            vec![c.value_map[&value]]
        );
        crate::t2::verify::verify(&f).unwrap();
    }

    #[test]
    fn repair_threads_definitions_through_a_downstream_loop() {
        let (mut f, _, original, exit, value, c) = diamond();
        let done = f.make_block();
        // Replace the return with a loop that can exit or return to itself.
        let term = f.terminator(exit).unwrap();
        f.inst_mut(term).opcode = Opcode::Brif;
        f.inst_mut(term).args = vec![value];
        f.inst_mut(term).targets = vec![
            BlockCall {
                block: exit,
                args: vec![],
            },
            BlockCall {
                block: done,
                args: vec![],
            },
        ];
        ret(&mut f, done, value);
        repair_outside_uses(&mut f, &[original], std::slice::from_ref(&c)).unwrap();
        let phi = f.block(exit).params[0];
        assert_eq!(f.inst(term).args, vec![phi]);
        assert_eq!(f.inst(term).targets[0].args, vec![phi]);
        assert_ne!(f.inst(f.terminator(done).unwrap()).args, vec![value]);
        crate::t2::verify::verify(&f).unwrap();
    }

    #[test]
    fn unsupported_bypass_is_rejected_transactionally() {
        let (mut f, entry, original, exit, value, c) = diamond();
        let term = f.terminator(entry).unwrap();
        f.inst_mut(term).targets[1].block = exit;
        let blocks = f.num_blocks();
        let params = f.block(exit).params.clone();
        assert!(repair_outside_uses(&mut f, &[original], &[c]).is_err());
        assert_eq!(f.num_blocks(), blocks);
        assert_eq!(f.block(exit).params, params);
        assert_eq!(f.inst(f.terminator(exit).unwrap()).args, vec![value]);
    }
}
