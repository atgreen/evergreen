// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::{
    control_scope::{Ownership, ScopeKind},
    t2::{
        build,
        ir::{AuxData, Opcode},
        verify,
    },
};
use egcl_rt::bytecode::{BytecodeFunction, Instr};

fn special_body() -> BytecodeFunction {
    let x = egcl_rt::symbols::intern("*NATIVE-SPECIAL-X*");
    let y = egcl_rt::symbols::intern("*NATIVE-SPECIAL-Y*");
    BytecodeFunction {
        self_call_sites: Vec::new(),
        notinline_sites: Vec::new(),
        name: "native-special-cfg".into(),
        code: vec![
            Instr::LoadLocal(0),
            Instr::BindSpecial(x),
            Instr::LoadLocal(0),
            Instr::BindSpecial(y),
            Instr::LoadGlobal(x),
            Instr::StoreGlobal(y),
            Instr::LoadLocal(0),
            Instr::BrIfTrue(4),
            Instr::LoadGlobal(y),
            Instr::UnbindSpecial(2),
            Instr::Return,
        ],
        constants: vec![],
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
        params_form: egcl_rt::value::NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
        binder_cache: Default::default(),
    }
}

#[test]
fn native_special_operations_preserve_preoperation_stack_and_scope_maps() {
    let body = special_body();
    let ir = build::build_from_bytecode_for_native_cleanups(&body)
        .expect("native binding/global lowering");
    verify::verify(&ir).unwrap();
    let expected = [
        (1, 1, 0),
        (3, 1, 1),
        (4, 0, 2),
        (5, 1, 2),
        (8, 0, 2),
        (9, 1, 2),
    ];
    let calls: Vec<_> = ir
        .block_order()
        .iter()
        .flat_map(|&b| &ir.block(b).insts)
        .copied()
        .filter(|&inst| ir.inst(inst).opcode == Opcode::Invoke)
        .collect();
    assert_eq!(calls.len(), expected.len());
    for (bcp, depth, bindings) in expected {
        let inst = calls
            .iter()
            .copied()
            .find(|&inst| {
                ir.frame_states
                    .get(ir.inst(inst).frame_state.unwrap())
                    .scopes[0]
                    .bcp
                    == bcp
            })
            .expect("every special operation has its own published Invoke");
        let call = ir.inst(inst);
        let frame = &ir.frame_states.get(call.frame_state.unwrap()).scopes[0];
        assert_eq!(frame.stack.len(), depth);
        assert_eq!(call.args.len(), usize::from(matches!(bcp, 1 | 3 | 5)));
        let cold = ir.inst(ir.terminator(call.targets[1].block).unwrap());
        let AuxData::TransferSite { origin_bcp, scopes } = &cold.aux else {
            panic!("exact exceptional provenance")
        };
        assert_eq!(*origin_bcp, bcp);
        assert_eq!(scopes.len(), bindings);
        assert!(
            scopes
                .iter()
                .all(|scope| scope.ownership == Ownership::Local
                    && matches!(scope.kind, ScopeKind::SpecialBinding { .. }))
        );
    }
    assert!(
        !ir.block_order()
            .iter()
            .flat_map(|&b| &ir.block(b).insts)
            .any(|&inst| ir.inst(inst).opcode == Opcode::ClearMv),
        "binding and value operations preserve MV"
    );
    let osr = ir.osr_entries.iter().find(|entry| entry.bcp == 4).unwrap();
    assert_eq!(osr.control_scopes.len(), 2);
    assert_eq!(osr.control_scopes[0].push_bcp, 1);
    assert_eq!(osr.control_scopes[1].push_bcp, 3);

    let enter = calls
        .iter()
        .copied()
        .find(|&inst| {
            ir.frame_states
                .get(ir.inst(inst).frame_state.unwrap())
                .scopes[0]
                .bcp
                == 1
        })
        .unwrap();
    let mut malformed = ir.clone();
    malformed.inst_mut(enter).args.clear();
    assert!(
        verify::verify(&malformed).is_err(),
        "binding entry needs its rooted value"
    );
    let load = calls
        .iter()
        .copied()
        .find(|&inst| {
            ir.frame_states
                .get(ir.inst(inst).frame_state.unwrap())
                .scopes[0]
                .bcp
                == 4
        })
        .unwrap();
    let mut malformed = ir.clone();
    malformed.inst_mut(load).args = ir.inst(enter).args.clone();
    assert!(
        verify::verify(&malformed).is_err(),
        "global read must not consume a stack value"
    );
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn native_special_operations_emit_published_requests_and_scoped_osr() {
    let body = special_body();
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    let (code, sites) =
        egcl_compiler::t2::emit::emit_framed_native_handlers_with_poll(&ir, 1, 2, 1, 1, 1, 1, 1, 1)
            .expect("all special operations use native published helper requests");
    assert!(code.osr_entries.iter().any(|(bcp, _)| *bcp == 4));
    let x = egcl_rt::symbols::intern("*NATIVE-SPECIAL-X*");
    let y = egcl_rt::symbols::intern("*NATIVE-SPECIAL-Y*");
    for request in [
        (11_u64 << 32) | 1,
        (11_u64 << 32) | 3,
        (12_u64 << 32) | 9,
        (13_u64 << 32) | u64::from(x),
        (13_u64 << 32) | u64::from(y),
        (14_u64 << 32) | u64::from(y),
    ] {
        assert!(
            code.code
                .windows(8)
                .any(|word| word == request.to_le_bytes()),
            "missing helper request {request:x}"
        );
    }
    assert!(
        sites
            .sites()
            .any(|site| site.map().origin_bcp == 9 && site.map().control_scopes.len() == 2)
    );
}

#[test]
fn native_special_global_failure_retains_binding_until_outer_cleanup() {
    let mut body = special_body();
    let symbol = egcl_rt::symbols::intern("*NATIVE-SPECIAL-X*");
    body.code = vec![
        Instr::PushUnwind {
            cleanup_bcp: 9,
            sp_restore: 0,
        },
        Instr::LoadLocal(0),
        Instr::BindSpecial(symbol),
        Instr::LoadGlobal(symbol),
        Instr::StoreLocal(0),
        Instr::UnbindSpecial(1),
        Instr::LoadLocal(0),
        Instr::PopHandler,
        Instr::EnterCleanupNormal {
            cleanup_bcp: 9,
            resume_bcp: 12,
        },
        Instr::LoadGlobal(symbol),
        Instr::Pop,
        Instr::CleanupReturn,
        Instr::Return,
    ];
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&ir).unwrap();
    for (bcp, expected_special) in [(3, true), (9, false)] {
        let call = ir
            .block_order()
            .iter()
            .flat_map(|&block| &ir.block(block).insts)
            .map(|&inst| ir.inst(inst))
            .find(|call| {
                call.opcode == Opcode::Invoke
                    && ir.frame_states.get(call.frame_state.unwrap()).scopes[0].bcp == bcp
            })
            .unwrap();
        let cold = ir.inst(ir.terminator(call.targets[1].block).unwrap());
        let AuxData::TransferSite { scopes, .. } = &cold.aux else {
            panic!("cold transfer scope map")
        };
        assert_eq!(
            scopes
                .iter()
                .any(|scope| matches!(scope.kind, ScopeKind::SpecialBinding { .. })),
            expected_special
        );
        if expected_special {
            assert!(
                !cold.targets.is_empty(),
                "failing global read retains outer cleanup route"
            );
        }
    }
}
