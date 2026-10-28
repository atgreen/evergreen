// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Discharge redundant checks using facts at the exact instruction pre-state.
//! The conversion and result identity remain; only a proven-total conversion
//! loses recovery state. Verification recomputes the proof independently.

use super::infer::{InferenceResult, infer_for_sites};
use super::ir::{
    AuxData, Function, IRType, Inst, InstData, InstFlags, Opcode, Range, TypeBits,
    ValueRepresentation,
};

fn shape(f: &Function, inst: Inst, guarded: bool) -> bool {
    let d = f.inst(inst);
    let contagion = super::speculate::unbox_accepts_fixnum(&d.aux);
    d.opcode == Opcode::UnboxFloat
        && (d.args.len() == 1 || guarded && contagion && d.args.len() == 2)
        && d.results.len() == 1
        && d.targets.is_empty()
        && (matches!(d.aux, AuxData::None) || guarded && contagion)
        && d.args
            .iter()
            .all(|&v| f.value(v).repr == ValueRepresentation::Tagged)
        && f.value(d.results[0]).repr == ValueRepresentation::UnboxedF64
        && f.value(d.results[0]).ty.bits == TypeBits::DOUBLE_FLOAT
        && d.flags
            == if guarded {
                InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                }
            } else {
                InstFlags::default()
            }
        && d.frame_state.is_some() == guarded
}

fn proven(f: &Function, facts: &InferenceResult, inst: Inst) -> bool {
    // Imported instruction-result homes can bypass their defining checks. This
    // initial consumer deliberately excludes every external OSR entry.
    f.osr_entries.is_empty()
        && f.inst(inst)
            .args
            .first()
            .is_some_and(|&input| facts.ty_before(inst, input).bits == TypeBits::DOUBLE_FLOAT)
}

/// Retain SSA conversions, dropping only redundant boxed-double checks.
/// Rebuild scoped facts on every invocation; no facts survive IR mutation.
pub fn discharge_guards(f: &mut Function) -> usize {
    if !f.osr_entries.is_empty() {
        return 0;
    }
    let sites: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .copied()
        .filter(|&i| shape(f, i, true))
        .collect();
    if sites.is_empty() {
        return refine_fixnum_operands(f);
    }
    let facts = infer_for_sites(f, &sites);
    let eligible: Vec<_> = sites
        .into_iter()
        .filter(|&i| proven(f, &facts, i))
        .collect();
    for &inst in &eligible {
        let d = f.inst_mut(inst);
        // A known double never takes the fixnum conversion arm; its partner
        // cannot affect this exact operand's conversion or numeric contagion.
        d.args.truncate(1);
        d.aux = AuxData::None;
        d.flags = InstFlags::default();
        d.frame_state = None;
    }
    eligible.len() + refine_fixnum_operands(f)
}

fn fixnum_guard_shape(f: &Function, inst: Inst, guarded: bool) -> bool {
    let d = f.inst(inst);
    let AuxData::TypeTag(ty) = d.aux else {
        return false;
    };
    d.opcode == Opcode::Guard
        && d.args.len() == 1
        && d.results.len() == 1
        && d.targets.is_empty()
        && ty.bits == TypeBits::FIXNUM
        && ty.class_id.is_none()
        && ty.range.is_none_or(valid_fixnum_range)
        && f.value(d.args[0]).repr == ValueRepresentation::Tagged
        && f.value(d.results[0]).repr == ValueRepresentation::Tagged
        && f.value(d.results[0]).ty == ty
        && d.flags
            == if guarded {
                InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                }
            } else {
                InstFlags::default()
            }
        && d.frame_state.is_some() == guarded
}

fn valid_fixnum_range(range: Range) -> bool {
    range.lo <= range.hi
        && range.lo >= egcl_rt::bignum::FIXNUM_MIN
        && range.hi <= egcl_rt::bignum::FIXNUM_MAX
}

fn implies_fixnum(fact: IRType, target: IRType) -> bool {
    if fact.bits != TypeBits::FIXNUM || target.bits != TypeBits::FIXNUM || target.class_id.is_some()
    {
        return false;
    }
    match target.range {
        None => true,
        Some(wanted) => {
            valid_fixnum_range(wanted)
                && fact.range.is_some_and(|actual| {
                    valid_fixnum_range(actual) && actual.lo >= wanted.lo && actual.hi <= wanted.hi
                })
        }
    }
}

/// Make scoped fixnum bounds available through fresh SSA identities. Only math
/// operands change: older uses, original values and every recovery frame remain
/// untouched. This producer is wired only to the x86-64 Linux backend initially.
pub fn refine_fixnum_operands(f: &mut Function) -> usize {
    if !f.osr_entries.is_empty() {
        return 0;
    }
    let sites: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .copied()
        .filter(|&i| {
            fixnum_guard_shape(f, i, true)
                || matches!(f.inst(i).opcode, Opcode::FixnumAdd | Opcode::FixnumSub)
                    && f.inst(i).args.len() == 2
        })
        .collect();
    if sites.is_empty() {
        return 0;
    }
    let facts = infer_for_sites(f, &sites);
    let mut discharge = Vec::new();
    let mut copies = Vec::new();
    for site in sites {
        let d = f.inst(site);
        if d.opcode == Opcode::Guard {
            let AuxData::TypeTag(target) = d.aux else {
                unreachable!()
            };
            if implies_fixnum(facts.ty_before(site, d.args[0]), target) {
                discharge.push(site);
            }
        } else {
            for (position, &input) in d.args.iter().enumerate() {
                let ty = facts.ty_before(site, input);
                let Some(range) = ty.range else {
                    continue;
                };
                if ty.bits != TypeBits::FIXNUM
                    || !valid_fixnum_range(range)
                    || f.value(input).repr != ValueRepresentation::Tagged
                    || (range.lo == egcl_rt::bignum::FIXNUM_MIN
                        && range.hi == egcl_rt::bignum::FIXNUM_MAX)
                    || f.value(input)
                        .ty
                        .range
                        .is_some_and(|old| old.lo >= range.lo && old.hi <= range.hi)
                {
                    continue;
                }
                copies.push((
                    site,
                    position,
                    input,
                    IRType {
                        class_id: None,
                        ..ty
                    },
                ));
            }
        }
    }
    for &site in &discharge {
        let d = f.inst_mut(site);
        d.flags = InstFlags::default();
        d.frame_state = None;
    }
    let count = discharge.len() + copies.len();
    for (site, position, input, ty) in copies {
        let block = *f
            .block_order()
            .iter()
            .find(|&&b| f.block(b).insts.contains(&site))
            .expect("requested site remains in its original block");
        let source_pos = f.inst(site).source_pos;
        let (alias, results) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::Guard,
                args: vec![input],
                results: vec![],
                aux: AuxData::TypeTag(ty),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos,
            },
            &[(ty, ValueRepresentation::Tagged)],
        );
        let insts = &mut f.block_mut(block).insts;
        assert_eq!(insts.pop(), Some(alias));
        let at = insts
            .iter()
            .position(|&i| i == site)
            .expect("math site remains present");
        insts.insert(at, alias);
        f.inst_mut(site).args[position] = results[0];
    }
    count
}

/// Validate pure unboxes after structural SSA/FrameState checks have passed.
/// A cleared guard flag alone is never evidence of a valid pointer load.
pub(crate) fn verify_accesses(f: &Function) -> Result<(), String> {
    let sites: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .copied()
        .filter(|&i| {
            matches!(f.inst(i).opcode, Opcode::UnboxFloat | Opcode::Guard) && !f.inst(i).flags.guard
        })
        .collect();
    if sites.is_empty() {
        return Ok(());
    }
    if !f.osr_entries.is_empty() {
        return Err("pure numeric refinement with OSR imports".into());
    }
    for &site in &sites {
        let valid = if f.inst(site).opcode == Opcode::Guard {
            fixnum_guard_shape(f, site, false)
        } else {
            shape(f, site, false)
        };
        if !valid {
            return Err("malformed pure numeric refinement".into());
        }
    }
    let facts = infer_for_sites(f, &sites);
    for site in sites {
        let d = f.inst(site);
        let valid = if d.opcode == Opcode::Guard {
            let AuxData::TypeTag(target) = d.aux else {
                unreachable!()
            };
            implies_fixnum(facts.ty_before(site, d.args[0]), target)
        } else {
            proven(f, &facts, site)
        };
        if !valid {
            return Err("pure numeric refinement lacks a scoped prestate proof".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{Block, BlockCall, IRType, InstData, OsrEntry, Value};

    fn data(opcode: Opcode, args: Vec<Value>) -> InstData {
        InstData {
            opcode,
            args,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    // An actual Boolean predicate, not a declared parameter or a synthetic
    // numeric result annotation, establishes the true-edge operand fact.
    fn branch_fixture(location: &str, osr: bool, contagion: bool) -> (Function, Inst) {
        let mut f = Function::new("double-branch-discharge");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let yes = f.make_block();
        let no = f.make_block();
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: input,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let target = match location {
            "prior" => entry,
            "false" => no,
            _ => yes,
        };
        let emit_unbox = |f: &mut Function, block: Block| {
            let mut d = data(Opcode::UnboxFloat, vec![input]);
            if contagion {
                d.args.push(input);
                d.aux = super::super::speculate::contagion_unbox_aux();
            }
            d.flags = InstFlags {
                guard: true,
                effectful: true,
                ..Default::default()
            };
            d.frame_state = Some(state);
            f.push_inst(
                block,
                d,
                &[(
                    IRType::of(TypeBits::DOUBLE_FLOAT),
                    ValueRepresentation::UnboxedF64,
                )],
            )
        };
        let early = (target == entry).then(|| emit_unbox(&mut f, entry));
        let mut predicate = data(Opcode::TypeCheck, vec![input]);
        predicate.aux = AuxData::TypeTag(IRType::of(TypeBits::DOUBLE_FLOAT));
        let (_, predicate) = f.push_inst(
            entry,
            predicate,
            &[(
                IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        let mut branch = data(Opcode::Brif, vec![predicate[0]]);
        branch.targets = vec![
            BlockCall {
                block: yes,
                args: vec![],
            },
            BlockCall {
                block: no,
                args: vec![],
            },
        ];
        let (unbox, raw) = early.unwrap_or_else(|| emit_unbox(&mut f, target));
        let mut compare = data(Opcode::FloatCmpEq, vec![raw[0], raw[0]]);
        compare.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        compare.frame_state = Some(state);
        let (_, result) = f.push_inst(
            target,
            compare,
            &[(
                IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        f.set_terminator(entry, branch);
        if target == yes {
            f.set_terminator(yes, data(Opcode::Return, vec![result[0]]));
        } else {
            let (_, nil) = f.push_inst(
                yes,
                data(Opcode::ConstNil, vec![]),
                &[(IRType::of(TypeBits::NULL), ValueRepresentation::Tagged)],
            );
            f.set_terminator(yes, data(Opcode::Return, nil));
        }
        if target == no {
            f.set_terminator(no, data(Opcode::Return, vec![result[0]]));
        } else {
            let (_, nil) = f.push_inst(
                no,
                data(Opcode::ConstNil, vec![]),
                &[(IRType::of(TypeBits::NULL), ValueRepresentation::Tagged)],
            );
            f.set_terminator(no, data(Opcode::Return, nil));
        }
        if osr {
            f.osr_entries.push(OsrEntry {
                block: yes,
                bcp: 0,
                frame_state: state,
                checks: vec![],
                control_scopes: vec![],
            });
        }
        (f, unbox)
    }

    #[test]
    fn double_true_edge_discharge_retains_conversion_and_removes_partner() {
        for contagion in [false, true] {
            let (mut f, site) = branch_fixture("true", false, contagion);
            crate::t2::verify::verify(&f).unwrap();
            let result = f.inst(site).results.clone();
            assert_eq!(discharge_guards(&mut f), 1);
            assert_eq!(f.inst(site).results, result);
            assert_eq!(f.inst(site).args.len(), 1);
            assert!(matches!(f.inst(site).aux, AuxData::None));
            assert_eq!(f.inst(site).flags, InstFlags::default());
            assert!(f.inst(site).frame_state.is_none());
            crate::t2::verify::verify(&f).unwrap();
            assert_eq!(discharge_guards(&mut f), 0);
        }
    }

    #[test]
    fn false_prior_and_osr_sites_keep_checks_and_reject_forged_purity() {
        for (location, osr) in [("false", false), ("prior", false), ("true", true)] {
            let (mut f, site) = branch_fixture(location, osr, false);
            // A prior unbox's result is used only in its own block; this test
            // checks scoped inference directly before checking forged purity.
            assert_eq!(discharge_guards(&mut f), 0, "{location}/{osr}");
            assert!(f.inst(site).flags.guard && f.inst(site).frame_state.is_some());
            let d = f.inst_mut(site);
            d.flags = InstFlags::default();
            d.frame_state = None;
            assert!(verify_accesses(&f).is_err(), "{location}/{osr}");
        }
    }

    #[test]
    fn proof_does_not_authorize_fixnum_contagion_or_malformed_pure_shapes() {
        let (mut f, site) = branch_fixture("true", false, true);
        assert_eq!(discharge_guards(&mut f), 1);
        let original = f.inst(site).clone();
        for mutation in 0..4 {
            *f.inst_mut(site) = original.clone();
            let d = f.inst_mut(site);
            match mutation {
                0 => {
                    d.args.push(d.args[0]);
                    d.aux = super::super::speculate::contagion_unbox_aux();
                }
                1 => d.flags.effectful = true,
                2 => d.frame_state = Some(crate::t2::frame_state::FrameStateId(0)),
                _ => d.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
            }
            assert!(verify_accesses(&f).is_err());
        }
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn native_double_branch_discharge_removes_recovery_and_executes() {
        use crate::t2::emit::{
            NativeCallStorage, NativeRootPublication,
            emit_framed_native_handlers_with_entries_and_storage,
        };
        use crate::t2::x64_calls::NativeCallOrigin;
        use egcl_rt::{EgclStack, jit::JitBuffer, native_transfer};
        extern "C" fn single_values() {}
        fn emit(f: &Function) -> crate::t2::emit::FramedCode {
            emit_framed_native_handlers_with_entries_and_storage(
                f,
                1,
                1,
                1,
                1,
                single_values as *const () as usize as u64,
                1,
                1,
                0,
                None,
                Some(1),
                &[],
                NativeRootPublication::PublishedHomes,
                NativeCallStorage::NativeStack,
            )
            .unwrap()
            .0
        }
        let (mut f, _) = branch_fixture("true", false, false);
        let guarded = emit(&f);
        assert_eq!(discharge_guards(&mut f), 1);
        let pure = emit(&f);
        let deopts = |code: &crate::t2::emit::FramedCode| {
            code.native_calls
                .as_ref()
                .unwrap()
                .iter()
                .filter(|site| matches!(site.origin, NativeCallOrigin::Deopt(_)))
                .count()
        };
        assert_eq!(
            deopts(&guarded),
            deopts(&pure) + 1,
            "the conversion's real returning recovery attachment must disappear"
        );
        assert!(
            pure.code.len() < guarded.code.len(),
            "actual guard/recovery bytes must disappear"
        );
        let code = JitBuffer::new(&pure.code).unwrap();
        let stack = EgclStack::new(64 * 1024);
        egcl_rt::rooted!(boxed = egcl_rt::gc::alloc_double_float(1.25));
        for (input, expected) in [
            (*boxed, egcl_rt::value::T),
            (egcl_rt::value::EgclVal::from_fixnum(7), egcl_rt::value::NIL),
        ] {
            let mut slots = vec![input; 1 + usize::from(pure.shadow_root_slots)];
            egcl_rt::rooted_ref!(_slots = &mut slots);
            let outcome = unsafe {
                native_transfer::invoke_native_segment(
                    code.as_ptr(),
                    slots.as_mut_ptr().cast(),
                    &stack,
                )
            }
            .unwrap();
            assert_eq!(outcome.exit, native_transfer::NativeExit::Returned);
            assert_eq!(outcome.value, expected);
        }
    }
    fn bounded_fixnum_fixture(lo: i64, hi: i64) -> (Function, Inst, Inst, Value) {
        let mut f = Function::new("scoped-bounded-add");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let low = f.make_block();
        let high = f.make_block();
        let body = f.make_block();
        let outside = f.make_block();
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: input,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let constant = |f: &mut Function, b, value| {
            let mut d = data(Opcode::ConstFixnum, vec![]);
            d.aux = AuxData::FixnumImm(value);
            f.push_inst(
                b,
                d,
                &[(
                    IRType {
                        range: Some(Range {
                            lo: value,
                            hi: value,
                        }),
                        ..IRType::of(TypeBits::FIXNUM)
                    },
                    ValueRepresentation::Tagged,
                )],
            )
            .1[0]
        };
        let branch = |f: &mut Function, b, condition, yes, no| {
            let mut d = data(Opcode::Brif, vec![condition]);
            d.targets = vec![
                BlockCall {
                    block: yes,
                    args: vec![],
                },
                BlockCall {
                    block: no,
                    args: vec![],
                },
            ];
            f.set_terminator(b, d);
        };
        let mut predicate = data(Opcode::TypeCheck, vec![input]);
        predicate.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
        let boolean = IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL));
        let (_, predicate) =
            f.push_inst(entry, predicate, &[(boolean, ValueRepresentation::Tagged)]);
        branch(&mut f, entry, predicate[0], low, outside);
        for (block, bound, opcode, yes) in [
            (low, lo, Opcode::FixnumCmpGe, high),
            (high, hi, Opcode::FixnumCmpLe, body),
        ] {
            let bound = constant(&mut f, block, bound);
            let mut d = data(opcode, vec![input, bound]);
            d.flags = InstFlags {
                guard: true,
                effectful: true,
                ..Default::default()
            };
            d.frame_state = Some(state);
            let (_, condition) = f.push_inst(block, d, &[(boolean, ValueRepresentation::Tagged)]);
            branch(&mut f, block, condition[0], yes, outside);
        }
        let mut d = data(Opcode::Guard, vec![input]);
        d.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
        d.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        d.frame_state = Some(state);
        let (guard, guarded) = f.push_inst(
            body,
            d,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let one = constant(&mut f, body, 1);
        let mut d = data(Opcode::FixnumAdd, vec![guarded[0], one]);
        d.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        d.frame_state = Some(state);
        let (math, result) = f.push_inst(
            body,
            d,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        f.set_terminator(body, data(Opcode::Return, result));
        let (_, nil) = f.push_inst(
            outside,
            data(Opcode::ConstNil, vec![]),
            &[(IRType::of(TypeBits::NULL), ValueRepresentation::Tagged)],
        );
        f.set_terminator(outside, data(Opcode::Return, nil));
        (f, guard, math, input)
    }

    #[test]
    fn scoped_fixnum_bounds_materialize_only_local_verified_aliases() {
        let (mut f, guard, math, original) = bounded_fixnum_fixture(-20, 20);
        crate::t2::verify::verify(&f).unwrap();
        let old_args = f.inst(math).args.clone();
        let old_state = f.inst(math).frame_state;
        let old_result = f.inst(guard).results[0];
        assert_eq!(
            refine_fixnum_operands(&mut f),
            2,
            "one guard and one ranged copy"
        );
        assert_eq!(f.value(original).ty, IRType::TOP);
        assert_eq!(f.inst(guard).results[0], old_result);
        assert_eq!(
            f.value(old_result).ty.range,
            None,
            "old result metadata stays unchanged"
        );
        assert_eq!(f.inst(math).frame_state, old_state);
        let source = &f.frame_states.get(old_state.unwrap()).scopes[0].locals[0];
        assert!(
            matches!(source, ValueSource::Value { value, repr: ValueRepresentation::Tagged } if *value == original)
        );
        assert_ne!(f.inst(math).args[0], old_args[0]);
        assert_eq!(f.inst(math).args[1], old_args[1]);
        assert_eq!(
            f.value(f.inst(math).args[0]).ty.range,
            Some(Range { lo: -20, hi: 20 })
        );
        crate::t2::verify::verify(&f).unwrap();
        assert_eq!(refine_fixnum_operands(&mut f), 0);
    }

    #[test]
    fn fixnum_guard_before_predicate_retains_its_runtime_check() {
        let (mut f, guard, _, original) = bounded_fixnum_fixture(-20, 20);
        let from = *f
            .block_order()
            .iter()
            .find(|&&b| f.block(b).insts.contains(&guard))
            .unwrap();
        f.block_mut(from).insts.retain(|&i| i != guard);
        let entry = f.entry();
        f.block_mut(entry).insts.insert(0, guard);
        crate::t2::verify::verify(&f).unwrap();
        assert_eq!(refine_fixnum_operands(&mut f), 0);
        assert!(f.inst(guard).flags.guard && f.inst(guard).frame_state.is_some());
        assert_eq!(f.value(original).ty, IRType::TOP);
    }

    #[test]
    fn forged_pure_fixnum_guard_does_not_prove_its_own_range_or_type() {
        for wrong_type in [false, true] {
            let (mut f, guard, _, original) = bounded_fixnum_fixture(-20, 20);
            assert_eq!(refine_fixnum_operands(&mut f), 2);
            let target = IRType {
                range: Some(Range { lo: 0, hi: 0 }),
                ..IRType::of(TypeBits::FIXNUM)
            };
            // Change a valid copy into an unjustified stronger assertion, while
            // making its result metadata agree: verifier must check the source.
            let result = f.inst(guard).results[0];
            f.inst_mut(guard).aux = AuxData::TypeTag(target);
            f.refine_type(result, target);
            if wrong_type {
                let outside = *f.block_order().last().unwrap();
                let guard_block = *f
                    .block_order()
                    .iter()
                    .find(|&&b| f.block(b).insts.contains(&guard))
                    .unwrap();
                f.block_mut(guard_block).insts.retain(|&i| i != guard);
                f.block_mut(outside).insts.insert(0, guard);
                f.inst_mut(guard).args[0] = original;
            }
            assert!(verify_accesses(&f).is_err());
        }
        let (mut f, guard, _, _) = bounded_fixnum_fixture(-20, 20);
        let block = *f
            .block_order()
            .iter()
            .find(|&&b| f.block(b).insts.contains(&guard))
            .unwrap();
        f.osr_entries.push(OsrEntry {
            block,
            bcp: 0,
            frame_state: f.inst(guard).frame_state.unwrap(),
            checks: vec![],
            control_scopes: vec![],
        });
        assert_eq!(refine_fixnum_operands(&mut f), 0);
        assert!(f.inst(guard).flags.guard);
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn native_scoped_fixnum_add_loses_overflow_and_recovery_bytes() {
        use crate::t2::emit::{FramedCode, emit_framed};
        use crate::t2::x64_calls::NativeCallOrigin;
        use egcl_rt::{jit::JitBuffer, value::EgclVal};
        fn emit(f: &Function) -> FramedCode {
            emit_framed(f, 0, 1, 0, 0, 0, 0, 0, 0, None).unwrap()
        }
        fn overflow_checks(code: &FramedCode) -> usize {
            let end = code
                .native_calls
                .as_ref()
                .unwrap()
                .iter()
                .filter(|site| matches!(site.origin, NativeCallOrigin::Deopt(_)))
                .map(|site| site.return_offset)
                .min()
                .unwrap_or(code.code.len());
            iced_x86::Decoder::with_ip(64, &code.code[..end], 0, iced_x86::DecoderOptions::NONE)
                .into_iter()
                .filter(|i| i.mnemonic() == iced_x86::Mnemonic::Jo)
                .count()
        }
        let (mut f, _, _, _) = bounded_fixnum_fixture(-20, 20);
        let original = emit(&f);
        assert_eq!(overflow_checks(&original), 1);
        assert_eq!(refine_fixnum_operands(&mut f), 2);
        let refined = emit(&f);
        assert_eq!(overflow_checks(&refined), 0);
        assert!(refined.code.len() < original.code.len());
        let buffer = JitBuffer::new(&refined.code).unwrap();
        let entry: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(buffer.as_ptr().add(refined.frame_entry.unwrap())) };
        for (input, expected) in [
            (-21, None),
            (-20, Some(-19)),
            (0, Some(1)),
            (20, Some(21)),
            (21, None),
            (egcl_rt::bignum::FIXNUM_MAX, None),
        ] {
            let mut slots = [EgclVal::from_fixnum(input).0, 0, 0, 0];
            let expected = expected
                .map(EgclVal::from_fixnum)
                .unwrap_or(egcl_rt::value::NIL);
            assert_eq!(EgclVal(entry(slots.as_mut_ptr())), expected);
        }
        let (mut f, _, _, _) =
            bounded_fixnum_fixture(egcl_rt::bignum::FIXNUM_MAX - 1, egcl_rt::bignum::FIXNUM_MAX);
        assert_eq!(refine_fixnum_operands(&mut f), 2);
        assert_eq!(
            overflow_checks(&emit(&f)),
            1,
            "possible overflow must keep JO"
        );
    }
}
