// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Fuse a private `(ldb (byte width 0) integer)` without allocating a byte
//! descriptor. A failing integer guard replays the exact pre-BYTE continuation.

use std::collections::{HashMap, HashSet};

use super::builtin_binding::{self, Binding};
use super::frame_state::{FrameState, ValueSource};
use super::inlining::{InlineOptions, InlinePolicy};
use super::ir::{
    AuxData, Block, Function, IRType, Inst, InstData, InstFlags, Opcode, Range, TypeBits, Value,
    ValueDef,
};

fn canonical(data: &InstData, key: &str) -> bool {
    let AuxData::CallTarget(symbol) = data.aux else {
        return false;
    };
    matches!(data.opcode, Opcode::Call | Opcode::Invoke)
        && egcl_rt::symbols::registry_key(symbol).as_deref() == Some(key)
        && builtin_binding::snapshot(symbol) == Some(Binding::FixedStandard)
}

fn literal(f: &Function, value: Value) -> Option<i64> {
    let ValueDef::Result { inst, .. } = f.value(value).def else {
        return None;
    };
    match (&f.inst(inst).opcode, &f.inst(inst).aux) {
        (Opcode::ConstFixnum, AuxData::FixnumImm(value)) => Some(*value),
        _ => None,
    }
}

fn resolve(mut value: Value, aliases: &HashMap<Value, Value>) -> Value {
    for _ in 0..=aliases.len() {
        let Some(&source) = aliases.get(&value) else {
            break;
        };
        if source == value {
            break;
        }
        value = source;
    }
    value
}

fn same_source(a: &ValueSource, b: &ValueSource, aliases: &HashMap<Value, Value>) -> bool {
    match (a, b) {
        (ValueSource::Value { value: a, repr: ar }, ValueSource::Value { value: b, repr: br }) => {
            resolve(*a, aliases) == resolve(*b, aliases) && ar == br
        }
        (ValueSource::Const(a), ValueSource::Const(b)) => a == b,
        (ValueSource::Unbound, ValueSource::Unbound) => true,
        _ => false,
    }
}

fn mentions(state: &FrameState, values: &HashSet<Value>) -> bool {
    state
        .scopes
        .iter()
        .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
        .chain(state.remat.iter().flat_map(|recipe| &recipe.inputs))
        .any(|source| matches!(source, ValueSource::Value { value, .. } if values.contains(value)))
}

struct Match {
    byte: Inst,
    byte_block: Block,
    ldb: Inst,
    ldb_block: Block,
    mask: i64,
    aliases: HashSet<Value>,
    forwarding: Vec<(Block, usize)>,
    middle: Vec<Inst>,
    guard_state: FrameState,
    normal_state: FrameState,
}

fn find(f: &Function, ldb_block: Block, ldb: Inst, options: &InlineOptions) -> Option<Match> {
    let data = f.inst(ldb);
    if !canonical(data, "LDB") || data.args.len() != 2 || data.results.len() != 1 {
        return None;
    }
    let ldb_state = f.frame_states.get(data.frame_state?);
    let [ldb_scope] = ldb_state.scopes.as_slice() else {
        return None;
    };
    if !ldb_state.remat.is_empty() || options.policy_at(ldb_scope.bcp) == InlinePolicy::NotInline {
        return None;
    }
    let integer = data.args[1];
    let mut descriptor = data.args[0];
    let mut aliases = HashSet::new();
    let mut forwarding = Vec::new();
    let mut normal_aliases = HashMap::new();
    let mut middle = Vec::new();
    let mut visited = HashSet::new();
    let mut block = ldb_block;
    let mut end = f.block(block).insts.iter().position(|&i| i == ldb)?;
    let (byte_block, byte) = 'trace: loop {
        if !visited.insert(block) || f.osr_entries.iter().any(|entry| entry.block == block) {
            return None;
        }
        aliases.insert(descriptor);
        for &previous in f.block(block).insts[..end].iter().rev() {
            let prior = f.inst(previous);
            if prior.results.contains(&descriptor) {
                if !canonical(prior, "BYTE") || prior.args.len() != 2 || prior.results.len() != 1 {
                    return None;
                }
                break 'trace (block, previous);
            }
            // Loading the integer argument can be an SSA alias or a literal.
            // Never replay an intervening call, guard, allocation or mutation.
            if prior.opcode != Opcode::ClearMv
                && (!matches!(
                    prior.opcode,
                    Opcode::ConstFixnum
                        | Opcode::ConstFloat
                        | Opcode::ConstChar
                        | Opcode::ConstSymbol
                        | Opcode::ConstNil
                        | Opcode::ConstT
                        | Opcode::ConstHeapObj
                        | Opcode::ConstDouble
                ) || prior.flags.effectful
                    || prior.flags.call
                    || prior.flags.safepoint
                    || prior.flags.guard)
            {
                return None;
            }
            middle.push(previous);
        }
        let predecessors = f.preds(block);
        let [predecessor] = predecessors.as_slice() else {
            return None;
        };
        let instructions = &f.block(*predecessor).insts;
        let edge = f.inst(*instructions.last()?);
        if !matches!(edge.opcode, Opcode::Jump | Opcode::Invoke)
            || edge.targets.first()?.block != block
        {
            return None;
        }
        if edge.targets[0].args.len() != f.block(block).params.len() {
            return None;
        }
        for (&parameter, &argument) in f.block(block).params.iter().zip(&edge.targets[0].args) {
            normal_aliases.insert(parameter, argument);
        }
        if let ValueDef::Param { block: owner, num } = f.value(descriptor).def {
            if owner == block {
                forwarding.push((block, usize::from(num)));
                descriptor = *edge.targets[0].args.get(usize::from(num))?;
            }
        }
        block = *predecessor;
        end = instructions.len() - usize::from(edge.opcode == Opcode::Jump);
    };
    let constructor = f.inst(byte);
    let width = literal(f, constructor.args[0])?;
    if !(0..=60).contains(&width) || literal(f, constructor.args[1])? != 0 {
        return None;
    }
    let byte_state = f.frame_states.get(constructor.frame_state?);
    let [byte_scope] = byte_state.scopes.as_slice() else {
        return None;
    };
    if !byte_state.remat.is_empty()
        || byte_scope.function != ldb_scope.function
        || byte_scope.bcp.checked_add(2)? != ldb_scope.bcp
        || options.policy_at(byte_scope.bcp) == InlinePolicy::NotInline
        || byte_scope.locals.len() != ldb_scope.locals.len()
        || !byte_scope
            .locals
            .iter()
            .zip(&ldb_scope.locals)
            .all(|(a, b)| matches!(b, ValueSource::Unbound) || same_source(a, b, &normal_aliases))
        || byte_scope.stack.len() < 2
        || byte_scope.stack.len() != ldb_scope.stack.len()
    {
        return None;
    }
    let prefix = byte_scope.stack.len() - 2;
    let source_is = |source: &ValueSource, value: Value| match source {
        ValueSource::Value { value: source, .. } => {
            resolve(*source, &normal_aliases) == resolve(value, &normal_aliases)
        }
        ValueSource::Const(source) => literal(f, resolve(value, &normal_aliases))
            .is_some_and(|n| *source == egcl_rt::value::EgclVal::from_fixnum(n)),
        _ => false,
    };
    if !source_is(&byte_scope.stack[prefix], constructor.args[0])
        || !source_is(&byte_scope.stack[prefix + 1], constructor.args[1])
    {
        return None;
    }

    if !byte_scope.stack[..prefix]
        .iter()
        .zip(&ldb_scope.stack[..prefix])
        .all(|(a, b)| same_source(a, b, &normal_aliases))
        || !matches!(ldb_scope.stack[prefix], ValueSource::Value { value, .. } if aliases.contains(&value))
        || !source_is(&ldb_scope.stack[prefix + 1], integer)
        || mentions(byte_state, &aliases)
    {
        return None;
    }
    // The sole executable consumer of each descriptor alias must be the LDB
    // or its one forwarding edge. Frame-state uses are checked separately.
    let mut uses = HashMap::<Value, usize>::new();
    for &b in f.block_order() {
        for &i in &f.block(b).insts {
            let instruction = f.inst(i);
            for &value in instruction
                .args
                .iter()
                .chain(instruction.targets.iter().flat_map(|edge| &edge.args))
            {
                if aliases.contains(&value) {
                    *uses.entry(value).or_default() += 1;
                }
            }
        }
    }
    if aliases.iter().any(|value| uses.get(value) != Some(&1)) {
        return None;
    }
    let mut normal_state = ldb_state.clone();
    let scope = &mut normal_state.scopes[0];
    scope.bcp = scope.bcp.checked_add(1)?;
    scope.stack.truncate(prefix);
    scope.stack.push(ValueSource::Value {
        value: data.results[0],
        repr: f.value(data.results[0]).repr,
    });
    Some(Match {
        byte,
        byte_block,
        ldb,
        ldb_block,
        mask: (1i64 << width) - 1,
        aliases,
        forwarding,
        middle,
        guard_state: byte_state.clone(),
        normal_state,
    })
}

fn apply(f: &mut Function, found: &Match) -> bool {
    let previously_reachable: HashSet<_> = f.reverse_postorder().into_iter().collect();
    // Only mutate a private clone. Any unresolved metadata reference rejects
    // the whole candidate before the caller publishes it.
    if f.inst(found.byte).opcode == Opcode::Invoke {
        let normal = f.inst(found.byte).targets[0].clone();
        let byte = f.inst_mut(found.byte);
        byte.opcode = Opcode::Jump;
        byte.args.clear();
        byte.results.clear();
        byte.aux = AuxData::None;
        byte.flags = InstFlags {
            terminator: true,
            ..InstFlags::default()
        };
        byte.targets = vec![normal];
        byte.frame_state = None;
    } else {
        f.block_mut(found.byte_block)
            .insts
            .retain(|&i| i != found.byte);
    }
    let normal_edge =
        (f.inst(found.ldb).opcode == Opcode::Invoke).then(|| f.inst(found.ldb).targets[0].clone());
    let guard = f.frame_states.add(found.guard_state.clone());
    let normal = f.frame_states.add(found.normal_state.clone());
    let integer = f.inst(found.ldb).args[1];
    let result = f.inst(found.ldb).results[0];
    let ldb = f.inst_mut(found.ldb);
    ldb.opcode = Opcode::IntegerLowBits;
    ldb.args = vec![integer];
    ldb.aux = AuxData::FixnumImm(found.mask);
    ldb.flags = InstFlags {
        effectful: true,
        guard: true,
        ..InstFlags::default()
    };
    ldb.targets.clear();
    ldb.frame_state = Some(guard);
    f.refine_type(
        result,
        IRType {
            bits: TypeBits::FIXNUM,
            range: Some(Range {
                lo: 0,
                hi: found.mask,
            }),
            class_id: None,
        },
    );
    if let Some(edge) = normal_edge {
        f.set_terminator(
            found.ldb_block,
            InstData {
                opcode: Opcode::Jump,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    terminator: true,
                    ..InstFlags::default()
                },
                targets: vec![edge],
                frame_state: None,
                source_pos: 0,
            },
        );
    }
    // BYTE is gone, so its MV reset is redundant. Pure argument instructions
    // have no recovery point and must not keep the removed descriptor alive.
    for &i in &found.middle {
        if f.inst(i).opcode == Opcode::ClearMv {
            for &b in f.block_order().to_vec().iter() {
                f.block_mut(b).insts.retain(|&live| live != i);
            }
        } else {
            f.inst_mut(i).frame_state = None;
        }
    }
    let (clear, _) = f.push_inst(
        found.ldb_block,
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
            frame_state: Some(normal),
            source_pos: 0,
        },
        &[],
    );
    assert_eq!(f.block_mut(found.ldb_block).insts.pop(), Some(clear));
    let pos = f
        .block(found.ldb_block)
        .insts
        .iter()
        .position(|&i| i == found.ldb)
        .unwrap();
    f.block_mut(found.ldb_block).insts.insert(pos + 1, clear);
    for &(block, index) in &found.forwarding {
        f.remove_block_param(block, index);
    }
    // Removed exceptional edges must not remain compilation/OSR roots. This
    // function's surviving entry graph is authoritative for this transaction.
    let reachable: HashSet<_> = f.reverse_postorder().into_iter().collect();
    f.osr_entries.retain(|entry| {
        !previously_reachable.contains(&entry.block) || reachable.contains(&entry.block)
    });
    for b in f.block_order().to_vec() {
        if previously_reachable.contains(&b) && !reachable.contains(&b) {
            f.block_mut(b).params.clear();
            f.block_mut(b).insts.clear();
            f.set_terminator(
                b,
                InstData {
                    opcode: Opcode::Trap,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags {
                        terminator: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: None,
                    source_pos: 0,
                },
            );
            f.remove_from_layout(b);
        }
    }
    for &b in f.block_order() {
        for &i in &f.block(b).insts {
            let data = f.inst(i);
            if data
                .args
                .iter()
                .chain(data.targets.iter().flat_map(|edge| &edge.args))
                .any(|value| found.aliases.contains(value))
                || data
                    .frame_state
                    .is_some_and(|state| mentions(f.frame_states.get(state), &found.aliases))
            {
                return false;
            }
        }
    }
    if f.osr_entries.iter().any(|entry| {
        mentions(f.frame_states.get(entry.frame_state), &found.aliases)
            || entry
                .checks
                .iter()
                .any(|(value, _)| found.aliases.contains(value))
    }) {
        return false;
    }
    super::verify::verify(f).is_ok()
}

/// Rewrite only fully proven private descriptors; every declined candidate
/// leaves the original function and all of its recovery metadata unchanged.
pub fn specialize(f: &mut Function, options: &InlineOptions) -> usize {
    let mut count = 0;
    for block in f.block_order().to_vec() {
        for inst in f.block(block).insts.clone() {
            let Some(found) = find(f, block, inst, options) else {
                continue;
            };
            let mut candidate = f.clone();
            if apply(&mut candidate, &found) {
                *f = candidate;
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameStateId};
    use crate::t2::ir::{BlockCall, ValueRepresentation};

    fn value(value: Value) -> ValueSource {
        ValueSource::Value {
            value,
            repr: ValueRepresentation::Tagged,
        }
    }
    fn plain(opcode: Opcode) -> InstData {
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
    fn constant(f: &mut Function, block: Block, n: i64) -> Value {
        f.push_inst(
            block,
            InstData {
                aux: AuxData::FixnumImm(n),
                ..plain(Opcode::ConstFixnum)
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0]
    }
    fn state(
        f: &mut Function,
        function: u32,
        bcp: u32,
        local: Value,
        stack: Vec<ValueSource>,
    ) -> FrameStateId {
        f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function,
                bcp,
                locals: vec![value(local)],
                stack,
            }],
            remat: vec![],
        })
    }
    fn call(
        f: &mut Function,
        block: Block,
        name: &str,
        args: Vec<Value>,
        frame_state: FrameStateId,
    ) -> (Inst, Value) {
        let (inst, result) = f.push_inst(
            block,
            InstData {
                args,
                aux: AuxData::CallTarget(egcl_rt::symbols::intern(name)),
                flags: InstFlags {
                    effectful: true,
                    call: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                frame_state: Some(frame_state),
                ..plain(Opcode::Call)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        (inst, result[0])
    }
    fn fixture(width: i64, position: i64) -> (Function, Inst, Inst, Value) {
        let name = "INTEGER-EXTRACT-TEST";
        let function = egcl_rt::symbols::intern(name);
        let mut f = Function::new(name);
        let block = f.entry();
        let integer = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
        let width = constant(&mut f, block, width);
        let position = constant(&mut f, block, position);
        let byte_state = state(
            &mut f,
            function,
            10,
            integer,
            vec![value(width), value(position)],
        );
        let (byte, descriptor) = call(&mut f, block, "BYTE", vec![width, position], byte_state);
        let ldb_state = state(
            &mut f,
            function,
            12,
            integer,
            vec![value(descriptor), value(integer)],
        );
        let (ldb, result) = call(&mut f, block, "LDB", vec![descriptor, integer], ldb_state);
        f.set_terminator(
            block,
            InstData {
                args: vec![result],
                ..plain(Opcode::Return)
            },
        );
        (f, byte, ldb, descriptor)
    }

    #[test]
    fn real_builder_dead_argument_local_does_not_block_extraction() {
        use egcl_rt::bytecode::{BytecodeFunction, Instr};
        use egcl_rt::value::{EgclVal, NIL};
        let name = "INTEGER-EXTRACT-BUILDER-TEST";
        let root = egcl_rt::symbols::intern(name);
        let byte = egcl_rt::symbols::intern("BYTE");
        let ldb = egcl_rt::symbols::intern("LDB");
        let body = BytecodeFunction {
            code: vec![
                Instr::Const(0),
                Instr::Const(1),
                Instr::CallNamed {
                    sym: byte,
                    nargs: 2,
                },
                Instr::LoadLocal(0),
                Instr::CallNamed { sym: ldb, nargs: 2 },
                Instr::Return,
            ],
            constants: vec![EgclVal::from_fixnum(8), EgclVal::from_fixnum(0)],
            self_call_sites: vec![],
            notinline_sites: Vec::new(),
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals: 1,
            max_stack: 2,
            arity: 1,
            name: name.into(),
            params_form: NIL,
            min_args: 1,
            max_args: Some(1),
            variadic: false,
            binder_cache: Default::default(),
        };
        for transfer in [false, true] {
            let mut f = if transfer {
                super::super::build::build_for_transfer_optimization(&body, root)
            } else {
                super::super::build::build_from_bytecode(&body)
            }
            .expect("real bytecode builder");
            let constructor = f
                .block_order()
                .iter()
                .flat_map(|&b| &f.block(b).insts)
                .copied()
                .find(|&i| canonical(f.inst(i), "BYTE"))
                .unwrap();
            let extraction = f
                .block_order()
                .iter()
                .flat_map(|&b| &f.block(b).insts)
                .copied()
                .find(|&i| canonical(f.inst(i), "LDB"))
                .unwrap();
            let before = f.frame_states.get(f.inst(constructor).frame_state.unwrap());
            assert!(matches!(
                before.scopes[0].locals[0],
                ValueSource::Value { .. }
            ));
            let after = f.frame_states.get(f.inst(extraction).frame_state.unwrap());
            assert!(matches!(after.scopes[0].locals[0], ValueSource::Unbound));
            assert_eq!(
                specialize(&mut f, &InlineOptions::default()),
                1,
                "transfer={transfer}"
            );
            assert_eq!(f.inst(extraction).opcode, Opcode::IntegerLowBits);
            assert!(super::super::verify::verify(&f).is_ok());
        }
    }

    #[test]
    fn private_byte_ldb_becomes_guarded_low_bits_with_exact_replay() {
        for width in [0, 8, 60] {
            let (mut f, byte, ldb, _) = fixture(width, 0);
            assert!(super::super::verify::verify(&f).is_ok());
            let before = f
                .frame_states
                .get(f.inst(byte).frame_state.unwrap())
                .clone();
            assert_eq!(specialize(&mut f, &InlineOptions::default()), 1);
            assert_eq!(f.inst(ldb).opcode, Opcode::IntegerLowBits);
            assert!(
                matches!(f.inst(ldb).aux, AuxData::FixnumImm(mask) if mask == (1i64 << width) - 1)
            );
            assert!(!f.block(f.entry()).insts.contains(&byte));
            let guard = f.frame_states.get(f.inst(ldb).frame_state.unwrap());
            assert_eq!(format!("{guard:?}"), format!("{before:?}"));
            let clear = f
                .block(f.entry())
                .insts
                .iter()
                .copied()
                .find(|&i| f.inst(i).opcode == Opcode::ClearMv)
                .unwrap();
            let after = f.frame_states.get(f.inst(clear).frame_state.unwrap());
            assert_eq!(after.scopes[0].bcp, 13);
            assert_eq!(after.scopes[0].stack.len(), 1);
            assert!(
                matches!(after.scopes[0].stack[0], ValueSource::Value { value, .. } if value == f.inst(ldb).results[0])
            );
            assert!(super::super::verify::verify(&f).is_ok());
        }
    }

    #[test]
    fn unsafe_patterns_decline_without_partial_changes() {
        for case in 0..8 {
            let (mut f, byte, ldb, descriptor) = fixture(8, 0);
            let mut options = InlineOptions::default();
            match case {
                0 => f.inst_mut(byte).args[1] = f.inst(byte).args[0], // nonzero position
                1 => {
                    let width = f.inst(byte).args[0];
                    let ValueDef::Result { inst, .. } = f.value(width).def else {
                        unreachable!()
                    };
                    f.inst_mut(inst).aux = AuxData::FixnumImm(61);
                }
                2 => {
                    f.inst_mut(byte).aux =
                        AuxData::CallTarget(egcl_rt::symbols::intern("EXTRACT-SHADOW::BYTE"))
                }
                3 => {
                    f.inst_mut(ldb).aux =
                        AuxData::CallTarget(egcl_rt::symbols::intern("EXTRACT-SHADOW::LDB"))
                }
                4 => options = options.with_policy(10, InlinePolicy::NotInline),
                5 => options = options.with_policy(12, InlinePolicy::NotInline),
                6 => {
                    let fs = f.inst(ldb).frame_state.unwrap();
                    f.frame_states.get_mut(fs).scopes[0].bcp = 14;
                }
                7 => {
                    // Escaping descriptor even to a pure extra consumer is unsafe.
                    f.inst_mut(f.terminator(f.entry()).unwrap())
                        .args
                        .push(descriptor);
                }
                _ => unreachable!(),
            }
            let before = format!("{f:?}");
            assert_eq!(specialize(&mut f, &options), 0, "case {case}");
            assert_eq!(format!("{f:?}"), before, "case {case}");
        }
    }

    #[test]
    fn invoke_forwarding_aliases_are_removed_and_cold_blocks_retired() {
        let (mut f, byte, ldb, descriptor) = fixture(8, 0);
        let entry = f.entry();
        let normal = f.make_block();
        let cold = f.make_block();
        let alias = f.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let local_alias = f.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let integer = f.inst(ldb).args[1];
        let ret = f.block(entry).insts.last().copied().unwrap();
        f.block_mut(entry).insts.retain(|&i| i != ldb && i != ret);
        f.inst_mut(byte).opcode = Opcode::Invoke;
        f.inst_mut(byte).flags.terminator = true;
        f.inst_mut(byte).targets = vec![
            BlockCall {
                block: normal,
                args: vec![descriptor, integer],
            },
            BlockCall {
                block: cold,
                args: vec![],
            },
        ];
        f.inst_mut(ldb).args = vec![alias, local_alias];
        let fs = f.inst(ldb).frame_state.unwrap();
        f.frame_states.get_mut(fs).scopes[0].locals = vec![value(local_alias)];
        f.frame_states.get_mut(fs).scopes[0].stack = vec![value(alias), value(local_alias)];
        let returned = f.make_block();
        let ldb_cold = f.make_block();
        let output = f.add_block_param(returned, IRType::TOP, ValueRepresentation::Tagged);
        let ldb_result = f.inst(ldb).results[0];
        f.inst_mut(ldb).opcode = Opcode::Invoke;
        f.inst_mut(ldb).flags.terminator = true;
        f.inst_mut(ldb).targets = vec![
            BlockCall {
                block: returned,
                args: vec![ldb_result],
            },
            BlockCall {
                block: ldb_cold,
                args: vec![],
            },
        ];
        f.inst_mut(ret).args = vec![output];
        f.block_mut(normal).insts.push(ldb);
        f.block_mut(returned).insts.push(ret);
        f.set_terminator(cold, plain(Opcode::Trap));
        f.set_terminator(ldb_cold, plain(Opcode::Trap));
        assert!(super::super::verify::verify(&f).is_ok());
        assert_eq!(specialize(&mut f, &InlineOptions::default()), 1);
        assert_eq!(f.block(normal).params, vec![local_alias]);
        assert_eq!(f.inst(byte).targets[0].args, vec![integer]);
        assert!(!f.block_order().contains(&cold));
        assert!(!f.block_order().contains(&ldb_cold));
        assert_eq!(f.inst(ldb).opcode, Opcode::IntegerLowBits);
        assert!(super::super::verify::verify(&f).is_ok());
    }

    #[test]
    fn surviving_descriptor_recovery_or_intervening_effect_rejects() {
        for opaque in [false, true] {
            let (mut f, _, ldb, descriptor) = fixture(8, 0);
            let entry = f.entry();
            if opaque {
                let fs = f.inst(ldb).frame_state.unwrap();
                let (extra, _) = call(&mut f, entry, "EXTRACT-TEST::OPAQUE", vec![], fs);
                assert_eq!(f.block_mut(entry).insts.pop(), Some(extra));
                let pos = f.block(entry).insts.iter().position(|&i| i == ldb).unwrap();
                f.block_mut(entry).insts.insert(pos, extra);
            } else {
                let mut later = f.frame_states.get(f.inst(ldb).frame_state.unwrap()).clone();
                later.scopes[0].bcp = 13;
                later.scopes[0].stack = vec![value(descriptor)];
                let fs = f.frame_states.add(later);
                f.inst_mut(f.terminator(entry).unwrap()).frame_state = Some(fs);
            }
            let before = format!("{f:?}");
            assert_eq!(specialize(&mut f, &InlineOptions::default()), 0);
            assert_eq!(format!("{f:?}"), before);
        }
    }
}
