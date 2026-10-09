// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

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
    assert!(!framed.code.is_empty());
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

#[test]
fn native_poll_preserves_immediate_live_values() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static POLLS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn clear_mv() -> u64 {
        NIL.0
    }
    extern "C" fn poll(_frame: *mut u64) -> u64 {
        POLLS.fetch_add(1, Ordering::Relaxed);
        // A runtime call may overwrite every SysV caller-saved register.
        unsafe {
            core::arch::asm!(
                "xor rdi, rdi", "xor rsi, rsi", "xor rdx, rdx",
                "xor rcx, rcx", "xor r8, r8", "xor r9, r9",
                "xor r10, r10", "xor r11, r11",
                out("rdi") _, out("rsi") _, out("rdx") _, out("rcx") _,
                out("r8") _, out("r9") _, out("r10") _, out("r11") _,
                options(nomem, nostack)
            );
        }
        NIL.0
    }
    let loop_code = vec![
        Instr::Br(1),
        Instr::Const(0),
        Instr::BrIfFalse(4),
        Instr::Br(1),
        Instr::LoadLocal(0),
        Instr::Return,
    ];
    let loop_phi_code = vec![
        Instr::Br(1),
        Instr::Const(0),
        Instr::BrIfFalse(6),
        Instr::Const(0),
        Instr::StoreLocal(0),
        Instr::Br(1),
        Instr::LoadLocal(0),
        Instr::Return,
    ];
    let mut straight_code = Vec::new();
    for _ in 0..65 {
        straight_code.extend([
            Instr::LoadLocal(0),
            Instr::TypeP(egcl_rt::bytecode::typep_class::BOOLEAN),
            Instr::Pop,
        ]);
    }
    straight_code.extend([Instr::LoadLocal(0), Instr::Return]);
    for (shape, code) in [
        ("loop live-in", loop_code),
        ("loop phi", loop_phi_code),
        ("straight-line", straight_code),
    ] {
        let mut bytecode = call_free_loop_fn();
        bytecode.code = code;
        bytecode.constants = vec![NIL];
        bytecode.n_locals = 1;
        bytecode.arity = 1;
        bytecode.min_args = 1;
        bytecode.max_args = Some(1);
        let mut function = build_from_bytecode_for_transfers(&bytecode).expect("build poll probe");
        // Test the emitter with pure checks: the bytecode builder also adds
        // ClearMv calls for Lisp TYPEP semantics, which would make allocation
        // account for ordinary calls and hide the inserted-poll clobber.
        for block in function.block_order().to_vec() {
            let pure_instructions = function.block(block).insts.iter().copied()
                .filter(|&inst| {
                    function.inst(inst).opcode != egcl_compiler::t2::ir::Opcode::ClearMv
                })
                .collect();
            function.block_mut(block).insts = pure_instructions;
        }
        for param in function.block(function.entry()).params.clone() {
            function.refine_type(
                param,
                egcl_compiler::t2::ir::IRType::of(egcl_compiler::t2::ir::TypeBits::FIXNUM),
            );
        }
        egcl_compiler::t2::verify::verify(&function).expect("verify poll probe");
        let helper = poll as *const () as usize as u64;
        let (framed, _) = emit_framed_native_handlers_with_poll(
            &function,
            helper,
            bytecode.num_slots(),
            helper,
            helper,
            clear_mv as *const () as usize as u64,
            helper,
            helper,
            helper,
        )
        .expect("emit poll probe");
        let buffer = egcl_rt::jit::JitBuffer::new(&framed.code).expect("executable memory");
        let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
        let expected = EgclVal::from_fixnum(42);
        let mut frame = vec![NIL.0; usize::from(bytecode.num_slots() + framed.shadow_root_slots)];
        frame[0] = expected.0;
        POLLS.store(0, Ordering::Relaxed);
        assert_eq!(run(frame.as_mut_ptr()), expected.0, "{shape}");
        assert!(
            POLLS.load(Ordering::Relaxed) > 0,
            "{shape} probe must execute a poll"
        );
    }
}

#[test]
fn inserted_polls_relocate_live_heap_values() {
    use egcl_rt::{Collector, HeapCollector};
    extern "C" fn collect(_: *mut u64) -> u64 {
        HeapCollector::new().minor_gc().unwrap();
        unsafe {
            core::arch::asm!("xor rcx,rcx", "xor rsi,rsi", "xor r8,r8",
                out("rcx") _, out("rsi") _, out("r8") _, options(nomem, nostack));
        }
        NIL.0
    }
    let mut straight = Vec::new();
    for _ in 0..65 {
        straight.extend([
            Instr::LoadLocal(0),
            Instr::TypeP(egcl_rt::bytecode::typep_class::BOOLEAN),
            Instr::Pop,
        ]);
    }
    straight.extend([Instr::LoadLocal(0), Instr::Return]);
    for code in [
        vec![
            Instr::Br(1),
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Br(1),
            Instr::LoadLocal(0),
            Instr::Return,
        ],
        straight,
    ] {
        let mut source = call_free_loop_fn();
        source.code = code;
        source.constants = vec![NIL];
        source.n_locals = 1;
        source.arity = 1;
        source.min_args = 1;
        source.max_args = Some(1);
        let mut function = build_from_bytecode_for_transfers(&source).unwrap();
        for block in function.block_order().to_vec() {
            let instructions = function
                .block(block)
                .insts
                .iter()
                .copied()
                .filter(|&inst| {
                    function.inst(inst).opcode != egcl_compiler::t2::ir::Opcode::ClearMv
                })
                .collect();
            function.block_mut(block).insts = instructions;
        }
        let helper = collect as *const () as u64;
        let (framed, _) = emit_framed_native_handlers_with_poll(
            &function,
            helper,
            source.num_slots(),
            helper,
            helper,
            helper,
            helper,
            helper,
            helper,
        )
        .unwrap();
        assert!(framed.shadow_root_slots > 0);
        let buffer = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
        let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
        egcl_rt::rooted!(
            activation = vec![NIL; usize::from(source.num_slots() + framed.shadow_root_slots)]
        );
        let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
        unsafe {
            body.cast::<f64>().write(123.5);
        }
        egcl_rt::rooted!(expected = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
        activation[0] = *expected;
        let original = expected.to_raw();
        let result = run(activation.as_mut_ptr().cast());
        assert_ne!(
            expected.to_raw(),
            original,
            "the poll must relocate a live root"
        );
        assert_eq!(
            result,
            expected.to_raw(),
            "return uses the moved native value"
        );
        assert_eq!(expected.as_double_float(), 123.5);
    }
}
