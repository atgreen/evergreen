// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Keep speculated double-floats unboxed between operations (bliss-thcbb).
//!
//! # Purpose
//!
//! Double-float speculation (`speculate::speculate_with`) expands each site
//! locally into raw IR: guarded `UnboxFloat` operands, a raw `Float*`
//! operation and a `BoxFloat` of its result. That expansion is always correct
//! but allocates a box per operation. This pass removes the boxes a value does
//! not need, so a double flows raw (`UnboxedF64`, an eight-byte non-root home)
//! from one operation to the next and is boxed only where it escapes:
//!
//! * `UnboxFloat(BoxFloat(x))` is `x`: the guard cannot fail.
//! * A block parameter whose every incoming value is a box, a double literal,
//!   another converted parameter or an entry parameter becomes a raw
//!   parameter, provided it is consumed as a double (an `UnboxFloat` use, or
//!   it flows into such a parameter). Loop-carried accumulators therefore stay
//!   in a register across iterations. An entry parameter is unboxed once in
//!   the entry block by a guard that resumes the whole call in the
//!   interpreter, the same placement as `speculate`'s fixnum pre-guards.
//! * Tagged consumers of a converted parameter (a return, a call argument, an
//!   edge into a tagged parameter) get a `BoxFloat` placed just before them.
//! * Deopt frame states that named a box's tagged result name its raw input
//!   instead, as `UnboxedF64`; the deopt path reboxes it. Frame states that
//!   named a converted parameter change representation the same way.
//! * An `UnboxFloat` dominated by another of the same value reuses it.
//! * A box left with no use is removed.
//!
//! # Contract
//!
//! The function must verify on entry; [`optimize`] returns `false`, leaving
//! the function unusable, if the result does not verify, and the caller then
//! discards this candidate. Only the x86-64 Linux mapped artifact consumes
//! raw values. OSR entries are kept, their header states naming the raw
//! values: the full-call code drops them, and its OSR companion unboxes the
//! interpreter's double-floats on entry (bliss-vwpaz).

use super::frame_state::ValueSource;
use super::ir::{
    AuxData, Block, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value,
    ValueRepresentation as Repr,
};
use std::collections::{HashMap, HashSet};

fn raw_double() -> (IRType, Repr) {
    (IRType::of(TypeBits::DOUBLE_FLOAT), Repr::UnboxedF64)
}

fn boxed_double() -> (IRType, Repr) {
    (IRType::of(TypeBits::DOUBLE_FLOAT), Repr::Tagged)
}

fn location(f: &Function, inst: Inst) -> Option<(Block, usize)> {
    f.block_order().iter().find_map(|&block| {
        f.block(block)
            .insts
            .iter()
            .position(|&candidate| candidate == inst)
            .map(|position| (block, position))
    })
}

/// Insert `data` before `before` in its block (or at `position` of `block`).
fn insert_at(
    f: &mut Function,
    block: Block,
    position: usize,
    data: InstData,
    result: (IRType, Repr),
) -> Value {
    let (inst, results) = f.push_inst(block, data, &[result]);
    let insts = &mut f.block_mut(block).insts;
    let appended = insts.pop();
    debug_assert_eq!(appended, Some(inst));
    insts.insert(position, inst);
    results[0]
}

fn box_data(raw: Value, source_pos: u32) -> InstData {
    InstData {
        opcode: Opcode::BoxFloat,
        args: vec![raw],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags {
            effectful: true,
            ..InstFlags::default()
        },
        targets: vec![],
        frame_state: None,
        source_pos,
    }
}

/// Replace every use of `old` (operands, edge arguments, frame-state and
/// recipe sources) with `new`, carrying `new`'s representation into the
/// frame states.
fn replace_all_uses(f: &mut Function, old: Value, new: Value) {
    let repr = f.value(new).repr;
    for index in 0..f.num_insts() {
        let data = f.inst_mut(Inst(index as u32));
        for arg in data.args.iter_mut().chain(
            data.targets
                .iter_mut()
                .flat_map(|target| target.args.iter_mut()),
        ) {
            if *arg == old {
                *arg = new;
            }
        }
    }
    for state in f.frame_states.iter_mut() {
        for source in state
            .scopes
            .iter_mut()
            .flat_map(|scope| scope.locals.iter_mut().chain(scope.stack.iter_mut()))
            .chain(
                state
                    .remat
                    .iter_mut()
                    .flat_map(|recipe| recipe.inputs.iter_mut()),
            )
        {
            if let ValueSource::Value { value, repr: r } = source
                && *value == old
            {
                *value = new;
                *r = repr;
            }
        }
    }
}

fn remove_inst(f: &mut Function, inst: Inst) {
    if let Some((block, _)) = location(f, inst) {
        f.block_mut(block)
            .insts
            .retain(|&candidate| candidate != inst);
    }
}

/// `BoxFloat` result → raw input, for every box in the layout.
fn boxes(f: &Function) -> HashMap<Value, Value> {
    f.block_order()
        .iter()
        .flat_map(|&block| f.block(block).insts.iter())
        .map(|&inst| f.inst(inst))
        .filter(|data| {
            data.opcode == Opcode::BoxFloat && data.args.len() == 1 && data.results.len() == 1
        })
        .map(|data| (data.results[0], data.args[0]))
        .collect()
}

fn unbox_insts(f: &Function) -> Vec<Inst> {
    f.block_order()
        .iter()
        .flat_map(|&block| f.block(block).insts.iter().copied())
        .filter(|&inst| {
            let data = f.inst(inst);
            data.opcode == Opcode::UnboxFloat && !data.args.is_empty() && data.results.len() == 1
        })
        .collect()
}

/// A fixnum-accepting unbox checks its partner only so that two fixnums
/// cannot meet; once either operand is known to be a double the pair check is
/// moot. Drop it, so a raw or boxed partner is not kept alive or read tagged.
fn drop_settled_partners(f: &mut Function) {
    for index in 0..f.num_insts() {
        let inst = Inst(index as u32);
        let data = f.inst(inst);
        if data.opcode != Opcode::UnboxFloat || data.args.len() != 2 {
            continue;
        }
        let settled = |value: Value| {
            f.value(value).repr == Repr::UnboxedF64
                || f.value(value).ty.bits == TypeBits::DOUBLE_FLOAT
        };
        if settled(data.args[0]) || settled(data.args[1]) {
            f.inst_mut(inst).args.truncate(1);
        }
    }
}

fn fold_unbox_of_box(f: &mut Function) {
    let boxes = boxes(f);
    for inst in unbox_insts(f) {
        let data = f.inst(inst);
        let input = data.args[0];
        let raw = boxes
            .get(&input)
            .copied()
            .or((f.value(input).repr == Repr::UnboxedF64).then_some(input));
        if let Some(raw) = raw {
            let result = data.results[0];
            replace_all_uses(f, result, raw);
            remove_inst(f, inst);
        }
    }
}

/// Every edge argument passed to each block parameter.
fn incoming(f: &Function) -> HashMap<Value, Vec<(Inst, usize, usize)>> {
    let mut map: HashMap<Value, Vec<(Inst, usize, usize)>> = HashMap::new();
    for &block in f.block_order() {
        let Some(term) = f.terminator(block) else {
            continue;
        };
        for (target_index, target) in f.inst(term).targets.iter().enumerate() {
            for (arg_index, _) in target.args.iter().enumerate() {
                if let Some(&param) = f.block(target.block).params.get(arg_index) {
                    map.entry(param)
                        .or_default()
                        .push((term, target_index, arg_index));
                }
            }
        }
    }
    map
}

fn may_be_double(f: &Function, value: Value) -> bool {
    let bits = f.value(value).ty.bits;
    !bits.is_bottom() && !bits.meet(TypeBits::DOUBLE_FLOAT).is_bottom()
}

/// Convert loop-carried and merged doubles to raw block parameters.
fn convert_params(f: &mut Function) {
    let entry = f.entry();
    let entry_params: HashSet<Value> = if f.entry_frame_state.is_some() {
        f.block(entry)
            .params
            .iter()
            .copied()
            .filter(|&p| may_be_double(f, p))
            .collect()
    } else {
        HashSet::new()
    };
    let boxes = boxes(f);
    let incoming = incoming(f);
    let edge_arg = |f: &Function, (term, target, arg): (Inst, usize, usize)| {
        f.inst(term).targets[target].args[arg]
    };
    let unboxed_uses: HashSet<Value> = unbox_insts(f)
        .iter()
        .map(|&inst| f.inst(inst).args[0])
        .collect();
    let params: Vec<Value> = f
        .block_order()
        .iter()
        .filter(|&&block| block != entry)
        .flat_map(|&block| f.block(block).params.iter().copied())
        .filter(|&p| {
            f.value(p).repr == Repr::Tagged && may_be_double(f, p) && incoming.contains_key(&p)
        })
        .collect();
    // Consumed as a double: an unbox use, or flowing into such a parameter.
    // Only these may unbox an entry parameter, whose guard would otherwise
    // deopt on a value nothing treats as a double.
    let mut consumed: HashSet<Value> = params
        .iter()
        .copied()
        .filter(|p| unboxed_uses.contains(p))
        .collect();
    loop {
        let grown: Vec<Value> = params
            .iter()
            .copied()
            .filter(|p| !consumed.contains(p))
            .filter(|p| {
                consumed
                    .iter()
                    .any(|q| incoming[q].iter().any(|&site| edge_arg(f, site) == *p))
            })
            .collect();
        if grown.is_empty() {
            break;
        }
        consumed.extend(grown);
    }
    let mut candidates: HashSet<Value> = params.iter().copied().collect();
    loop {
        // Admissible: every incoming value can be supplied raw. Useful: the
        // value is consumed as a double, or an incoming edge would otherwise
        // box it; boxing at the tagged consumers instead is then no worse.
        let next: HashSet<Value> = candidates
            .iter()
            .copied()
            .filter(|p| {
                incoming[p].iter().all(|&site| {
                    let arg = edge_arg(f, site);
                    boxes.contains_key(&arg)
                        || super::speculate::double_literal_bits(f, arg).is_some()
                        || candidates.contains(&arg)
                        || (entry_params.contains(&arg) && consumed.contains(p))
                })
            })
            .filter(|p| {
                consumed.contains(p)
                    || incoming[p].iter().any(|&site| {
                        let arg = edge_arg(f, site);
                        boxes.contains_key(&arg) || (candidates.contains(&arg) && arg != *p)
                    })
            })
            .collect();
        if next.len() == candidates.len() {
            break;
        }
        candidates = next;
    }
    if candidates.is_empty() {
        return;
    }
    // OSR entries stay: a header state that names a converted parameter
    // marks it `UnboxedF64` below, and the mapped entry unboxes that
    // interpreter slot on import (bliss-vwpaz).
    let entry_state = f.entry_frame_state;
    let mut entry_unboxed: HashMap<Value, Value> = HashMap::new();
    let mut ordered: Vec<Value> = candidates.iter().copied().collect();
    ordered.sort_by_key(|value| value.0);
    for &p in &ordered {
        f.set_repr(p, Repr::UnboxedF64);
        f.refine_type(p, IRType::of(TypeBits::DOUBLE_FLOAT));
    }
    for &p in &ordered {
        for &(term, target, arg_index) in &incoming[&p] {
            let arg = f.inst(term).targets[target].args[arg_index];
            let raw = if let Some(&raw) = boxes.get(&arg) {
                raw
            } else if candidates.contains(&arg) {
                arg
            } else if let Some(bits) = super::speculate::double_literal_bits(f, arg) {
                let (block, position) = location(f, term).expect("terminator in layout");
                insert_at(
                    f,
                    block,
                    position,
                    InstData {
                        opcode: Opcode::ConstDouble,
                        args: vec![],
                        results: vec![],
                        aux: AuxData::DoubleImm(bits),
                        flags: InstFlags::default(),
                        targets: vec![],
                        frame_state: None,
                        source_pos: 0,
                    },
                    raw_double(),
                )
            } else {
                debug_assert!(entry_params.contains(&arg));
                match entry_unboxed.get(&arg) {
                    Some(&raw) => raw,
                    None => {
                        let state = f.frame_states.add(
                            f.frame_states
                                .get(entry_state.expect("entry state"))
                                .clone(),
                        );
                        let position = entry_unboxed.len();
                        let raw = insert_at(
                            f,
                            entry,
                            position,
                            InstData {
                                opcode: Opcode::UnboxFloat,
                                args: vec![arg],
                                results: vec![],
                                aux: AuxData::None,
                                flags: InstFlags {
                                    guard: true,
                                    effectful: true,
                                    ..InstFlags::default()
                                },
                                targets: vec![],
                                frame_state: Some(state),
                                source_pos: 0,
                            },
                            raw_double(),
                        );
                        entry_unboxed.insert(arg, raw);
                        raw
                    }
                }
            };
            f.inst_mut(term).targets[target].args[arg_index] = raw;
        }
    }
    // Consumers of each converted parameter.
    for &p in &ordered {
        // Every consumer other than an unbox expected the tagged value; collect
        // them before folding the unboxes, whose users then read `p` raw.
        let users: Vec<(Block, Inst)> = f
            .block_order()
            .iter()
            .flat_map(|&block| f.block(block).insts.iter().map(move |&inst| (block, inst)))
            .filter(|&(_, inst)| {
                let data = f.inst(inst);
                data.opcode != Opcode::UnboxFloat
                    && (data.args.contains(&p)
                        || data.targets.iter().any(|target| target.args.contains(&p)))
            })
            .collect();
        for inst in unbox_insts(f) {
            if f.inst(inst).args[0] == p {
                let result = f.inst(inst).results[0];
                replace_all_uses(f, result, p);
                remove_inst(f, inst);
            }
        }
        // One box per consuming block, before its first tagged consumer.
        let mut block_boxes: HashMap<Block, Value> = HashMap::new();
        for (block, inst) in users {
            {
                let data = f.inst(inst).clone();
                let tagged_operand = data.args.contains(&p);
                let tagged_edge = data.targets.iter().any(|target| {
                    target.args.iter().enumerate().any(|(index, &arg)| {
                        arg == p
                            && f.block(target.block)
                                .params
                                .get(index)
                                .is_some_and(|&param| f.value(param).repr == Repr::Tagged)
                    })
                });
                if !tagged_operand && !tagged_edge {
                    continue;
                }
                let boxed = match block_boxes.get(&block) {
                    Some(&boxed) => boxed,
                    None => {
                        let position = f
                            .block(block)
                            .insts
                            .iter()
                            .position(|&i| i == inst)
                            .unwrap();
                        let boxed = insert_at(
                            f,
                            block,
                            position,
                            box_data(p, data.source_pos),
                            boxed_double(),
                        );
                        block_boxes.insert(block, boxed);
                        boxed
                    }
                };
                let target_params: Vec<Vec<Value>> = data
                    .targets
                    .iter()
                    .map(|target| f.block(target.block).params.clone())
                    .collect();
                let user = f.inst_mut(inst);
                for arg in user.args.iter_mut() {
                    if *arg == p {
                        *arg = boxed;
                    }
                }
                for (target, params) in user.targets.iter_mut().zip(&target_params) {
                    for (index, arg) in target.args.iter_mut().enumerate() {
                        if *arg == p
                            && params
                                .get(index)
                                .is_some_and(|&param| !candidates.contains(&param))
                        {
                            *arg = boxed;
                        }
                    }
                }
            }
        }
        for state in f.frame_states.iter_mut() {
            for source in state
                .scopes
                .iter_mut()
                .flat_map(|scope| scope.locals.iter_mut().chain(scope.stack.iter_mut()))
                .chain(
                    state
                        .remat
                        .iter_mut()
                        .flat_map(|recipe| recipe.inputs.iter_mut()),
                )
            {
                if let ValueSource::Value { value, repr } = source
                    && *value == p
                {
                    *repr = Repr::UnboxedF64;
                }
            }
        }
    }
}

/// Frame states name a box's raw input rather than its tagged result.
fn raw_frame_state_sources(f: &mut Function) {
    let boxes = boxes(f);
    for state in f.frame_states.iter_mut() {
        for source in state
            .scopes
            .iter_mut()
            .flat_map(|scope| scope.locals.iter_mut().chain(scope.stack.iter_mut()))
            .chain(
                state
                    .remat
                    .iter_mut()
                    .flat_map(|recipe| recipe.inputs.iter_mut()),
            )
        {
            if let ValueSource::Value { value, repr } = source
                && let Some(&raw) = boxes.get(value)
            {
                *value = raw;
                *repr = Repr::UnboxedF64;
            }
        }
    }
}

/// An `UnboxFloat` dominated by another of the same tagged value reuses it.
fn reuse_dominating_unboxes(f: &mut Function) {
    let dom = f.dominators();
    let mut by_value: HashMap<Value, Vec<(Inst, Block, usize)>> = HashMap::new();
    for &block in f.block_order() {
        for (position, &inst) in f.block(block).insts.iter().enumerate() {
            let data = f.inst(inst);
            if data.opcode == Opcode::UnboxFloat && data.args.len() == 1 && data.results.len() == 1
            {
                by_value
                    .entry(data.args[0])
                    .or_default()
                    .push((inst, block, position));
            }
        }
    }
    let mut replaced = HashSet::new();
    for (_, sites) in by_value {
        for &(later, later_block, later_position) in &sites {
            let dominating = sites.iter().find(|&&(earlier, block, position)| {
                earlier != later
                    && !replaced.contains(&earlier)
                    && if block == later_block {
                        position < later_position
                    } else {
                        dom.dominates(block, later_block)
                    }
            });
            // The reused guard must be at least as strict: a double-only
            // unbox serves any later one; a fixnum-accepting one serves only
            // another fixnum-accepting unbox of the same value.
            let dominating = dominating.filter(|&&(earlier, _, _)| {
                !super::speculate::unbox_accepts_fixnum(&f.inst(earlier).aux)
                    || super::speculate::unbox_accepts_fixnum(&f.inst(later).aux)
            });
            if let Some(&(earlier, _, _)) = dominating {
                let old = f.inst(later).results[0];
                let new = f.inst(earlier).results[0];
                replace_all_uses(f, old, new);
                remove_inst(f, later);
                replaced.insert(later);
            }
        }
    }
}

/// Remove boxes whose tagged result is no longer used anywhere.
pub fn remove_dead_boxes(f: &mut Function) {
    let mut used: HashSet<Value> = HashSet::new();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            used.extend(data.args.iter().copied());
            used.extend(
                data.targets
                    .iter()
                    .flat_map(|target| target.args.iter().copied()),
            );
        }
    }
    for (_, state) in f.frame_states.iter() {
        for source in state
            .scopes
            .iter()
            .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
            .chain(state.remat.iter().flat_map(|recipe| &recipe.inputs))
        {
            if let ValueSource::Value { value, .. } = source {
                used.insert(*value);
            }
        }
    }
    let dead: Vec<Inst> = f
        .block_order()
        .iter()
        .flat_map(|&block| f.block(block).insts.iter().copied())
        .filter(|&inst| {
            let data = f.inst(inst);
            data.opcode == Opcode::BoxFloat && data.results.iter().all(|r| !used.contains(r))
        })
        .collect();
    for inst in dead {
        remove_inst(f, inst);
    }
}

/// `(raw double operations, calls)` in `f`: raw arithmetic and comparisons,
/// and the `Call`/`Invoke` sites that remain. Raw values are admitted only
/// by the mapped calling convention, whose calls cost more than the ordinary
/// ABI's (bliss-k4psf); a caller weighs the two before choosing raw code.
pub fn raw_operation_and_call_counts(f: &Function) -> (usize, usize) {
    let mut operations = 0;
    let mut calls = 0;
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            match data.opcode {
                Opcode::FloatAdd
                | Opcode::FloatSub
                | Opcode::FloatMul
                | Opcode::FloatDiv
                | Opcode::FloatSqrt
                | Opcode::FloatNeg
                | Opcode::FloatCmpEq
                | Opcode::FloatCmpLt
                | Opcode::FloatCmpLe
                | Opcode::FloatCmpGt
                | Opcode::FloatCmpGe
                    if data
                        .args
                        .iter()
                        .any(|&value| f.value(value).repr == Repr::UnboxedF64) =>
                {
                    operations += 1
                }
                Opcode::Call | Opcode::Invoke if matches!(data.aux, AuxData::CallTarget(_)) => {
                    calls += 1
                }
                _ => {}
            }
        }
    }
    (operations, calls)
}

/// Whether `f` holds any raw double value.
pub fn has_raw_doubles(f: &Function) -> bool {
    (0..f.num_values()).any(|index| f.value(Value(index as u32)).repr == Repr::UnboxedF64)
}

/// Run the cleanup. Returns `false` when the result does not verify; the
/// caller must then discard `f`.
pub fn optimize(f: &mut Function) -> bool {
    if !has_raw_doubles(f) {
        return true;
    }
    drop_settled_partners(f);
    fold_unbox_of_box(f);
    convert_params(f);
    drop_settled_partners(f);
    fold_unbox_of_box(f);
    raw_frame_state_sources(f);
    reuse_dominating_unboxes(f);
    remove_dead_boxes(f);
    super::verify::verify(f).is_ok()
}

/// Compact one-line-per-instruction rendering for diagnostics.
pub fn render(f: &Function) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let value = |v: Value| {
        let data = f.value(v);
        let repr = match data.repr {
            Repr::Tagged => "",
            Repr::UnboxedF64 => ":f64",
            Repr::UnboxedF32 => ":f32",
            Repr::UnboxedFixnum => ":i64",
        };
        format!("v{}{}<{:#x}>", v.0, repr, data.ty.bits.0)
    };
    for &block in f.block_order() {
        let params: Vec<String> = f.block(block).params.iter().map(|&p| value(p)).collect();
        let _ = writeln!(out, "block{}({}):", block.0, params.join(", "));
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            let results: Vec<String> = data.results.iter().map(|&r| value(r)).collect();
            let args: Vec<String> = data.args.iter().map(|&a| value(a)).collect();
            let targets: Vec<String> = data
                .targets
                .iter()
                .map(|t| {
                    let a: Vec<String> = t.args.iter().map(|&x| format!("v{}", x.0)).collect();
                    format!("block{}({})", t.block.0, a.join(","))
                })
                .collect();
            let _ = writeln!(
                out,
                "  i{} {} = {:?}({}) {:?} {} fs={:?}",
                inst.0,
                results.join(","),
                data.opcode,
                args.join(", "),
                data.aux,
                targets.join(" "),
                data.frame_state.map(|id| id.0)
            );
        }
    }
    out
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::BlockCall;
    use crate::t2::speculate::{SpecType, SpeculateOptions, speculate_with};

    fn inst(opcode: Opcode, args: Vec<Value>, aux: AuxData) -> InstData {
        InstData {
            opcode,
            args,
            results: vec![],
            aux,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    /// `(let ((s x)) (loop (setq s (+ s x)) (when ... (return s))))`:
    /// entry(x) -> header(s): r = (+ s x); brif -> header(r) | exit(r);
    /// exit(v): return v.
    fn accumulation_loop() -> (Function, Block, Block, Value) {
        let mut f = Function::new("acc");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, Repr::Tagged);
        let entry_state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: x,
                    repr: Repr::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        f.entry_frame_state = Some(entry_state);
        let header = f.make_block();
        let exit = f.make_block();
        let s = f.add_block_param(header, IRType::TOP, Repr::Tagged);
        let v = f.add_block_param(exit, IRType::TOP, Repr::Tagged);
        f.set_terminator(
            entry,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![x],
                }],
                ..inst(Opcode::Jump, vec![], AuxData::None)
            },
        );
        let site_state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 5,
                locals: vec![
                    ValueSource::Value {
                        value: s,
                        repr: Repr::Tagged,
                    },
                    ValueSource::Value {
                        value: x,
                        repr: Repr::Tagged,
                    },
                ],
                stack: vec![],
            }],
            remat: vec![],
        });
        let plus = egcl_rt::symbols::intern("+");
        let (_, sum) = f.push_inst(
            header,
            InstData {
                flags: InstFlags {
                    call: true,
                    effectful: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                frame_state: Some(site_state),
                ..inst(Opcode::Call, vec![s, x], AuxData::CallTarget(plus))
            },
            &[(IRType::TOP, Repr::Tagged)],
        );
        let (_, condition) = f.push_inst(
            header,
            inst(Opcode::ConstT, vec![], AuxData::None),
            &[(IRType::of(TypeBits::SYMBOL), Repr::Tagged)],
        );
        f.set_terminator(
            header,
            InstData {
                targets: vec![
                    BlockCall {
                        block: header,
                        args: vec![sum[0]],
                    },
                    BlockCall {
                        block: exit,
                        args: vec![sum[0]],
                    },
                ],
                ..inst(Opcode::Brif, vec![condition[0]], AuxData::None)
            },
        );
        f.set_terminator(exit, inst(Opcode::Return, vec![v], AuxData::None));
        assert!(crate::t2::verify::verify(&f).is_ok());
        (f, header, exit, x)
    }

    fn opcodes(f: &Function, block: Block) -> Vec<Opcode> {
        f.block(block)
            .insts
            .iter()
            .map(|&inst| f.inst(inst).opcode)
            .collect()
    }

    #[test]
    fn loop_carried_double_stays_raw_and_boxes_only_at_the_exit() {
        let (mut f, header, exit, x) = accumulation_loop();
        let options = SpeculateOptions { double_float: true };
        assert_eq!(
            speculate_with(&mut f, &|_| Some(SpecType::DoubleFloat), options),
            1
        );
        assert!(optimize(&mut f), "{}", render(&f));
        let s = f.block(header).params[0];
        assert_eq!(f.value(s).repr, Repr::UnboxedF64, "{}", render(&f));
        assert_eq!(
            f.value(f.block(exit).params[0]).repr,
            Repr::UnboxedF64,
            "{}",
            render(&f)
        );
        // The loop body neither unboxes nor boxes: the parameter is unboxed
        // once at entry, guarded with the entry frame state.
        let body = opcodes(&f, header);
        assert!(
            !body.contains(&Opcode::BoxFloat) && !body.contains(&Opcode::UnboxFloat),
            "{}",
            render(&f)
        );
        assert!(body.contains(&Opcode::FloatAdd), "{}", render(&f));
        let entry = f.entry();
        let first = f.inst(f.block(entry).insts[0]);
        assert_eq!(first.opcode, Opcode::UnboxFloat, "{}", render(&f));
        assert_eq!(first.args, [x]);
        assert!(
            first
                .frame_state
                .is_some_and(|state| f.frame_states.get(state).scopes[0].bcp == 0)
        );
        // The only box is the escape at the return.
        assert_eq!(
            opcodes(&f, exit),
            [Opcode::BoxFloat, Opcode::Return],
            "{}",
            render(&f)
        );
        // The site's frame state now names the raw accumulator, reboxed on deopt.
        let named_raw = f.frame_states.iter().any(|(_, state)| {
            state.scopes[0].locals.iter().any(|source|
            matches!(source, ValueSource::Value { value, repr: Repr::UnboxedF64 } if *value == s))
        });
        assert!(named_raw, "{}", render(&f));
        // A loop header keeps its OSR entry, whose state names the raw
        // accumulator for the entry to unbox (bliss-vwpaz).
        assert!(f.osr_entries.iter().all(|entry| {
            f.frame_states.get(entry.frame_state).scopes[0]
                .locals
                .iter()
                .all(|source| {
                    !matches!(source,
                ValueSource::Value { value, repr: Repr::Tagged } if *value == s)
                })
        }));
    }

    #[test]
    fn tagged_artifact_keeps_the_loop_tagged() {
        let (mut f, header, _, _) = accumulation_loop();
        assert_eq!(
            speculate_with(
                &mut f,
                &|_| Some(SpecType::DoubleFloat),
                SpeculateOptions::default()
            ),
            0
        );
        assert!(!has_raw_doubles(&f));
        assert!(optimize(&mut f));
        assert_eq!(f.value(f.block(header).params[0]).repr, Repr::Tagged);
    }
}
