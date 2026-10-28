// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Expectations derived from immutable callee bodies, NOT SSA type proofs.
//! A suspended caller or redefined callee can invalidate an expectation. Every
//! consuming arithmetic operation must retain its own operand/recovery checks.

use super::inlining::InlineOptions;
use super::ir::{AuxData, Function, IRType, Opcode, TypeBits, Value, ValueDef};
use std::collections::{HashMap, HashSet};

const DEPTH: usize = 3;
const BUDGET: usize = 512;
fn canonical_float(symbol: u32) -> bool {
    egcl_rt::symbols::registry_key(symbol).as_deref() == Some("FLOAT")
        && egcl_rt::symbols::find_index("FLOAT") == Some(symbol)
}
fn float_result(args: &[TypeBits]) -> TypeBits {
    let single = TypeBits::SINGLE_FLOAT;
    let double = TypeBits::DOUBLE_FLOAT;
    let floats = single.join(double);
    if args.iter().any(|t| t.is_bottom()) {
        return TypeBits::BOTTOM;
    }
    match args {
        [input] => {
            let real = TypeBits::FIXNUM
                .join(TypeBits::BIGNUM)
                .join(TypeBits::RATIO)
                .join(floats);
            if !real.contains(*input) {
                return floats;
            }
            let mut result = input.meet(floats);
            if !input
                .meet(
                    TypeBits::FIXNUM
                        .join(TypeBits::BIGNUM)
                        .join(TypeBits::RATIO),
                )
                .is_bottom()
            {
                result = result.join(single);
            }
            result
        }
        [_, prototype] if !prototype.is_bottom() && floats.contains(*prototype) => *prototype,
        _ => TypeBits::TOP,
    }
}

/// Backward data dependencies only; never prune a branch by its condition.
/// Unsupported definitions become TOP without inspecting their arguments.
fn demand(
    f: &Function,
    roots: impl IntoIterator<Item = Value>,
    options: &InlineOptions,
) -> HashSet<Value> {
    let mut needed = HashSet::new();
    let mut work: Vec<_> = roots.into_iter().collect();
    let mut preds = None;
    while let Some(value) = work.pop() {
        if !needed.insert(value) {
            continue;
        }
        match f.value(value).def {
            ValueDef::Result { inst, .. } => {
                let d = f.inst(inst);
                if d.opcode == Opcode::Guard && d.args.len() == 1
                    || matches!(d.opcode, Opcode::Call | Opcode::Invoke)
                        && matches!(d.aux, AuxData::CallTarget(s)
                            if canonical_float(s) || options.has_captured_body(s))
                {
                    work.extend(d.args.iter().copied());
                }
            }
            ValueDef::Param { block, num } if block != f.entry() => {
                for &pred in &preds.get_or_insert_with(|| f.pred_lists())[block.index()] {
                    if let Some(term) = f.terminator(pred) {
                        for edge in &f.inst(term).targets {
                            if edge.block == block
                                && let Some(&arg) = edge.args.get(num as usize)
                            {
                                work.push(arg);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    needed
}

struct Context<'a> {
    options: &'a InlineOptions,
    active: HashSet<u32>,
    budget: usize,
    cache: HashMap<(u32, Vec<u16>), TypeBits>,
}
impl Context<'_> {
    fn call(&mut self, symbol: u32, args: &[TypeBits], depth: usize) -> TypeBits {
        if canonical_float(symbol) {
            return float_result(args);
        }
        let key = (symbol, args.iter().map(|t| t.0).collect::<Vec<_>>());
        if let Some(&cached) = self.cache.get(&key) {
            return cached;
        }
        if depth >= DEPTH || self.active.contains(&symbol) {
            return TypeBits::TOP;
        }
        let Some(snapshot) = self.options.captured_body_snapshot(symbol) else {
            return TypeBits::TOP;
        };
        let body = &snapshot.body;
        if body.variadic
            || body.has_env
            || body.arity as usize != args.len()
            || body.min_args != body.arity
            || body.max_args != Some(body.arity)
            || body.code.len() > 128
            || body.n_locals > 64
            || body.max_stack > 64
            || body.code.len() > self.budget
        {
            return TypeBits::TOP;
        }
        self.budget -= body.code.len();
        let options = InlineOptions::default().with_root_symbol(symbol);
        let Ok(ir) = super::build::build_from_snapshot(&snapshot, options) else {
            return TypeBits::TOP;
        };
        if ir.num_insts() > self.budget {
            return TypeBits::TOP;
        }
        self.budget -= ir.num_insts();
        self.active.insert(symbol);
        let needed = demand(
            &ir,
            ir.block_order()
                .iter()
                .filter_map(|&b| ir.terminator(b))
                .filter(|&i| ir.inst(i).opcode == Opcode::Return)
                .flat_map(|i| ir.inst(i).args.first().copied()),
            self.options,
        );
        let result = self
            .evaluate(&ir, args, depth + 1, &needed)
            .map_or(TypeBits::TOP, |facts| {
                ir.block_order()
                    .iter()
                    .filter_map(|&b| ir.terminator(b))
                    .filter(|&i| ir.inst(i).opcode == Opcode::Return)
                    .fold(TypeBits::BOTTOM, |result, i| {
                        result.join(
                            ir.inst(i)
                                .args
                                .first()
                                .and_then(|v| facts.get(v))
                                .copied()
                                .unwrap_or(TypeBits::TOP),
                        )
                    })
            });
        self.active.remove(&symbol);
        let result = if result.is_bottom() {
            TypeBits::TOP
        } else {
            result
        };
        self.cache.insert(key, result);
        result
    }
    fn evaluate(
        &mut self,
        f: &Function,
        args: &[TypeBits],
        depth: usize,
        needed: &HashSet<Value>,
    ) -> Option<HashMap<Value, TypeBits>> {
        let mut facts: HashMap<Value, TypeBits> = f
            .block(f.entry())
            .params
            .iter()
            .copied()
            .zip(args.iter().copied())
            .filter(|(value, _)| needed.contains(value))
            .collect();
        let get = |facts: &HashMap<Value, TypeBits>, v| {
            facts.get(&v).copied().unwrap_or(TypeBits::BOTTOM)
        };
        let rpo = f.reverse_postorder();
        for _ in 0..32 {
            let mut changed = false;
            for &block in &rpo {
                if block != f.entry() {
                    for (index, &param) in f.block(block).params.iter().enumerate() {
                        if !needed.contains(&param) {
                            continue;
                        }
                        let mut ty = TypeBits::BOTTOM;
                        for pred in f.preds(block) {
                            for edge in &f.inst(f.terminator(pred)?).targets {
                                if edge.block == block {
                                    ty = ty.join(get(&facts, *edge.args.get(index)?));
                                }
                            }
                        }
                        let ty = get(&facts, param).join(ty);
                        if get(&facts, param) != ty {
                            facts.insert(param, ty);
                            changed = true;
                        }
                    }
                }
                for &inst in &f.block(block).insts {
                    let d = f.inst(inst);
                    if !d.results.iter().any(|value| needed.contains(value)) {
                        continue;
                    }
                    let ty = match d.opcode {
                        Opcode::ConstFixnum => TypeBits::FIXNUM,
                        Opcode::ConstFloat => TypeBits::SINGLE_FLOAT,
                        Opcode::ConstDouble => TypeBits::DOUBLE_FLOAT,
                        Opcode::ConstHeapObj if matches!(d.aux, AuxData::HeapLiteral { slot } if f.double_literals.contains_key(&slot)) => {
                            TypeBits::DOUBLE_FLOAT
                        }
                        Opcode::Guard if d.args.len() == 1 => get(&facts, d.args[0]),
                        Opcode::Call | Opcode::Invoke => {
                            if let AuxData::CallTarget(symbol) = d.aux
                                && (canonical_float(symbol)
                                    || self.options.has_captured_body(symbol))
                            {
                                let operands: Vec<_> =
                                    d.args.iter().map(|&v| get(&facts, v)).collect();
                                if operands.iter().any(|t| t.is_bottom()) {
                                    TypeBits::BOTTOM
                                } else {
                                    self.call(symbol, &operands, depth)
                                }
                            } else {
                                TypeBits::TOP
                            }
                        }
                        _ => TypeBits::TOP,
                    };
                    for &result in &d.results {
                        if !needed.contains(&result) {
                            continue;
                        }
                        let next = get(&facts, result).join(ty);
                        if next != get(&facts, result) {
                            facts.insert(result, next);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                return Some(facts);
            }
        }
        None // Never publish a partial or last-iteration summary.
    }
}

/// Collect only when a captured call/FLOAT may feed numeric speculation.
/// All bounds and formats here remain expectations, even for simple constants.
pub fn collect(f: &Function, options: &InlineOptions) -> HashMap<Value, IRType> {
    if f.num_insts() > 2048 {
        return HashMap::new();
    }
    let roots: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .filter_map(|&i| {
            let d = f.inst(i);
            match d.aux {
                AuxData::CallTarget(symbol)
                    if matches!(d.opcode, Opcode::Call | Opcode::Invoke)
                        && super::speculate::accepts_return_hints(symbol, d.args.len()) =>
                {
                    Some(&d.args)
                }
                _ => None,
            }
        })
        .flatten()
        .copied()
        .collect();
    if roots.is_empty() {
        return HashMap::new();
    }
    let needed = demand(f, roots, options);
    let producer = needed.iter().any(|&value| {
        let ValueDef::Result { inst, .. } = f.value(value).def else {
            return false;
        };
        let d = f.inst(inst);
        matches!(d.opcode, Opcode::Call | Opcode::Invoke)
            && matches!(d.aux, AuxData::CallTarget(s)
                if canonical_float(s) || options.has_captured_body(s))
    });
    if !producer {
        return HashMap::new();
    }
    let args: Vec<_> = f
        .block(f.entry())
        .params
        .iter()
        .map(|&v| f.value(v).ty.bits)
        .collect();
    let mut context = Context {
        options,
        active: HashSet::new(),
        budget: BUDGET,
        cache: HashMap::new(),
    };
    let Some(facts) = context.evaluate(f, &args, 0, &needed) else {
        return HashMap::new();
    };
    let numbers = TypeBits::FIXNUM
        .join(TypeBits::BIGNUM)
        .join(TypeBits::RATIO)
        .join(TypeBits::SINGLE_FLOAT)
        .join(TypeBits::DOUBLE_FLOAT);
    facts
        .into_iter()
        .filter(|(v, ty)| !ty.is_bottom() && numbers.contains(*ty) && f.value(*v).ty.bits != *ty)
        .map(|(v, ty)| (v, IRType::of(ty)))
        .collect()
}

pub fn apply(f: &mut Function, options: &InlineOptions) -> usize {
    f.return_type_hints = collect(f, options);
    f.return_type_hints.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::constants::BytecodeSnapshot;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{Inst, InstData, InstFlags, ValueRepresentation};
    use egcl_rt::bytecode::{BytecodeFunction, Instr};
    use egcl_rt::value::NIL;

    fn body(code: Vec<Instr>, arity: u16) -> BytecodeFunction {
        BytecodeFunction {
            self_call_sites: vec![],
            notinline_sites: vec![],
            code,
            constants: vec![],
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals: arity,
            max_stack: 4,
            arity,
            name: "hint-wrapper".into(),
            params_form: NIL,
            min_args: arity,
            max_args: Some(arity),
            variadic: false,
            binder_cache: Default::default(),
        }
    }
    fn data(opcode: Opcode, args: Vec<Value>, aux: AuxData) -> InstData {
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
    fn caller(symbol: u32, arguments: &[TypeBits]) -> (Function, Value, Inst) {
        let mut f = Function::new("return-hint-consumer");
        let entry = f.entry();
        let args: Vec<_> = arguments
            .iter()
            .map(|&bits| f.add_block_param(entry, IRType::of(bits), ValueRepresentation::Tagged))
            .collect();
        let frame = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 5,
                locals: args
                    .iter()
                    .map(|&value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
                stack: vec![],
            }],
            remat: vec![],
        });
        let mut call = data(Opcode::Call, args, AuxData::CallTarget(symbol));
        call.flags = InstFlags {
            call: true,
            effectful: true,
            safepoint: true,
            ..Default::default()
        };
        call.frame_state = Some(frame);
        let (_, result) = f.push_inst(entry, call, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        let (_, one) = f.push_inst(
            entry,
            data(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let frame = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 77,
                locals: vec![],
                stack: vec![ValueSource::Value {
                    value: result[0],
                    repr: ValueRepresentation::Tagged,
                }],
            }],
            remat: vec![],
        });
        let mut add = data(
            Opcode::Call,
            vec![result[0], one[0]],
            AuxData::CallTarget(egcl_rt::symbols::intern("+")),
        );
        add.flags = InstFlags {
            call: true,
            effectful: true,
            safepoint: true,
            ..Default::default()
        };
        add.frame_state = Some(frame);
        let (site, sum) = f.push_inst(entry, add, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        f.set_terminator(entry, data(Opcode::Return, sum, AuxData::None));
        (f, result[0], site)
    }
    fn wrapper(symbol: u32, arity: u16) -> InlineOptions {
        let float = egcl_rt::symbols::intern("FLOAT");
        let mut code = vec![Instr::LoadLocal(0)];
        if arity == 2 {
            code.push(Instr::LoadLocal(1));
        }
        code.extend([
            Instr::CallNamed {
                sym: float,
                nargs: arity,
            },
            Instr::Return,
        ]);
        let snapshot = BytecodeSnapshot::capture(&body(code, arity));
        InlineOptions::default().with_body_snapshot(symbol, &snapshot)
    }
    #[test]
    fn float_summary_is_argument_specific_and_preserves_uncertain_prototypes() {
        let symbol = egcl_rt::symbols::intern("RETURN-HINT-FLOAT-WRAPPER");
        let floats = TypeBits::SINGLE_FLOAT.join(TypeBits::DOUBLE_FLOAT);
        for (arguments, expected) in [
            (vec![TypeBits::FIXNUM], Some(TypeBits::SINGLE_FLOAT)),
            (vec![TypeBits::BIGNUM], Some(TypeBits::SINGLE_FLOAT)),
            (vec![TypeBits::SINGLE_FLOAT], Some(TypeBits::SINGLE_FLOAT)),
            (vec![TypeBits::DOUBLE_FLOAT], Some(TypeBits::DOUBLE_FLOAT)),
            (vec![TypeBits::TOP], Some(floats)),
            (
                vec![TypeBits::FIXNUM, TypeBits::SINGLE_FLOAT],
                Some(TypeBits::SINGLE_FLOAT),
            ),
            (
                vec![TypeBits::FIXNUM, TypeBits::DOUBLE_FLOAT],
                Some(TypeBits::DOUBLE_FLOAT),
            ),
            (vec![TypeBits::FIXNUM, floats], Some(floats)),
            (vec![TypeBits::FIXNUM, TypeBits::FIXNUM], None),
        ] {
            let options = wrapper(symbol, arguments.len() as u16);
            let (f, value, _) = caller(symbol, &arguments);
            assert_eq!(collect(&f, &options).get(&value).map(|t| t.bits), expected);
            assert_eq!(f.value(value).ty, IRType::TOP);
        }
    }
    #[test]
    fn hints_choose_guarded_family_without_becoming_inference_or_ssa_proofs() {
        use crate::t2::speculate::{SpecType, SpeculateOptions, speculate_with};
        let symbol = egcl_rt::symbols::intern("RETURN-HINT-GUARDED-WRAPPER");
        for bits in [TypeBits::FIXNUM, TypeBits::DOUBLE_FLOAT] {
            let (mut f, value, site) = caller(symbol, &[bits]);
            assert!(apply(&mut f, &wrapper(symbol, 1)) > 0);
            assert_eq!(crate::t2::infer::infer(&f).ty(value), IRType::TOP);
            let options = SpeculateOptions { double_float: true };
            assert_eq!(
                speculate_with(&mut f, &|_| Some(SpecType::Fixnum), options),
                1
            );
            assert_eq!(
                f.value(value).ty,
                IRType::TOP,
                "hint must not narrow call result"
            );
            assert_eq!(
                crate::t2::infer::infer(&f).ty(value),
                IRType::TOP,
                "guarded selection must not turn a hint into a global call-result proof"
            );
            if bits == TypeBits::FIXNUM {
                assert_eq!(f.inst(site).opcode, Opcode::FloatAdd);
                assert!(f.inst(site).flags.guard && f.inst(site).frame_state.is_some());
            } else {
                assert!(
                    f.block_order()
                        .iter()
                        .flat_map(|&b| &f.block(b).insts)
                        .any(|&i| f.inst(i).opcode == Opcode::UnboxFloat
                            && f.inst(i).args.contains(&value)
                            && f.inst(i).flags.guard
                            && f.inst(i).frame_state.is_some_and(|id| f
                                .frame_states
                                .get(id)
                                .scopes
                                .last()
                                .unwrap()
                                .bcp
                                == 77))
                );
            }
            crate::t2::verify::verify(&f).unwrap();
        }
        let (mut f, value, site) = caller(symbol, &[TypeBits::FIXNUM]);
        apply(&mut f, &wrapper(symbol, 1));
        assert_eq!(
            speculate_with(&mut f, &|_| None, SpeculateOptions { double_float: true }),
            0
        );
        assert_eq!(f.inst(site).opcode, Opcode::Call);
        assert_eq!(f.value(value).ty, IRType::TOP);
    }
    #[test]
    fn captured_identity_float_chain_works_but_opaque_raw_and_recursive_bodies_do_not() {
        let identity = egcl_rt::symbols::intern("RETURN-HINT-IDENTITY");
        let wrapper = egcl_rt::symbols::intern("RETURN-HINT-NESTED");
        let float = egcl_rt::symbols::intern("FLOAT");
        let identity_body = body(vec![Instr::LoadLocal(0), Instr::Return], 1);
        let wrapper_body = body(
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: identity,
                    nargs: 1,
                },
                Instr::CallNamed {
                    sym: float,
                    nargs: 1,
                },
                Instr::Return,
            ],
            1,
        );
        let options = InlineOptions::default()
            .with_body_snapshot(identity, &BytecodeSnapshot::capture(&identity_body))
            .with_body_snapshot(wrapper, &BytecodeSnapshot::capture(&wrapper_body));
        let (mut f, value, _) = caller(wrapper, &[TypeBits::FIXNUM]);
        assert_eq!(collect(&f, &options)[&value].bits, TypeBits::SINGLE_FLOAT);
        let raw = InlineOptions::default().with_body(wrapper, std::sync::Arc::new(wrapper_body));
        assert!(collect(&f, &raw).is_empty(), "no live-pool fallback");
        let call = match f.value(value).def {
            super::super::ir::ValueDef::Result { inst, .. } => inst,
            _ => unreachable!(),
        };
        f.inst_mut(call).aux = AuxData::OpaqueCallTarget(wrapper);
        assert!(collect(&f, &options).is_empty());
        let recursive = body(
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: wrapper,
                    nargs: 1,
                },
                Instr::Return,
            ],
            1,
        );
        let options = InlineOptions::default()
            .with_body_snapshot(wrapper, &BytecodeSnapshot::capture(&recursive));
        f.inst_mut(call).aux = AuxData::CallTarget(wrapper);
        assert!(collect(&f, &options).is_empty());
        let mut notinline = body(
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: float,
                    nargs: 1,
                },
                Instr::Return,
            ],
            1,
        );
        notinline.notinline_sites = vec![1];
        let options = InlineOptions::default()
            .with_body_snapshot(wrapper, &BytecodeSnapshot::capture(&notinline));
        assert!(collect(&f, &options).is_empty());
    }
    #[test]
    fn ambiguous_formats_and_noncanonical_float_do_not_supply_a_precise_family() {
        use crate::t2::speculate::{SpecType, SpeculateOptions, speculate_with};
        let symbol = egcl_rt::symbols::intern("RETURN-HINT-AMBIGUOUS");
        let floats = TypeBits::SINGLE_FLOAT.join(TypeBits::DOUBLE_FLOAT);
        for observed in [
            SpecType::Fixnum,
            SpecType::SingleFloat,
            SpecType::DoubleFloat,
        ] {
            let (mut f, value, site) = caller(symbol, &[TypeBits::FIXNUM, floats]);
            apply(&mut f, &wrapper(symbol, 2));
            assert_eq!(f.return_type_hints[&value].bits, floats);
            let count = speculate_with(
                &mut f,
                &|_| Some(observed),
                SpeculateOptions { double_float: true },
            );
            if observed == SpecType::Fixnum {
                assert_eq!(
                    count, 0,
                    "definitely floating return excludes a fixnum guess"
                );
                assert_eq!(f.inst(site).opcode, Opcode::Call);
            } else {
                assert_eq!(
                    count, 1,
                    "compatible observed floating family stays guarded"
                );
                if observed == SpecType::SingleFloat {
                    assert_eq!(f.inst(site).opcode, Opcode::FloatAdd);
                    assert!(f.inst(site).flags.guard && f.inst(site).frame_state.is_some());
                } else {
                    assert!(
                        f.block_order()
                            .iter()
                            .flat_map(|&b| &f.block(b).insts)
                            .any(|&i| f.inst(i).opcode == Opcode::UnboxFloat
                                && f.inst(i).args.contains(&value)
                                && f.inst(i).flags.guard
                                && f.inst(i).frame_state.is_some())
                    );
                }
            }
            assert_eq!(f.value(value).ty, IRType::TOP);
            assert_eq!(crate::t2::infer::infer(&f).ty(value), IRType::TOP);
        }
        for float in [
            egcl_rt::symbols::intern("RETURN-HINT-LOCAL::FLOAT"),
            egcl_rt::symbols::make_uninterned("FLOAT").as_symbol_index(),
        ] {
            let snapshot = BytecodeSnapshot::capture(&body(
                vec![
                    Instr::LoadLocal(0),
                    Instr::CallNamed {
                        sym: float,
                        nargs: 1,
                    },
                    Instr::Return,
                ],
                1,
            ));
            let options = InlineOptions::default().with_body_snapshot(symbol, &snapshot);
            let (f, _, _) = caller(symbol, &[TypeBits::FIXNUM]);
            assert!(collect(&f, &options).is_empty());
        }
        let options = wrapper(symbol, 1);
        let mut context = Context {
            options: &options,
            active: HashSet::new(),
            budget: 1,
            cache: HashMap::new(),
        };
        assert_eq!(context.call(symbol, &[TypeBits::FIXNUM], 0), TypeBits::TOP);
        let mut oversized = vec![Instr::LoadLocal(0)];
        for _ in 0..64 {
            oversized.extend([Instr::Dup, Instr::Pop]);
        }
        oversized.push(Instr::Return);
        let options = InlineOptions::default()
            .with_body_snapshot(symbol, &BytecodeSnapshot::capture(&body(oversized, 1)));
        let (f, _, _) = caller(symbol, &[TypeBits::FIXNUM]);
        assert!(collect(&f, &options).is_empty());
    }
    #[test]
    fn demand_ignores_unrelated_captured_calls_and_preserves_small_summary_budget() {
        let needed_symbol = egcl_rt::symbols::intern("RETURN-HINT-NEEDED");
        let unrelated = egcl_rt::symbols::intern("RETURN-HINT-UNRELATED");
        let oversized = egcl_rt::symbols::intern("RETURN-HINT-OVERSIZED");
        let mut options = wrapper(needed_symbol, 1);
        let extra = wrapper(unrelated, 1)
            .captured_body_snapshot(unrelated)
            .unwrap();
        options = options.with_body_snapshot(unrelated, &extra);
        let mut huge = vec![Instr::LoadLocal(0)];
        for _ in 0..80 {
            huge.extend([Instr::Dup, Instr::Pop]);
        }
        huge.push(Instr::Return);
        options = options.with_body_snapshot(oversized, &BytecodeSnapshot::capture(&body(huge, 1)));
        let (mut f, value, _) = caller(needed_symbol, &[TypeBits::FIXNUM]);
        let entry = f.entry();
        let first_call = match f.value(value).def {
            ValueDef::Result { inst, .. } => inst,
            _ => unreachable!(),
        };
        let template = f.inst(first_call).clone();
        let mut unused = Vec::new();
        for symbol in [unrelated, oversized] {
            let mut d = template.clone();
            d.aux = AuxData::CallTarget(symbol);
            d.results.clear();
            let (inst, result) =
                f.push_inst(entry, d, &[(IRType::TOP, ValueRepresentation::Tagged)]);
            let instructions = &mut f.block_mut(entry).insts;
            assert_eq!(instructions.pop(), Some(inst));
            instructions.insert(0, inst);
            unused.push(result[0]);
        }
        let hints = collect(&f, &options);
        assert_eq!(hints[&value].bits, TypeBits::SINGLE_FLOAT);
        assert!(
            unused.iter().all(|v| !hints.contains_key(v)),
            "unused call results have no hint entry"
        );
        let needed = demand(&f, [value], &options);
        let mut context = Context {
            options: &options,
            active: HashSet::new(),
            budget: 10,
            cache: HashMap::new(),
        };
        let evaluated = context
            .evaluate(&f, &[TypeBits::FIXNUM], 0, &needed)
            .unwrap();
        assert_eq!(
            evaluated[&value],
            TypeBits::SINGLE_FLOAT,
            "unrelated calls must not spend the needed wrapper's small budget"
        );
        assert!(unused.iter().all(|v| !evaluated.contains_key(v)));
        // With no numeric consumer depending on the captured call, even a
        // perfectly precise summary must not populate an expectation table.
        let (mut only_unrelated, original, site) = caller(unrelated, &[TypeBits::FIXNUM]);
        let one = only_unrelated.inst(site).args[1];
        only_unrelated.inst_mut(site).args = vec![one, one];
        assert!(collect(&only_unrelated, &options).is_empty());
        assert_eq!(only_unrelated.value(original).ty, IRType::TOP);
        assert_eq!(
            crate::t2::infer::infer(&only_unrelated).ty(original),
            IRType::TOP
        );
    }
    #[test]
    fn hint_only_snapshots_supply_formats_without_enabling_body_inlining() {
        let symbol = egcl_rt::symbols::intern("RETURN-HINT-COLD-ONLY");
        let snapshot = wrapper(symbol, 1).captured_body_snapshot(symbol).unwrap();
        let options = InlineOptions::default().with_return_hint_body_snapshot(symbol, &snapshot);
        assert!(
            options.body(symbol).is_none(),
            "cold summary is not an inline body"
        );
        assert_eq!(
            super::super::inlining::body_cost(symbol, &options, &mut HashSet::new()),
            Err(super::super::inlining::DeclineReason::UnsupportedBody)
        );
        for (input, expected) in [
            (TypeBits::FIXNUM, TypeBits::SINGLE_FLOAT),
            (TypeBits::DOUBLE_FLOAT, TypeBits::DOUBLE_FLOAT),
        ] {
            let (f, value, _) = caller(symbol, &[input]);
            assert_eq!(collect(&f, &options)[&value].bits, expected);
            assert_eq!(crate::t2::infer::infer(&f).ty(value), IRType::TOP);
        }
        let filtered = options.constant_globals_only(symbol);
        assert!(filtered.body(symbol).is_none());
        assert!(
            !filtered.has_captured_body(symbol),
            "transfer builder drops both body tables"
        );
    }
}
