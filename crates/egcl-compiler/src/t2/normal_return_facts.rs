// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Audited original-argument postconditions of a checked IR instruction.
//!
//! These facts hold only on its successful optimized continuation. A failed
//! check leaves that continuation by deoptimization; interpreter replacement
//! values/restarts cannot return here with the original unchecked arguments.
//! Generic Call/Invoke have no such contract: a successful call may have handled
//! an error, replaced arguments, or returned without validating them at all.
//!
//! This interface describes arguments, not merely the result's type. Callers
//! apply it AFTER the instruction or in strict all-entry-dominated blocks.
//! It allocates nothing and never reads profiles, symbol cells or heap contents.

use super::ir::{AuxData, Function, IRType, Inst, Opcode, TypeBits, Value, ValueRepresentation};

/// Visit facts established about the actual SSA arguments by a normal return.
/// Unsupported operations/shapes produce no facts. Predicate truth is a
/// separate edge contract; normal Boolean return proves no operand type.
pub(crate) fn visit_argument_facts(f: &Function, inst: Inst, mut visit: impl FnMut(Value, IRType)) {
    let d = f.inst(inst);
    if !d.flags.guard
        || !d.flags.effectful
        || d.flags.call
        || d.flags.safepoint
        || d.flags.terminator
        || !d.targets.is_empty()
        || d
            .frame_state
            .is_none_or(|id| (id.0 as usize) >= f.frame_states.len())
        || d.args
            .iter()
            .chain(&d.results)
            .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
    {
        return;
    }

    if d.opcode == Opcode::Guard {
        if d.args.len() != 1 || d.results.len() > 1 {
            return;
        }
        let ty = match d.aux {
            AuxData::TypeTag(ty)
                if ty.class_id.is_none()
                    && ty
                        .range
                        .is_none_or(|r| ty.bits == TypeBits::FIXNUM && r.lo <= r.hi) =>
            {
                ty
            }
            AuxData::StringLayout => IRType::of(TypeBits::STRING),
            _ => return,
        };
        visit(d.args[0], ty);
        return;
    }

    // These tagged operators check each operand as FIXNUM and either return
    // normally or leave optimized execution. Retained CallTarget is source
    // provenance, not an alternate dispatch behavior of the typed opcode.
    let arity = match d.opcode {
        Opcode::FixnumAdd
        | Opcode::FixnumSub
        | Opcode::FixnumMul
        | Opcode::FixnumMin
        | Opcode::FixnumMax
        | Opcode::FixnumCmpEq
        | Opcode::FixnumCmpLt
        | Opcode::FixnumCmpLe
        | Opcode::FixnumCmpGt
        | Opcode::FixnumCmpGe => 2,
        Opcode::FixnumNeg => 1,
        // ASH counts and generic bitwise integers require their own audit.
        // Calls, predicates, conversions and conditional kernels are excluded.
        _ => return,
    };
    if d.args.len() != arity
        || d.results.len() != 1
        || !matches!(d.aux, AuxData::None | AuxData::CallTarget(_))
    {
        return;
    }
    for &value in &d.args {
        visit(value, IRType::of(TypeBits::FIXNUM));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, FrameStateId, ValueSource};
    use crate::t2::infer::infer_for_sites;
    use crate::t2::ir::{Block, BlockCall, InstData, InstFlags, OsrEntry, Range};

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
    fn state(f: &mut Function, input: Value) -> FrameStateId {
        let repr = f.value(input).repr;
        f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value { value: input, repr }],
                stack: vec![],
            }],
            remat: vec![],
        })
    }
    fn constant(f: &mut Function, block: Block, value: i64) -> Value {
        let mut d = data(Opcode::ConstFixnum, vec![]);
        d.aux = AuxData::FixnumImm(value);
        f.push_inst(
            block,
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
    }
    fn checked(
        f: &mut Function,
        block: Block,
        opcode: Opcode,
        args: Vec<Value>,
        fs: FrameStateId,
    ) -> Inst {
        let mut d = data(opcode, args);
        d.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        d.frame_state = Some(fs);
        f.push_inst(block, d, &[(IRType::TOP, ValueRepresentation::Tagged)])
            .0
    }
    fn observe(f: &mut Function, block: Block, input: Value) -> Inst {
        let mut d = data(Opcode::GenericEq, vec![input, input]);
        d.aux = AuxData::None;
        f.push_inst(block, d, &[(IRType::TOP, ValueRepresentation::Tagged)])
            .0
    }
    fn facts(f: &Function, inst: Inst) -> Vec<(Value, IRType)> {
        let mut result = Vec::new();
        visit_argument_facts(f, inst, |value, ty| result.push((value, ty)));
        result
    }

    #[test]
    fn audited_checked_ops_refine_original_arguments_only_after_success() {
        for opcode in [
            Opcode::FixnumAdd,
            Opcode::FixnumSub,
            Opcode::FixnumMul,
            Opcode::FixnumNeg,
            Opcode::FixnumMin,
            Opcode::FixnumMax,
            Opcode::FixnumCmpEq,
            Opcode::FixnumCmpLt,
            Opcode::FixnumCmpLe,
            Opcode::FixnumCmpGt,
            Opcode::FixnumCmpGe,
        ] {
            let mut f = Function::new("checked-argument-contract");
            let entry = f.entry();
            let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
            let fs = state(&mut f, input);
            let prior = observe(&mut f, entry, input);
            let one = constant(&mut f, entry, 1);
            let args = if opcode == Opcode::FixnumNeg {
                vec![input]
            } else {
                vec![input, one]
            };
            let check = checked(&mut f, entry, opcode, args.clone(), fs);
            let later = observe(&mut f, entry, input);
            let next = f.make_block();
            let mut jump = data(Opcode::Jump, vec![]);
            jump.targets = vec![BlockCall {
                block: next,
                args: vec![],
            }];
            f.set_terminator(entry, jump);
            let dominated = observe(&mut f, next, input);
            f.set_terminator(next, data(Opcode::Return, vec![]));
            let inferred = infer_for_sites(&f, &[prior, check, later, dominated]);
            assert_eq!(inferred.ty_before(prior, input), IRType::TOP, "{opcode:?}");
            assert_eq!(
                inferred.ty_before(check, input),
                IRType::TOP,
                "self proof {opcode:?}"
            );
            assert_eq!(
                inferred.ty_before(later, input),
                IRType::of(TypeBits::FIXNUM),
                "{opcode:?}"
            );
            assert_eq!(
                inferred.ty_before(dominated, input),
                IRType::of(TypeBits::FIXNUM),
                "{opcode:?}"
            );
            assert_eq!(
                inferred.ty(input),
                IRType::TOP,
                "no global narrowing {opcode:?}"
            );
            assert_eq!(facts(&f, check).len(), args.len());
            f.inst_mut(check).aux = AuxData::CallTarget(123);
            assert_eq!(
                facts(&f, check).len(),
                args.len(),
                "retained source metadata"
            );
            f.inst_mut(check).frame_state = None;
            assert!(facts(&f, check).is_empty(), "unchecked shape {opcode:?}");
        }
    }

    #[test]
    fn calls_predicates_shifts_raw_and_pure_guards_do_not_claim_argument_validation() {
        for opcode in [
            Opcode::Call,
            Opcode::Invoke,
            Opcode::TypeCheck,
            Opcode::FixnumShl,
            Opcode::FixnumShr,
            Opcode::TryIntegerShr,
        ] {
            let mut f = Function::new("unsupported-contract");
            let entry = f.entry();
            let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
            let fs = state(&mut f, input);
            // Even flags resembling a checked arithmetic instruction cannot
            // turn an unaudited opcode into an argument contract.
            let check = if opcode == Opcode::Invoke {
                let mut d = data(opcode, vec![input]);
                d.frame_state = Some(fs);
                d.flags = InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                };
                f.set_terminator_with_results(
                    entry,
                    d,
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                )
                .0
            } else {
                checked(&mut f, entry, opcode, vec![input, input], fs)
            };
            assert!(facts(&f, check).is_empty(), "{opcode:?}");
        }
        let mut f = Function::new("pure-guard-is-no-new-proof");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let mut d = data(Opcode::Guard, vec![input]);
        d.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
        let (copy, _) = f.push_inst(
            entry,
            d,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let later = observe(&mut f, entry, input);
        f.set_terminator(entry, data(Opcode::Return, vec![]));
        assert!(facts(&f, copy).is_empty());
        assert_eq!(
            infer_for_sites(&f, &[later]).ty_before(later, input),
            IRType::TOP
        );
        let mut raw = Function::new("raw-argument-contract-excluded");
        let entry = raw.entry();
        let input = raw.add_block_param(
            entry,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        let fs = state(&mut raw, input);
        let check = checked(&mut raw, entry, Opcode::FixnumNeg, vec![input], fs);
        raw.set_terminator(entry, data(Opcode::Return, vec![]));
        assert!(
            facts(&raw, check).is_empty(),
            "raw representations need an explicit contract"
        );
    }

    #[test]
    fn conditional_check_and_osr_bypass_do_not_refine_unchecked_join() {
        for conditional in [false, true] {
            let mut f = Function::new("contract-bypass");
            let entry = f.entry();
            let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
            let condition = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
            let fs = state(&mut f, input);
            let checked_block = if conditional { f.make_block() } else { entry };
            let join = f.make_block();
            if conditional {
                let mut branch = data(Opcode::Brif, vec![condition]);
                branch.targets = vec![
                    BlockCall {
                        block: checked_block,
                        args: vec![],
                    },
                    BlockCall {
                        block: join,
                        args: vec![],
                    },
                ];
                f.set_terminator(entry, branch);
            }
            checked(&mut f, checked_block, Opcode::FixnumNeg, vec![input], fs);
            let mut jump = data(Opcode::Jump, vec![]);
            jump.targets = vec![BlockCall {
                block: join,
                args: vec![],
            }];
            f.set_terminator(checked_block, jump);
            let observer = observe(&mut f, join, input);
            f.set_terminator(join, data(Opcode::Return, vec![]));
            if !conditional {
                assert_eq!(
                    infer_for_sites(&f, &[observer])
                        .ty_before(observer, input)
                        .bits,
                    TypeBits::FIXNUM
                );
                f.osr_entries.push(OsrEntry {
                    block: join,
                    bcp: 0,
                    frame_state: fs,
                    checks: vec![],
                    control_scopes: vec![],
                });
            }
            assert_eq!(
                infer_for_sites(&f, &[observer]).ty_before(observer, input),
                IRType::TOP
            );
        }
    }

    #[test]
    fn real_bytecode_check_result_bounds_refine_original_local_not_arbitrary_call_arguments() {
        use egcl_rt::bytecode::{BytecodeFunction, DeclaredType, Instr};
        use egcl_rt::value::{EgclVal, NIL};
        let helper = egcl_rt::symbols::intern("NORMAL-RETURN-OPAQUE-HELPER");
        let body = BytecodeFunction {
            self_call_sites: vec![],
            notinline_sites: vec![],
            code: vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: helper,
                    nargs: 1,
                },
                Instr::Pop,
                Instr::LoadLocal(0),
                Instr::CheckResult(DeclaredType::FixnumRange {
                    lower: -10,
                    upper: 10,
                }),
                Instr::Pop,
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: helper,
                    nargs: 1,
                },
                Instr::Return,
            ],
            constants: Vec::<EgclVal>::new(),
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
            max_stack: 1,
            arity: 1,
            name: "normal-check-result".into(),
            params_form: NIL,
            min_args: 1,
            max_args: Some(1),
            variadic: false,
            binder_cache: Default::default(),
        };
        let f = crate::t2::build::build_from_bytecode(&body).unwrap();
        let calls: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .copied()
            .filter(|&i| f.inst(i).opcode == Opcode::Call)
            .collect();
        assert_eq!(calls.len(), 2);
        let inferred = infer_for_sites(&f, &calls);
        let original = f.inst(calls[0]).args[0];
        assert_eq!(
            f.inst(calls[1]).args[0],
            original,
            "proof concerns the unchanged original local"
        );
        assert_eq!(inferred.ty_before(calls[0], original), IRType::TOP);
        assert_eq!(
            inferred.ty_before(calls[1], original),
            IRType {
                range: Some(Range { lo: -10, hi: 10 }),
                ..IRType::of(TypeBits::FIXNUM)
            }
        );
        assert_eq!(inferred.ty(original), IRType::TOP);
        assert!(facts(&f, calls[0]).is_empty());
        assert!(facts(&f, calls[1]).is_empty());
        assert_eq!(
            inferred.ty(f.inst(calls[1]).results[0]),
            IRType::TOP,
            "a checked argument does not specify an arbitrary call result"
        );
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn native_checked_min_normal_return_discharges_later_original_input_guard() {
        use crate::t2::emit::{
            FramedCode, NativeCallStorage, NativeRootPublication,
            emit_framed_native_handlers_with_entries_and_storage,
        };
        use crate::t2::x64_calls::NativeCallOrigin;
        use egcl_rt::{EgclStack, jit::JitBuffer, native_transfer, value::EgclVal};
        let mut f = Function::new("checked-min-refines-original");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let fs = state(&mut f, input);
        let zero = constant(&mut f, entry, 0);
        checked(&mut f, entry, Opcode::FixnumMin, vec![input, zero], fs);
        let mut d = data(Opcode::Guard, vec![input]);
        d.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
        d.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        d.frame_state = Some(fs);
        let (guard, narrowed) = f.push_inst(
            entry,
            d,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let one = constant(&mut f, entry, 1);
        let add = checked(&mut f, entry, Opcode::FixnumAdd, vec![narrowed[0], one], fs);
        let result = f.inst(add).results[0];
        f.set_terminator(entry, data(Opcode::Return, vec![result]));
        extern "C" fn single_values() {}
        fn emit(f: &Function) -> FramedCode {
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
        let original = emit(&f);
        assert_eq!(crate::t2::opt_refine::discharge_guards(&mut f), 1);
        assert!(!f.inst(guard).flags.guard && f.inst(guard).frame_state.is_none());
        assert_eq!(f.value(input).ty, IRType::TOP);
        crate::t2::verify::verify(&f).unwrap();
        let refined = emit(&f);
        let deopts = |code: &FramedCode| {
            code.native_calls
                .as_ref()
                .unwrap()
                .iter()
                .filter(|site| matches!(site.origin, NativeCallOrigin::Deopt(_)))
                .count()
        };
        assert_eq!(deopts(&original), deopts(&refined) + 1);
        assert!(refined.code.len() < original.code.len());
        let buffer = JitBuffer::new(&refined.code).unwrap();
        let stack = EgclStack::new(64 * 1024);
        for value in [-5, 0, 5] {
            let mut slots =
                vec![EgclVal::from_fixnum(value); 1 + usize::from(refined.shadow_root_slots)];
            egcl_rt::rooted_ref!(_slots = &mut slots);
            let outcome = unsafe {
                native_transfer::invoke_native_segment(
                    buffer.as_ptr(),
                    slots.as_mut_ptr().cast(),
                    &stack,
                )
            }
            .unwrap();
            assert_eq!(outcome.exit, native_transfer::NativeExit::Returned);
            assert_eq!(outcome.value, EgclVal::from_fixnum(value + 1));
        }
    }
}
