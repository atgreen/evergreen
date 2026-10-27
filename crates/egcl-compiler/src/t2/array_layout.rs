// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Runtime layout witnesses, never constructor/profile return-type guesses.
//! A witness contains tagged header width (8/16) or NIL, not an interior pointer.
use super::ir::{
    AuxData, Block, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value, ValueDef,
    ValueRepresentation,
};
use std::collections::{HashMap, HashSet};

const BUDGET: usize = 512;

fn owner(f: &Function, inst: Inst) -> Option<(Block, usize)> {
    f.block_order().iter().find_map(|&block| {
        f.block(block)
            .insts
            .iter()
            .position(|&candidate| candidate == inst)
            .map(|position| (block, position))
    })
}

/// Resolve closed forwarding graphs without accepting a seedless phi cycle.
/// Every incoming edge participates, including repeated edges from one block.
fn origin(f: &Function, value: Value) -> Option<Value> {
    let mut todo = vec![value];
    let mut graph = HashMap::<Value, Vec<Value>>::new();
    let mut seeds = HashSet::new();
    while let Some(value) = todo.pop() {
        if graph.contains_key(&value) {
            continue;
        }
        if graph.len() == BUDGET || f.value(value).repr != ValueRepresentation::Tagged {
            return None;
        }
        let inputs = match f.value(value).def {
            ValueDef::Result { inst, num: 0 }
                if f.inst(inst).opcode == Opcode::Guard
                    && f.inst(inst).args.len() == 1
                    && f.inst(inst).results.len() == 1 =>
            {
                f.inst(inst).args.clone()
            }
            ValueDef::Param { block, num } if block != f.entry() => {
                let mut inputs = Vec::new();
                for &predecessor in f.block_order() {
                    let Some(terminator) = f.terminator(predecessor) else {
                        continue;
                    };
                    for edge in &f.inst(terminator).targets {
                        if edge.block == block {
                            inputs.push(*edge.args.get(num as usize)?);
                        }
                    }
                }
                if inputs.is_empty() {
                    return None;
                }
                inputs
            }
            _ => {
                seeds.insert(value);
                Vec::new()
            }
        };
        todo.extend(inputs.iter().copied());
        graph.insert(value, inputs);
    }
    if seeds.len() != 1 {
        return None;
    }
    let seed = *seeds.iter().next()?;
    let mut reaches = seeds;
    for _ in 0..graph.len() {
        let before = reaches.len();
        for (&node, inputs) in &graph {
            if inputs.iter().any(|input| reaches.contains(input)) {
                reaches.insert(node);
            }
        }
        if reaches.len() == before {
            break;
        }
    }
    (reaches.len() == graph.len()).then_some(seed)
}

fn probe_location(f: &Function, seed: Value) -> Option<(Block, Option<Inst>, Value)> {
    match f.value(seed).def {
        ValueDef::Param { block, .. } if block == f.entry() => Some((block, None, seed)),
        ValueDef::Result { inst, .. } => {
            let (block, _) = owner(f, inst)?;
            let data = f.inst(inst);
            if !data.flags.terminator {
                return Some((block, Some(inst), seed));
            }
            if data.opcode != Opcode::Invoke {
                return None;
            }
            let normal = data.targets.first()?;
            let index = normal.args.iter().position(|&argument| argument == seed)?;
            let parameter = *f.block(normal.block).params.get(index)?;
            (origin(f, parameter) == Some(seed)).then_some((normal.block, None, parameter))
        }
        _ => None,
    }
}

// OSR imports can break equality between distinct SSA aliases. Until those
// equalities are guarded, reject every OSR path reaching the consumer, even
// one that would execute the probe. Ordinary compilation clears these records.
fn osr_reaches(f: &Function, target: Block) -> bool {
    let mut todo: Vec<_> = f.osr_entries.iter().map(|entry| entry.block).collect();
    let mut seen = HashSet::new();
    while let Some(block) = todo.pop() {
        if !seen.insert(block) {
            continue;
        }
        if block == target {
            return true;
        }
        if let Some(terminator) = f.terminator(block) {
            todo.extend(f.inst(terminator).targets.iter().map(|edge| edge.block));
        }
    }
    false
}

fn dominates(f: &Function, probe: Inst, consumer: Inst) -> bool {
    let Some((before, position)) = owner(f, probe) else {
        return false;
    };
    let Some((after, use_position)) = owner(f, consumer) else {
        return false;
    };
    (if before == after {
        position < use_position
    } else {
        f.dominators().dominates(before, after)
    }) && !osr_reaches(f, after)
}

/// Hoist only witnesses used by repeated kernels. Keep existing effects,
/// recovery states, argument identities and checked scalar fallback unchanged.
pub fn hoist_layouts(f: &mut Function) -> usize {
    let dom = f.dominators();
    let loops = super::opt_licm::detect_loops(f, &dom);
    let sites: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&block| {
            f.block(block)
                .insts
                .iter()
                .copied()
                .map(move |inst| (block, inst))
        })
        .filter(|&(_, inst)| f.inst(inst).opcode == Opcode::TryStoreIntegerOctets1To8)
        .collect();
    let mut witnesses = HashMap::<Value, Value>::new();
    let mut changed = 0;
    for (block, site) in sites {
        if f.inst(site).args.len() != 4 {
            continue;
        }
        let array = f.inst(site).args[0];
        let Some(seed) = origin(f, array) else {
            continue;
        };
        let Some((probe_block, after, argument)) = probe_location(f, seed) else {
            continue;
        };
        if !loops
            .iter()
            .any(|lp| lp.body.contains(&block) && !lp.body.contains(&probe_block))
            || !dom.dominates(probe_block, block)
            || osr_reaches(f, block)
        {
            continue;
        }
        let witness = if let Some(&witness) = witnesses.get(&seed) {
            witness
        } else {
            let (_, results) = f.push_inst(
                probe_block,
                InstData {
                    opcode: Opcode::PackedU8Layout,
                    args: vec![argument],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags::default(),
                    frame_state: None,
                    targets: vec![],
                    source_pos: f.inst(site).source_pos,
                },
                &[(
                    IRType::of(TypeBits::FIXNUM.join(TypeBits::NULL)),
                    ValueRepresentation::Tagged,
                )],
            );
            let probe = f.block_mut(probe_block).insts.pop().unwrap();
            let position = after
                .map(|inst| {
                    f.block(probe_block)
                        .insts
                        .iter()
                        .position(|&i| i == inst)
                        .unwrap()
                        + 1
                })
                .unwrap_or_else(|| {
                    usize::from(
                        f.block(probe_block)
                            .insts
                            .first()
                            .is_some_and(|&i| f.inst(i).opcode == Opcode::NativePoll),
                    )
                });
            f.block_mut(probe_block).insts.insert(position, probe);
            witnesses.insert(seed, results[0]);
            results[0]
        };
        f.inst_mut(site).opcode = Opcode::TryStoreIntegerOctets1To8Layout;
        f.inst_mut(site).args.push(witness);
        changed += 1;
    }
    changed
}

/// Called after structural SSA verification. Re-establish every proof from IR;
/// a forged Boolean, profile or type annotation is never a layout witness.
pub fn verify_accesses(f: &Function) -> Result<(), String> {
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            if data.opcode == Opcode::PackedU8Layout
                && (data.args.len() != 1
                    || data.results.len() != 1
                    || data.flags != InstFlags::default()
                    || !matches!(data.aux, AuxData::None)
                    || data.frame_state.is_some()
                    || !data.targets.is_empty()
                    || data
                        .args
                        .iter()
                        .chain(&data.results)
                        .any(|&v| f.value(v).repr != ValueRepresentation::Tagged))
            {
                return Err("malformed packed-U8 layout producer".into());
            }
            if data.opcode != Opcode::TryStoreIntegerOctets1To8Layout {
                continue;
            }
            if data.args.len() != 5
                || data.results.len() != 2
                || !matches!(data.aux, AuxData::None)
                || data.flags
                    != (InstFlags {
                        effectful: true,
                        ..Default::default()
                    })
                || data.frame_state.is_some()
                || !data.targets.is_empty()
                || data
                    .args
                    .iter()
                    .chain(&data.results)
                    .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
            {
                return Err("malformed layout-backed octet kernel".into());
            }
            let ValueDef::Result {
                inst: probe,
                num: 0,
            } = f.value(data.args[4]).def
            else {
                return Err("layout witness is not a producer result".into());
            };
            let producer = f.inst(probe);
            if producer.opcode != Opcode::PackedU8Layout
                || producer.args.len() != 1
                || producer.results.first() != Some(&data.args[4])
                || !dominates(f, probe, inst)
            {
                return Err("layout producer does not dominate every entry".into());
            }
            let array = origin(f, data.args[0]).ok_or("ambiguous kernel array origin")?;
            if origin(f, producer.args[0]) != Some(array) {
                return Err("layout witness names a different array".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::frame_state::FrameState;
    use super::super::ir::{BlockCall, OsrEntry};
    use super::*;

    fn data(opcode: Opcode, args: Vec<Value>) -> InstData {
        InstData {
            opcode,
            args,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            frame_state: None,
            targets: vec![],
            source_pos: 0,
        }
    }
    fn edge(
        f: &mut Function,
        block: Block,
        opcode: Opcode,
        args: Vec<Value>,
        targets: Vec<BlockCall>,
    ) {
        let mut d = data(opcode, args);
        d.flags.terminator = true;
        d.targets = targets;
        f.set_terminator(block, d);
    }
    fn fixture() -> (Function, Inst, Block, Value, Value) {
        let mut f = Function::new("layout-proof");
        let entry = f.entry();
        let array = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let other = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let condition = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let header = f.make_block();
        let exit = f.make_block();
        let phi = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
        edge(
            &mut f,
            entry,
            Opcode::Jump,
            vec![],
            vec![BlockCall {
                block: header,
                args: vec![array],
            }],
        );
        let mut kernel = data(
            Opcode::TryStoreIntegerOctets1To8,
            vec![phi, condition, condition, condition],
        );
        kernel.flags.effectful = true;
        let (site, results) = f.push_inst(
            header,
            kernel,
            &[
                (IRType::TOP, ValueRepresentation::Tagged),
                (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged),
            ],
        );
        edge(
            &mut f,
            header,
            Opcode::Brif,
            vec![condition],
            vec![
                BlockCall {
                    block: header,
                    args: vec![phi],
                },
                BlockCall {
                    block: exit,
                    args: vec![],
                },
            ],
        );
        edge(&mut f, exit, Opcode::Return, vec![results[0]], vec![]);
        (f, site, header, array, other)
    }
    #[test]
    fn closed_phi_with_actual_seed_hoists_once_and_verifies() {
        let (mut f, site, _, array, _) = fixture();
        assert_eq!(hoist_layouts(&mut f), 1);
        assert_eq!(f.inst(site).opcode, Opcode::TryStoreIntegerOctets1To8Layout);
        let witness = f.inst(site).args[4];
        let ValueDef::Result { inst, .. } = f.value(witness).def else {
            panic!()
        };
        assert_eq!(f.inst(inst).args, [array]);
        assert!(verify_accesses(&f).is_ok());
        assert_eq!(hoist_layouts(&mut f), 0);
    }
    #[test]
    fn different_array_or_forged_witness_is_rejected() {
        let (mut f, site, _, _, other) = fixture();
        assert_eq!(hoist_layouts(&mut f), 1);
        let witness = f.inst(site).args[4];
        let ValueDef::Result { inst, .. } = f.value(witness).def else {
            panic!()
        };
        f.inst_mut(inst).args[0] = other;
        assert!(verify_accesses(&f).is_err());
        f.inst_mut(site).args[4] = other;
        assert!(verify_accesses(&f).is_err());
    }
    #[test]
    fn mixed_phi_and_seedless_cycle_do_not_prove_identity() {
        let (mut f, _, header, _, other) = fixture();
        let term = f.terminator(header).unwrap();
        f.inst_mut(term).targets[0].args[0] = other;
        assert_eq!(hoist_layouts(&mut f), 0);
        let phi = f.block(header).params[0];
        let entry = f.entry();
        let term = f.terminator(entry).unwrap();
        f.inst_mut(term).targets[0].args[0] = phi;
        f.inst_mut(f.terminator(header).unwrap()).targets[0].args[0] = phi;
        assert_eq!(origin(&f, phi), None);
    }
    #[test]
    fn osr_bypass_rejects_hoist_and_existing_consumer() {
        let (mut f, _, header, _, _) = fixture();
        let state = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let osr = OsrEntry {
            block: header,
            bcp: 0,
            frame_state: state,
            control_scopes: vec![],
            checks: vec![],
        };
        f.osr_entries.push(osr.clone());
        assert_eq!(hoist_layouts(&mut f), 0);
        f.osr_entries.clear();
        assert_eq!(hoist_layouts(&mut f), 1);
        f.osr_entries.push(osr);
        assert!(verify_accesses(&f).is_err());
    }
    #[test]
    fn one_shot_kernel_is_unchanged() {
        let (mut f, _, header, _, _) = fixture();
        let term = f.terminator(header).unwrap();
        let exit = f.inst(term).targets[1].block;
        f.inst_mut(term).opcode = Opcode::Jump;
        f.inst_mut(term).args.clear();
        f.inst_mut(term).targets = vec![BlockCall {
            block: exit,
            args: vec![],
        }];
        assert_eq!(hoist_layouts(&mut f), 0);
    }
    #[test]
    fn identity_guard_forwarding_preserves_same_seed() {
        let (mut f, site, header, array, _) = fixture();
        let phi = f.block(header).params[0];
        let mut guard = data(Opcode::Guard, vec![phi]);
        guard.flags.guard = true;
        let (inst, result) =
            f.push_inst(header, guard, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        assert_eq!(f.block_mut(header).insts.pop(), Some(inst));
        f.block_mut(header).insts.insert(0, inst);
        f.inst_mut(site).args[0] = result[0];
        assert_eq!(origin(&f, result[0]), Some(array));
        assert_eq!(hoist_layouts(&mut f), 1);
        assert!(verify_accesses(&f).is_ok());
    }
    #[test]
    fn invoke_result_probe_uses_only_normal_edge_projection() {
        let mut f = Function::new("invoke-layout");
        let entry = f.entry();
        let argument = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let normal = f.make_block();
        let exceptional = f.make_block();
        let projected = f.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let mut call = data(Opcode::Invoke, vec![argument]);
        call.flags.call = true;
        call.flags.effectful = true;
        call.flags.safepoint = true;
        let (site, result) = f.set_terminator_with_results(
            entry,
            call,
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.inst_mut(site).targets = vec![
            BlockCall {
                block: normal,
                args: vec![result[0]],
            },
            BlockCall {
                block: exceptional,
                args: vec![],
            },
        ];
        edge(&mut f, normal, Opcode::Return, vec![projected], vec![]);
        edge(&mut f, exceptional, Opcode::Return, vec![argument], vec![]);
        assert_eq!(origin(&f, projected), Some(result[0]));
        assert_eq!(
            probe_location(&f, result[0]),
            Some((normal, None, projected))
        );
    }
    #[test]
    fn late_probe_and_malformed_producer_are_rejected() {
        let (mut f, site, header, _, _) = fixture();
        assert_eq!(hoist_layouts(&mut f), 1);
        let witness = f.inst(site).args[4];
        let ValueDef::Result { inst: probe, .. } = f.value(witness).def else {
            panic!()
        };
        f.inst_mut(probe).flags.effectful = true;
        assert!(verify_accesses(&f).is_err());
        f.inst_mut(probe).flags = InstFlags::default();
        let entry = f.entry();
        f.block_mut(entry).insts.retain(|&inst| inst != probe);
        let position = f
            .block(header)
            .insts
            .iter()
            .position(|&inst| inst == site)
            .unwrap();
        f.block_mut(header).insts.insert(position + 1, probe);
        assert!(verify_accesses(&f).is_err());
    }
}
