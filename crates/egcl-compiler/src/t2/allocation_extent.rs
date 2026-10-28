// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Captured/checked immutable allocation extents and conservative upper bounds.
//! Mutable dimension lists have no SSA length correlation. Every nullable
//! failure keeps the scalar loop; no allocation/type/profile guess is a bound.
use super::ir::{
    AuxData, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value, ValueDef,
    ValueRepresentation,
};
use super::value_facts::StructuralFacts;

pub(crate) fn producer_site(f: &Function, inst: Inst, facts: &StructuralFacts) -> Option<Inst> {
    let d = f.inst(inst);
    if d.opcode != Opcode::PackedU8AllocationExtent
        || d.args.len() != 3
        || d.results.len() != 1
        || d.flags != InstFlags::default()
        || !matches!(d.aux, AuxData::None)
        || d.frame_state.is_some()
        || !d.targets.is_empty()
        || d.args
            .iter()
            .chain(&d.results)
            .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
        || f.value(d.results[0]).ty.bits != TypeBits::FIXNUM.join(TypeBits::NULL)
    {
        return None;
    }
    let call = super::allocation_facts::constructor_for(f, d.args[2])?;
    (facts.precedes(call, inst)
        && facts.same_identity_at(d.args[0], d.args[2], inst)
        && facts.witness_for(d.args[0], d.args[1], inst))
    .then_some(call)
}

/// Evidence is about this executed continuation, not a type annotation alone.
fn fixnum_validated_before(f: &Function, value: Value, at: Inst, facts: &StructuralFacts) -> bool {
    if f.value(value).repr != ValueRepresentation::Tagged {
        return false;
    }
    if let Some(seed) = facts.origin(value) {
        if (f.is_entry_param_checked(seed) && f.value(seed).ty.bits == TypeBits::FIXNUM)
            || matches!(f.value(seed).def, ValueDef::Result { inst, .. }
                if f.inst(inst).opcode == Opcode::ConstFixnum)
        {
            return true;
        }
    }
    f.block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .copied()
        .any(|inst| {
            if !facts.precedes(inst, at) {
                return false;
            }
            let mut proven = false;
            super::normal_return_facts::visit_argument_facts(f, inst, |argument, ty| {
                proven |=
                    ty.bits == TypeBits::FIXNUM && facts.same_identity_at(argument, value, at);
            });
            proven
        })
}

/// Prove bounds without transporting or loading a captured extent. Width still
/// guards layout; canonical normal-return capacity equals an actual direct
/// fixnum dimension. Neither observed profiles nor an old list CAR qualify.
pub(crate) fn dimension_in_bounds(f: &Function, kernel: Inst, facts: &StructuralFacts) -> bool {
    let d = f.inst(kernel);
    if d.opcode != Opcode::TryStoreIntegerOctets1To8Layout
        || d.args.len() != 5
        || !facts.witness_for(d.args[0], d.args[4], kernel)
    {
        return false;
    }
    let Some(result) = facts.origin(d.args[0]) else {
        return false;
    };
    let Some(call) = super::allocation_facts::constructor_for(f, result) else {
        return false;
    };
    if !facts.precedes(call, kernel) {
        return false;
    }
    let dimension = f.inst(call).args[0];
    fn upper(
        f: &Function,
        value: Value,
        limit: Value,
        at: Inst,
        facts: &StructuralFacts,
        budget: &mut usize,
    ) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        if facts.same_identity_at(value, limit, at) {
            return true;
        }
        let Some(seed) = facts.origin(value) else {
            return false;
        };
        let ValueDef::Result { inst, num: 0 } = f.value(seed).def else {
            return false;
        };
        let min = f.inst(inst);
        min.opcode == Opcode::FixnumMin
            && min.args.len() == 2
            && facts.precedes(inst, at)
            && min
                .args
                .iter()
                .any(|&arg| upper(f, arg, limit, at, facts, budget))
    }
    upper(f, d.args[2], dimension, kernel, facts, &mut 32)
        && fixnum_validated_before(f, dimension, kernel, facts)
}

/// Bound proof is interpreted only after the correlated extent's NIL check.
/// An immutable direct fixnum dimension equals normal-return capacity. A list's
/// old CAR is deliberately not considered, even if its allocation was fresh.
pub(crate) fn end_in_bounds(f: &Function, kernel: Inst, facts: &StructuralFacts) -> bool {
    let d = f.inst(kernel);
    if d.args.len() != 6 {
        return false;
    }
    let Some(proof) = facts.extent_before(d.args[0], kernel) else {
        return false;
    };
    if !facts.extent_for(d.args[0], d.args[4], d.args[5], kernel) {
        return false;
    }
    let source = f.inst(proof.producer);
    let Some(call) = producer_site(f, proof.producer, facts) else {
        return false;
    };
    let dimension = f.inst(call).args[0];
    let direct_fixnum = fixnum_validated_before(f, dimension, kernel, facts);
    fn upper(
        f: &Function,
        value: Value,
        extent: Value,
        direct: Option<Value>,
        at: Inst,
        facts: &StructuralFacts,
        budget: &mut usize,
    ) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        if facts.same_identity_at(value, extent, at)
            || direct.is_some_and(|n| facts.same_identity_at(value, n, at))
        {
            return true;
        }
        let Some(seed) = facts.origin(value) else {
            return false;
        };
        let ValueDef::Result { inst, num: 0 } = f.value(seed).def else {
            return false;
        };
        let min = f.inst(inst);
        min.opcode == Opcode::FixnumMin
            && min.args.len() == 2
            && facts.precedes(inst, at)
            && min
                .args
                .iter()
                .any(|&v| upper(f, v, extent, direct, at, facts, budget))
    }
    upper(
        f,
        d.args[2],
        source.results[0],
        direct_fixnum.then_some(dimension),
        kernel,
        facts,
        &mut 32,
    )
}

/// Original five-input kernel is a real consumer. Hoist one checked extent per
/// allocation witness, then retain or prove away only its capacity comparison.
pub fn specialize(f: &mut Function) -> usize {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *DISABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_ALLOCATION_EXTENTS").is_some()) {
        return 0;
    }
    static FORCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let force = *FORCE.get_or_init(|| std::env::var_os("EGCL_FORCE_ALLOCATION_EXTENTS").is_some());
    specialize_mode(f, force)
}

fn specialize_mode(f: &mut Function, force: bool) -> usize {
    let probes: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .copied()
        .filter(|&i| {
            let d = f.inst(i);
            d.opcode == Opcode::PackedU8AllocationLayout
                && d.args.len() == 2
                && d.results.len() == 1
                && d.flags == InstFlags::default()
                && matches!(d.aux, AuxData::None)
                && d.frame_state.is_none()
                && d.targets.is_empty()
        })
        .collect();
    if probes.is_empty() {
        return 0;
    }
    let facts = StructuralFacts::analyze(f);
    let mut count = 0;
    let mut removed = 0;
    for probe in probes {
        let d = f.inst(probe).clone();
        if super::allocation_facts::allocation_site(f, probe, &facts).is_none() {
            continue;
        }
        let sites: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .copied()
            .filter(|&i| {
                f.inst(i).opcode == Opcode::TryStoreIntegerOctets1To8Layout
                    && facts.witness_for(f.inst(i).args[0], f.inst(i).args[4], i)
                    && facts.same_identity_at(f.inst(i).args[4], d.results[0], i)
            })
            .collect();
        if sites.is_empty() {
            continue;
        }
        if !force {
            for kernel in sites {
                if !matches!(f.inst(kernel).aux, AuxData::AllocationDimensionInBounds)
                    && dimension_in_bounds(f, kernel, &facts)
                {
                    f.inst_mut(kernel).aux = AuxData::AllocationDimensionInBounds;
                    count += 1;
                    removed += 1;
                }
            }
            continue;
        }
        let block = f
            .block_order()
            .iter()
            .copied()
            .find(|&b| f.block(b).insts.contains(&probe))
            .unwrap();
        let position = f
            .block(block)
            .insts
            .iter()
            .position(|&i| i == probe)
            .unwrap();
        let (extent, result) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::PackedU8AllocationExtent,
                args: vec![d.args[0], d.results[0], d.args[1]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: d.source_pos,
            },
            &[(
                IRType::of(TypeBits::FIXNUM.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        assert_eq!(f.block_mut(block).insts.pop(), Some(extent));
        f.block_mut(block).insts.insert(position + 1, extent);
        for kernel in sites {
            f.inst_mut(kernel).opcode = Opcode::TryStoreIntegerOctets1To8Extent;
            f.inst_mut(kernel).args.push(result[0]);
            f.inst_mut(kernel).aux = AuxData::None;
            let updated = StructuralFacts::analyze(f);
            if end_in_bounds(f, kernel, &updated) {
                f.inst_mut(kernel).aux = AuxData::AllocationExtentInBounds;
                removed += 1;
            }
            count += 1;
        }
    }
    if count != 0 && std::env::var_os("EGCL_ALLOCATION_FACTS_LOG").is_some() {
        eprintln!(
            "[allocation-facts] {}: extent consumers={count} bounds removed={removed}",
            f.name()
        );
    }
    count
}

/// Only pure adjacent extent production may be skipped by validated allocation
/// success. Any implicit poll remains an actual instruction boundary.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(crate) fn adjacent_completion(
    f: &Function,
    block: super::ir::Block,
    probe: Inst,
    polls: &std::collections::HashSet<Inst>,
) -> Option<(Inst, Value)> {
    let list = &f.block(block).insts;
    let position = list.iter().position(|&i| i == probe)?;
    let extent = *list.get(position + 1)?;
    if polls.contains(&extent) || f.inst(extent).opcode != Opcode::PackedU8AllocationExtent {
        return None;
    }
    let facts = StructuralFacts::analyze(f);
    producer_site(f, extent, &facts)?;
    let d = f.inst(extent);
    (d.args[1] == f.inst(probe).results[0]).then_some((extent, d.results[0]))
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::BlockCall;
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
    fn force_extents(f: &mut Function) -> usize {
        let changed = super::super::allocation_facts::specialize(f);
        specialize_mode(f, true);
        changed
    }
    fn fixture(list_dimension: bool, unrelated_cap: bool) -> (Function, Inst, Inst) {
        assert!(super::super::emit::register_packed_u8_adapter(
            0xf61a0,
            super::super::emit::PackedU8AdapterSymbols {
                element_type: 0xf61a1,
                unsigned_byte: 0xf61a2
            }
        ));
        let mut f = Function::new("allocation-extent-bounds");
        let e = f.entry();
        let dimension = f.add_block_param(
            e,
            if list_dimension {
                IRType::TOP
            } else {
                IRType::of(TypeBits::FIXNUM)
            },
            ValueRepresentation::Tagged,
        );
        let keyword = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let descriptor = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let n = f.add_block_param(e, IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged);
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 3,
                locals: [dimension, keyword, descriptor, n]
                    .into_iter()
                    .map(|value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
                stack: vec![],
            }],
            remat: vec![],
        });
        let mut c = data(Opcode::Call, vec![dimension, keyword, descriptor]);
        c.aux = AuxData::CallTarget(0xf61a0);
        c.frame_state = Some(state);
        c.flags = InstFlags {
            call: true,
            effectful: true,
            safepoint: true,
            ..Default::default()
        };
        let (call, array) = f.push_inst(e, c, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        let (_, width) = f.push_inst(
            e,
            data(Opcode::PackedU8Layout, vec![array[0]]),
            &[(
                IRType::of(TypeBits::FIXNUM.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        let mut zero = data(Opcode::ConstFixnum, vec![]);
        zero.aux = AuxData::FixnumImm(0);
        let start = f
            .push_inst(
                e,
                zero,
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            )
            .1[0];
        let mut eight = data(Opcode::ConstFixnum, vec![]);
        eight.aux = AuxData::FixnumImm(8);
        let eight = f
            .push_inst(
                e,
                eight,
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            )
            .1[0];
        let mut min = data(
            Opcode::FixnumMin,
            vec![
                eight,
                if list_dimension || unrelated_cap {
                    n
                } else {
                    dimension
                },
            ],
        );
        min.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        min.frame_state = Some(state);
        let end = f
            .push_inst(
                e,
                min,
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            )
            .1[0];
        let mut kernel = data(
            Opcode::TryStoreIntegerOctets1To8Layout,
            vec![array[0], start, end, eight, width[0]],
        );
        kernel.flags.effectful = true;
        let (kernel, _) = f.push_inst(
            e,
            kernel,
            &[
                (
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    ValueRepresentation::Tagged,
                ),
                (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged),
            ],
        );
        f.set_terminator(e, data(Opcode::Return, vec![array[0]]));
        (f, call, kernel)
    }
    fn extent(f: &Function) -> Inst {
        f.block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::PackedU8AllocationExtent)
            .unwrap()
    }

    #[test]
    fn direct_fixnum_dimension_min_cap_eliminates_only_capacity_check() {
        let (mut f, call, kernel) = fixture(false, false);
        let before = format!("{:?}", f.inst(call));
        crate::t2::verify::verify(&f).unwrap();
        assert_eq!(force_extents(&mut f), 1);
        assert_eq!(
            f.inst(kernel).opcode,
            Opcode::TryStoreIntegerOctets1To8Extent
        );
        assert!(matches!(
            f.inst(kernel).aux,
            AuxData::AllocationExtentInBounds
        ));
        assert!(end_in_bounds(&f, kernel, &StructuralFacts::analyze(&f)));
        assert_eq!(format!("{:?}", f.inst(call)), before);
        crate::t2::verify::verify(&f).unwrap();
        // Changing the actual dimension to a mutable/unknown list invalidates
        // the old cap relation, even though the original N SSA is still live.
        let unrelated_list = f.block(f.entry()).params[2];
        f.inst_mut(call).args[0] = unrelated_list;
        assert!(crate::t2::verify::verify(&f).is_err());
    }

    #[test]
    fn mutable_list_and_unrelated_fixnum_keep_scalar_capacity_check() {
        for list in [false, true] {
            let (mut f, _, kernel) = fixture(list, true);
            assert_eq!(force_extents(&mut f), 1);
            assert_eq!(
                f.inst(kernel).opcode,
                Opcode::TryStoreIntegerOctets1To8Extent
            );
            assert!(matches!(f.inst(kernel).aux, AuxData::None));
            assert!(!end_in_bounds(&f, kernel, &StructuralFacts::analyze(&f)));
            crate::t2::verify::verify(&f).unwrap();
            f.inst_mut(kernel).aux = AuxData::AllocationExtentInBounds;
            assert!(
                crate::t2::verify::verify(&f).is_err(),
                "forged capacity waiver"
            );
        }
    }

    #[test]
    fn exact_extent_end_and_closed_paired_phis_verify_but_mixed_extent_does_not() {
        let (mut f, _, kernel) = fixture(true, true);
        assert_eq!(force_extents(&mut f), 1);
        let producer = extent(&f);
        let capacity = f.inst(producer).results[0];
        f.inst_mut(kernel).args[2] = capacity;
        f.inst_mut(kernel).aux = AuxData::AllocationExtentInBounds;
        let e = f.entry();
        let h = f.make_block();
        let exit = f.make_block();
        let index = f.block(e).insts.iter().position(|&i| i == kernel).unwrap();
        let suffix = f.block_mut(e).insts.split_off(index);
        f.block_mut(h).insts = suffix;
        let originals = [f.inst(kernel).args[0], f.inst(kernel).args[4], capacity];
        let phis: Vec<_> = originals
            .iter()
            .map(|_| f.add_block_param(h, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        f.inst_mut(kernel).args[0] = phis[0];
        f.inst_mut(kernel).args[4] = phis[1];
        f.inst_mut(kernel).args[5] = phis[2];
        f.inst_mut(kernel).args[2] = phis[2];
        let mut jump = data(Opcode::Jump, vec![]);
        jump.targets = vec![BlockCall {
            block: h,
            args: originals.to_vec(),
        }];
        f.set_terminator(e, jump);
        let old_return = f.block_mut(h).insts.pop().unwrap();
        assert_eq!(f.inst(old_return).opcode, Opcode::Return);
        let mut branch = data(Opcode::Brif, vec![f.block(e).params[1]]);
        branch.targets = vec![
            BlockCall {
                block: h,
                args: phis.clone(),
            },
            BlockCall {
                block: exit,
                args: vec![],
            },
        ];
        let back = f.set_terminator(h, branch);
        f.set_terminator(exit, data(Opcode::Return, vec![phis[0]]));
        crate::t2::verify::verify(&f).unwrap();
        assert!(end_in_bounds(&f, kernel, &StructuralFacts::analyze(&f)));
        let unrelated = f.block(e).params[3];
        f.inst_mut(back).targets[0].args[2] = unrelated;
        assert!(crate::t2::verify::verify(&f).is_err());
    }
    #[test]
    fn direct_fixnum_capacity_proof_survives_top_declared_dimension_alias() {
        let (mut f, call, kernel) = fixture(false, false);
        let entry = f.entry();
        let dimension = f.inst(call).args[0];
        let forwarded = f.make_block();
        let alias = f.add_block_param(forwarded, IRType::TOP, ValueRepresentation::Tagged);
        let instructions = std::mem::take(&mut f.block_mut(entry).insts);
        f.block_mut(forwarded).insts = instructions;
        let mut jump = data(Opcode::Jump, vec![]);
        jump.targets = vec![BlockCall {
            block: forwarded,
            args: vec![dimension],
        }];
        f.set_terminator(entry, jump);
        f.inst_mut(call).args[0] = alias;
        assert_eq!(f.value(alias).ty, IRType::TOP);
        crate::t2::verify::verify(&f).unwrap();
        assert_eq!(force_extents(&mut f), 1);
        assert!(matches!(
            f.inst(kernel).aux,
            AuxData::AllocationExtentInBounds
        ));
        crate::t2::verify::verify(&f).unwrap();
    }
    #[test]
    fn existing_allocation_witness_still_receives_extent_without_plain_layout_candidates() {
        let (mut f, call, kernel) = fixture(false, false);
        let width = f.inst(kernel).args[4];
        let ValueDef::Result { inst: probe, .. } = f.value(width).def else {
            panic!()
        };
        let result = f.inst(call).results[0];
        f.inst_mut(probe).opcode = Opcode::PackedU8AllocationLayout;
        f.inst_mut(probe).args.push(result);
        crate::t2::verify::verify(&f).unwrap();
        assert_eq!(super::super::allocation_facts::specialize(&mut f), 0);
        assert_eq!(
            f.inst(kernel).opcode,
            Opcode::TryStoreIntegerOctets1To8Layout
        );
        assert!(matches!(
            f.inst(kernel).aux,
            AuxData::AllocationDimensionInBounds
        ));
        crate::t2::verify::verify(&f).unwrap();
    }
    #[test]
    fn default_dimension_proof_uses_successful_min_even_for_top_operand() {
        let (mut f, call, kernel) = fixture(true, true);
        let dimension = f.inst(call).args[0];
        let ValueDef::Result { inst: min, .. } = f.value(f.inst(kernel).args[2]).def else {
            panic!()
        };
        f.inst_mut(min).args[1] = dimension;
        assert_eq!(f.value(dimension).ty, IRType::TOP);
        assert_eq!(super::super::allocation_facts::specialize(&mut f), 1);
        assert_eq!(f.inst(kernel).args.len(), 5);
        assert_eq!(
            f.inst(kernel).opcode,
            Opcode::TryStoreIntegerOctets1To8Layout
        );
        assert!(matches!(
            f.inst(kernel).aux,
            AuxData::AllocationDimensionInBounds
        ));
        assert!(
            !f.block_order()
                .iter()
                .flat_map(|&b| &f.block(b).insts)
                .any(|&i| f.inst(i).opcode == Opcode::PackedU8AllocationExtent)
        );
        crate::t2::verify::verify(&f).unwrap();
        assert_eq!(specialize_mode(&mut f, false), 0);
        // The optimized continuation, not a naked declaration, supplies FX.
        f.inst_mut(min).flags = InstFlags::default();
        f.inst_mut(min).frame_state = None;
        assert!(!dimension_in_bounds(
            &f,
            kernel,
            &StructuralFacts::analyze(&f)
        ));
        assert!(crate::t2::verify::verify(&f).is_err());
    }

    #[test]
    fn default_dimension_proof_does_not_use_a_later_min_or_mutable_list_cap() {
        for list in [false, true] {
            let (mut f, call, kernel) = fixture(list, true);
            let dim = f.inst(call).args[0];
            if !list {
                let ValueDef::Result { inst: min, .. } = f.value(f.inst(kernel).args[2]).def else {
                    panic!()
                };
                f.inst_mut(min).args[1] = dim;
                f.inst_mut(kernel).args[2] = dim;
                let e = f.entry();
                f.block_mut(e).insts.retain(|&i| i != min);
                let position = f.block(e).insts.iter().position(|&i| i == kernel).unwrap();
                f.block_mut(e).insts.insert(position + 1, min);
            }
            assert_eq!(super::super::allocation_facts::specialize(&mut f), 1);
            assert!(matches!(f.inst(kernel).aux, AuxData::None));
            assert_eq!(f.inst(kernel).args.len(), 5);
            crate::t2::verify::verify(&f).unwrap();
        }
    }
}
