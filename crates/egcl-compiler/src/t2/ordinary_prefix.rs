// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! A single noncollecting early return before the ordinary prologue.
//!
//! Selection interprets only register arguments, immediate constants, exact
//! fixnum guards, signed comparisons and SSA edges. Failed prefix conditions
//! fall through to the original framed entry; no incoming ABI word is changed.

use super::*;
use crate::t2::ir::{
    AuxData, Block, BlockCall, Inst, Opcode, TypeBits, Value, ValueRepresentation,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Atom {
    Argument(u8),
    Constant(u64),
}
#[derive(Clone, Copy)]
enum Expression {
    Atom(Atom),
    Less(Atom, Atom),
}
#[derive(Clone)]
enum Check {
    Fixnum(Atom),
    Less(Atom, Atom, bool),
}
pub(super) struct Prefix {
    checks: Vec<Check>,
    result: Atom,
}

fn atom(values: &HashMap<Value, Expression>, value: Value) -> Option<Atom> {
    match values.get(&value)? {
        Expression::Atom(atom) => Some(*atom),
        _ => None,
    }
}

fn edge(
    f: &Function,
    target: &BlockCall,
    values: &HashMap<Value, Expression>,
) -> Option<HashMap<Value, Expression>> {
    let params = &f.block(target.block).params;
    if params.len() != target.args.len() {
        return None;
    }
    // Snapshot every source before assigning any destination (parallel phi moves).
    let incoming: Option<Vec<_>> = target
        .args
        .iter()
        .map(|arg| values.get(arg).copied())
        .collect();
    let mut next = values.clone();
    for (&param, value) in params.iter().zip(incoming?) {
        next.insert(param, value);
    }
    Some(next)
}

fn walk(
    f: &Function,
    block: Block,
    mut values: HashMap<Value, Expression>,
    mut checks: Vec<Check>,
    mut path: HashSet<Block>,
    budget: &mut usize,
    single: &HashSet<Inst>,
) -> Option<Prefix> {
    if path.len() >= 8 || !path.insert(block) {
        return None;
    }
    for &inst in &f.block(block).insts {
        *budget = budget.checked_sub(1)?;
        let d = f.inst(inst);
        // The framed ordinary emitter omits this same proven-idempotent reset.
        // An unknown tuple must retain its reset/helper in the original body.
        if d.opcode == Opcode::ClearMv && single.contains(&inst) {
            continue;
        }
        // Speculation marks guards and typed comparisons effectful to preserve
        // deopt ordering. Their successful paths are pure; failed prefix tests
        // enter the original framed guards. No other effects may be bypassed.
        if d.flags.call || d.flags.safepoint
            || (d.flags.effectful && !matches!(d.opcode, Opcode::Guard | Opcode::FixnumCmpLt))
        {
            return None;
        }
        if d.args
            .iter()
            .chain(&d.results)
            .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
        {
            return None;
        }
        let expression = match d.opcode {
            Opcode::ConstFixnum => {
                let AuxData::FixnumImm(value) = d.aux else {
                    return None;
                };
                if !(-(1_i64 << 60)..(1_i64 << 60)).contains(&value) {
                    return None;
                }
                Expression::Atom(Atom::Constant((value as u64) << 3))
            }
            Opcode::ConstNil => Expression::Atom(Atom::Constant(egcl_rt::value::NIL.0)),
            Opcode::ConstT => Expression::Atom(Atom::Constant(egcl_rt::value::T.0)),
            Opcode::Guard => {
                let AuxData::TypeTag(ty) = d.aux else {
                    return None;
                };
                if ty.bits != TypeBits::FIXNUM
                    || ty.range.is_some()
                    || ty.class_id.is_some()
                    || d.args.len() != 1
                    || d.results.len() > 1
                {
                    return None;
                }
                let value = atom(&values, d.args[0])?;
                checks.push(Check::Fixnum(value));
                if d.results.is_empty() {
                    continue;
                }
                Expression::Atom(value)
            }
            Opcode::FixnumCmpLt => {
                if d.args.len() != 2 {
                    return None;
                }
                let lhs = atom(&values, d.args[0])?;
                let rhs = atom(&values, d.args[1])?;
                // Bound the initial implementation to argument/constant tests.
                if !matches!(rhs, Atom::Constant(_)) {
                    return None;
                }
                checks.push(Check::Fixnum(lhs));
                checks.push(Check::Fixnum(rhs));
                Expression::Less(lhs, rhs)
            }
            Opcode::Jump => {
                if d.targets.len() != 1 {
                    return None;
                }
                return walk(
                    f,
                    d.targets[0].block,
                    edge(f, &d.targets[0], &values)?,
                    checks,
                    path,
                    budget,
                    single,
                );
            }
            Opcode::Brif => {
                if d.args.len() != 1 || d.targets.len() != 2 {
                    return None;
                }
                let Expression::Less(lhs, rhs) = *values.get(&d.args[0])? else {
                    return None;
                };
                for (index, target) in d.targets.iter().enumerate() {
                    let Some(next) = edge(f, target, &values) else {
                        continue;
                    };
                    let mut branch_checks = checks.clone();
                    branch_checks.push(Check::Less(lhs, rhs, index == 0));
                    if let Some(prefix) = walk(
                        f,
                        target.block,
                        next,
                        branch_checks,
                        path.clone(),
                        budget,
                        single,
                    ) {
                        return Some(prefix);
                    }
                }
                return None;
            }
            Opcode::Return => {
                if !single.contains(&inst) || d.args.len() != 1 {
                    return None;
                }
                return Some(Prefix {
                    checks,
                    result: atom(&values, d.args[0])?,
                });
            }
            _ => return None,
        };
        if d.results.len() != 1 {
            return None;
        }
        values.insert(d.results[0], expression);
    }
    None
}

pub(super) fn select(f: &Function, arity: usize, single: &HashSet<Inst>) -> Option<Prefix> {
    if arity > 3
        || f.is_variadic()
        || !f.osr_entries.is_empty()
        || !f.handler_cases.is_empty()
        || f.frame_states
            .iter()
            .any(|(_, state)| state.scopes.len() != 1)
        || f.block(f.entry()).params.len() != arity
    {
        return None;
    }
    let values = f
        .block(f.entry())
        .params
        .iter()
        .enumerate()
        .map(|(index, &value)| (value, Expression::Atom(Atom::Argument([7, 6, 2][index]))))
        .collect();
    walk(
        f,
        f.entry(),
        values,
        Vec::new(),
        HashSet::new(),
        &mut 64,
        single,
    )
}

fn load(a: &mut Asm, destination: u8, value: Atom) {
    match value {
        Atom::Argument(register) => mov_rr(a, destination, register),
        Atom::Constant(bits) => mov_imm64(a, destination, bits as i64),
    }
}

pub(super) fn emit(a: &mut Asm, f: &Function, arity: usize, single: &HashSet<Inst>) {
    let Some(prefix) = select(f, arity, single) else {
        return;
    };
    emit_selected(a, &prefix, None);
}

/// A call-site prefix joins its caller's result continuation without creating
/// or retiring a frame. The caller has already checked poll and MV state.
pub(super) fn emit_selected(a: &mut Asm, prefix: &Prefix, continuation: Option<egcl_rt::asm::Label>) {
    // A statically impossible guard must not produce an unconditional return.
    if prefix
        .checks
        .iter()
        .any(|check| matches!(check, Check::Fixnum(Atom::Constant(bits)) if bits & 7 != 0))
    {
        return;
    }
    let framed = a.label();
    let mut guarded = HashSet::new();
    for check in prefix.checks.iter().cloned() {
        match check {
            Check::Fixnum(Atom::Argument(register)) => {
                if guarded.insert(register) {
                    guard_fixnum(a, register, framed);
                }
            }
            Check::Fixnum(Atom::Constant(_)) => {}
            Check::Less(lhs, rhs, accepted) => {
                let register = match lhs {
                    Atom::Argument(register) => register,
                    Atom::Constant(_) => {
                        load(a, 10, lhs);
                        10
                    }
                };
                if let Atom::Constant(bits) = rhs
                    && let Ok(immediate) = i32::try_from(bits as i64)
                {
                    alu_r_imm(a, 7, register, immediate); // CMP, never writes the input.
                } else {
                    load(a, 11, rhs);
                    cmp_rr(a, register, 11);
                }
                a.jcc(if accepted { Cc::Ge } else { Cc::L }, framed);
            }
        }
    }
    load(a, RAX, prefix.result);
    if let Some(continuation) = continuation {
        a.push(0xf8); // clc: exactly one value, matching the ordinary return ABI
        a.jmp(continuation);
    } else {
        crate::t2::native_return::emit_single_return(a);
    }
    a.bind(framed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::ir::{IRType, InstData, InstFlags};

    fn instruction(opcode: Opcode, args: Vec<Value>, aux: AuxData) -> InstData {
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
    fn identity() -> Function {
        let mut f = Function::new("prefix-selection");
        let entry = f.entry();
        let x = f.add_block_param(
            entry,
            IRType::of(TypeBits::TOP),
            ValueRepresentation::Tagged,
        );
        f.set_terminator(entry, instruction(Opcode::Return, vec![x], AuxData::None));
        f
    }
    #[test]
    fn requires_single_value_return_proof() {
        let f = identity();
        assert!(select(&f, 1, &HashSet::new()).is_none());
        assert!(
            select(
                &f,
                1,
                &crate::t2::opt_clear_mv::single_value_state(&f, true)
            )
            .is_some()
        );
        assert!(select(&f, 4, &HashSet::new()).is_none());
    }
    #[test]
    fn rejects_effects_and_cycles() {
        let mut f = identity();
        let entry = f.entry();
        let ret = f.terminator(entry).unwrap();
        f.inst_mut(ret).flags.effectful = true;
        assert!(select(&f, 1, &HashSet::from([ret])).is_none());
        let x = f.block(entry).params[0];
        let mut jump = instruction(Opcode::Jump, vec![], AuxData::None);
        jump.targets.push(BlockCall {
            block: entry,
            args: vec![x],
        });
        f.block_mut(entry).insts.clear();
        f.set_terminator(entry, jump);
        assert!(select(&f, 1, &HashSet::from([ret])).is_none());
    }
    #[test]
    fn skips_only_a_proven_redundant_mv_reset() {
        let mut f = identity();
        let entry = f.entry();
        let x = f.block(entry).params[0];
        f.block_mut(entry).insts.clear();
        let mut clear = instruction(Opcode::ClearMv, vec![], AuxData::None);
        clear.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..InstFlags::default()
        };
        f.push_inst(entry, clear, &[]);
        f.set_terminator(entry, instruction(Opcode::Return, vec![x], AuxData::None));
        let known = crate::t2::opt_clear_mv::single_value_state(&f, true);
        assert!(select(&f, 1, &known).is_some());
        let unknown = crate::t2::opt_clear_mv::single_value_state(&f, false);
        assert!(select(&f, 1, &unknown).is_none());
    }
    #[test]
    fn follows_guard_alias_through_parallel_block_arguments() {
        let mut f = Function::new("prefix-alias");
        let entry = f.entry();
        let dest = f.make_block();
        let x = f.add_block_param(
            entry,
            IRType::of(TypeBits::TOP),
            ValueRepresentation::Tagged,
        );
        let parameter = f.add_block_param(
            dest,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        );
        let guarded = f
            .push_inst(
                entry,
                instruction(
                    Opcode::Guard,
                    vec![x],
                    AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                ),
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            )
            .1[0];
        let guard_inst = f.block(entry).insts[0];
        f.inst_mut(guard_inst).flags.guard = true;
        f.inst_mut(guard_inst).flags.effectful = true;
        let mut jump = instruction(Opcode::Jump, vec![], AuxData::None);
        jump.targets.push(BlockCall {
            block: dest,
            args: vec![guarded],
        });
        f.set_terminator(entry, jump);
        f.set_terminator(
            dest,
            instruction(Opcode::Return, vec![parameter], AuxData::None),
        );
        let prefix = select(
            &f,
            1,
            &crate::t2::opt_clear_mv::single_value_state(&f, true),
        )
        .unwrap();
        assert_eq!(prefix.result, Atom::Argument(7));
        assert_eq!(prefix.checks.len(), 1);
    }
}
