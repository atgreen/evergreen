// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Guarded rank-one array accesses. Unsupported layouts retain the original
//! call through precise deoptimization; no interior pointer crosses a safepoint.
use super::ir::{
    AuxData, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value, ValueDef,
    ValueRepresentation as Repr,
};

fn insert(
    f: &mut Function,
    before: Inst,
    data: InstData,
    result: Option<(IRType, Repr)>,
) -> Option<Value> {
    let block = f
        .block_order()
        .iter()
        .copied()
        .find(|&block| f.block(block).insts.contains(&before))
        .unwrap();
    let types: Vec<_> = result.into_iter().collect();
    let (inst, values) = f.push_inst(block, data, &types);
    let body = &mut f.block_mut(block).insts;
    assert_eq!(body.pop(), Some(inst));
    let pos = body.iter().position(|&inst| inst == before).unwrap();
    body.insert(pos, inst);
    values.first().copied()
}

fn clear_mv(f: &mut Function, site: Inst, old: &InstData) {
    let block = f
        .block_order()
        .iter()
        .copied()
        .find(|&block| f.block(block).insts.contains(&site))
        .unwrap();
    let state = f
        .frame_states
        .add(f.frame_states.get(old.frame_state.unwrap()).clone());
    let (clear, _) = f.push_inst(
        block,
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
            frame_state: Some(state),
            source_pos: old.source_pos,
        },
        &[],
    );
    let body = &mut f.block_mut(block).insts;
    assert_eq!(body.pop(), Some(clear));
    let pos = body.iter().position(|&inst| inst == site).unwrap();
    body.insert(pos + 1, clear);
}

/// Run after numeric speculation so double-consuming reads are visible as
/// UnboxFloat uses. The tagged artifact accepts general vectors and packed U8 arrays;
/// the raw candidate additionally loads packed or boxed doubles directly.
pub fn specialize(f: &mut Function, raw: bool, options: &super::inlining::InlineOptions) -> usize {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
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
            || old.results.len() != 1
            || old.frame_state.is_none()
            || !old.targets.is_empty()
            || egcl_rt::symbols::is_uninterned(symbol)
        {
            continue;
        }
        let scope = f.frame_states.get(old.frame_state.unwrap()).scopes.last();
        if scope.is_some_and(|scope| {
            options.policy_at(scope.bcp) == super::inlining::InlinePolicy::NotInline
        }) {
            continue;
        }
        let Some(key) = egcl_rt::symbols::registry_key(symbol) else {
            continue;
        };
        if egcl_rt::symbols::find_index(&key) != Some(symbol) {
            continue;
        }
        let load = matches!(key.as_str(), "AREF" | "SVREF");
        let simple_store = key == "EGCL::SET-SVREF";
        let store = key == "EGCL::SET-AREF" || simple_store;
        if (!load && !store) || old.args.len() != if load { 2 } else { 3 } {
            continue;
        }
        let Some(binding) = super::builtin_binding::snapshot(symbol) else {
            continue;
        };
        let block = f
            .block_order()
            .iter()
            .copied()
            .find(|&b| f.block(b).insts.contains(&site))
            .unwrap();
        let pos = f
            .block(block)
            .insts
            .iter()
            .position(|&i| i == site)
            .unwrap();
        super::builtin_binding::insert(f, block, pos, old.frame_state.unwrap(), binding);
        let result = old.results[0];
        let discarded = load && !f.block_order().iter().any(|&b| f.block(b).insts.iter().any(|&i| {
            let d = f.inst(i);
            d.args.iter().chain(d.targets.iter().flat_map(|edge| &edge.args)).any(|&v| v == result)
        })) && !f.frame_states.iter().any(|(_, state)| state.scopes.iter()
            .flat_map(|s| s.locals.iter().chain(&s.stack))
            .chain(state.remat.iter().flat_map(|r| &r.inputs))
            .any(|source| matches!(source, super::frame_state::ValueSource::Value { value, .. } if *value == result)));
        let double_read = raw
            && load
            && ((0..f.num_insts()).any(|i| {
                let use_data = f.inst(Inst(i as u32));
                use_data.opcode == Opcode::UnboxFloat
                    && use_data.args.first() == Some(&result)
                    && use_data
                        .results
                        .first()
                        .is_some_and(|&value| f.value(value).repr == Repr::UnboxedF64)
            }) || f.value(result).ty.bits == TypeBits::DOUBLE_FLOAT);
        if discarded {
            let data = f.inst_mut(site);
            data.opcode = Opcode::ArrayReadDiscarded;
            data.aux = if key == "AREF" { AuxData::ArrayRef } else { AuxData::None };
            data.results.clear();
            data.flags = InstFlags {
                effectful: true,
                guard: true,
                ..InstFlags::default()
            };
        } else if double_read {
            let input = insert(
                f,
                site,
                InstData {
                    opcode: if key == "SVREF" {
                        Opcode::VecF64Ref
                    } else {
                        Opcode::PackedF64Ref
                    },
                    args: old.args.clone(),
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags {
                        guard: true,
                        effectful: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: old.frame_state,
                    source_pos: old.source_pos,
                },
                Some((IRType::of(TypeBits::DOUBLE_FLOAT), Repr::UnboxedF64)),
            )
            .unwrap();
            let data = f.inst_mut(site);
            data.opcode = Opcode::BoxFloat;
            data.args = vec![input];
            data.aux = AuxData::None;
            data.flags = InstFlags {
                effectful: true,
                ..InstFlags::default()
            };
            data.frame_state = None;
            f.refine_type(result, IRType::of(TypeBits::DOUBLE_FLOAT));
        } else if raw && load && key == "AREF" {
            clear_mv(f, site, &old);
            specialize_boxed_read(f, site, &old);
            count += 1;
            continue;
        } else {
            let data = f.inst_mut(site);
            data.opcode = if load { Opcode::VecRef } else { Opcode::VecSet };
            data.aux = if load && key == "AREF" { AuxData::ArrayRef }
                else if simple_store { AuxData::SimpleVectorStore } else { AuxData::None };
            data.flags = InstFlags {
                effectful: true,
                guard: true,
                ..InstFlags::default()
            };
        }
        clear_mv(f, site, &old);
        count += 1;
    }
    if raw {
        count += specialize_raw_stores(f);
    }
    count
}

/// Delay a store-only escape box until the destination actually needs tagged
/// elements. The packed arm consumes raw bits and does not allocate at all.
fn specialize_raw_stores(f: &mut Function) -> usize {
    use super::frame_state::ValueSource;
    use super::ir::BlockCall;
    use std::collections::HashSet;
    let stores: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.clone())
        .filter(|&i| f.inst(i).opcode == Opcode::VecSet
            && matches!(f.inst(i).aux, AuxData::None))
        .collect();
    let mut count = 0;
    for store in stores {
        let data = f.inst(store).clone();
        let mut value = data.args[2];
        let mut aliases = HashSet::new();
        let mut removed = HashSet::new();
        loop {
            let ValueDef::Result { inst, .. } = f.value(value).def else {
                break;
            };
            let producer = f.inst(inst);
            if producer.opcode == Opcode::TakeValuesToLocals
                && producer.args.len() == 1
                && producer.results.len() == 1
                && matches!(producer.aux, AuxData::ValuesLocals { nvars: 1, .. })
            {
                aliases.insert(value);
                removed.insert(inst);
                value = producer.args[0];
            } else {
                break;
            }
        }
        let ValueDef::Result { inst: boxed, .. } = f.value(value).def else {
            continue;
        };
        if f.inst(boxed).opcode != Opcode::BoxFloat {
            continue;
        }
        let raw = f.inst(boxed).args[0];
        if f.value(raw).repr != Repr::UnboxedF64 {
            continue;
        }
        aliases.insert(value);
        removed.insert(boxed);
        // No other consumer may need the box on the packed arm. The store's
        // result must also be unused, since a used SETF result needs a box.
        let result = data.results[0];
        let used_elsewhere = f.block_order().iter().any(|&b| {
            f.block(b).insts.iter().any(|&i| {
                let d = f.inst(i);
                d.targets
                    .iter()
                    .flat_map(|edge| &edge.args)
                    .any(|v| aliases.contains(v) || *v == result)
                    || (i != store
                        && !removed.contains(&i)
                        && d.args.iter().any(|v| aliases.contains(v) || *v == result))
            })
        });
        if used_elsewhere {
            continue;
        }
        let block = f
            .block_order()
            .iter()
            .copied()
            .find(|&b| f.block(b).insts.contains(&store))
            .unwrap();
        // Removing a box in another block would require proving that moving its
        // allocation across the intervening control flow preserves effects.
        if removed.iter().any(|i| !f.block(block).insts.contains(i)) {
            continue;
        }
        for b in f.block_order().to_vec() {
            f.block_mut(b).insts.retain(|i| !removed.contains(i));
        }
        for state in f.frame_states.iter_mut() {
            for source in state
                .scopes
                .iter_mut()
                .flat_map(|s| s.locals.iter_mut().chain(&mut s.stack))
                .chain(state.remat.iter_mut().flat_map(|r| &mut r.inputs))
            {
                if let ValueSource::Value { value, repr } = source
                    && (aliases.contains(value) || *value == result)
                {
                    *value = raw;
                    *repr = Repr::UnboxedF64;
                }
            }
        }
        let pos = f
            .block(block)
            .insts
            .iter()
            .position(|&i| i == store)
            .unwrap();
        let tail = f.block_mut(block).insts.split_off(pos);
        let join = f.make_block_after(block);
        let slow = f.make_block_after(block);
        f.block_mut(join).insts = tail[1..].to_vec();
        let (_, succeeded) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::TryPackedF64Set,
                args: vec![data.args[0], data.args[1], raw],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: data.source_pos,
            },
            &[(
                IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                Repr::Tagged,
            )],
        );
        f.set_terminator(
            block,
            InstData {
                opcode: Opcode::Brif,
                args: succeeded,
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    terminator: true,
                    ..InstFlags::default()
                },
                targets: vec![
                    BlockCall {
                        block: join,
                        args: vec![],
                    },
                    BlockCall {
                        block: slow,
                        args: vec![],
                    },
                ],
                frame_state: None,
                source_pos: data.source_pos,
            },
        );
        // Give the slow box a fresh result; all old deopt-only references now
        // name raw, so this value exists solely on this arm.
        let (_, tagged) = f.push_inst(
            slow,
            InstData {
                opcode: Opcode::BoxFloat,
                args: vec![raw],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: data.source_pos,
            },
            &[(IRType::of(TypeBits::DOUBLE_FLOAT), Repr::Tagged)],
        );
        f.inst_mut(store).args[2] = tagged[0];
        f.block_mut(slow).insts.push(store);
        f.set_terminator(
            slow,
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
                    args: vec![],
                }],
                frame_state: None,
                source_pos: data.source_pos,
            },
        );
        count += 1;
    }
    count
}

/// Preserve polymorphism for an escaping AREF result: ordinary vectors read
/// a tagged word; packed doubles load raw and box only their escaping result.
fn specialize_boxed_read(f: &mut Function, site: Inst, old: &InstData) {
    use super::ir::BlockCall;
    use std::collections::HashMap;
    let block = f
        .block_order()
        .iter()
        .copied()
        .find(|&b| f.block(b).insts.contains(&site))
        .unwrap();
    let pos = f
        .block(block)
        .insts
        .iter()
        .position(|&i| i == site)
        .unwrap();
    let tail = f.block_mut(block).insts.split_off(pos);
    let join = f.make_block_after(block);
    let vector = f.make_block_after(block);
    let packed = f.make_block_after(block);
    f.block_mut(join).insts = tail[1..].to_vec();
    let merged = f.add_block_param(join, f.value(old.results[0]).ty, Repr::Tagged);
    // Rename original uses before adding the vector arm's own result edge.
    f.replace_values(&HashMap::from([(old.results[0], merged)]));
    let (_, predicate) = f.push_inst(
        block,
        InstData {
            opcode: Opcode::PackedF64P,
            args: vec![old.args[0]],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                effectful: true,
                ..InstFlags::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: old.source_pos,
        },
        &[(
            IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
            Repr::Tagged,
        )],
    );
    f.set_terminator(
        block,
        InstData {
            opcode: Opcode::Brif,
            args: predicate,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                terminator: true,
                ..InstFlags::default()
            },
            targets: vec![
                BlockCall {
                    block: packed,
                    args: vec![],
                },
                BlockCall {
                    block: vector,
                    args: vec![],
                },
            ],
            frame_state: None,
            source_pos: old.source_pos,
        },
    );
    let (_, raw) = f.push_inst(
        packed,
        InstData {
            opcode: Opcode::PackedF64Ref,
            args: old.args.clone(),
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..InstFlags::default()
            },
            targets: vec![],
            frame_state: old.frame_state,
            source_pos: old.source_pos,
        },
        &[(IRType::of(TypeBits::DOUBLE_FLOAT), Repr::UnboxedF64)],
    );
    let (_, boxed) = f.push_inst(
        packed,
        InstData {
            opcode: Opcode::BoxFloat,
            args: raw,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                effectful: true,
                ..InstFlags::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: old.source_pos,
        },
        &[(IRType::of(TypeBits::DOUBLE_FLOAT), Repr::Tagged)],
    );
    let exit = |value| InstData {
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
    };
    f.set_terminator(packed, exit(boxed[0]));
    let data = f.inst_mut(site);
    data.opcode = Opcode::VecRef;
    data.aux = AuxData::ArrayRef;
    data.flags = InstFlags {
        effectful: true,
        guard: true,
        ..InstFlags::default()
    };
    f.block_mut(vector).insts.push(site);
    f.set_terminator(vector, exit(old.results[0]));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::FrameState;

    #[test]
    fn canonical_accessors_become_guarded_memory_operations() {
        let mut f = Function::new("array-access");
        let block = f.entry();
        let array = f.add_block_param(block, IRType::TOP, Repr::Tagged);
        let index = f.add_block_param(block, IRType::of(TypeBits::FIXNUM), Repr::Tagged);
        let symbol = egcl_rt::symbols::intern("AREF");
        let state = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let (_, result) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::Call,
                args: vec![array, index],
                results: vec![],
                aux: AuxData::CallTarget(symbol),
                flags: InstFlags {
                    effectful: true,
                    call: true,
                    safepoint: true,
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
        assert_eq!(
            specialize(
                &mut f,
                false,
                &super::super::inlining::InlineOptions::default()
            ),
            1
        );
        let access = f
            .block(block)
            .insts
            .iter()
            .map(|&i| f.inst(i))
            .find(|i| i.opcode == Opcode::VecRef)
            .unwrap();
        assert!(access.flags.guard && access.flags.effectful);
        assert!(matches!(access.aux, AuxData::ArrayRef));
        assert_eq!(access.frame_state, Some(state));
    }
    #[test]
    fn simple_vector_store_does_not_select_raw_packed_double_store() {
        let mut f = Function::new("svref-raw-store-layout-contract");
        let block = f.entry();
        let array = f.add_block_param(block, IRType::TOP, Repr::Tagged);
        let index = f.add_block_param(block, IRType::TOP, Repr::Tagged);
        let state = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let instruction = |opcode, args, aux, flags, frame_state| InstData {
            opcode,
            args,
            results: vec![],
            aux,
            flags,
            targets: vec![],
            frame_state,
            source_pos: 0,
        };
        let raw = f
            .push_inst(
                block,
                instruction(
                    Opcode::ConstDouble,
                    vec![],
                    AuxData::DoubleImm(1.5f64.to_bits()),
                    InstFlags::default(),
                    None,
                ),
                &[(IRType::of(TypeBits::DOUBLE_FLOAT), Repr::UnboxedF64)],
            )
            .1[0];
        let boxed = f
            .push_inst(
                block,
                instruction(
                    Opcode::BoxFloat,
                    vec![raw],
                    AuxData::None,
                    InstFlags {
                        effectful: true,
                        ..Default::default()
                    },
                    None,
                ),
                &[(IRType::of(TypeBits::DOUBLE_FLOAT), Repr::Tagged)],
            )
            .1[0];
        let symbol = egcl_rt::symbols::intern("EGCL::SET-SVREF");
        let store = f
            .push_inst(
                block,
                instruction(
                    Opcode::Call,
                    vec![array, index, boxed],
                    AuxData::CallTarget(symbol),
                    InstFlags {
                        call: true,
                        safepoint: true,
                        effectful: true,
                        ..Default::default()
                    },
                    Some(state),
                ),
                &[(IRType::TOP, Repr::Tagged)],
            )
            .0;
        f.set_terminator(
            block,
            instruction(
                Opcode::Return,
                vec![array],
                AuxData::None,
                InstFlags::default(),
                None,
            ),
        );
        super::super::verify::verify(&f).unwrap();
        assert_eq!(
            specialize(
                &mut f,
                true,
                &super::super::inlining::InlineOptions::default()
            ),
            1
        );
        assert_eq!(f.inst(store).frame_state, Some(state));
        assert!(
            f.block(block)
                .insts
                .iter()
                .any(|&i| matches!(f.inst(i).aux, AuxData::BuiltinBinding { .. }))
        );
        assert!(
            f.block(block)
                .insts
                .iter()
                .any(|&i| f.inst(i).opcode == Opcode::ClearMv)
        );
        assert_eq!(specialize_raw_stores(&mut f), 0);
        assert_eq!(f.inst(store).opcode, Opcode::VecSet);
        assert!(matches!(f.inst(store).aux, AuxData::SimpleVectorStore));
        // The identical fixture IS eligible for the ordinary AREF rewrite,
        // proving this checks the layout exclusion rather than another bailout.
        f.inst_mut(store).aux = AuxData::None;
        assert_eq!(specialize_raw_stores(&mut f), 1);
        super::super::verify::verify(&f).unwrap();
    }

}
