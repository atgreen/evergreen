// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use egcl_compiler::t2::{emit::*, ir::*};

fn global_read() -> Function {
    let mut f = Function::new("ordinary-global-read");
    let entry = f.entry();
    let (_, result) = f.push_inst(
        entry,
        InstData {
            opcode: Opcode::SymbolValue,
            args: vec![],
            results: vec![],
            aux: AuxData::SymbolRef(42),
            flags: InstFlags {
                call: true,
                safepoint: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    f.set_terminator(
        entry,
        InstData {
            opcode: Opcode::Return,
            args: result,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 1,
        },
    );
    f
}

#[test]
fn global_value_read_compiles_through_the_ordinary_abi() {
    let mut f = global_read();
    assert_eq!(
        lower_ordinary_primitives(&mut f, |operation| {
            assert_eq!(operation, OrdinaryPrimitive::LoadGlobal);
            Some(77)
        })
        .unwrap(),
        1
    );
    egcl_compiler::t2::verify::verify(&f).unwrap();
    let call = f.block(f.entry()).insts[1];
    assert_eq!(f.inst(call).opcode, Opcode::Call);
    let targets = [(
        call,
        OrdinaryCallTarget {
            symbol: 77,
            descriptor_slot: 1,
            revision_address: 1,
            heat_slot: 0,
        },
    )];
    let result = emit_framed_ordinary_function(
        &f,
        OrdinaryLispEntryOptions {
            arity: 0,
            arity_error_veneer: 1,
        },
        &targets,
        OrdinaryCallFlags {
            gc_requested: 1,
            sigusr1_pending: 1,
            posted_signal: 1,
            native_errors: 1,
            signal_epoch: 1,
        },
        1,
        OrdinaryGuardOptions {
            veneer: 1,
            multiple_values: 1,
        },
    );
    assert!(
        result.is_ok(),
        "global load requires an ordinary semantic helper: {:?}",
        result.err()
    );
}

#[test]
fn semantic_store_keeps_its_continuation_and_discards_the_primary() {
    use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
    let mut f = global_read();
    let entry = f.entry();
    let ret = f.block_mut(entry).insts.pop().unwrap();
    let old_read = f.block(entry).insts[0];
    let read_value = f.inst(old_read).results[0];
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 10,
            bcp: 12,
            locals: vec![],
            stack: vec![ValueSource::Value {
                value: read_value,
                repr: ValueRepresentation::Tagged,
            }],
        }],
        remat: vec![],
    });
    let (store, _) = f.push_inst(
        entry,
        InstData {
            opcode: Opcode::SetSymbolValue,
            args: vec![read_value],
            results: vec![],
            aux: AuxData::SymbolRef(43),
            flags: InstFlags {
                call: true,
                safepoint: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: Some(state),
            source_pos: 17,
        },
        &[],
    );
    f.push_inst(
        entry,
        InstData {
            opcode: Opcode::ClearMv,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                call: true,
                safepoint: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 18,
        },
        &[],
    );
    f.block_mut(entry).insts.push(ret);
    assert_eq!(
        lower_ordinary_primitives(&mut f, |op| Some(match op {
            OrdinaryPrimitive::LoadGlobal => 100,
            OrdinaryPrimitive::StoreGlobal => 101,
            _ => panic!("unexpected semantic operation"),
        }))
        .unwrap(),
        2
    );
    egcl_compiler::t2::verify::verify(&f).unwrap();
    let data = f.inst(store);
    assert_eq!(data.frame_state, Some(state));
    assert_eq!(data.source_pos, 17);
    assert!(data.results.is_empty());
    assert_eq!(data.args[1], read_value);
    let ValueDef::Result { inst: symbol, .. } = f.value(data.args[0]).def else {
        panic!("symbol operand");
    };
    assert!(matches!(f.inst(symbol).aux, AuxData::SymbolRef(43)));
    let targets = [old_read, store].map(|inst| {
        let AuxData::CallTarget(symbol) = f.inst(inst).aux else {
            panic!("call target");
        };
        (
            inst,
            OrdinaryCallTarget {
                symbol,
                descriptor_slot: 1,
                revision_address: 1,
                heat_slot: 0,
            },
        )
    });
    let code = emit_framed_ordinary_function(
        &f,
        OrdinaryLispEntryOptions {
            arity: 0,
            arity_error_veneer: 1,
        },
        &targets,
        OrdinaryCallFlags {
            gc_requested: 1,
            sigusr1_pending: 1,
            posted_signal: 1,
            native_errors: 1,
            signal_epoch: 1,
        },
        1,
        OrdinaryGuardOptions {
            veneer: 1,
            multiple_values: 1,
        },
    )
    .unwrap();
    assert!(code.ordinary_entry.is_some());
}

#[test]
fn unresolved_semantic_adapter_leaves_original_ir_untouched() {
    let mut f = global_read();
    let old_values = f.num_values();
    let old_layout = f.block(f.entry()).insts.clone();
    assert!(lower_ordinary_primitives(&mut f, |_| None).is_err());
    assert_eq!(f.num_values(), old_values);
    assert_eq!(f.block(f.entry()).insts, old_layout);
    assert_eq!(f.inst(old_layout[0]).opcode, Opcode::SymbolValue);
}

#[test]
fn semantic_lowering_does_not_discard_an_unknown_multiple_value_tuple() {
    let mut f = global_read();
    let entry = f.entry();
    let original = f.block(entry).insts[0];
    let (call, _) = f.push_inst(
        entry,
        InstData {
            opcode: Opcode::Call,
            args: vec![],
            results: vec![],
            aux: AuxData::CallTarget(99),
            flags: InstFlags {
                call: true,
                safepoint: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    let block = f.block_mut(entry);
    assert_eq!(block.insts.pop(), Some(call));
    block.insts.insert(0, call);
    let old_layout = f.block(entry).insts.clone();
    let old_values = f.num_values();
    assert!(lower_ordinary_primitives(&mut f, |_| Some(77)).is_err());
    assert_eq!(f.inst(original).opcode, Opcode::SymbolValue);
    assert_eq!(f.block(entry).insts, old_layout);
    assert_eq!(f.num_values(), old_values);
}
