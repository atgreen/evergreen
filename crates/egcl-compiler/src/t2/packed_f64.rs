// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Closed packed-double expressions for mapped full-entry compilation.
//!
//! Keep the caller's original tagged artifact available for fallback. This pass
//! admits array reads and double add/multiply ending in array stores, with no
//! raw argument, return or edge escape. The mapped caller must separately
//! arrange native representation maps and decline raw OSR imports.

use super::builtin_binding::{self, Binding};
use super::frame_state::ValueSource;
use super::ir::{
    AuxData, Block, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value, ValueDef,
    ValueRepresentation as Repr,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Load,
    Store,
    Add,
    Mul,
}

fn kind(f: &Function, inst: Inst) -> Option<Kind> {
    let data = f.inst(inst);
    let AuxData::CallTarget(symbol) = data.aux else {
        return None;
    };
    if data.opcode != Opcode::Call || egcl_rt::symbols::is_uninterned(symbol) {
        return None;
    }
    let key = egcl_rt::symbols::registry_key(symbol)?;
    if egcl_rt::symbols::find_index(&key) != Some(symbol) {
        return None;
    }
    match key.as_str() {
        "AREF" => Some(Kind::Load),
        "EGCL::SET-AREF" => Some(Kind::Store),
        "+" => Some(Kind::Add),
        "*" => Some(Kind::Mul),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Expansion {
    kind: Kind,
    binding: Binding,
}

fn expansion(
    f: &Function,
    inst: Inst,
    kind: Kind,
    permits: &dyn Fn(u32, super::speculate::SpecType) -> bool,
) -> Option<Expansion> {
    let data = f.inst(inst);
    if data.args.len() != (if kind == Kind::Store { 3 } else { 2 })
        || data.results.len() != 1
        || data.frame_state.is_none()
        || !data.targets.is_empty()
    {
        return None;
    }
    let AuxData::CallTarget(symbol) = data.aux else {
        return None;
    };
    let bcp = f
        .frame_states
        .get(data.frame_state?)
        .scopes
        .last()
        .map_or(0, |scope| scope.bcp);
    if !permits(bcp, super::speculate::SpecType::DoubleFloat) {
        return None;
    }
    Some(Expansion {
        kind,
        binding: builtin_binding::snapshot(symbol)?,
    })
}

fn expression(
    f: &Function,
    value: Value,
    block: Block,
    locations: &HashMap<Inst, Block>,
    plan: &mut HashMap<Inst, Expansion>,
    permits: &dyn Fn(u32, super::speculate::SpecType) -> bool,
) -> Option<bool> {
    let ValueDef::Result { inst, .. } = f.value(value).def else {
        return Some(false);
    };
    if locations.get(&inst) != Some(&block) {
        return Some(false);
    }
    let Some(kind) = kind(f, inst) else {
        return Some(false);
    };
    if kind == Kind::Store {
        return Some(false);
    }
    let selected = expansion(f, inst, kind, permits)?;
    if plan.contains_key(&inst) {
        return Some(true);
    }
    if kind != Kind::Load {
        let mut from_array = false;
        for &arg in &f.inst(inst).args {
            from_array |= expression(f, arg, block, locations, plan, permits)?;
        }
        if !from_array {
            return Some(false);
        }
    }
    plan.insert(inst, selected);
    Some(true)
}

fn plan(
    f: &Function,
    permits: &dyn Fn(u32, super::speculate::SpecType) -> bool,
) -> Option<HashMap<Inst, Expansion>> {
    if super::verify::verify(f).is_err()
        || (0..f.num_values()).any(|i| f.value(Value(i as u32)).repr != Repr::Tagged)
    {
        return None;
    }
    let locations: HashMap<_, _> = f
        .block_order()
        .iter()
        .flat_map(|&block| f.block(block).insts.iter().map(move |&inst| (inst, block)))
        .collect();
    let mut plan = HashMap::new();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            if kind(f, inst) != Some(Kind::Store) {
                continue;
            }
            let selected = expansion(f, inst, Kind::Store, permits)?;
            if expression(
                f,
                f.inst(inst).args[2],
                block,
                &locations,
                &mut plan,
                permits,
            )? {
                plan.insert(inst, selected);
            }
        }
    }
    if plan.is_empty() {
        return None;
    }
    let raw: HashSet<_> = plan
        .iter()
        .filter(|(_, e)| e.kind != Kind::Store)
        .map(|(&inst, _)| f.inst(inst).results[0])
        .collect();
    let discarded: HashSet<_> = plan
        .iter()
        .filter(|(_, e)| e.kind == Kind::Store)
        .map(|(&inst, _)| f.inst(inst).results[0])
        .collect();
    if raw
        .iter()
        .any(|&value| f.value(value).ty.bits.meet(TypeBits::DOUBLE_FLOAT) != TypeBits::DOUBLE_FLOAT)
    {
        return None;
    }
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            for (position, value) in data.args.iter().enumerate() {
                if discarded.contains(value) {
                    return None;
                }
                if raw.contains(value)
                    && !plan.get(&inst).is_some_and(|e| {
                        matches!(e.kind, Kind::Add | Kind::Mul)
                            || (e.kind == Kind::Store && position == 2)
                    })
                {
                    return None;
                }
            }
            if data
                .targets
                .iter()
                .flat_map(|edge| &edge.args)
                .any(|value| raw.contains(value) || discarded.contains(value))
            {
                return None;
            }
        }
    }
    for (_, state) in f.frame_states.iter() {
        for source in state
            .scopes
            .iter()
            .flat_map(|s| s.locals.iter().chain(&s.stack))
        {
            if matches!(source, ValueSource::Value { value, .. } if discarded.contains(value)) {
                return None;
            }
        }
        if state.remat.iter().flat_map(|recipe| &recipe.inputs).any(|source|
            matches!(source, ValueSource::Value { value, .. } if raw.contains(value) || discarded.contains(value)))
        { return None; }
    }
    Some(plan)
}

fn insert_before(f: &mut Function, block: Block, before: Inst, data: InstData) -> Value {
    let types = [(IRType::of(TypeBits::DOUBLE_FLOAT), Repr::UnboxedF64)];
    let (inst, results) = f.push_inst(block, data, &types);
    let body = &mut f.block_mut(block).insts;
    assert_eq!(body.pop(), Some(inst));
    let position = body.iter().position(|&i| i == before).unwrap();
    body.insert(position, inst);
    results[0]
}

/// Boot SETF binds its sole store value through TakeValuesToLocals. For the
/// selected single-valued producer this copies the primary without changing
/// MV state. Keep the producer's eventual ClearMv; add no binder reset.
/// Restrict this normalization to adjacent producers in the same block, and
/// require those producers to belong to the final closed raw graph below.
fn bypass_store_transports(
    f: &mut Function,
    permits: &dyn Fn(u32, super::speculate::SpecType) -> bool,
) -> HashMap<Value, Value> {
    let mut aliases = HashMap::new();
    let mut removed = HashSet::new();
    for &block in f.block_order() {
        for pair in f.block(block).insts.windows(2) {
            let [producer, transport] = pair else {
                unreachable!()
            };
            let data = f.inst(*transport);
            if data.opcode != Opcode::TakeValuesToLocals
                || !matches!(data.aux, AuxData::ValuesLocals { nvars: 1, .. })
                || data.args.len() != 1
                || data.results.len() != 1
                || !data.targets.is_empty()
                || f.inst(*producer).results != data.args
            {
                continue;
            }
            let Some(kind @ (Kind::Load | Kind::Add | Kind::Mul)) = kind(f, *producer) else {
                continue;
            };
            if expansion(f, *producer, kind, permits).is_none() {
                continue;
            }
            aliases.insert(data.results[0], data.args[0]);
            removed.insert(*transport);
        }
    }
    if aliases.is_empty() {
        return aliases;
    }
    for index in 0..f.num_insts() {
        let data = f.inst_mut(Inst(index as u32));
        for value in data
            .args
            .iter_mut()
            .chain(data.targets.iter_mut().flat_map(|edge| &mut edge.args))
        {
            if let Some(&primary) = aliases.get(value) {
                *value = primary;
            }
        }
    }
    let states: Vec<_> = f.frame_states.iter().map(|(id, _)| id).collect();
    for id in states {
        let state = f.frame_states.get_mut(id);
        for source in state
            .scopes
            .iter_mut()
            .flat_map(|scope| scope.locals.iter_mut().chain(&mut scope.stack))
            .chain(state.remat.iter_mut().flat_map(|recipe| &mut recipe.inputs))
        {
            if let ValueSource::Value { value, .. } = source
                && let Some(&primary) = aliases.get(value)
            {
                *value = primary;
            }
        }
    }
    for block in f.block_order().to_vec() {
        f.block_mut(block)
            .insts
            .retain(|inst| !removed.contains(inst));
    }
    aliases
}

/// Specialize atomically: declined shapes retain their exact tagged IR.
/// Returns the number of replaced calls, excluding inserted guards/MV effects.
pub fn specialize(original: &mut Function) -> usize {
    specialize_with_feedback(original, &|_, _| true)
}

/// A rejected family leaves the whole original closed expression unchanged.
/// Do not commit a partially raw graph after any selected guard site is vetoed.
pub fn specialize_with_feedback(
    original: &mut Function,
    permits: &dyn Fn(u32, super::speculate::SpecType) -> bool,
) -> usize {
    if super::verify::verify(original).is_err() {
        return 0;
    }
    let mut candidate = original.clone();
    let aliases = bypass_store_transports(&mut candidate, permits);
    let Some(plan) = plan(&candidate, permits) else {
        return 0;
    };
    let raw: HashSet<_> = plan
        .iter()
        .filter(|(_, e)| e.kind != Kind::Store)
        .map(|(&inst, _)| candidate.inst(inst).results[0])
        .collect();
    if aliases.values().any(|primary| !raw.contains(primary)) {
        return 0;
    }
    let f = &mut candidate;
    for &value in &raw {
        f.set_repr(value, Repr::UnboxedF64);
        f.refine_type(value, IRType::of(TypeBits::DOUBLE_FLOAT));
    }
    for block in f.block_order().to_vec() {
        for inst in f.block(block).insts.clone() {
            let Some(expansion) = plan.get(&inst) else {
                continue;
            };
            let old = f.inst(inst).clone();
            let state = old.frame_state.unwrap();
            let position = f
                .block(block)
                .insts
                .iter()
                .position(|&i| i == inst)
                .unwrap();
            builtin_binding::insert(f, block, position, state, expansion.binding);
            if matches!(expansion.kind, Kind::Add | Kind::Mul) {
                for (index, &value) in old.args.iter().enumerate() {
                    if raw.contains(&value) {
                        continue;
                    }
                    if f.value(value).ty.bits.meet(TypeBits::DOUBLE_FLOAT) != TypeBits::DOUBLE_FLOAT
                    {
                        return 0;
                    }
                    let guard_state = f.frame_states.add(f.frame_states.get(state).clone());
                    let result = insert_before(
                        f,
                        block,
                        inst,
                        InstData {
                            opcode: Opcode::UnboxFloat,
                            args: vec![value],
                            results: vec![],
                            aux: AuxData::None,
                            flags: InstFlags {
                                guard: true,
                                effectful: true,
                                ..InstFlags::default()
                            },
                            targets: vec![],
                            frame_state: Some(guard_state),
                            source_pos: old.source_pos,
                        },
                    );
                    f.inst_mut(inst).args[index] = result;
                }
            }
            let data = f.inst_mut(inst);
            data.aux = AuxData::None;
            data.opcode = match expansion.kind {
                Kind::Load => Opcode::PackedF64Ref,
                Kind::Store => Opcode::PackedF64Set,
                Kind::Add => Opcode::FloatAdd,
                Kind::Mul => Opcode::FloatMul,
            };
            data.flags = if matches!(expansion.kind, Kind::Load | Kind::Store) {
                InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                }
            } else {
                data.frame_state = None;
                InstFlags::default()
            };
            if expansion.kind == Kind::Store {
                data.results.clear();
            }
            let mv_state = f.frame_states.add(f.frame_states.get(state).clone());
            let (clear, _) = f.push_inst(
                block,
                InstData {
                    opcode: Opcode::ClearMv,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags {
                        effectful: true,
                        call: true,
                        safepoint: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: Some(mv_state),
                    source_pos: old.source_pos,
                },
                &[],
            );
            let body = &mut f.block_mut(block).insts;
            assert_eq!(body.pop(), Some(clear));
            let position = body.iter().position(|&i| i == inst).unwrap();
            body.insert(position + 1, clear);
        }
    }
    let states: Vec<_> = f.frame_states.iter().map(|(id, _)| id).collect();
    for id in states {
        for source in f
            .frame_states
            .get_mut(id)
            .scopes
            .iter_mut()
            .flat_map(|scope| scope.locals.iter_mut().chain(&mut scope.stack))
        {
            if let ValueSource::Value { value, repr } = source {
                if raw.contains(value) {
                    *repr = Repr::UnboxedF64;
                }
            }
        }
    }
    if super::verify::verify(f).is_err() {
        return 0;
    }
    let count = plan.len();
    // The original function is replaced only after the complete candidate passes.
    *original = candidate;
    count
}
