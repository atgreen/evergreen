//! Regression tests for the split native poll/return contract.
//!
//! The poll-enabled native-transfer emitter must carry an independent poll
//! veneer.  It must not grow the old successful-return transfer check, whose
//! address is intentionally absent from this API.

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::build::build_from_bytecode_for_transfers;
use egcl_compiler::t2::emit::emit_framed_native_handlers_with_poll;
use egcl_compiler::t2::emit::emit_framed_with_activation_slots;
use egcl_rt::bytecode::{BytecodeFunction, Instr};
use egcl_rt::value::{EgclVal, NIL};

fn bytecode_fn() -> BytecodeFunction {
    let sym = egcl_rt::symbols::intern("NATIVE-POLL-ABI-CALLEE");
    let mut code = Vec::with_capacity(150);
    for _ in 0..70 {
        code.push(Instr::CallNamed { sym, nargs: 0 });
        code.push(Instr::Pop);
    }
    code.extend([Instr::CallNamed { sym, nargs: 0 }, Instr::Return]);
    BytecodeFunction {
        code,
        constants: vec![EgclVal::from_fixnum(1)],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 0,
        max_stack: 1,
        arity: 0,
        name: "NATIVE-POLL-ABI".into(),
        params_form: NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
    }
}

fn legacy_bytecode_fn() -> BytecodeFunction {
    let sym = egcl_rt::symbols::intern("NATIVE-POLL-ABI-LEGACY-CALLEE");
    BytecodeFunction {
        code: vec![Instr::CallNamed { sym, nargs: 0 }, Instr::Return],
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
        n_locals: 0,
        max_stack: 1,
        arity: 0,
        name: "NATIVE-POLL-ABI-LEGACY".into(),
        params_form: NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
    }
}

fn call_free_loop_fn() -> BytecodeFunction {
    // The loop has no bytecode call.  Its only call is the native poll veneer
    // inserted by the transfer emitter, which must still receive an aligned
    // SysV stack.
    BytecodeFunction {
        code: vec![
            Instr::Const(0),
            Instr::Br(2),
            Instr::Const(0),
            Instr::BrIfFalse(5),
            Instr::Br(2),
            Instr::Const(0),
            Instr::Return,
        ],
        constants: vec![EgclVal::from_fixnum(1)],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 0,
        max_stack: 1,
        arity: 0,
        name: "NATIVE-POLL-ALIGNMENT".into(),
        params_form: NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
    }
}

#[test]
fn poll_enabled_native_emission_has_no_success_return_check() {
    let function = build_from_bytecode_for_transfers(&bytecode_fn()).expect("build bytecode");
    let poll = 0x5555_6666_7777_8888_u64;
    let legacy_pending = 0xdead_beef_cafe_babe_u64;
    let (framed, _) = emit_framed_native_handlers_with_poll(
        &function,
        0x1111_2222_3333_4444,
        8,
        0x10,
        0x20,
        0x30,
        0x40,
        0x50,
        poll,
    )
    .expect("emit poll-enabled native body");

    // The poll veneer is an independent hook.  This test uses a body without
    // a loop, so its presence is represented in the transfer metadata rather
    // than in an emitted back-edge; the emitter must still retain it.
    assert!(framed.code.len() > 0);
    assert!(framed.emitted_safepoints > 0);

    // The old emitter does encode the checked-return target when one is
    // supplied.  The poll-enabled transfer emitter must not copy that ABI
    // into its call sites.
    let legacy_function = egcl_compiler::t2::build::build_from_bytecode(&legacy_bytecode_fn())
        .expect("build legacy bytecode");
    let legacy = emit_framed_with_activation_slots(
        &legacy_function,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        8,
        9,
        legacy_pending,
        0,
        8,
        None,
    )
    .expect("emit checked legacy body");
    assert!(legacy
        .code
        .windows(legacy_pending.to_le_bytes().len())
        .any(|w| w == legacy_pending.to_le_bytes()));

    // This API has no successful-return transfer address parameter.  A
    // sentinel chosen outside the call/poll addresses must therefore never be
    // embedded as an immediate transfer-check target.
    let forbidden = legacy_pending.to_le_bytes();
    assert!(!framed.code.windows(forbidden.len()).any(|w| w == forbidden));
}

#[test]
fn call_free_poll_loop_emits_an_aligned_call_frame() {
    let function = build_from_bytecode_for_transfers(&call_free_loop_fn()).expect("build loop");
    egcl_compiler::t2::verify::verify(&function).expect("verify loop");
    let (framed, _) = emit_framed_native_handlers_with_poll(
        &function,
        0x1111_2222_3333_4444,
        8,
        0x10,
        0x20,
        0x30,
        0x40,
        0x50,
        0x5555_6666_7777_8888,
    )
    .expect("emit poll loop");
    // No value register needs saving, so the alignment-only prologue is the
    // first instruction: sub rsp, 8.  Before this regression fix the emitter
    // omitted it because the bytecode itself contained no call.
    assert_eq!(&framed.code[..4], &[0x48, 0x83, 0xec, 0x08]);
}
