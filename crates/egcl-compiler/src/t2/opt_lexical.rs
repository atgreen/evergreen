// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Block-local available values of captured lexical bindings, before ordinary
//! lowering hides them behind runtime calls. No binding or value is frozen.
//!
//! Keep the first potentially failing read and every store. A store alone does
//! not establish existence: the interpreter ignores a failed environment write.
//! Once a read succeeded, a same-index write updates its available SSA value.
//! Source lexical binders populate both EnvFrame key maps. Assignments may
//! leave a legacy name value stale, but never delete its binding key; the named
//! store updates that key and its symbol cache together. A bare store to an
//! unknown binding has no such proof and cannot seed the available map.
//! Unknown effects, calls, cooperation points and block boundaries kill facts.
//! ClearMv is deliberately retained: its helper only resets thread-local MV
//! state and cannot allocate, collect, suspend or signal. Guards may leave the
//! function, but on their successful continuation do not mutate lexical cells.
//!
//! Shared lexical cells can race; an unsynchronized read may observe a stale
//! valid value. Never move/forward a read across synchronization or a callback.
//! Rooted SSA values, including replacements in recovery recipes, remain precise
//! when a later collecting operation moves the object. There is no raw cache.

use std::collections::{HashMap, HashSet};

use super::frame_state::{FrameStateId, ValueSource};
use super::ir::{AuxData, Function, Inst, Opcode, Value, ValueRepresentation};

/// Only full ordinary entry candidates may use this pass. OSR candidates are
/// left untouched; facts never flow from one basic block to another.
pub fn forward(f: &mut Function) -> usize {
    if !f.osr_entries.is_empty() {
        return 0;
    }
    let mut replacements = HashMap::new();
    let mut removed = HashSet::new();
    for &block in f.block_order() {
        let mut available = HashMap::<u16, Value>::new();
        for &inst in &f.block(block).insts {
            let d = f.inst(inst);
            match (&d.opcode, &d.aux) {
                (Opcode::EnvironmentValue, AuxData::EnvironmentName(index))
                    if d.args.is_empty()
                        && d.results.len() == 1
                        && d.targets.is_empty()
                        && f.value(d.results[0]).repr == ValueRepresentation::Tagged =>
                {
                    if let Some(&value) = available.get(index) {
                        replacements.insert(d.results[0], resolve(&replacements, value));
                        removed.insert(inst);
                    } else {
                        available.insert(*index, d.results[0]);
                    }
                }
                (Opcode::SetEnvironmentValue, AuxData::EnvironmentName(index))
                    if d.args.len() == 1
                        && d.results.is_empty()
                        && d.targets.is_empty()
                        && f.value(d.args[0]).repr == ValueRepresentation::Tagged =>
                {
                    // Different name indexes might alias one canonical binding.
                    // Keep only this binding, and only if a read established it.
                    let known = available.contains_key(index);
                    available.clear();
                    if known {
                        available.insert(*index, resolve(&replacements, d.args[0]));
                    }
                }
                (Opcode::ClearMv, AuxData::None)
                    if d.args.is_empty() && d.results.is_empty() && d.targets.is_empty() => {}
                _ if transparent(d) => {}
                _ => available.clear(),
            }
        }
    }
    if removed.is_empty() {
        return 0;
    }
    // One rewrite, not one whole-function scan per eliminated read.
    for index in 0..f.num_insts() {
        let d = f.inst_mut(Inst(index as u32));
        for value in &mut d.args {
            *value = resolve(&replacements, *value);
        }
        for target in &mut d.targets {
            for value in &mut target.args {
                *value = resolve(&replacements, *value);
            }
        }
    }
    for index in 0..f.frame_states.len() {
        let state = f.frame_states.get_mut(FrameStateId(index as u32));
        let rewrite = |source: &mut ValueSource| {
            if let ValueSource::Value { value, .. } = source {
                *value = resolve(&replacements, *value);
            }
        };
        for scope in &mut state.scopes {
            scope.locals.iter_mut().for_each(rewrite);
            scope.stack.iter_mut().for_each(rewrite);
        }
        for recipe in &mut state.remat {
            recipe.inputs.iter_mut().for_each(rewrite);
        }
    }
    for block in f.block_order().to_vec() {
        f.block_mut(block)
            .insts
            .retain(|inst| !removed.contains(inst));
    }
    removed.len()
}

fn resolve(replacements: &HashMap<Value, Value>, mut value: Value) -> Value {
    while let Some(&replacement) = replacements.get(&value) {
        value = replacement;
    }
    value
}

fn transparent(d: &super::ir::InstData) -> bool {
    if d.flags.call || d.flags.safepoint || !d.targets.is_empty() {
        return false;
    }
    if d.flags.effectful && !(d.flags.guard && d.frame_state.is_some()) {
        return false;
    }
    // The guard effect orders a possible exit; its normal path has no memory
    // effect. Keep the guard and its exact FrameState, rather than weakening it.
    // An explicit allowlist also rejects scope changes or barriers even if an
    // old producer has omitted its effect flag. No memory-reading opcode here.
    matches!(
        d.opcode,
        Opcode::ConstFixnum
            | Opcode::ConstFloat
            | Opcode::ConstChar
            | Opcode::ConstSymbol
            | Opcode::ConstNil
            | Opcode::ConstT
            | Opcode::ConstHeapObj
            | Opcode::FixnumAdd
            | Opcode::FixnumSub
            | Opcode::FixnumMul
            | Opcode::FixnumNeg
            | Opcode::FixnumCmpEq
            | Opcode::FixnumCmpLt
            | Opcode::FixnumCmpLe
            | Opcode::FixnumCmpGt
            | Opcode::FixnumCmpGe
            | Opcode::Guard
            | Opcode::TypeCheck
    )
}

#[cfg(test)]
#[path = "opt_lexical_tests.rs"]
mod tests;
