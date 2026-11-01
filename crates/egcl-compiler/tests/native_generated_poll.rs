// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execute generated poll guards, including each independent reason to go cold.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::emit::{
    NativePollControl, NativePollFastPath, NativeRootPublication,
    emit_framed_native_handlers_with_entries_and_poll_fast_path,
};
use egcl_rt::bytecode::{BytecodeFunction, Instr};
use egcl_rt::exec_context::{ExecutionKind, LispExecutionContext};
use egcl_rt::value::{EgclVal, NIL};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};

thread_local! {
    static CONTROL: Cell<*mut NativePollControl> = const { Cell::new(std::ptr::null_mut()) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
    static COLLECT: Cell<bool> = const { Cell::new(false) };
}
extern "C" fn initialize() -> *mut NativePollControl {
    CONTROL.with(Cell::get)
}
extern "C" fn unrelated_helper(_: *mut u64) -> u64 {
    NIL.0
}
extern "C" fn poll(_: *mut u64) -> u64 {
    use egcl_rt::{Collector, HeapCollector};
    CALLS.with(|calls| calls.set(calls.get() + 1));
    if COLLECT.with(Cell::get) {
        HeapCollector::new().minor_gc().unwrap();
    }
    NIL.0
}

#[test]
fn generated_loop_poll_checks_each_pending_source_and_reloads_moved_roots() {
    let execution = LispExecutionContext::new(ExecutionKind::NativeThread, 1, 1);
    assert!(
        execution.begin_poll(),
        "new execution must enter through a poll"
    );
    let observer = Cell::new(0usize);
    let skips = Cell::new(0usize);
    let mut control = NativePollControl {
        execution: &execution,
        seen_carrier: 1,
        seen_signal_epoch: 0,
        deadline_armed: 0,
        reserved: [0; 7],
        test_observer: observer.as_ptr(),
        test_skips: skips.as_ptr(),
    };
    CONTROL.with(|slot| slot.set(&mut control));
    struct ClearControl;
    impl Drop for ClearControl {
        fn drop(&mut self) {
            CONTROL.with(|slot| slot.set(std::ptr::null_mut()));
        }
    }
    let _clear = ClearControl;
    let gc = AtomicBool::new(false);
    let sigusr1 = AtomicBool::new(false);
    let posted = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);
    let epoch = AtomicU64::new(0);
    let fast = NativePollFastPath {
        initialize: initialize as *const () as u64,
        gc_requested: &gc as *const _ as u64,
        sigusr1_pending: &sigusr1 as *const _ as u64,
        posted_signal: &posted as *const _ as u64,
        native_errors: &errors as *const _ as u64,
        signal_epoch: &epoch as *const _ as u64,
        observe_forces_cold: true,
        instrument_skips: true,
    };
    let source = BytecodeFunction {
        self_call_sites: Vec::new(),
        notinline_sites: Vec::new(),
        code: vec![
            Instr::Br(1),
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Br(1),
            Instr::LoadLocal(0),
            Instr::Return,
        ],
        constants: vec![NIL],
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
        name: "GENERATED-POLL".into(),
        params_form: NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
        binder_cache: Default::default(),
        closure_template: Default::default(),
    };
    let mut function =
        egcl_compiler::t2::build::build_from_bytecode_for_transfers(&source).unwrap();
    for block in function.block_order().to_vec() {
        let instructions = function
            .block(block)
            .insts
            .iter()
            .copied()
            .filter(|&inst| function.inst(inst).opcode != egcl_compiler::t2::ir::Opcode::ClearMv)
            .collect();
        function.block_mut(block).insts = instructions;
    }
    let helper = unrelated_helper as *const () as u64;
    let (framed, _) = emit_framed_native_handlers_with_entries_and_poll_fast_path(
        &function,
        helper,
        source.num_slots(),
        helper,
        helper,
        helper,
        helper,
        helper,
        poll as *const () as u64,
        None,
        None,
        &[],
        NativeRootPublication::ActivationShadows,
        Some(fast),
    )
    .unwrap();
    assert!(
        framed
            .native_calls
            .as_ref()
            .unwrap()
            .iter()
            .any(|site| matches!(
                site.origin,
                egcl_compiler::t2::x64_calls::NativeCallOrigin::LoopPoll(_)
            ))
    );
    for address in [
        fast.gc_requested,
        fast.sigusr1_pending,
        fast.posted_signal,
        fast.native_errors,
        fast.signal_epoch,
    ] {
        assert!(
            !framed
                .code
                .windows(8)
                .any(|bytes| bytes == address.to_le_bytes()),
            "generated poll must observe execution-local work, not a process-global source"
        );
    }
    let buffer = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
    egcl_rt::rooted!(
        activation = vec![NIL; usize::from(source.num_slots() + framed.shadow_root_slots)]
    );
    activation[0] = EgclVal::from_fixnum(37);
    let mut check = |cold: bool| {
        CALLS.with(|calls| calls.set(0));
        assert_eq!(run(activation.as_mut_ptr().cast()), activation[0].to_raw());
        assert_eq!(CALLS.with(Cell::get) != 0, cold, "skips={}", skips.get());
    };
    check(false);
    assert!(skips.get() > 0);
    control.test_skips = std::ptr::null_mut();
    check(true);
    control.test_skips = skips.as_ptr();
    // Other executions' work does not force this execution into the runtime.
    let other = LispExecutionContext::new(ExecutionKind::NativeThread, 2, 2);
    assert!(other.poll_pending());
    check(false);
    execution.request_poll();
    check(true);
    assert!(execution.begin_poll());
    check(false);
    assert!(
        other.poll_pending(),
        "draining this execution cannot acknowledge another"
    );
    execution.request_yield();
    check(true);
    execution.take_yield_request();
    execution.begin_poll();
    execution.set_carrier_id(2);
    check(true);
    execution.begin_poll();
    check(false);
    observer.set(1);
    check(true);
    observer.set(0);
    CONTROL.with(|slot| slot.set(std::ptr::null_mut()));
    check(true);
    CONTROL.with(|slot| slot.set(&mut control));
    check(false);

    // The cold edge must retain the existing synchronization/reload contract.
    let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(123.5);
    }
    egcl_rt::rooted!(expected = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
    activation[0] = *expected;
    let old = expected.to_raw();
    execution.request_poll();
    COLLECT.with(|slot| slot.set(true));
    let result = run(activation.as_mut_ptr().cast());
    COLLECT.with(|slot| slot.set(false));
    assert_ne!(
        expected.to_raw(),
        old,
        "cold poll must actually relocate the live value"
    );
    assert_eq!(
        result,
        expected.to_raw(),
        "native return must use the updated home"
    );
}
