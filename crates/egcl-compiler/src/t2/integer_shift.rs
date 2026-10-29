// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Success-only integer right-shift probes. Unsupported integers keep the
//! original ASH call and its precise recovery state, never a speculative exit.
use super::ir::{
    AuxData, BlockCall, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueDef,
    ValueRepresentation as Repr,
};

/// Run after loop peeling/numeric selection, before transfer legalization.
/// Invoke is deliberately excluded: its normal-result dominance is a separate
/// contract. The later legalizer gives the retained slow call its usual edges.
pub fn specialize(f: &mut Function, options: &super::inlining::InlineOptions) -> usize {
    if !super::target_support::integer_right_shift_probe() {
        return 0;
    }
    let sites: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.clone())
        .collect();
    let mut count = 0;
    for site in sites {
        let old = f.inst(site).clone();
        let AuxData::CallTarget(symbol) = old.aux else {
            continue;
        };
        if old.opcode != Opcode::Call
            || old.args.len() != 2
            || old.results.len() != 1
            || old.frame_state.is_none()
            || !old.targets.is_empty()
            || old
                .args
                .iter()
                .chain(&old.results)
                .any(|&v| f.value(v).repr != Repr::Tagged)
            || egcl_rt::symbols::registry_key(symbol).as_deref() != Some("ASH")
            || super::builtin_binding::snapshot(symbol)
                != Some(super::builtin_binding::Binding::FixedStandard)
        {
            continue;
        }
        if old
            .frame_state
            .and_then(|id| f.frame_states.get(id).scopes.last())
            .is_some_and(|scope| {
                options.policy_at(scope.bcp) == super::inlining::InlinePolicy::NotInline
            })
        {
            continue;
        }
        let ValueDef::Result { inst, .. } = f.value(old.args[1]).def else {
            continue;
        };
        let amount = match f.inst(inst).aux {
            AuxData::FixnumImm(amount)
                if f.inst(inst).opcode == Opcode::ConstFixnum && amount < 0 =>
            {
                amount.unsigned_abs()
            }
            _ => continue,
        };
        let block = f
            .block_order()
            .iter()
            .copied()
            .find(|&b| f.block(b).insts.contains(&site))
            .unwrap();
        // A preserved slow call is deliberately still generic. Do not wrap it
        // again if an optimizer driver repeats this pass on the same IR.
        if f.preds(block).iter().any(|&pred| {
            let Some(term) = f.terminator(pred) else {
                return false;
            };
            let branch = f.inst(term);
            if branch.opcode != Opcode::Brif
                || branch.args.len() != 1
                || branch.targets.get(1).is_none_or(|edge| edge.block != block)
            {
                return false;
            }
            let ValueDef::Result { inst, .. } = f.value(branch.args[0]).def else {
                return false;
            };
            let probe = f.inst(inst);
            probe.opcode == Opcode::TryIntegerShr
                && probe.args == vec![old.args[0]]
                && probe.results.first() == branch.args.first()
                && matches!(probe.aux, AuxData::FixnumImm(bits) if bits == amount as i64)
        }) {
            continue;
        }
        let position = f
            .block(block)
            .insts
            .iter()
            .position(|&i| i == site)
            .unwrap();
        let tail = f.block_mut(block).insts.split_off(position);
        let join = f.make_block_after(block);
        let slow = f.make_block_after(block);
        let fast = f.make_block_after(block);
        let joined = f.add_block_param(join, f.value(old.results[0]).ty, Repr::Tagged);
        f.block_mut(join).insts = tail[1..].to_vec();
        // Replace existing uses before adding the slow edge, whose argument
        // must remain the original call result rather than the join parameter.
        f.replace_values(&std::collections::HashMap::from([(old.results[0], joined)]));
        for entry in &mut f.osr_entries {
            for (value, _) in &mut entry.checks {
                if *value == old.results[0] {
                    *value = joined;
                }
            }
        }
        let (_, probe) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::TryIntegerShr,
                args: vec![old.args[0]],
                results: vec![],
                aux: AuxData::FixnumImm(amount as i64),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: old.source_pos,
            },
            &[
                (
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    Repr::Tagged,
                ),
                (IRType::of(TypeBits::FIXNUM), Repr::Tagged),
            ],
        );
        f.set_terminator(
            block,
            InstData {
                opcode: Opcode::Brif,
                args: vec![probe[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    terminator: true,
                    ..InstFlags::default()
                },
                targets: vec![
                    BlockCall {
                        block: fast,
                        args: vec![],
                    },
                    BlockCall {
                        block: slow,
                        args: vec![],
                    },
                ],
                frame_state: None,
                source_pos: old.source_pos,
            },
        );
        // ASH returns one value even when an incoming tuple was active. The
        // retained call performs this reset on the slow path; mirror it here.
        f.push_inst(
            fast,
            InstData {
                opcode: Opcode::ClearMv,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    call: true,
                    safepoint: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: old.source_pos,
            },
            &[],
        );
        f.block_mut(slow).insts.push(site);
        for (from, value) in [(fast, probe[1]), (slow, old.results[0])] {
            f.set_terminator(
                from,
                InstData {
                    opcode: Opcode::Jump,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags {
                        terminator: true,
                        ..InstFlags::default()
                    },
                    targets: vec![BlockCall {
                        block: join,
                        args: vec![value],
                    }],
                    frame_state: None,
                    source_pos: old.source_pos,
                },
            );
        }
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};

    fn fixture() -> Function {
        let mut f = Function::new("canonical-ash-fallback");
        let block = f.entry();
        let input = f.add_block_param(block, IRType::TOP, Repr::Tagged);
        let (_, amount) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(-8),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), Repr::Tagged)],
        );
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: input,
                    repr: Repr::Tagged,
                }],
                stack: vec![ValueSource::Value {
                    value: input,
                    repr: Repr::Tagged,
                }],
            }],
            remat: vec![],
        });
        let (_, result) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::Call,
                args: vec![input, amount[0]],
                results: vec![],
                aux: AuxData::CallTarget(egcl_rt::symbols::intern("ASH")),
                flags: InstFlags {
                    call: true,
                    safepoint: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: Some(state),
                source_pos: 0,
            },
            &[(IRType::TOP, Repr::Tagged)],
        );
        f.set_terminator(
            block,
            InstData {
                opcode: Opcode::Return,
                args: result,
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
        f
    }
    #[test]
    fn shift_probe_keeps_slow_call_state_and_fast_multiple_value_reset() {
        let mut f = fixture();
        assert!(crate::t2::verify::verify(&f).is_ok());
        let call = f
            .block(f.entry())
            .insts
            .iter()
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::Call)
            .unwrap();
        let old = f.inst(call).clone();
        assert_eq!(
            specialize(&mut f, &super::super::inlining::InlineOptions::default()),
            1
        );
        assert!(
            crate::t2::verify::verify(&f).is_ok(),
            "{:?}",
            crate::t2::verify::verify(&f)
        );
        assert_eq!(f.inst(call).frame_state, old.frame_state);
        assert_eq!(f.inst(call).args, old.args);
        assert_eq!(f.inst(call).flags, old.flags);
        let probe = f
            .block(f.entry())
            .insts
            .iter()
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::TryIntegerShr)
            .unwrap();
        let inferred = crate::t2::infer::infer(&f);
        assert_eq!(
            inferred.get(f.inst(probe).results[0]).unwrap().bits,
            TypeBits::SYMBOL.join(TypeBits::NULL)
        );
        assert_eq!(
            inferred.get(f.inst(probe).results[1]).unwrap().bits,
            TypeBits::FIXNUM
        );
        assert!(f.block_order().iter().any(|&b| {
            f.block(b)
                .insts
                .iter()
                .any(|&i| f.inst(i).opcode == Opcode::ClearMv)
        }));
        assert_eq!(
            specialize(&mut f, &super::super::inlining::InlineOptions::default()),
            0,
            "keep the slow call generic across repeated passes"
        );
    }
    #[test]
    fn unrelated_ash_names_and_nonconstant_counts_do_not_acquire_probes() {
        let mut f = fixture();
        let block = f.entry();
        let call = f
            .block(block)
            .insts
            .iter()
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::Call)
            .unwrap();
        f.inst_mut(call).aux = AuxData::CallTarget(egcl_rt::symbols::intern("OTHER::ASH"));
        assert_eq!(
            specialize(&mut f, &super::super::inlining::InlineOptions::default()),
            0
        );
        let mut f = fixture();
        let block = f.entry();
        let input = f.block(block).params[0];
        let call = f
            .block(block)
            .insts
            .iter()
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::Call)
            .unwrap();
        f.inst_mut(call).args[1] = input;
        assert_eq!(
            specialize(&mut f, &super::super::inlining::InlineOptions::default()),
            0
        );
    }
    #[test]
    fn explicit_notinline_keeps_the_entire_generic_call() {
        let mut f = fixture();
        let options = super::super::inlining::InlineOptions::default()
            .with_policy(0, super::super::inlining::InlinePolicy::NotInline);
        assert_eq!(specialize(&mut f, &options), 0);
    }
}
