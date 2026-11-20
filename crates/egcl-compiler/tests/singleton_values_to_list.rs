// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{
    emit::{OrdinaryPrimitive, lower_ordinary_primitives},
    frame_state::{FrameScope, FrameState, ValueSource},
    ir::*,
};

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
fn clear(f: &mut Function, block: Block) {
    let mut data = instruction(Opcode::ClearMv, vec![], AuxData::None);
    data.flags.effectful = true;
    f.push_inst(block, data, &[]);
}
fn list(f: &mut Function, block: Block, primary: Value, bcp: u32) -> (Inst, Value) {
    let source = ValueSource::Value {
        value: primary,
        repr: ValueRepresentation::Tagged,
    };
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 77,
            bcp,
            locals: vec![source.clone()],
            stack: vec![source],
        }],
        remat: vec![],
    });
    let mut data = instruction(Opcode::Call, vec![primary], AuxData::ValuesToList);
    data.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    data.frame_state = Some(state);
    data.source_pos = bcp;
    let (site, result) = f.push_inst(block, data, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    (site, result[0])
}
fn finish(f: &mut Function, block: Block, value: Value) {
    f.set_terminator(
        block,
        instruction(Opcode::Return, vec![value], AuxData::None),
    );
}
fn simple(reset: bool) -> (Function, Inst, Value) {
    let mut f = Function::new("singleton-list");
    let block = f.entry();
    let primary = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
    if reset {
        clear(&mut f, block);
    }
    let (site, result) = list(&mut f, block, primary, 145);
    finish(&mut f, block, result);
    egcl_compiler::t2::verify::verify(&f).unwrap();
    (f, site, primary)
}
fn assert_lowered(mut f: Function, site: Inst, primary: Value) {
    let original = f.inst(site).clone();
    let mut requests = vec![];
    assert_eq!(
        lower_ordinary_primitives(&mut f, |primitive| {
            requests.push(primitive);
            Some(991)
        })
        .unwrap(),
        1
    );
    assert_eq!(requests, vec![OrdinaryPrimitive::Cons]);
    let data = f.inst(site);
    assert_eq!(data.opcode, Opcode::Call);
    assert!(matches!(data.aux, AuxData::CallTarget(991)));
    assert_eq!(data.args.len(), 2);
    assert_eq!(data.args[0], primary);
    let ValueDef::Result { inst: nil, .. } = f.value(data.args[1]).def else {
        panic!("NIL constant")
    };
    assert_eq!(f.inst(nil).opcode, Opcode::ConstNil);
    assert_eq!(data.results, original.results);
    assert_eq!(data.frame_state, original.frame_state);
    assert_eq!(data.source_pos, original.source_pos);
    assert_eq!(data.flags, original.flags);
    assert!(
        !f.block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .any(|&i| matches!(f.inst(i).aux, AuxData::ValuesToList))
    );
    egcl_compiler::t2::verify::verify(&f).unwrap();
}
fn assert_declined(mut f: Function) {
    let before = format!("{f:?}");
    assert!(lower_ordinary_primitives(&mut f, |_| Some(991)).is_err());
    assert_eq!(
        format!("{f:?}"),
        before,
        "failed lowering leaves mapped input byte-for-byte intact"
    );
}

#[test]
fn clear_singleton_becomes_cons_with_unchanged_result_and_recovery() {
    let (f, site, primary) = simple(true);
    assert_lowered(f, site, primary);
}

#[test]
fn singleton_nil_and_heap_literal_keep_their_exact_primary() {
    egcl_rt::rooted!(heap = egcl_rt::gc::alloc_double_float(2.5));
    for heap_literal in [false, true] {
        let mut f = Function::new("singleton-literal");
        let block = f.entry();
        let data = if heap_literal {
            instruction(
                Opcode::ConstHeapObj,
                vec![],
                AuxData::HeapLiteral {
                    slot: std::ptr::from_ref(&*heap) as usize,
                },
            )
        } else {
            instruction(Opcode::ConstNil, vec![], AuxData::None)
        };
        let primary = f
            .push_inst(block, data, &[(IRType::TOP, ValueRepresentation::Tagged)])
            .1[0];
        clear(&mut f, block);
        let (site, result) = list(&mut f, block, primary, 149);
        finish(&mut f, block, result);
        assert_lowered(f, site, primary);
    }
}

fn joined(right_clear: bool) -> (Function, Inst, Value) {
    let mut f = Function::new("local-block-return-merge");
    let entry = f.entry();
    let primary = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let left = f.make_block();
    let right = f.make_block();
    let resume = f.make_block();
    let mut branch = instruction(Opcode::Brif, vec![primary], AuxData::None);
    branch.targets = vec![
        BlockCall {
            block: left,
            args: vec![],
        },
        BlockCall {
            block: right,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    // build.rs lowers an unregistered local RETURN-FROM and normal completion
    // to these Jump edges; both retain MV and converge at the same resume BCP.
    for (block, reset) in [(left, true), (right, right_clear)] {
        if reset {
            clear(&mut f, block);
        }
        let mut jump = instruction(Opcode::Jump, vec![], AuxData::None);
        jump.targets = vec![BlockCall {
            block: resume,
            args: vec![],
        }];
        f.set_terminator(block, jump);
    }
    let (site, result) = list(&mut f, resume, primary, 266);
    finish(&mut f, resume, result);
    egcl_compiler::t2::verify::verify(&f).unwrap();
    (f, site, primary)
}
#[test]
fn local_block_all_clear_merge_is_single_but_mixed_merge_declines() {
    let (f, site, primary) = joined(true);
    assert_lowered(f, site, primary);
    assert_declined(joined(false).0);
}

#[test]
fn unknown_zero_and_many_producers_cannot_be_assumed_single_even_with_bad_flags() {
    assert_declined(simple(false).0);
    for count in [0, 1, 3] {
        for bad_call_flag in [false, true] {
            let mut f = Function::new("unknown-values-producer");
            let block = f.entry();
            let primary = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
            clear(&mut f, block);
            let mut producer =
                instruction(Opcode::Call, vec![primary; count], AuxData::CallTarget(88));
            producer.flags = InstFlags {
                call: !bad_call_flag,
                safepoint: true,
                effectful: true,
                ..Default::default()
            };
            let produced = f
                .push_inst(
                    block,
                    producer,
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                )
                .1[0];
            let (_, result) = list(&mut f, block, produced, 7);
            finish(&mut f, block, result);
            assert_declined(f);
        }
    }
}

#[test]
fn osr_entry_and_any_unproven_consumer_decline_atomically() {
    let mut f = Function::new("osr-overrides-proven-single-predecessor");
    let entry = f.entry();
    let supplied = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    clear(&mut f, entry);
    let header = f.make_block();
    let primary = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
    let mut jump = instruction(Opcode::Jump, vec![], AuxData::None);
    jump.targets = vec![BlockCall {
        block: header,
        args: vec![supplied],
    }];
    f.set_terminator(entry, jump);
    let (site, result) = list(&mut f, header, primary, 145);
    finish(&mut f, header, result);
    // The SAME body succeeds without an external entry: entry ClearMv is a
    // proof along its ordinary predecessor. OSR must override exactly that fact.
    assert_lowered(f.clone(), site, primary);
    let state = f.inst(site).frame_state.unwrap();
    f.osr_entries.push(OsrEntry {
        bcp: 145,
        block: header,
        frame_state: state,
        checks: vec![],
        control_scopes: vec![],
    });
    egcl_compiler::t2::verify::verify(&f).unwrap();
    assert_declined(f);
    let (mut f, _, primary) = simple(true);
    let block = f.entry();
    f.block_mut(block).insts.pop();
    // First consumer can lower, but its call invalidates single state for the
    // second. Entire input must remain unchanged on refusal.
    let (_, result) = list(&mut f, block, primary, 150);
    finish(&mut f, block, result);
    assert_declined(f);
}

#[test]
fn malformed_consumer_and_missing_cons_target_leave_input_unchanged() {
    // A legal mapped Invoke has exceptional CFG semantics and must not be
    // rewritten by the ordinary Call-only transformation, even after ClearMv.
    let mut invoked = Function::new("mapped-values-list-invoke");
    let entry = invoked.entry();
    let primary = invoked.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    clear(&mut invoked, entry);
    let (site, result) = list(&mut invoked, entry, primary, 42);
    let normal = invoked.make_block();
    let failure = invoked.make_block();
    let forwarded = invoked.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
    let data = invoked.inst_mut(site);
    data.opcode = Opcode::Invoke;
    data.flags.terminator = true;
    data.targets = vec![
        BlockCall {
            block: normal,
            args: vec![result],
        },
        BlockCall {
            block: failure,
            args: vec![],
        },
    ];
    finish(&mut invoked, normal, forwarded);
    finish(&mut invoked, failure, primary);
    egcl_compiler::t2::verify::verify(&invoked).unwrap();
    assert_declined(invoked);
    for bad in 0..6 {
        let (mut f, site, _) = simple(true);
        let entry = f.entry();
        match bad {
            0 => f.inst_mut(site).frame_state = None,
            1 => f.inst_mut(site).flags.safepoint = false,
            2 => f.inst_mut(site).args.clear(),
            3 => f.inst_mut(site).flags.guard = true,
            4 => f.inst_mut(site).targets.push(BlockCall {
                block: entry,
                args: vec![],
            }),
            5 => {
                let result = f.inst(site).results[0];
                f.set_repr(result, ValueRepresentation::UnboxedF64);
            }
            _ => unreachable!(),
        }
        assert_declined(f);
    }
    let (mut f, _, _) = simple(true);
    let before = format!("{f:?}");
    assert!(lower_ordinary_primitives(&mut f, |_| None).is_err());
    assert_eq!(format!("{f:?}"), before);
}
