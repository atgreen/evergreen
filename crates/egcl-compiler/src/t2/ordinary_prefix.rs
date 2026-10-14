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

/// Exact-input partial evaluations adjacent to an existing early-return boundary.
/// These are only used after a direct self-call's poll and MV checks, never at
/// public entry. All work is done at compile time; no expression tree, virtual
/// frame, or partially executed call survives into the generated shortcut.
pub(super) struct Specialization {
    argument: u64,
    result: u64,
}

fn evaluate_pure(
    f: &Function,
    argument: u64,
    self_calls: &[Inst],
    depth: usize,
    budget: &mut usize,
    saw_call: &mut bool,
) -> Option<u64> {
    let entry = f.entry();
    let mut values = HashMap::from([(f.block(entry).params[0], argument)]);
    let mut block = entry;
    'blocks: loop {
        for &inst in &f.block(block).insts {
            *budget = budget.checked_sub(1)?;
            let d = f.inst(inst);
            if d.args.iter().chain(&d.results)
                .any(|&value| f.value(value).repr != ValueRepresentation::Tagged)
            {
                return None;
            }
            let args: Vec<u64> = d.args.iter().map(|v| values.get(v).copied())
                .collect::<Option<_>>()?;
            // Nothing below mutates the MV buffer. A successfully evaluated
            // recursive call also returns exactly one value, so every reset on
            // the selected execution is redundant with the call-site precheck.
            if d.opcode == Opcode::ClearMv && args.is_empty() && d.results.is_empty() {
                continue;
            }
            if d.opcode != Opcode::Call && (d.flags.call || d.flags.safepoint) {
                return None;
            }
            if d.flags.effectful && !matches!(d.opcode,
                Opcode::Guard | Opcode::FixnumCmpLt | Opcode::FixnumAdd
                    | Opcode::FixnumSub | Opcode::Call)
            {
                return None;
            }
            let result = match d.opcode {
                Opcode::ConstFixnum => {
                    let AuxData::FixnumImm(value) = d.aux else { return None };
                    if !(-(1_i64 << 60)..(1_i64 << 60)).contains(&value) { return None; }
                    (value as u64) << 3
                }
                Opcode::ConstNil => egcl_rt::value::NIL.0,
                Opcode::ConstT => egcl_rt::value::T.0,
                Opcode::Guard => {
                    let AuxData::TypeTag(ty) = d.aux else { return None };
                    if ty.bits != TypeBits::FIXNUM || ty.range.is_some()
                        || ty.class_id.is_some() || args.len() != 1 || args[0] & 7 != 0
                        || d.results.len() > 1
                    { return None; }
                    if d.results.is_empty() { continue; }
                    args[0]
                }
                Opcode::FixnumAdd | Opcode::FixnumSub | Opcode::FixnumCmpLt => {
                    if args.len() != 2 || (args[0] | args[1]) & 7 != 0 { return None; }
                    let (lhs, rhs) = (args[0] as i64, args[1] as i64);
                    // Evaluate every executed checked operation, even if its
                    // result is dead. A failed check must preserve the original
                    // call's type error or bignum/overflow path.
                    match d.opcode {
                        Opcode::FixnumAdd => lhs.checked_add(rhs)? as u64,
                        Opcode::FixnumSub => lhs.checked_sub(rhs)? as u64,
                        _ => if lhs < rhs { egcl_rt::value::T.0 } else { egcl_rt::value::NIL.0 },
                    }
                }
                Opcode::Call => {
                    if !self_calls.contains(&inst) || args.len() != 1
                        || !d.flags.call || !d.flags.safepoint
                    { return None; }
                    let AuxData::CallTarget(symbol) = d.aux else { return None };
                    let state = f.frame_states.get(d.frame_state?);
                    if state.scopes.len() != 1 || state.scopes[0].function != symbol {
                        return None;
                    }
                    *saw_call = true;
                    evaluate_pure(f, args[0], self_calls, depth.checked_sub(1)?, budget, saw_call)?
                }
                Opcode::Jump | Opcode::Brif => {
                    let target = match d.opcode {
                        Opcode::Jump if args.is_empty() && d.targets.len() == 1 => &d.targets[0],
                        Opcode::Brif if args.len() == 1 && d.targets.len() == 2 =>
                            &d.targets[usize::from(args[0] == egcl_rt::value::NIL.0)],
                        _ => return None,
                    };
                    let params = &f.block(target.block).params;
                    if params.len() != target.args.len() { return None; }
                    let incoming: Vec<_> = target.args.iter().map(|v| values.get(v).copied())
                        .collect::<Option<_>>()?;
                    for (&param, value) in params.iter().zip(incoming) {
                        values.insert(param, value);
                    }
                    block = target.block;
                    continue 'blocks;
                }
                Opcode::Return if args.len() == 1 => return Some(args[0]),
                _ => return None,
            };
            if d.results.len() != 1 { return None; }
            values.insert(d.results[0], result);
        }
        return None;
    }
}

pub(super) fn specialize(
    f: &Function, arity: usize, single: &HashSet<Inst>, self_calls: &[Inst],
) -> Vec<Specialization> {
    if arity != 1 || self_calls.is_empty() { return Vec::new(); }
    let Some(prefix) = select(f, arity, single) else { return Vec::new() };
    let Some((boundary, accepted)) = prefix.checks.iter().find_map(|check| match check {
        Check::Less(Atom::Argument(7), Atom::Constant(bits), accepted) if bits & 7 == 0 =>
            Some(((*bits as i64) >> 3, *accepted)),
        _ => None,
    }) else { return Vec::new() };
    let mut specializations = Vec::new();
    // Bound both compile-time work and the amount of execution skipped between
    // polls: four candidates, at most three recursive edges deep and 512 IR
    // instructions per candidate. There is no runtime lookup table or helper.
    for offset in 0..4 {
        let candidate = if accepted { boundary.checked_add(offset) }
            else { boundary.checked_sub(offset + 1) };
        let Some(candidate) = candidate.filter(|n| (-(1_i64 << 60)..(1_i64 << 60)).contains(n))
            else { continue };
        let argument = (candidate as u64) << 3;
        let mut saw_call = false;
        if let Some(result) = evaluate_pure(f, argument, self_calls, 3, &mut 512, &mut saw_call)
            && saw_call
        {
            specializations.push(Specialization { argument, result });
        }
    }
    specializations
}

pub(super) fn emit_specializations(
    a: &mut Asm, specializations: &[Specialization], continuation: egcl_rt::asm::Label,
) {
    for specialization in specializations {
        let miss = a.label();
        if let Ok(immediate) = i32::try_from(specialization.argument as i64) {
            alu_r_imm(a, 7, 7, immediate);
        } else {
            mov_imm64(a, 11, specialization.argument as i64);
            cmp_rr(a, 7, 11);
        }
        a.jcc(Cc::Ne, miss);
        mov_imm64(a, RAX, specialization.result as i64);
        a.push(0xf8);
        a.jmp(continuation);
        a.bind(miss);
    }
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

    fn counter(reverse: bool) -> (Function, Inst) {
        use crate::t2::frame_state::{FrameScope, FrameState};
        let mut f = identity();
        let entry = f.entry();
        let x = f.block(entry).params[0];
        f.block_mut(entry).insts.clear();
        let base = f.make_block();
        let recur = f.make_block();
        let tagged = [(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)];
        let zero = f.push_inst(entry,
            instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(0)), &tagged).1[0];
        let less = f.push_inst(entry,
            instruction(Opcode::FixnumCmpLt, vec![x, zero], AuxData::None), &tagged).1[0];
        let mut branch = instruction(Opcode::Brif, vec![less], AuxData::None);
        branch.targets = (if reverse { [recur, base] } else { [base, recur] }).into_iter()
            .map(|block| BlockCall { block, args: vec![] }).collect();
        f.set_terminator(entry, branch);
        f.set_terminator(base, instruction(Opcode::Return, vec![x], AuxData::None));
        let delta = f.push_inst(recur, instruction(Opcode::ConstFixnum, vec![],
            AuxData::FixnumImm(if reverse { 1 } else { -1 })), &tagged).1[0];
        let next = f.push_inst(recur,
            instruction(Opcode::FixnumAdd, vec![x, delta], AuxData::None), &tagged).1[0];
        let mut call = instruction(Opcode::Call, vec![next], AuxData::CallTarget(17));
        call.flags.call = true;
        call.flags.safepoint = true;
        call.flags.effectful = true;
        call.frame_state = Some(f.frame_states.add(FrameState {
            scopes: vec![FrameScope { function: 17, bcp: 0, locals: vec![], stack: vec![] }],
            remat: vec![],
        }));
        let (call, result) = f.push_inst(recur, call, &tagged);
        let two = f.push_inst(recur,
            instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(2)), &tagged).1[0];
        let result = f.push_inst(recur,
            instruction(Opcode::FixnumAdd, vec![result[0], two], AuxData::None), &tagged).1[0];
        f.set_terminator(recur, instruction(Opcode::Return, vec![result], AuxData::None));
        (f, call)
    }

    #[test]
    fn partially_evaluates_both_sides_of_a_boundary_without_function_specific_rules() {
        for reverse in [false, true] {
            let (f, call) = counter(reverse);
            let single = crate::t2::opt_clear_mv::single_value_state(&f, true);
            let cases = specialize(&f, 1, &single, &[call]);
            let cases: Vec<_> = cases.iter()
                .map(|case| ((case.argument as i64) >> 3, (case.result as i64) >> 3)).collect();
            assert_eq!(cases, if reverse { vec![(-1, 2), (-2, 4), (-3, 6)] }
                else { vec![(0, 1), (1, 3), (2, 5)] });
            assert!(specialize(&f, 1, &single, &[]).is_empty(), "source proof is required");
            assert!(specialize(&f, 2, &single, &[call]).is_empty(), "all inputs must be known");
        }
    }

    #[test]
    fn partial_evaluation_preserves_unused_overflow_and_type_failures() {
        for (opcode, input) in [(Opcode::FixnumAdd, (1_i64 << 60) - 1),
            (Opcode::FixnumSub, -(1_i64 << 60))]
        {
            let mut f = identity();
            let entry = f.entry();
            let x = f.block(entry).params[0];
            f.block_mut(entry).insts.clear();
            let tagged = [(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)];
            let one = f.push_inst(entry,
                instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)), &tagged).1[0];
            f.push_inst(entry, instruction(opcode, vec![x, one], AuxData::None), &tagged);
            f.set_terminator(entry, instruction(Opcode::Return, vec![one], AuxData::None));
            assert_eq!(evaluate_pure(&f, 0, &[], 3, &mut 512, &mut false), Some(8));
            assert_eq!(evaluate_pure(&f, (input as u64) << 3, &[], 3, &mut 512, &mut false), None);
            assert_eq!(evaluate_pure(&f, egcl_rt::value::NIL.0, &[], 3, &mut 512, &mut false), None);
        }
    }

    #[test]
    fn partial_evaluation_requires_identity_and_finite_work() {
        let (mut f, call) = counter(false);
        assert_eq!(evaluate_pure(&f, 0, &[call], 3, &mut 512, &mut false), Some(8));
        assert_eq!(evaluate_pure(&f, 0, &[], 3, &mut 512, &mut false), None);
        assert_eq!(evaluate_pure(&f, 0, &[call], 0, &mut 512, &mut false), None);
        assert_eq!(evaluate_pure(&f, 0, &[call], 3, &mut 2, &mut false), None);
        f.inst_mut(call).aux = AuxData::CallTarget(18);
        assert_eq!(evaluate_pure(&f, 0, &[call], 3, &mut 512, &mut false), None);
    }

    #[test]
    fn partial_evaluation_rejects_later_effects_and_failed_second_calls() {
        let (mut f, call) = counter(false);
        let recur = *f.block_order().iter()
            .find(|&&block| f.block(block).insts.contains(&call)).unwrap();
        let ret = f.block_mut(recur).insts.pop().unwrap();
        let tagged = [(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)];
        let nil = f.push_inst(recur,
            instruction(Opcode::ConstNil, vec![], AuxData::None), &tagged).1[0];
        let mut later = f.inst(call).clone();
        later.args = vec![nil];
        later.results.clear();
        let (later, _) = f.push_inst(recur, later, &tagged);
        f.block_mut(recur).insts.push(ret);
        let mut saw_call = false;
        assert_eq!(evaluate_pure(&f, 0, &[call, later], 3, &mut 512, &mut saw_call), None);
        assert!(saw_call, "the first recursive operand completed before the second failed");
        f.inst_mut(later).opcode = Opcode::MemoryFence;
        assert_eq!(evaluate_pure(&f, 0, &[call], 3, &mut 512, &mut false), None);
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
