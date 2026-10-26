// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Choose real signed-61-bit machine values for arithmetic SSA islands. Tagged
//! boundaries remain explicit; frame states describe the machine value so cold
//! exits rebox it. OSR entries are deliberately left to the tagged artifact.
use super::frame_state::ValueSource;
use super::ir::{
    AuxData, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value,
    ValueRepresentation as Repr,
};
use std::collections::{HashMap, HashSet};

fn arithmetic(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::FixnumAdd
            | Opcode::FixnumSub
            | Opcode::FixnumMul
            | Opcode::FixnumNeg
            | Opcode::FixnumShl
            | Opcode::FixnumShr
            | Opcode::LogAnd
            | Opcode::LogOr
            | Opcode::LogXor
            | Opcode::LogNot
    )
}
fn comparison(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::FixnumCmpEq
            | Opcode::FixnumCmpLt
            | Opcode::FixnumCmpLe
            | Opcode::FixnumCmpGt
            | Opcode::FixnumCmpGe
    )
}
/// Runtime proof independent of a speculative/inferred type annotation. These
/// tagged producers cannot produce a non-fixnum on their successful path.
pub(crate) fn proven_tagged_fixnum(f: &Function, value: Value) -> bool {
    use super::ir::ValueDef;
    if f.value(value).repr != Repr::Tagged || f.value(value).ty.bits != TypeBits::FIXNUM {
        return false;
    }
    match f.value(value).def {
        ValueDef::Result { inst, .. } => {
            let data = f.inst(inst);
            data.opcode == Opcode::ConstFixnum
                || data.opcode == Opcode::BoxFixnum
                    && data.args.len() == 1
                    && f.value(data.args[0]).repr == Repr::UnboxedFixnum
                || data.opcode == Opcode::Guard
                    && data.flags.guard
                    && data.frame_state.is_some()
                    && matches!(data.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM)
        }
        ValueDef::Param { block, .. } => block == f.entry() && f.is_entry_param_checked(value),
    }
}

fn conversion(f: &mut Function, before: Inst, value: Value, raw: bool) -> Value {
    let block = f
        .block_order()
        .iter()
        .copied()
        .find(|&b| f.block(b).insts.contains(&before))
        .unwrap();
    let old = f.inst(before).clone();
    let literal = if raw {
        match f.value(value).def {
            super::ir::ValueDef::Result { inst, .. }
                if f.inst(inst).opcode == Opcode::ConstFixnum =>
            {
                Some(f.inst(inst).aux.clone())
            }
            _ => None,
        }
    } else {
        None
    };
    let checked = raw && literal.is_none() && !proven_tagged_fixnum(f, value);
    let ty = IRType {
        range: f.value(value).ty.range.or(match literal.as_ref() {
            Some(AuxData::FixnumImm(n)) => Some(super::ir::Range { lo: *n, hi: *n }),
            _ => None,
        }),
        ..IRType::of(TypeBits::FIXNUM)
    };
    let (inst, results) = f.push_inst(
        block,
        InstData {
            opcode: if literal.is_some() {
                Opcode::ConstFixnum
            } else if raw {
                Opcode::UnboxFixnum
            } else {
                Opcode::BoxFixnum
            },
            args: if literal.is_some() {
                vec![]
            } else {
                vec![value]
            },
            results: vec![],
            aux: literal.unwrap_or(AuxData::None),
            flags: InstFlags {
                guard: checked,
                ..InstFlags::default()
            },
            targets: vec![],
            frame_state: if checked { old.frame_state } else { None },
            source_pos: old.source_pos,
        },
        &[(
            ty,
            if raw {
                Repr::UnboxedFixnum
            } else {
                Repr::Tagged
            },
        )],
    );
    let list = &mut f.block_mut(block).insts;
    assert_eq!(list.pop(), Some(inst));
    let pos = list.iter().position(|&i| i == before).unwrap();
    list.insert(pos, inst);
    results[0]
}

/// Full representation capability, also used by backend execution tests.
pub fn run(f: &mut Function) -> usize {
    run_internal(f, false)
}

/// Production admission: keep only connected numeric islands containing both
/// a closed raw loop phi and a right shift/low-bit extraction. In particular,
/// isolated additions and additive counters retain their efficient tagged ABI.
pub fn run_profitable(f: &mut Function) -> usize {
    run_internal(f, true)
}

fn profitable_values(f: &Function, raw: &HashSet<Value>) -> HashSet<Value> {
    let is_constant = |value| {
        matches!(f.value(value).def,
        super::ir::ValueDef::Result { inst, .. } if f.inst(inst).opcode == Opcode::ConstFixnum)
    };
    let nodes: HashSet<_> = raw.iter().copied().filter(|&v| !is_constant(v)).collect();
    let mut edges: HashMap<Value, Vec<Value>> = HashMap::new();
    let mut loops = HashSet::new();
    let mut shifts = HashSet::new();
    let dom = f.dominators();
    let mut connect = |left, right| {
        if nodes.contains(&left) && nodes.contains(&right) {
            edges.entry(left).or_default().push(right);
            edges.entry(right).or_default().push(left);
        }
    };
    for &block in f.block_order() {
        for &site in &f.block(block).insts {
            let d = f.inst(site);
            if (arithmetic(d.opcode) || d.opcode == Opcode::IntegerLowBits) && d.results.len() == 1
            {
                let result = d.results[0];
                for &arg in &d.args {
                    connect(result, arg);
                }
                let negative_shift = matches!(d.opcode, Opcode::FixnumShl | Opcode::FixnumShr)
                    && d.args
                        .get(1)
                        .is_some_and(|&amount| match f.value(amount).def {
                            super::ir::ValueDef::Result { inst, .. } => matches!(f.inst(inst).aux,
                            AuxData::FixnumImm(n) if f.inst(inst).opcode == Opcode::ConstFixnum
                                && (n < 0 || d.opcode == Opcode::FixnumShr)),
                            _ => false,
                        });
                if negative_shift || d.opcode == Opcode::IntegerLowBits {
                    shifts.insert(result);
                }
            }
            for edge in &d.targets {
                for (&source, &param) in edge.args.iter().zip(&f.block(edge.block).params) {
                    connect(source, param);
                    if nodes.contains(&source)
                        && nodes.contains(&param)
                        && dom.dominates(edge.block, block)
                    {
                        loops.insert(param);
                    }
                }
            }
        }
    }
    let mut visited = HashSet::new();
    let mut selected = HashSet::new();
    for &root in &nodes {
        if !visited.insert(root) {
            continue;
        }
        let mut component = vec![root];
        let mut cursor = 0;
        while cursor < component.len() {
            let value = component[cursor];
            cursor += 1;
            for &next in edges.get(&value).into_iter().flatten() {
                if visited.insert(next) {
                    component.push(next);
                }
            }
        }
        if component.iter().any(|v| loops.contains(v))
            && component.iter().any(|v| shifts.contains(v))
        {
            selected.extend(component);
        }
    }
    // Constants never join islands and remain tagged globally. Each selected
    // use gets its own raw literal, avoiding conversion churn in another loop
    // that happens to share the same constant SSA definition.
    selected
}

/// Return the number of SSA values assigned the raw fixnum representation.
fn run_internal(f: &mut Function, profitable: bool) -> usize {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return 0;
    }
    let sites: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.clone())
        .collect();
    let mut imported = HashSet::new();
    for entry in &f.osr_entries {
        imported.extend(f.block(entry.block).params.iter().copied());
        imported.extend(entry.checks.iter().map(|(value, _)| *value));
        let state = f.frame_states.get(entry.frame_state);
        for source in state
            .scopes
            .iter()
            .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
            .chain(state.remat.iter().flat_map(|recipe| &recipe.inputs))
        {
            if let ValueSource::Value { value, .. } = source {
                imported.insert(*value);
            }
        }
    }
    let mut raw = HashSet::new();
    for &site in &sites {
        let d = f.inst(site);
        if d.results.len() == 1
            && (d.opcode == Opcode::ConstFixnum
                || d.opcode == Opcode::IntegerLowBits
                || arithmetic(d.opcode) && d.frame_state.is_some())
        {
            raw.insert(d.results[0]);
        }
    }
    // A phi is raw if all incoming sources are raw (including other fixnum
    // phis), or independently proven tagged fixnums that can be unboxed on
    // their edge without a new failure point. This includes a peeled loop's
    // preheader Guard result; speculative type metadata alone is insufficient.
    for &b in f.block_order() {
        if b != f.entry() && !f.osr_entries.iter().any(|entry| entry.block == b) {
            for &param in &f.block(b).params {
                if f.value(param).ty.bits == TypeBits::FIXNUM {
                    raw.insert(param);
                }
            }
        }
    }
    raw.retain(|value| !imported.contains(value));
    loop {
        let mut remove = Vec::new();
        for &b in f.block_order() {
            for &site in &f.block(b).insts {
                for edge in &f.inst(site).targets {
                    for (&source, &param) in edge.args.iter().zip(&f.block(edge.block).params) {
                        if raw.contains(&param)
                            && !raw.contains(&source)
                            && !proven_tagged_fixnum(f, source)
                        {
                            remove.push(param);
                        }
                    }
                }
            }
        }
        if remove.is_empty() {
            break;
        }
        for v in remove {
            raw.remove(&v);
        }
    }
    if profitable {
        raw = profitable_values(f, &raw);
    }
    if !raw.iter().any(|&v| {
        !matches!(f.value(v).def,
        super::ir::ValueDef::Result { inst, .. } if f.inst(inst).opcode == Opcode::ConstFixnum)
    }) {
        return 0;
    }
    for &value in &raw {
        f.set_repr(value, Repr::UnboxedFixnum);
    }
    for site in sites {
        let d = f.inst(site).clone();
        let wants_raw = arithmetic(d.opcode) && d.results.first().is_some_and(|v| raw.contains(v))
            || comparison(d.opcode) && d.frame_state.is_some()
                && (!profitable || d.args.iter().any(|value| raw.contains(value))
                    && d.args.iter().all(|&value| raw.contains(&value)
                        || matches!(f.value(value).def, super::ir::ValueDef::Result { inst, .. }
                            if f.inst(inst).opcode == Opcode::ConstFixnum)));
        for (index, &value) in d.args.iter().enumerate() {
            // IntegerLowBits accepts either representation, including a generic
            // integer at the peeled boundary; never narrow its input to fixnum.
            if d.opcode == Opcode::IntegerLowBits {
                continue;
            }
            if index == 1 && matches!(d.opcode, Opcode::FixnumShl | Opcode::FixnumShr) {
                continue;
            }
            let is_raw = raw.contains(&value);
            if wants_raw != is_raw {
                let converted = conversion(f, site, value, wants_raw);
                f.inst_mut(site).args[index] = converted;
            }
        }
        for (edge_index, edge) in d.targets.iter().enumerate() {
            let params = f.block(edge.block).params.clone();
            for (index, (&value, param)) in edge.args.iter().zip(params).enumerate() {
                if raw.contains(&value) != raw.contains(&param) {
                    let converted = conversion(f, site, value, raw.contains(&param));
                    f.inst_mut(site).targets[edge_index].args[index] = converted;
                }
            }
        }
    }
    for state in f.frame_states.iter_mut() {
        for source in state
            .scopes
            .iter_mut()
            .flat_map(|scope| scope.locals.iter_mut().chain(&mut scope.stack))
            .chain(state.remat.iter_mut().flat_map(|recipe| &mut recipe.inputs))
        {
            if let ValueSource::Value { value, repr } = source {
                if raw.contains(value) {
                    *repr = Repr::UnboxedFixnum;
                }
            }
        }
    }
    raw.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::{BlockCall, OsrEntry};

    fn instruction(opcode: Opcode) -> InstData {
        InstData {
            opcode,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }
    #[test]
    fn raw_loop_phi_keeps_osr_import_tagged_and_boxes_exit() {
        let mut f = Function::new("raw-loop-with-tagged-osr-route");
        let entry = f.entry();
        let osr = f.make_block();
        let fast = f.make_block();
        let done = f.make_block();
        let fx = IRType::of(TypeBits::FIXNUM);
        let imported = f.add_block_param(osr, fx, Repr::Tagged);
        let i = f.add_block_param(fast, fx, Repr::Tagged);
        let result = f.add_block_param(done, IRType::TOP, Repr::Tagged);
        let fs = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: imported,
                    repr: Repr::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        f.osr_entries.push(OsrEntry {
            control_scopes: vec![],
            bcp: 0,
            block: osr,
            frame_state: fs,
            checks: vec![(imported, fx)],
        });
        let (_, zero) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(0),
                ..instruction(Opcode::ConstFixnum)
            },
            &[(fx, Repr::Tagged)],
        );
        let (_, one) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(1),
                ..instruction(Opcode::ConstFixnum)
            },
            &[(fx, Repr::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                targets: vec![BlockCall {
                    block: osr,
                    args: vec![zero[0]],
                }],
                ..instruction(Opcode::Jump)
            },
        );
        // Model the peeled fast preheader: the type guard still returns a
        // tagged fixnum, while the fast loop must carry a raw machine integer.
        let (_, narrowed) = f.push_inst(
            osr,
            InstData {
                args: vec![imported],
                aux: AuxData::TypeTag(fx),
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: Some(fs),
                ..instruction(Opcode::Guard)
            },
            &[(fx, Repr::Tagged)],
        );
        f.set_terminator(
            osr,
            InstData {
                targets: vec![BlockCall {
                    block: fast,
                    args: vec![narrowed[0]],
                }],
                ..instruction(Opcode::Jump)
            },
        );
        let loop_state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 1,
                locals: vec![ValueSource::Value {
                    value: i,
                    repr: Repr::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let (_, next) = f.push_inst(
            fast,
            InstData {
                args: vec![i, one[0]],
                frame_state: Some(loop_state),
                ..instruction(Opcode::FixnumAdd)
            },
            &[(fx, Repr::Tagged)],
        );
        let (_, cond) = f.push_inst(
            fast,
            InstData {
                args: vec![next[0], one[0]],
                frame_state: Some(loop_state),
                ..instruction(Opcode::FixnumCmpLt)
            },
            &[(IRType::TOP, Repr::Tagged)],
        );
        f.set_terminator(
            fast,
            InstData {
                args: cond,
                targets: vec![
                    BlockCall {
                        block: fast,
                        args: next.clone(),
                    },
                    BlockCall {
                        block: done,
                        args: next,
                    },
                ],
                ..instruction(Opcode::Brif)
            },
        );
        f.set_terminator(
            done,
            InstData {
                args: vec![result],
                ..instruction(Opcode::Return)
            },
        );
        let mut counter = f.clone();
        let original_instructions = counter.num_insts();
        assert_eq!(
            run_profitable(&mut counter),
            0,
            "an additive counter is already efficient tagged math"
        );
        assert_eq!(counter.num_insts(), original_instructions);
        assert!(
            (0..counter.num_values())
                .all(|index| counter.value(Value(index as u32)).repr == Repr::Tagged)
        );

        let mut shifted = f.clone();
        let entry_term = shifted.terminator(entry).unwrap();
        let (amount_inst, amount) = shifted.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(-8),
                ..instruction(Opcode::ConstFixnum)
            },
            &[(fx, Repr::Tagged)],
        );
        let body = &mut shifted.block_mut(entry).insts;
        body.retain(|&site| site != amount_inst);
        let pos = body.iter().position(|&site| site == entry_term).unwrap();
        body.insert(pos, amount_inst);
        let tail = shifted.terminator(fast).unwrap();
        let current = shifted.inst(tail).targets[0].args[0];
        let (shift_inst, shr) = shifted.push_inst(
            fast,
            InstData {
                args: vec![current, amount[0]],
                frame_state: Some(loop_state),
                ..instruction(Opcode::FixnumShl)
            },
            &[(fx, Repr::Tagged)],
        );
        let body = &mut shifted.block_mut(fast).insts;
        body.retain(|&site| site != shift_inst);
        let pos = body.iter().position(|&site| site == tail).unwrap();
        body.insert(pos, shift_inst);
        shifted.inst_mut(tail).targets[0].args[0] = shr[0];
        assert!(run_profitable(&mut shifted) > 0);
        assert_eq!(shifted.value(i).repr, Repr::UnboxedFixnum);
        assert!(
            !shifted
                .block(fast)
                .insts
                .iter()
                .any(|&site| shifted.inst(site).opcode == Opcode::UnboxFixnum)
        );
        assert_eq!(shifted.value(imported).repr, Repr::Tagged);

        assert!(run(&mut f) > 0);
        assert_eq!(f.value(imported).repr, Repr::Tagged);
        assert_eq!(f.value(i).repr, Repr::UnboxedFixnum);
        let unbox = f
            .block(osr)
            .insts
            .iter()
            .copied()
            .find(|&site| f.inst(site).opcode == Opcode::UnboxFixnum)
            .expect("unbox guarded preheader once");
        assert_eq!(f.inst(unbox).args, narrowed);
        assert!(!f.inst(unbox).flags.guard);
        assert!(f.inst(unbox).frame_state.is_none());
        assert!(
            !f.block(fast)
                .insts
                .iter()
                .any(|&site| matches!(f.inst(site).opcode, Opcode::UnboxFixnum))
        );

        assert_eq!(f.value(result).repr, Repr::Tagged);
        assert!(matches!(
            f.frame_states.get(loop_state).scopes[0].locals[0],
            ValueSource::Value {
                repr: Repr::UnboxedFixnum,
                ..
            }
        ));
        let terminator = f.inst(f.terminator(fast).unwrap());
        assert_eq!(
            f.value(terminator.targets[0].args[0]).repr,
            Repr::UnboxedFixnum
        );
        assert_eq!(f.value(terminator.targets[1].args[0]).repr, Repr::Tagged);
        assert!(
            f.block(fast)
                .insts
                .iter()
                .any(|&site| f.inst(site).opcode == Opcode::BoxFixnum)
        );
    }
    #[test]
    fn inferred_fixnum_type_is_not_permission_for_pure_unboxing() {
        let mut f = Function::new("unchecked-profile-is-not-proof");
        let entry = f.entry();
        let value = f.add_block_param(entry, IRType::of(TypeBits::FIXNUM), Repr::Tagged);
        assert!(!proven_tagged_fixnum(&f, value));
        f.mark_entry_param_checked(value);
        assert!(proven_tagged_fixnum(&f, value));
    }
    #[test]
    fn profitable_selection_leaves_isolated_add_and_shared_literal_tagged() {
        let mut f = Function::new("tagged-add-leaf");
        let block = f.entry();
        let fx = IRType::of(TypeBits::FIXNUM);
        let x = f.add_block_param(block, fx, Repr::Tagged);
        let fs = f.frame_states.add(FrameState {
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
        let (_, one) = f.push_inst(
            block,
            InstData {
                aux: AuxData::FixnumImm(1),
                ..instruction(Opcode::ConstFixnum)
            },
            &[(fx, Repr::Tagged)],
        );
        let (_, sum) = f.push_inst(
            block,
            InstData {
                args: vec![x, one[0]],
                frame_state: Some(fs),
                ..instruction(Opcode::FixnumAdd)
            },
            &[(fx, Repr::Tagged)],
        );
        f.set_terminator(
            block,
            InstData {
                args: sum,
                ..instruction(Opcode::Return)
            },
        );
        let before = f.num_insts();
        assert_eq!(run_profitable(&mut f), 0);
        assert_eq!(f.num_insts(), before);
        assert!((0..f.num_values()).all(|index| f.value(Value(index as u32)).repr == Repr::Tagged));
    }
}
