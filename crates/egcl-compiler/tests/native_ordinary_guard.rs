// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{emit::*, frame_state::*, ir::*};

fn guarded(clear: bool, resume_bcp: u32, target: u32) -> Function {
    let mut f = Function::new("ordinary-guard-admission");
    let block = f.entry();
    let x = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
    let tagged = |value| ValueSource::Value { value, repr: ValueRepresentation::Tagged };
    let state = f.frame_states.add(FrameState { scopes: vec![FrameScope {
        function: 1, bcp: 0, locals: vec![tagged(x)], stack: vec![tagged(x)],
    }], remat: vec![] });
    let (_, result) = f.push_inst(block, InstData {
        opcode: Opcode::Call, args: vec![x], results: vec![], aux: AuxData::CallTarget(target),
        flags: InstFlags { call: true, safepoint: true, effectful: true, ..Default::default() },
        targets: vec![], frame_state: Some(state), source_pos: 0,
    }, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    let state = f.frame_states.add(FrameState { scopes: vec![FrameScope {
        function: 1, bcp: resume_bcp, locals: vec![tagged(x)], stack: vec![tagged(result[0])],
    }], remat: vec![] });
    let (_, narrowed) = f.push_inst(block, InstData {
        opcode: Opcode::Guard, args: result, results: vec![],
        aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
        flags: InstFlags { guard: true, effectful: true, ..Default::default() },
        targets: vec![], frame_state: Some(state), source_pos: 1,
    }, &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)]);
    if clear {
        f.push_inst(block, InstData {
            opcode: Opcode::ClearMv, args: vec![], results: vec![], aux: AuxData::None,
            flags: InstFlags { call: true, safepoint: true, effectful: true, ..Default::default() },
            targets: vec![], frame_state: None, source_pos: 1,
        }, &[]);
    }
    f.set_terminator(block, InstData {
        opcode: Opcode::Return, args: narrowed, results: vec![], aux: AuxData::None,
        flags: InstFlags::default(), targets: vec![], frame_state: None, source_pos: 2,
    });
    f
}

fn emit(f: &Function, target: u32) -> Result<FramedCode, EmitError> {
    emit_framed_ordinary_guarded_call(f,
        OrdinaryLispEntryOptions { arity: 1, arity_error_veneer: 1 },
        OrdinaryCallTarget { symbol: target, descriptor_slot: 1, revision_address: 1 , heat_slot: 0},
        OrdinaryCallFlags { pending_events: 0, gc_requested: 1, sigusr1_pending: 1, posted_signal: 1,
            native_errors: 1, signal_epoch: 1 }, 1,
        OrdinaryGuardOptions { veneer: 1, multiple_values: 1 })
}

#[test]
fn ordinary_guard_requires_post_call_state_and_single_result_continuation() {
    let accepted = guarded(true, 1, 2);
    egcl_compiler::t2::verify::verify(&accepted).unwrap();
    assert!(emit(&accepted, 2).is_ok());
    assert!(emit(&guarded(false, 1, 2), 2).is_err(), "do not discard a live MV tuple");
    assert!(emit(&guarded(true, 0, 2), 2).is_err(), "pre-call state would replay effects");
    assert!(emit(&guarded(true, 1, 1), 1).is_err(), "direct recursion stays outside this slice");
}

#[test]
fn ordinary_general_entry_accepts_recursive_guarded_continuation() {
    let f = guarded(true, 1, 1);
    egcl_compiler::t2::verify::verify(&f).unwrap();
    let call = f.block(f.entry()).insts[0];
    let result = emit_framed_ordinary_function(&f,
        OrdinaryLispEntryOptions { arity: 1, arity_error_veneer: 1 },
        &[(call, OrdinaryCallTarget { symbol: 1, descriptor_slot: 1, revision_address: 1 , heat_slot: 0})],
        OrdinaryCallFlags { pending_events: 0, gc_requested: 1, sigusr1_pending: 1, posted_signal: 1,
            native_errors: 1, signal_epoch: 1 }, 1,
        OrdinaryGuardOptions { veneer: 1, multiple_values: 1 });
    assert!(result.is_ok(), "ordinary recursion must use the same entry as cross-function calls: {:?}", result.err());
}
