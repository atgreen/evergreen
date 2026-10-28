// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Explicit evidence from validated allocation success, joined with a real
//! slow-result probe. A constructor's public return type is never a proof:
//! its mutable element descriptor may select a completely different layout.
use super::ir::{
    AuxData, Function, Inst, InstFlags, Opcode, TypeBits, ValueDef, ValueRepresentation,
};
use super::value_facts::StructuralFacts;

fn canonical_adapter(target: u32) -> bool {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        super::emit::packed_u8_adapter(target).is_some()
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    {
        let _ = target;
        false
    }
}

/// Recover the exact semantic allocation site. All references are SSA operands,
/// not instruction IDs hidden from cloning, liveness, or operand rewriting.
pub(crate) fn allocation_site(
    f: &Function,
    witness: Inst,
    facts: &StructuralFacts,
) -> Option<Inst> {
    let d = f.inst(witness);
    if d.opcode != Opcode::PackedU8AllocationLayout
        || d.args.len() != 2
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
    let call = constructor_for(f, d.args[1])?;
    if !facts.precedes(call, witness) || !facts.same_identity_at(d.args[0], d.args[1], witness) {
        return None;
    }
    Some(call)
}

/// Exact private semantic constructor, never a public symbol-name guess.
pub(crate) fn constructor_for(f: &Function, result: super::ir::Value) -> Option<Inst> {
    let ValueDef::Result { inst: call, num: 0 } = f.value(result).def else {
        return None;
    };
    let constructor = f.inst(call);
    let AuxData::CallTarget(target) = constructor.aux else {
        return None;
    };
    if constructor.opcode != Opcode::Call
        || constructor.results.len() != 1
        || constructor.results[0] != result
        || constructor.args.len() != 3
        || !constructor.flags.call
        || !constructor.flags.effectful
        || !constructor.flags.safepoint
        || constructor.flags.terminator
        || constructor.frame_state.is_none()
        || !constructor.targets.is_empty()
        || constructor
            .args
            .iter()
            .chain(&constructor.results)
            .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
        || !canonical_adapter(target)
    {
        return None;
    }
    Some(call)
}

/// Materialize the producer in ordinary-call IR, after canonical adapter
/// lowering and OSR separation. Generic/mixed phi/OSR subjects keep real probes.
/// No instruction moves, call effects, recovery state or bounds are changed.
pub fn specialize(f: &mut Function) -> usize {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *DISABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_ALLOCATION_FACTS").is_some()) {
        return 0;
    }
    let facts = StructuralFacts::analyze(f);
    let candidates: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .copied()
        .filter(|&i| f.inst(i).opcode == Opcode::PackedU8Layout)
        .collect();
    let mut changed = 0;
    for probe in candidates {
        let d = f.inst(probe);
        if d.args.len() != 1
            || d.results.len() != 1
            || d.flags != InstFlags::default()
            || !matches!(d.aux, AuxData::None)
            || d.frame_state.is_some()
            || !d.targets.is_empty()
        {
            continue;
        }
        let Some(seed) = facts.origin(d.args[0]) else {
            continue;
        };
        let original = d.clone();
        f.inst_mut(probe).opcode = Opcode::PackedU8AllocationLayout;
        f.inst_mut(probe).args.push(seed);
        if allocation_site(f, probe, &facts).is_some() {
            changed += 1;
        } else {
            *f.inst_mut(probe) = original;
        }
    }
    if changed != 0 && std::env::var_os("EGCL_ALLOCATION_FACTS_LOG").is_some() {
        eprintln!(
            "[allocation-facts] {}: explicit witnesses={changed}",
            f.name()
        );
    }
    super::allocation_extent::specialize(f);
    changed
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
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
    struct Fixture {
        f: Function,
        call: Inst,
        probe: Inst,
        kernel: Inst,
        header: Block,
        other: Value,
    }
    fn fixture() -> Fixture {
        let target = 0xf6190;
        assert!(super::super::emit::register_packed_u8_adapter(
            target,
            super::super::emit::PackedU8AdapterSymbols {
                element_type: 0xf6191,
                unsigned_byte: 0xf6192,
            }
        ));
        let mut f = Function::new("allocation-result-phi");
        let entry = f.entry();
        let args: Vec<_> = (0..4)
            .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let state = f.frame_states.add(FrameState {
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
        let mut call_data = data(Opcode::Call, args[..3].to_vec());
        call_data.aux = AuxData::CallTarget(target);
        call_data.flags = InstFlags {
            call: true,
            effectful: true,
            safepoint: true,
            ..Default::default()
        };
        call_data.frame_state = Some(state);
        let (call, result) = f.push_inst(
            entry,
            call_data,
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let (probe, width) = f.push_inst(
            entry,
            data(Opcode::PackedU8Layout, vec![result[0]]),
            &[(
                IRType::of(TypeBits::FIXNUM.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        let mut constants = Vec::new();
        for n in [0, 1, 19] {
            let mut d = data(Opcode::ConstFixnum, vec![]);
            d.aux = AuxData::FixnumImm(n);
            constants.push(
                f.push_inst(
                    entry,
                    d,
                    &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
                )
                .1[0],
            );
        }
        let header = f.make_block();
        let exit = f.make_block();
        let array_phi = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
        let width_phi = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
        let mut jump = data(Opcode::Jump, vec![]);
        jump.targets = vec![BlockCall {
            block: header,
            args: vec![result[0], width[0]],
        }];
        f.set_terminator(entry, jump);
        let mut store = data(
            Opcode::TryStoreIntegerOctets1To8Layout,
            vec![
                array_phi,
                constants[0],
                constants[1],
                constants[2],
                width_phi,
            ],
        );
        store.flags.effectful = true;
        let (kernel, _) = f.push_inst(
            header,
            store,
            &[
                (
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    ValueRepresentation::Tagged,
                ),
                (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged),
            ],
        );
        let mut branch = data(Opcode::Brif, vec![args[3]]);
        branch.targets = vec![
            BlockCall {
                block: header,
                args: vec![array_phi, width_phi],
            },
            BlockCall {
                block: exit,
                args: vec![],
            },
        ];
        f.set_terminator(header, branch);
        f.set_terminator(exit, data(Opcode::Return, vec![array_phi]));
        Fixture {
            f,
            call,
            probe,
            kernel,
            header,
            other: args[3],
        }
    }

    #[test]
    fn allocation_witness_is_consumed_through_array_and_evidence_phis() {
        let Fixture {
            mut f,
            call,
            probe,
            kernel,
            ..
        } = fixture();
        crate::t2::verify::verify(&f).unwrap();
        let original_call = format!("{:?}", f.inst(call));
        let fs = f.inst(call).frame_state.unwrap();
        let original_state = format!("{:?}", f.frame_states.get(fs));
        assert_eq!(specialize(&mut f), 1);
        assert_eq!(f.inst(probe).opcode, Opcode::PackedU8AllocationLayout);
        assert_eq!(f.inst(probe).args[1], f.inst(call).results[0]);
        let facts = StructuralFacts::analyze(&f);
        assert_eq!(allocation_site(&f, probe, &facts), Some(call));
        assert!(facts.witness_for(f.inst(kernel).args[0], f.inst(kernel).args[4], kernel));
        assert_eq!(
            facts
                .layout_before(
                    f.inst(kernel).args[0],
                    super::super::value_facts::LayoutKind::PackedU8,
                    kernel
                )
                .unwrap()
                .encoding,
            super::super::value_facts::LayoutEvidence::AllocationOrProbeWidth
        );
        assert_eq!(format!("{:?}", f.inst(call)), original_call);
        assert_eq!(format!("{:?}", f.frame_states.get(fs)), original_state);
        crate::t2::verify::verify(&f).unwrap();
    }

    #[test]
    fn allocation_witness_rejects_mixed_evidence_subject_and_osr_bypass() {
        for column in [0, 1] {
            let Fixture {
                mut f,
                header,
                other,
                ..
            } = fixture();
            assert_eq!(specialize(&mut f), 1);
            let term = f.terminator(header).unwrap();
            f.inst_mut(term).targets[0].args[column] = other;
            assert!(super::super::array_layout::verify_accesses(&f).is_err());
        }
        let Fixture {
            mut f,
            header,
            call,
            ..
        } = fixture();
        assert_eq!(specialize(&mut f), 1);
        f.osr_entries.push(OsrEntry {
            block: header,
            bcp: 5,
            frame_state: f.inst(call).frame_state.unwrap(),
            control_scopes: vec![],
            checks: vec![],
        });
        assert!(super::super::array_layout::verify_accesses(&f).is_err());
    }

    #[test]
    fn allocation_witness_rejects_opaque_calls_and_forged_sources() {
        let Fixture {
            mut f,
            call,
            probe,
            other,
            ..
        } = fixture();
        let before = format!("{:?}", f.inst(call));
        f.inst_mut(call).aux = AuxData::OpaqueCallTarget(0xf6190);
        assert_eq!(specialize(&mut f), 0);
        assert_eq!(f.inst(probe).opcode, Opcode::PackedU8Layout);
        f.inst_mut(call).aux = AuxData::CallTarget(0xf6190);
        assert_eq!(format!("{:?}", f.inst(call)), before);
        assert_eq!(specialize(&mut f), 1);
        f.inst_mut(probe).args[1] = other;
        assert!(super::super::array_layout::verify_accesses(&f).is_err());
    }

    #[test]
    fn allocation_witness_clone_remaps_explicit_constructor_operand() {
        let Fixture {
            mut f, call, probe, ..
        } = fixture();
        assert_eq!(specialize(&mut f), 1);
        let entry = f.entry();
        let cloned = super::super::region_clone::clone_region(&mut f, &[entry]).unwrap();
        let old_result = f.inst(call).results[0];
        let new_result = cloned.value_map[&old_result];
        let new_width = cloned.value_map[&f.inst(probe).results[0]];
        let ValueDef::Result {
            inst: cloned_probe, ..
        } = f.value(new_width).def
        else {
            panic!()
        };
        assert_eq!(
            f.inst(cloned_probe).opcode,
            Opcode::PackedU8AllocationLayout
        );
        assert_eq!(f.inst(cloned_probe).args, [new_result, new_result]);
        assert_ne!(new_result, old_result);
    }
}
