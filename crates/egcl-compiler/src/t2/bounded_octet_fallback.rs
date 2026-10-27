// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Profitability proof only: retain the original scalar loop and its OSR entry.
use super::*;
use std::collections::HashSet;

fn standard(f: &Function, value: Value, name: &str) -> Option<Vec<Value>> {
    let ValueDef::Result { inst, .. } = f.value(alias(f, value)).def else {
        return None;
    };
    let d = f.inst(inst);
    let AuxData::CallTarget(symbol) = d.aux else {
        return None;
    };
    (matches!(d.opcode, Opcode::Call | Opcode::Invoke)
        && egcl_rt::symbols::registry_key(symbol).as_deref() == Some(name)
        && super::super::builtin_binding::snapshot(symbol)
            == Some(super::super::builtin_binding::Binding::FixedStandard))
    .then(|| d.args.clone())
}
fn operands(f: &Function, value: Value, opcode: Opcode, name: &str) -> Option<Vec<Value>> {
    let value = alias(f, value);
    if let ValueDef::Result { inst, .. } = f.value(value).def {
        let d = f.inst(inst);
        if d.opcode == opcode {
            return Some(d.args.clone());
        }
    }
    standard(f, value, name)
}

// A closed integer phi must have an actual constant/checked seed. No profile
// type or unguarded arbitrary argument supplies the mathematical integer proof.
fn integer(
    f: &Function,
    value: Value,
    seen: &mut HashSet<Value>,
    seed: &mut bool,
    depth: usize,
    visits_left: &mut usize,
) -> bool {
    // The active recursion set detects phi cycles, but a shared acyclic DAG
    // can still revisit both operands exponentially. Bound total work across
    // the entire proof, not just the depth of each path; exhaustion declines.
    if depth > 64 || *visits_left == 0 {
        return false;
    }
    *visits_left -= 1;
    let value = alias(f, value);
    if integer_constant(f, value).is_some()
        || (f.is_entry_param_checked(value) && f.value(value).ty.bits == TypeBits::FIXNUM)
    {
        *seed = true;
        return true;
    }
    if !seen.insert(value) {
        return true;
    }
    let result = match f.value(value).def {
        ValueDef::Param { block, num } => {
            let preds = f.preds(block);
            !preds.is_empty()
                && preds.iter().all(|&p| {
                    f.terminator(p).is_some_and(|t| {
                        let edges: Vec<_> = f
                            .inst(t)
                            .targets
                            .iter()
                            .filter(|e| e.block == block)
                            .collect();
                        !edges.is_empty()
                            && edges.iter().all(|e| {
                                e.args.get(num as usize).is_some_and(|&v| {
                                    integer(f, v, seen, seed, depth + 1, visits_left)
                                })
                            })
                    })
                })
        }
        ValueDef::Result { inst, .. } => {
            let d = f.inst(inst);
            if d.opcode == Opcode::Guard
                && matches!(d.aux, AuxData::TypeTag(t) if t.bits == TypeBits::FIXNUM)
                && d.flags.guard
                && d.frame_state.is_some()
            {
                *seed = true;
                true
            } else {
                let binary = operands(f, value, Opcode::FixnumAdd, "+")
                    .or_else(|| operands(f, value, Opcode::FixnumMul, "*"));
                if let Some(args) = binary {
                    args.len() == 2
                        && args
                            .iter()
                            .all(|&v| integer(f, v, seen, seed, depth + 1, visits_left))
                } else if let Some(args) = standard(f, value, "1+") {
                    args.len() == 1 && integer(f, args[0], seen, seed, depth + 1, visits_left)
                } else {
                    false
                }
            }
        }
    };
    seen.remove(&value);
    result
}

pub(super) fn bounded(f: &Function, header: Block, body: &[Block], latch: Block) -> bool {
    let outside: Vec<_> = f
        .preds(header)
        .into_iter()
        .filter(|b| !body.contains(b))
        .collect();
    let [entry_block] = outside.as_slice() else {
        return false;
    };
    let Some(entry_term) = f.terminator(*entry_block) else {
        return false;
    };
    let entry = f.inst(entry_term);
    if entry.opcode != Opcode::Jump || entry.targets.len() != 1 || entry.targets[0].block != header
    {
        return false;
    }
    let preds = f.preds(*entry_block);
    let [probe] = preds.as_slice() else {
        return false;
    };
    let Some(term) = f.terminator(*probe) else {
        return false;
    };
    let branch = f.inst(term);
    if branch.opcode != Opcode::Brif
        || branch.targets.len() != 2
        || branch.targets[1].block != *entry_block
        || branch.targets[0].block == *entry_block
        || branch.args.len() != 1
    {
        return false;
    }
    let ValueDef::Result {
        inst: kernel,
        num: 0,
    } = f.value(branch.args[0]).def
    else {
        return false;
    };
    if !matches!(
        f.inst(kernel).opcode,
        Opcode::TryStoreIntegerOctets8 | Opcode::TryStoreIntegerOctets1To8
    ) {
        return false;
    }
    let Some(tail) = f.terminator(latch) else {
        return false;
    };
    let Some(back) = f.inst(tail).targets.iter().find(|e| e.block == header) else {
        return false;
    };
    // A latch-dominating comparison alone could be bottom-tested (nine
    // body executions for start+8). Require the actual header pre-test, and
    // reject writes/opaque effects before it. Bound arithmetic and MV resets
    // are the only potentially effectful prefix operations admitted here.
    for &inst in &f.block(header).insts {
        let d = f.inst(inst);
        let allowed = matches!(
            d.opcode,
            Opcode::ConstFixnum
                | Opcode::ConstNil
                | Opcode::ConstT
                | Opcode::ClearMv
                | Opcode::Brif
        ) || (d.opcode == Opcode::Guard && matches!(d.aux, AuxData::TypeTag(_)))
            || d.results.first().is_some_and(|&v| {
                ["+", "MIN", ">=", "<"]
                    .iter()
                    .any(|name| standard(f, v, name).is_some())
            })
            || matches!(
                d.opcode,
                Opcode::FixnumAdd | Opcode::FixnumMin | Opcode::FixnumCmpGe | Opcode::FixnumCmpLt
            );
        if !allowed {
            return false;
        }
    }
    for (number, &index) in f.block(header).params.iter().enumerate() {
        let Some(&next) = back.args.get(number) else {
            continue;
        };
        let step = operands(f, next, Opcode::FixnumAdd, "+").is_some_and(|a| {
            a.len() == 2 && alias(f, a[0]) == index && integer_constant(f, a[1]) == Some(1)
        }) || standard(f, next, "1+")
            .is_some_and(|a| a.len() == 1 && alias(f, a[0]) == index);
        if !step {
            continue;
        }
        let Some(&start) = entry.targets[0].args.get(number) else {
            continue;
        };
        if f.inst(kernel).args.len() != 4 || alias(f, f.inst(kernel).args[1]) != alias(f, start) {
            continue;
        }
        let mut seed = false;
        if !integer(f, start, &mut HashSet::new(), &mut seed, 0, &mut 512) || !seed {
            continue;
        }
        // Only invariant header phis may be substituted with entry arguments.
        let resolve = |v: Value| {
            let v = alias(f, v);
            if let ValueDef::Param { block, num } = f.value(v).def {
                if block == header
                    && back
                        .args
                        .get(num as usize)
                        .is_some_and(|&x| alias(f, x) == v)
                {
                    return alias(f, entry.targets[0].args[num as usize]);
                }
            }
            v
        };
        let bound = |value: Value| {
            fn upper(
                f: &Function,
                v: Value,
                start: Value,
                resolve: &impl Fn(Value) -> Value,
                depth: usize,
            ) -> bool {
                if depth > 16 {
                    return false;
                }
                let v = resolve(v);
                if let Some(args) = operands(f, v, Opcode::FixnumMin, "MIN") {
                    // EGCL MIN preserves an actual argument, with exact finite
                    // comparisons (no integer->float coercion). Its unordered
                    // comparison is Equal: NaN cannot replace an integer cap;
                    // NaN retained as the first argument makes >= exit / < stop
                    // immediately. Thus either MIN order admits the cap. This
                    // relies on fixed-standard EGCL semantics, not a profile.
                    return !args.is_empty()
                        && args.iter().any(|&x| upper(f, x, start, resolve, depth + 1));
                }
                operands(f, v, Opcode::FixnumAdd, "+").is_some_and(|a| {
                    a.len() == 2
                        && [0, 1].into_iter().any(|i| {
                            resolve(a[i]) == resolve(start)
                                && integer_constant(f, a[1 - i])
                                    .is_some_and(|n| (0..=8).contains(&n))
                        })
                })
            }
            upper(f, value, start, &resolve, 0)
        };
        // Kernel and scalar ends may be distinct cloned SSA expressions;
        // independently prove both caps against the identical entry start.
        if !bound(f.inst(kernel).args[2]) {
            continue;
        }
        for block in [header] {
            let Some(term) = f.terminator(block) else {
                continue;
            };
            let d = f.inst(term);
            if d.opcode != Opcode::Brif || d.args.len() != 1 || d.targets.len() != 2 {
                continue;
            }
            let ge = operands(f, d.args[0], Opcode::FixnumCmpGe, ">=");
            let lt = operands(f, d.args[0], Opcode::FixnumCmpLt, "<");
            if ge.is_some_and(|a| {
                a.len() == 2
                    && alias(f, a[0]) == index
                    && bound(a[1])
                    && !body.contains(&d.targets[0].block)
                    && body.contains(&d.targets[1].block)
            }) || lt.is_some_and(|a| {
                a.len() == 2
                    && alias(f, a[0]) == index
                    && bound(a[1])
                    && body.contains(&d.targets[0].block)
                    && !body.contains(&d.targets[1].block)
            }) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::{OsrEntry, ValueRepresentation as Repr};
    fn plain(opcode: Opcode, args: Vec<Value>) -> InstData {
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
    fn constant(f: &mut Function, b: Block, n: i64) -> Value {
        let mut d = plain(Opcode::ConstFixnum, vec![]);
        d.aux = AuxData::FixnumImm(n);
        f.push_inst(b, d, &[(IRType::of(TypeBits::FIXNUM), Repr::Tagged)])
            .1[0]
    }
    fn call(
        f: &mut Function,
        b: Block,
        name: &str,
        args: Vec<Value>,
        fs: super::super::super::frame_state::FrameStateId,
    ) -> Value {
        let mut d = plain(Opcode::Call, args);
        d.aux = AuxData::CallTarget(egcl_rt::symbols::intern(name));
        d.frame_state = Some(fs);
        d.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..Default::default()
        };
        f.push_inst(b, d, &[(IRType::TOP, Repr::Tagged)]).1[0]
    }
    fn fixture(span: i64, kernel_false: bool) -> Function {
        fixture_start(span, kernel_false, Some(3))
    }
    fn fixture_start(span: i64, kernel_false: bool, start_value: Option<i64>) -> Function {
        let mut f = Function::new("bounded-octet-fallback");
        let probe = f.entry();
        let array = f.add_block_param(probe, IRType::TOP, Repr::Tagged);
        let input = f.add_block_param(probe, IRType::TOP, Repr::Tagged);
        let limit = f.add_block_param(probe, IRType::TOP, Repr::Tagged);
        let start = start_value.map_or(input, |n| constant(&mut f, probe, n));
        let eight = constant(&mut f, probe, span);
        let one = constant(&mut f, probe, 1);
        let minus8 = constant(&mut f, probe, -8);
        let entry = f.make_block();
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();
        let index = f.add_block_param(header, IRType::TOP, Repr::Tagged);
        let lane = f.add_block_param(header, IRType::TOP, Repr::Tagged);
        let fs = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 7,
                locals: vec![],
                stack: vec![],
            }],
            remat: vec![],
        });
        let sum = call(&mut f, probe, "+", vec![start, eight], fs);
        let end = call(&mut f, probe, "MIN", vec![sum, limit], fs);
        let mut d = plain(
            Opcode::TryStoreIntegerOctets8,
            vec![array, start, end, input],
        );
        d.flags.effectful = true;
        let outputs = f
            .push_inst(
                probe,
                d,
                &[
                    (
                        IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                        Repr::Tagged,
                    ),
                    (IRType::of(TypeBits::FIXNUM), Repr::Tagged),
                ],
            )
            .1;
        let mut branch = plain(Opcode::Brif, vec![outputs[0]]);
        branch.targets = if kernel_false {
            vec![
                BlockCall {
                    block: exit,
                    args: vec![],
                },
                BlockCall {
                    block: entry,
                    args: vec![],
                },
            ]
        } else {
            vec![
                BlockCall {
                    block: entry,
                    args: vec![],
                },
                BlockCall {
                    block: exit,
                    args: vec![],
                },
            ]
        };
        f.set_terminator(probe, branch);
        jump(&mut f, entry, header, vec![start, input]);
        let compare = call(&mut f, header, ">=", vec![index, end], fs);
        let mut branch = plain(Opcode::Brif, vec![compare]);
        branch.targets = vec![
            BlockCall {
                block: exit,
                args: vec![],
            },
            BlockCall {
                block: body,
                args: vec![],
            },
        ];
        f.set_terminator(header, branch);
        let shifted = call(&mut f, body, "ASH", vec![lane, minus8], fs);
        let next = call(&mut f, body, "+", vec![index, one], fs);
        jump(&mut f, body, header, vec![next, shifted]);
        f.set_terminator(exit, plain(Opcode::Return, vec![input]));
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 4,
                locals: vec![
                    ValueSource::Value {
                        value: index,
                        repr: Repr::Tagged,
                    },
                    ValueSource::Value {
                        value: lane,
                        repr: Repr::Tagged,
                    },
                ],
                stack: vec![],
            }],
            remat: vec![],
        });
        f.osr_entries.push(OsrEntry {
            block: header,
            bcp: 4,
            frame_state: state,
            checks: vec![],
            control_scopes: vec![],
        });
        super::super::super::verify::verify(&f).unwrap();
        f
    }
    #[test]
    fn bottom_test_and_unrelated_kernel_start_do_not_prove_bounded_fallback() {
        let mut f = fixture(8, true);
        let header = f.osr_entries[0].block;
        let lp = super::super::super::opt_licm::detect_loops(&f, &f.dominators())
            .into_iter()
            .find(|lp| lp.header == header)
            .unwrap();
        let latch = *lp
            .body
            .iter()
            .find(|&&b| f.succs(b).contains(&header))
            .unwrap();
        let kernel = f
            .block(f.entry())
            .insts
            .iter()
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::TryStoreIntegerOctets8)
            .unwrap();
        let original = f.inst(kernel).args[1];
        let unrelated = f.inst(kernel).args[3];
        f.inst_mut(kernel).args[1] = unrelated;
        assert!(!bounded(&f, header, &lp.body, latch));
        f.inst_mut(kernel).args[1] = original;
        let header_term = f.block_mut(header).insts.pop().unwrap();
        let tail = f.block_mut(latch).insts.pop().unwrap();
        let back = f.inst(tail).targets[0].clone();
        let mut branch = f.inst(header_term).clone();
        branch.targets[1] = back;
        f.set_terminator(latch, branch);
        jump(&mut f, header, latch, vec![]);
        super::super::super::verify::verify(&f).unwrap();
        assert!(
            !bounded(&f, header, &lp.body, latch),
            "a bottom test can execute nine bodies"
        );
    }
    #[test]
    fn repeated_ssa_dag_exhausts_one_shared_proof_budget() {
        let mut f = Function::new("shared-integer-dag");
        let entry = f.entry();
        let mut value = constant(&mut f, entry, 1);
        let fs = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        for _ in 0..4 {
            value = call(&mut f, entry, "+", vec![value, value], fs);
        }
        let mut visits_left = 512;
        let mut seed = false;
        assert!(
            integer(
                &f,
                value,
                &mut HashSet::new(),
                &mut seed,
                0,
                &mut visits_left
            ) && seed
        );
        assert_eq!(visits_left, 512 - 31);
        // Only 17 SSA operations, but tree traversal would make 262143 visits.
        // The depth bound alone would permit that work (and much larger DAGs).
        for _ in 4..17 {
            value = call(&mut f, entry, "*", vec![value, value], fs);
        }
        let mut visits_left = 512;
        let mut seed = false;
        assert!(!integer(
            &f,
            value,
            &mut HashSet::new(),
            &mut seed,
            0,
            &mut visits_left
        ));
        assert_eq!(visits_left, 0, "work is bounded over the full proof");
    }
    #[test]
    fn nested_integer_phi_arithmetic_proves_dynamic_octet_start() {
        let mut f = Function::new("dynamic-octet-start");
        let entry = f.entry();
        let condition = f.add_block_param(
            entry,
            IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
            Repr::Tagged,
        );
        let unknown = f.add_block_param(entry, IRType::TOP, Repr::Tagged);
        let zero = constant(&mut f, entry, 0);
        let five = constant(&mut f, entry, 5);
        let eight = constant(&mut f, entry, 8);
        let outer = f.make_block();
        let inner = f.make_block();
        let inner_back = f.make_block();
        let outer_back = f.make_block();
        let i = f.add_block_param(outer, IRType::TOP, Repr::Tagged);
        let j = f.add_block_param(inner, IRType::TOP, Repr::Tagged);
        jump(&mut f, entry, outer, vec![zero]);
        jump(&mut f, outer, inner, vec![zero]);
        let fs = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let scaled = call(&mut f, inner, "*", vec![j, five], fs);
        let joined = call(&mut f, inner, "+", vec![scaled, i], fs);
        let start = call(&mut f, inner, "*", vec![joined, eight], fs);
        let mut branch = plain(Opcode::Brif, vec![condition]);
        branch.targets = vec![
            BlockCall {
                block: inner_back,
                args: vec![],
            },
            BlockCall {
                block: outer_back,
                args: vec![],
            },
        ];
        f.set_terminator(inner, branch);
        let next_j = call(&mut f, inner_back, "1+", vec![j], fs);
        jump(&mut f, inner_back, inner, vec![next_j]);
        let next_i = call(&mut f, outer_back, "1+", vec![i], fs);
        jump(&mut f, outer_back, outer, vec![next_i]);
        let mut seed = false;
        assert!(integer(&f, start, &mut HashSet::new(), &mut seed, 0, &mut 512) && seed);
        let term = f.terminator(entry).unwrap();
        f.inst_mut(term).targets[0].args[0] = unknown;
        let mut seed = false;
        assert!(
            !integer(&f, start, &mut HashSet::new(), &mut seed, 0, &mut 512),
            "an unknown seed can be a nonprogressing float"
        );
    }
    #[test]
    fn bounded_fused_false_continuation_preserves_original_osr_without_peeling() {
        let mut f = fixture(8, true);
        let old = f.num_insts();
        let osr = f.osr_entries[0].clone();
        let stats = peel_for_numeric_types(&mut f, &|_| true, PeelOptions::default());
        assert_eq!(stats.loops, 0, "{stats:?}");
        assert_eq!(f.num_insts(), old);
        assert_eq!(f.osr_entries[0].block, osr.block);
        assert_eq!(f.osr_entries[0].frame_state, osr.frame_state);
        super::super::super::verify::verify(&f).unwrap();
    }
    #[test]
    fn negative_integer_start_and_either_min_order_are_bounded() {
        for reverse in [false, true] {
            let mut f = fixture_start(8, true, Some(-3));
            if reverse {
                let target = egcl_rt::symbols::find_index("MIN").unwrap();
                let min = f
                    .block(f.entry())
                    .insts
                    .iter()
                    .copied()
                    .find(|&i| matches!(f.inst(i).aux, AuxData::CallTarget(s) if s == target))
                    .unwrap();
                f.inst_mut(min).args.swap(0, 1);
            }
            assert_eq!(
                peel_for_numeric_types(&mut f, &|_| true, PeelOptions::default()).loops,
                0
            );
        }
    }
    #[test]
    fn unproved_start_may_be_noninteger_and_must_still_peel() {
        let mut f = fixture_start(8, true, None);
        assert_eq!(
            peel_for_numeric_types(&mut f, &|_| true, PeelOptions::default()).loops,
            1
        );
    }
    #[test]
    fn long_or_nonfallback_loops_still_peel() {
        for (span, false_edge) in [(64, true), (8, false)] {
            let mut f = fixture(span, false_edge);
            let stats = peel_for_numeric_types(&mut f, &|_| true, PeelOptions::default());
            assert_eq!(stats.loops, 1, "span{span} false{false_edge}: {stats:?}");
            super::super::super::verify::verify(&f).unwrap();
        }
    }
}
