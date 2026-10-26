// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use egcl_compiler::t2::{emit::*, frame_state::*, ir::*};
use egcl_rt::{
    call_table::*,
    exec_context::{ExecutionKind, LispExecutionContext},
    jit::JitBuffer,
    value::NIL,
};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicU64, AtomicUsize, Ordering};

// SysV adapter used only by this test: 7 actual tagged arguments, full ordinary
// register contract, and writable output independent of the callee's registers.
#[unsafe(naked)]
unsafe extern "C" fn run(
    _entry: *const u8,
    _args: *const u64,
    _count: usize,
    _execution: *const LispExecutionContext,
    _out: *mut u64,
) {
    core::arch::naked_asm!(
        "push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
        "sub rsp, 40", "mov [rsp+32], r8", "mov r11, rdi", "mov r10, rsi",
        "mov r13, rcx", "mov rcx, rdx", "mov rax, [r10+24]", "mov [rsp], rax",
        "mov rax, [r10+32]", "mov [rsp+8], rax", "mov rax, [r10+40]", "mov [rsp+16], rax",
        "mov rax, [r10+48]", "mov [rsp+24], rax",
        "mov rdi, [r10]", "mov rsi, [r10+8]", "mov rdx, [r10+16]",
        "mov rax, {nil}", "call r11", "mov r10, [rsp+32]", "mov [r10], rax",
        "mov [r10+8], rdx", "mov [r10+16], rcx", "setc byte ptr [r10+24]",
        "add rsp, 40", "pop r15", "pop r14", "pop r13", "pop r12", "pop rbx", "pop rbp", "ret",
        nil = const NIL.0,
    );
}
#[unsafe(naked)]
unsafe extern "C" fn run_with_stack_bound(
    _entry: *const u8, _args: *const u64, _count: usize,
    _execution: *const LispExecutionContext, _out: *mut u64, _distance: usize,
) {
    core::arch::naked_asm!(
        "push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
        "sub rsp, 40", "mov [rsp+32], r8", "mov r11, rdi", "mov r10, rsi",
        "mov r13, rcx", "mov rcx, rdx", "mov rax, [r10+24]", "mov [rsp], rax",
        "mov rax, [r10+32]", "mov [rsp+8], rax", "mov rax, [r10+40]", "mov [rsp+16], rax",
        "mov rax, [r10+48]", "mov [rsp+24], rax",
        "mov rdi, [r10]", "mov rsi, [r10+8]", "mov rdx, [r10+16]",
        // Anchor the synthetic bound to the actual machine call SP, avoiding
        // assumptions about the Rust test function's stack frame.
        "mov r10, [r13+{control}]", "mov rax, rsp", "sub rax, r9",
        "mov [r10+{limit}], rax",
        "mov rax, {nil}", "call r11", "mov r10, [rsp+32]", "mov [r10], rax",
        "mov [r10+8], rdx", "mov [r10+16], rcx", "setc byte ptr [r10+24]",
        "add rsp, 40", "pop r15", "pop r14", "pop r13", "pop r12", "pop rbx", "pop rbp", "ret",
        nil = const NIL.0,
        control = const egcl_rt::exec_context::offsets::ACTIVE_NATIVE_CONTROL,
        limit = const std::mem::offset_of!(NativeRecursiveControl, stack_limit),
    );
}
#[unsafe(naked)]
unsafe extern "C" fn rejected() {
    core::arch::naked_asm!("mov rax, -8", "clc", "ret");
}
unsafe extern "C" fn clear_mv(_primary: u64, _destination: *mut u64, _count: usize) -> u64 {
    CLEAR_MV_CALLS.with(|calls| calls.set(calls.get() + 1));
    0
}
thread_local! {
    static CLEAR_MV_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[test]
fn recursive_single_values_do_not_call_the_mv_clear_helper() {
    CLEAR_MV_CALLS.with(|calls| calls.set(0));
    execute_with_self_calls(&fib(), 1, &[12 * 8, 0, 0, 0, 0, 0, 0], 144 * 8,
        0, None, true);
    assert_eq!(CLEAR_MV_CALLS.with(std::cell::Cell::get), 0,
        "inactive recursive results require no runtime MV reset");
}

#[test]
fn recursive_scalar_results_consume_return_carry_before_clobbering_it() {
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};
    let code = execute_with_self_calls(&fib(), 1, &[12 * 8, 0, 0, 0, 0, 0, 0],
        144 * 8, 0, None, true);
    let instructions: Vec<_> = Decoder::new(64, &code, DecoderOptions::NONE).into_iter().collect();
    let calls: Vec<_> = instructions.iter().enumerate().filter(|(_, instruction)|
        instruction.mnemonic() == Mnemonic::Call && instruction.op0_kind() == OpKind::NearBranch64)
        .map(|(index, _)| index).collect();
    assert_eq!(calls.len(), 2);
    for call in calls {
        let mut index = call + 1;
        loop {
            let instruction = &instructions[index];
            match instruction.mnemonic() {
                Mnemonic::Jae => break, // JNC: consume the ordinary result descriptor.
                Mnemonic::Mov | Mnemonic::Dec | Mnemonic::Lea => index += 1,
                Mnemonic::Jmp => index = instructions.iter().position(|candidate|
                    candidate.ip() == instruction.near_branch_target()).unwrap(),
                unexpected => panic!("{unexpected:?} at {} precedes consumption of return carry",
                    instruction.ip()),
            }
        }
    }
}

#[test]
fn recursive_calls_enter_the_frameless_base_case_prefix() {
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind, Register};
    let code = execute_with_self_calls(&fib(), 1, &[12 * 8, 0, 0, 0, 0, 0, 0],
        144 * 8, 0, None, true);
    let instructions: Vec<_> = Decoder::new(64, &code, DecoderOptions::NONE).into_iter().collect();
    let calls: Vec<_> = instructions.iter().filter(|instruction|
        instruction.mnemonic() == Mnemonic::Call && instruction.op0_kind() == OpKind::NearBranch64)
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].near_branch_target(), calls[1].near_branch_target());
    let start = instructions.iter().position(|instruction|
        instruction.ip() == calls[0].near_branch_target()).unwrap();
    let frame = instructions[start..].iter().position(|instruction|
        instruction.mnemonic() == Mnemonic::Push && instruction.op0_register() == Register::RBP)
        .unwrap() + start;
    assert!(instructions[start..frame].iter().any(|instruction|
        instruction.mnemonic() == Mnemonic::Ret),
        "both recursive edges must enter the base-case return before frame setup");
}

#[test]
fn recursive_admission_tests_flags_without_register_round_trips() {
    use iced_x86::{Decoder, DecoderOptions, Mnemonic};
    let code = execute_with_self_calls(&fib(), 1, &[12 * 8, 0, 0, 0, 0, 0, 0],
        144 * 8, 0, None, true);
    let byte_loads = Decoder::new(64, &code, DecoderOptions::NONE).into_iter()
        .filter(|instruction| instruction.mnemonic() == Mnemonic::Movzx).count();
    assert_eq!(byte_loads, 0, "admission must test byte flags directly in memory");
}

#[test]
fn recursive_admission_observes_execution_poll_requests() {
    for pending in 1..=3 {
        // Both children of fib(2) are refused and return the cold sentinel -1.
        execute_with_self_stack_bound(&fib(), 1, &[2 * 8, 0, 0, 0, 0, 0, 0],
            (-16_i64) as u64, 0, None, true, None, pending);
    }
}
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
fn state(f: &mut Function, x: Value, stack: &[Value], bcp: u32) -> FrameStateId {
    let tagged = |&value| ValueSource::Value {
        value,
        repr: ValueRepresentation::Tagged,
    };
    f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 77,
            bcp,
            locals: vec![tagged(&x)],
            stack: stack.iter().map(tagged).collect(),
        }],
        remat: vec![],
    })
}
fn value(f: &mut Function, block: Block, data: InstData) -> Value {
    f.push_inst(
        block,
        data,
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    )
    .1[0]
}
fn fib() -> Function {
    let mut f = Function::new("ordinary-recursive-fib");
    let entry = f.entry();
    let base = f.make_block();
    let recurse = f.make_block();
    let x = f.add_block_param(
        entry,
        IRType::of(TypeBits::FIXNUM),
        ValueRepresentation::Tagged,
    );
    let one = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
    );
    let two = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(2)),
    );
    let mut comparison = instruction(Opcode::FixnumCmpLt, vec![x, two], AuxData::None);
    comparison.frame_state = Some(state(&mut f, x, &[x, two], 0));
    let test = value(&mut f, entry, comparison);
    let mut branch = instruction(Opcode::Brif, vec![test], AuxData::None);
    branch.targets = vec![
        BlockCall {
            block: base,
            args: vec![],
        },
        BlockCall {
            block: recurse,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    f.set_terminator(base, instruction(Opcode::Return, vec![x], AuxData::None));
    let mut sub = instruction(Opcode::FixnumSub, vec![x, one], AuxData::None);
    sub.frame_state = Some(state(&mut f, x, &[x, one], 1));
    let n1 = value(&mut f, recurse, sub);
    let mut call = instruction(Opcode::Call, vec![n1], AuxData::CallTarget(77));
    call.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    call.frame_state = Some(state(&mut f, x, &[n1], 2));
    let first = value(&mut f, recurse, call);
    let mut clear = instruction(Opcode::ClearMv, vec![], AuxData::None);
    clear.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    f.push_inst(recurse, clear.clone(), &[]);
    let mut sub = instruction(Opcode::FixnumSub, vec![x, two], AuxData::None);
    sub.frame_state = Some(state(&mut f, x, &[first, x, two], 3));
    let n2 = value(&mut f, recurse, sub);
    let mut call = instruction(Opcode::Call, vec![n2], AuxData::CallTarget(77));
    call.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    call.frame_state = Some(state(&mut f, x, &[first, n2], 4));
    let second = value(&mut f, recurse, call);
    let mut add = instruction(Opcode::FixnumAdd, vec![first, second], AuxData::None);
    add.frame_state = Some(state(&mut f, x, &[first, second], 5));
    let sum = value(&mut f, recurse, add);
    f.push_inst(recurse, clear, &[]);
    f.set_terminator(
        recurse,
        instruction(Opcode::Return, vec![sum], AuxData::None),
    );
    f
}

#[test]
fn recursive_fib_executes_using_ordinary_descriptors_and_native_frames() {
    let f = fib();
    egcl_compiler::t2::verify::verify(&f).unwrap();
    execute(&f, 1, &[24 << 3, 0, 0, 0, 0, 0, 0], 46368 << 3);
}

fn execute(f: &Function, arity: usize, args: &[u64; 7], expected: u64) {
    execute_with_poll(f, arity, args, expected, 0);
}

fn execute_with_poll(f: &Function, arity: usize, args: &[u64; 7], expected: u64, poll: u64) {
    execute_with_heat(f, arity, args, expected, poll, None);
}
fn execute_with_heat(f: &Function, arity: usize, args: &[u64; 7], expected: u64, poll: u64,
    heat: Option<(&AtomicU32, u32, &AtomicU32, u32, bool)>) {
    execute_with_self_calls(f, arity, args, expected, poll, heat, false);
}
#[allow(clippy::too_many_arguments)]
fn execute_with_self_calls(f: &Function, arity: usize, args: &[u64; 7], expected: u64, poll: u64,
    heat: Option<(&AtomicU32, u32, &AtomicU32, u32, bool)>, direct_self: bool) -> Vec<u8> {
    execute_with_self_stack_bound(f, arity, args, expected, poll, heat, direct_self, None, 0)
}
#[allow(clippy::too_many_arguments)]
fn execute_with_self_stack_bound(f: &Function, arity: usize, args: &[u64; 7], expected: u64, poll: u64,
    heat: Option<(&AtomicU32, u32, &AtomicU32, u32, bool)>, direct_self: bool, below_bottom: Option<i64>, pending: u8) -> Vec<u8> {
    let heat_slot = AtomicUsize::new(0);
    let slot = AtomicUsize::new(0);
    let revision = AtomicU64::new(7);
    let bytes = [AtomicU8::new(0), AtomicU8::new(0)];
    let words = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
    let flags = OrdinaryCallFlags {
        gc_requested: bytes[0].as_ptr() as u64,
        sigusr1_pending: bytes[1].as_ptr() as u64,
        posted_signal: words[0].as_ptr() as u64,
        native_errors: words[1].as_ptr() as u64,
        signal_epoch: words[2].as_ptr() as u64,
    };
    let targets: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter())
        .filter(|&&inst| f.inst(inst).opcode == Opcode::Call)
        .map(|&inst| {
            (
                inst,
                OrdinaryCallTarget {
                    symbol: 77,
                    descriptor_slot: slot.as_ptr() as u64,
                    revision_address: revision.as_ptr() as u64, heat_slot: heat_slot.as_ptr() as u64,
                },
            )
        })
        .collect();
    let self_sites = if direct_self { targets.iter().map(|(inst, _)| *inst).collect() } else { Vec::new() };
    let code = emit_framed_ordinary_function_with_self_calls(
        f,
        OrdinaryLispEntryOptions {
            arity,
            arity_error_veneer: rejected as *const () as u64, binding: None,
        },
        &targets,
        flags,
        rejected as *const () as u64,
        OrdinaryGuardOptions {
            veneer: rejected as *const () as u64,
            multiple_values: clear_mv as *const () as u64,
        },
        poll,
        &self_sites,
    )
    .unwrap();
    assert!(code.frame_entry.is_none());
    let buffer = JitBuffer::new(&code.code).unwrap();
    let entry = unsafe { buffer.as_ptr().add(code.ordinary_entry.unwrap()) };
    let descriptor = OrdinaryEntryDescriptor {
        kind: ORDINARY_X64_LISP_KIND,
        version: ORDINARY_X64_LISP_VERSION,
        revision: 7,
        entry: entry as usize,
        code_start: buffer.as_ptr() as usize,
        code_len: code.code.len(),
        arity,
        environment: ORDINARY_ENV_NONE,
        owner: buffer.as_ptr() as usize,
        stack_required: code.ordinary_stack_required.unwrap(),
    };
    let heat_record = heat.map(|(calls, threshold, backedges, backedge_threshold, stale)| OrdinaryCallHeat {
        descriptor: if stale { 1 } else { &descriptor as *const _ as usize },
        invocation_counter: calls.as_ptr() as usize,
        back_edge_counter: backedges.as_ptr() as usize,
        invocation_threshold: threshold,
        back_edge_threshold: backedge_threshold,
    });
    if let Some(record) = &heat_record {
        heat_slot.store(record as *const _ as usize, Ordering::Release);
    }
    if !direct_self {
        slot.store(&descriptor as *const _ as usize, Ordering::Release);
    }
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
    // The runtime consumes the initially armed poll before entering native code.
    execution.begin_poll();
    let mut depth = 1;
    let active = AtomicBool::new(false);
    let mut control = NativeRecursiveControl {
        execution: execution.as_ptr(),
        depth: &mut depth,
        reserved_counter: 0,
        mv_active: active.as_ptr(),
        expected_generation: 7,
        seen_carrier: 2,
        seen_signal_epoch: 0,
        depth_limit: 100,
        anchor_depth: 1,
        deadline_armed: 0,
        reserved: [0; 7],
        test_entries: std::ptr::null_mut(),
        test_observer: std::ptr::null(),
        stack_limit: 0,
    };
    match pending {
        0 => {},
        // Producers are tested in the runtime; generated code sees only this
        // execution's poll, including requests made by mount and explicit yield.
        1 => execution.request_poll(),
        2 => execution.set_carrier_id(3),
        3 => execution.request_yield(),
        _ => unreachable!(),
    }
    unsafe {
        execution.swap_active_native_control(&mut control as *mut _ as u64);
    }
    let mut out = [0; 4];
    unsafe {
        if let Some(below_bottom) = below_bottom {
            assert_eq!(arity, 1, "boundary fixture has no outgoing overflow arguments");
            let parent_frame = code.native_calls.as_ref().unwrap().unwind_recipe().unwrap().frame_bytes() as usize;
            // Root CALL + parent prologue + child CALL + complete child budget.
            let distance = (16 + parent_frame + code.ordinary_stack_required.unwrap()) as i64 + below_bottom;
            run_with_stack_bound(entry, args.as_ptr(), arity, execution.as_ptr(), out.as_mut_ptr(),
                usize::try_from(distance).unwrap());
        } else { run(
            entry,
            args.as_ptr(),
            arity,
            execution.as_ptr(),
            out.as_mut_ptr(),
        ); }
    }
    assert_eq!(out[0], expected);
    assert_eq!(out[3], 0, "exactly one value returns with clear carry");
    assert_eq!(depth, 1, "every recursive child releases native depth");
    code.code
}

#[test]
fn seven_argument_entry_reads_all_overflow_words() {
    for returned in 0..7 {
        let mut f = Function::new("ordinary-seven-arguments");
        let entry = f.entry();
        let params: Vec<_> = (0..7)
            .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        f.set_terminator(
            entry,
            instruction(Opcode::Return, vec![params[returned]], AuxData::None),
        );
        let args = [8, 16, 24, 32, 40, 48, 56];
        execute(&f, 7, &args, args[returned]);
    }
}

#[test]
fn seven_argument_recursive_call_marshals_every_overflow_word() {
    let mut f = Function::new("ordinary-seven-argument-call");
    let entry = f.entry();
    let base = f.make_block();
    let recurse = f.make_block();
    let args: Vec<_> = (0..7)
        .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    let zero = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(0)),
    );
    let condition = value(
        &mut f,
        entry,
        instruction(Opcode::GenericEq, vec![args[0], zero], AuxData::None),
    );
    let mut branch = instruction(Opcode::Brif, vec![condition], AuxData::None);
    branch.targets = vec![
        BlockCall {
            block: base,
            args: vec![],
        },
        BlockCall {
            block: recurse,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    f.set_terminator(
        base,
        instruction(Opcode::Return, vec![args[6]], AuxData::None),
    );
    let mut call = instruction(
        Opcode::Call,
        vec![zero, args[6], args[5], args[4], args[3], args[2], args[1]],
        AuxData::CallTarget(77),
    );
    call.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    call.frame_state = Some(f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 77, bcp: 0,
            locals: args.iter().map(|&value| ValueSource::Value {
                value, repr: ValueRepresentation::Tagged,
            }).collect(),
            stack: call.args.iter().map(|&value| ValueSource::Value {
                value, repr: ValueRepresentation::Tagged,
            }).collect(),
        }], remat: vec![],
    }));
    let result = value(&mut f, recurse, call);
    f.set_terminator(
        recurse,
        instruction(Opcode::Return, vec![result], AuxData::None),
    );
    execute(&f, 7, &[8, 16, 24, 32, 40, 48, 56], 16);
    execute_with_self_calls(&f, 7, &[8, 16, 24, 32, 40, 48, 56], 16, 0, None, true);
}

#[test]
fn ordinary_entry_rejects_unpolled_cycles() {
    let mut f = Function::new("unpolled-cycle");
    let entry = f.entry();
    let mut jump = instruction(Opcode::Jump, vec![], AuxData::None);
    jump.targets = vec![BlockCall {
        block: entry,
        args: vec![],
    }];
    f.set_terminator(entry, jump);
    let result = emit_framed_ordinary_function(
        &f,
        OrdinaryLispEntryOptions {
            arity: 0,
            arity_error_veneer: 1, binding: None,
        },
        &[],
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
    assert!(result.is_err(), "cycles require lifecycle polls");
}

#[test]
fn call_free_guarded_arithmetic_uses_precise_ordinary_recovery() {
    let mut f = Function::new("ordinary-guarded-leaf");
    let entry = f.entry();
    let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let one = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
    );
    let mut add = instruction(Opcode::FixnumAdd, vec![x, one], AuxData::None);
    add.frame_state = Some(state(&mut f, x, &[x, one], 0));
    let result = value(&mut f, entry, add);
    f.set_terminator(
        entry,
        instruction(Opcode::Return, vec![result], AuxData::None),
    );
    execute(&f, 1, &[40 << 3, 0, 0, 0, 0, 0, 0], 41 << 3);
}

#[test]
fn call_free_loop_runs_with_native_poll_sites() {
    let mut f = Function::new("ordinary-polled-countdown");
    let entry = f.entry();
    let header = f.make_block();
    let body = f.make_block();
    let done = f.make_block();
    let initial = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let remaining = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
    let zero = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(0)),
    );
    let one = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
    );
    let answer = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(42)),
    );
    let mut start = instruction(Opcode::Jump, vec![], AuxData::None);
    start.targets = vec![BlockCall {
        block: header,
        args: vec![initial],
    }];
    f.set_terminator(entry, start);
    let condition = value(
        &mut f,
        header,
        instruction(Opcode::GenericEq, vec![remaining, zero], AuxData::None),
    );
    let mut branch = instruction(Opcode::Brif, vec![condition], AuxData::None);
    branch.targets = vec![
        BlockCall {
            block: done,
            args: vec![],
        },
        BlockCall {
            block: body,
            args: vec![],
        },
    ];
    f.set_terminator(header, branch);
    let mut subtract = instruction(Opcode::FixnumSub, vec![remaining, one], AuxData::None);
    subtract.frame_state = Some(state(&mut f, initial, &[remaining, one], 1));
    let next = value(&mut f, body, subtract);
    let mut repeat = instruction(Opcode::Jump, vec![], AuxData::None);
    repeat.targets = vec![BlockCall {
        block: header,
        args: vec![next],
    }];
    f.set_terminator(body, repeat);
    f.set_terminator(
        done,
        instruction(Opcode::Return, vec![answer], AuxData::None),
    );
    static POLLS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn poll(_activation: *mut u8) -> u64 {
        POLLS.fetch_add(1, Ordering::Relaxed);
        0
    }
    POLLS.store(0, Ordering::Relaxed);
    execute_with_poll(
        &f,
        1,
        &[24 << 3, 0, 0, 0, 0, 0, 0],
        42 << 3,
        poll as *const () as u64,
    );
    assert!(
        POLLS.load(Ordering::Relaxed) >= 25,
        "every backedge must cross a poll"
    );
}

#[test]
fn three_named_calls_share_the_ordinary_entry_contract() {
    let mut f = Function::new("ordinary-three-calls");
    let entry = f.entry();
    let base = f.make_block();
    let caller = f.make_block();
    let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let zero = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(0)),
    );
    let one = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
    );
    let condition = value(
        &mut f,
        entry,
        instruction(Opcode::GenericEq, vec![x, zero], AuxData::None),
    );
    let mut branch = instruction(Opcode::Brif, vec![condition], AuxData::None);
    branch.targets = vec![
        BlockCall {
            block: base,
            args: vec![],
        },
        BlockCall {
            block: caller,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    f.set_terminator(base, instruction(Opcode::Return, vec![one], AuxData::None));
    let mut result = one;
    for count in 0..3 {
        let mut call = instruction(Opcode::Call, vec![zero], AuxData::CallTarget(77));
        call.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..Default::default()
        };
        result = value(&mut f, caller, call);
        if count < 2 {
            let mut clear = instruction(Opcode::ClearMv, vec![], AuxData::None);
            clear.flags = InstFlags {
                call: true,
                safepoint: true,
                effectful: true,
                ..Default::default()
            };
            f.push_inst(caller, clear, &[]);
        }
    }
    f.set_terminator(
        caller,
        instruction(Opcode::Return, vec![result], AuxData::None),
    );
    execute(&f, 1, &[8, 0, 0, 0, 0, 0, 0], 8);
}

#[test]
fn ordinary_direct_calls_count_exact_admissions_and_preserve_promotion() {
    let calls = AtomicU32::new(0);
    let backedges = AtomicU32::new(0);
    let f = fib();
    let args = [2 << 3, 0, 0, 0, 0, 0, 0];
    execute_with_heat(&f, 1, &args, 1 << 3, 0, Some((&calls, 100, &backedges, 100, false)));
    assert_eq!(calls.load(Ordering::Relaxed), 2, "both direct children record invocation heat");
    calls.store(0, Ordering::Relaxed);
    execute_with_heat(&f, 1, &args, 0, 0, Some((&calls, 2, &backedges, 100, false)));
    assert_eq!(calls.load(Ordering::Relaxed), 1, "threshold edge delegates counting to cold dispatch");
    calls.store(0, Ordering::Relaxed);
    execute_with_heat(&f, 1, &args, 1 << 3, 0, Some((&calls, 1, &backedges, 100, true)));
    assert_eq!(calls.load(Ordering::Relaxed), 0, "stale descriptor cannot access old heat");
    backedges.store(10, Ordering::Relaxed);
    execute_with_heat(&f, 1, &args, (-2_i64 << 3) as u64, 0, Some((&calls, 100, &backedges, 10, false)));
    assert_eq!(calls.load(Ordering::Relaxed), 0, "hot loops preserve the promotion cold edge");
}

#[test]
fn direct_recursive_fib_does_not_resolve_a_global_descriptor() {
    let f = fib();
    // The global slot stays empty. Every recursive edge must use the exact
    // current artifact, with balanced native depth and actual ordinary returns.
    execute_with_self_calls(&f, 1, &[24 << 3, 0, 0, 0, 0, 0, 0], 46368 << 3, 0, None, true);
}

#[test]
fn direct_recursive_fib_does_not_charge_global_invocation_heat() {
    let calls = AtomicU32::new(17);
    let backedges = AtomicU32::new(23);
    execute_with_self_calls(&fib(), 1, &[12 << 3, 0, 0, 0, 0, 0, 0], 144 << 3,
        0, Some((&calls, 1, &backedges, 1, false)), true);
    assert_eq!(calls.load(Ordering::Relaxed), 17);
    assert_eq!(backedges.load(Ordering::Relaxed), 23);
}

#[test]
fn direct_self_stack_admission_checks_exact_child_budget_and_balances_depth() {
    let f = fib();
    // fib(6)'s first child fib(5) requires a frame; its grandchildren and the
    // second child fib(4) are folded. Admit the first child for 5+3=8, or
    // reject it with the cold sentinel for -1+3=2. fib(2) would not exercise
    // stack admission at all: both calls are eliminated before this check.
    for (below_bottom, expected) in [(1, 8_i64), (0, 2), (-1, 2)] {
        execute_with_self_stack_bound(&f, 1, &[6 << 3, 0, 0, 0, 0, 0, 0],
            (expected << 3) as u64, 0, None, true, Some(below_bottom), 0);
    }
}

#[test]
fn eliminated_self_calls_do_not_require_child_stack_space() {
    let f = fib();
    for below_bottom in [1, 0, -1] {
        execute_with_self_stack_bound(&f, 1, &[2 << 3, 0, 0, 0, 0, 0, 0],
            1 << 3, 0, None, true, Some(below_bottom), 0);
    }
}

#[test]
fn raw_fixnum_math_executes_through_the_ordinary_native_abi() {
    let mut f = Function::new("ordinary-raw-fixnum-arithmetic");
    let entry = f.entry();
    let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let one = value(&mut f, entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)));
    let mut add = instruction(Opcode::FixnumAdd, vec![x, one], AuxData::None);
    add.flags.guard = true;
    add.frame_state = Some(state(&mut f, x, &[x, one], 0));
    let result = value(&mut f, entry, add);
    f.set_terminator(entry, instruction(Opcode::Return, vec![result], AuxData::None));
    // This is a backend capability test, not permission to make isolated
    // additions raw in the production profitability selector.
    assert!(egcl_compiler::t2::unbox_fixnum::run(&mut f) > 0);
    execute(&f, 1, &[40 << 3, 0, 0, 0, 0, 0, 0], 41 << 3);
    execute(&f, 1, &[(-43i64 << 3) as u64, 0, 0, 0, 0, 0, 0], (-42i64 << 3) as u64);
}

#[test]
fn ordinary_guarded_min_max_execute_signed_fixnum_selection() {
    for opcode in [Opcode::FixnumMin, Opcode::FixnumMax] {
        let mut f = Function::new("ordinary-guarded-fixnum-selection");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let y = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let mut select = instruction(opcode, vec![x, y], AuxData::None);
        select.flags = InstFlags { guard: true, effectful: true, ..Default::default() };
        select.frame_state = Some(state(&mut f, x, &[x, y], 0));
        let result = value(&mut f, entry, select);
        f.set_terminator(entry, instruction(Opcode::Return, vec![result], AuxData::None));
        for (left, right) in [(9i64, 3i64), (-9, -3), (-3, 9), (7, 7),
            (egcl_rt::bignum::FIXNUM_MIN, egcl_rt::bignum::FIXNUM_MAX)] {
            for (x, y) in [(left, right), (right, left)] {
                let expected = if opcode == Opcode::FixnumMin { x.min(y) } else { x.max(y) };
                execute(&f, 2, &[(x << 3) as u64, (y << 3) as u64, 0, 0, 0, 0, 0], (expected << 3) as u64);
            }
        }
    }
}

#[test]
fn ordinary_packed_double_read_executes_raw_load_with_tagged_return_boundary() {
    let mut f = Function::new("ordinary-packed-read");
    let entry = f.entry();
    let array = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let index = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let recovery = state(&mut f, array, &[array, index], 0);
    let mut load = instruction(Opcode::PackedF64Ref, vec![array, index], AuxData::None);
    load.flags = InstFlags { guard: true, effectful: true, ..Default::default() };
    load.frame_state = Some(recovery);
    let raw = f.push_inst(entry, load, &[(IRType::of(TypeBits::DOUBLE_FLOAT), ValueRepresentation::UnboxedF64)]).1[0];
    let constant = f.push_inst(entry, instruction(Opcode::ConstDouble, vec![], AuxData::DoubleImm(1.25f64.to_bits())),
        &[(IRType::of(TypeBits::DOUBLE_FLOAT), ValueRepresentation::UnboxedF64)]).1[0];
    let mut compare = instruction(Opcode::FloatCmpEq, vec![raw, constant], AuxData::None);
    compare.flags = InstFlags { guard: true, effectful: true, ..Default::default() };
    compare.frame_state = Some(recovery);
    let result = f.push_inst(entry, compare, &[(IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)), ValueRepresentation::Tagged)]).1[0];
    f.set_terminator(entry, instruction(Opcode::Return, vec![result], AuxData::None));
    egcl_rt::rooted!(packed = egcl_rt::packed_f64::make(2, 1.25).unwrap());
    execute(&f, 2, &[packed.0, 0, 0, 0, 0, 0, 0], egcl_rt::value::T.0);
    assert!(egcl_rt::packed_f64::set(*packed, 1, 2.5));
    execute(&f, 2, &[packed.0, 1 << 3, 0, 0, 0, 0, 0], NIL.0);
}

#[test]
fn ordinary_versioned_vector_store_executes_fast_clone_and_large_checked_loop() {
    use egcl_compiler::t2::{vector_ranges, verify};
    use egcl_rt::{
        EgclVal, gc,
        object::{ObjectHeader, type_id},
    };

    let mut f = Function::new("ordinary-versioned-vector-store");
    let entry = f.entry();
    let vector = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let start = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let end = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let one = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
    );
    let fast_value = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(77)),
    );
    let slow_value = value(
        &mut f,
        entry,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(88)),
    );
    let header = f.make_block();
    let body = f.make_block();
    let done = f.make_block();
    let index = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
    let mut jump = instruction(Opcode::Jump, vec![], AuxData::None);
    jump.targets = vec![BlockCall {
        block: header,
        args: vec![start],
    }];
    f.set_terminator(entry, jump);
    let recovery = state(&mut f, vector, &[vector, index, end], 0);
    let guarded = InstFlags {
        guard: true,
        effectful: true,
        ..Default::default()
    };
    let mut compare = instruction(Opcode::FixnumCmpGe, vec![index, end], AuxData::None);
    compare.flags = guarded;
    compare.frame_state = Some(recovery);
    let condition = f
        .push_inst(
            header,
            compare,
            &[(
                IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        )
        .1[0];
    let mut branch = instruction(Opcode::Brif, vec![condition], AuxData::None);
    branch.targets = vec![
        BlockCall {
            block: done,
            args: vec![],
        },
        BlockCall {
            block: body,
            args: vec![],
        },
    ];
    f.set_terminator(header, branch);
    let mut store = instruction(
        Opcode::VecSet,
        vec![vector, index, fast_value],
        AuxData::None,
    );
    store.flags = guarded;
    store.frame_state = Some(recovery);
    let (checked_store, _) =
        f.push_inst(body, store, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    let mut add = instruction(Opcode::FixnumAdd, vec![index, one], AuxData::None);
    add.flags = guarded;
    add.frame_state = Some(recovery);
    let next = value(&mut f, body, add);
    let mut jump = instruction(Opcode::Jump, vec![], AuxData::None);
    jump.targets = vec![BlockCall {
        block: header,
        args: vec![next],
    }];
    f.set_terminator(body, jump);
    f.set_terminator(
        done,
        instruction(Opcode::Return, vec![vector], AuxData::None),
    );
    verify::verify(&f).unwrap();
    let stats = vector_ranges::version_counted_stores(&mut f);
    assert_eq!(stats.loops, 1, "{stats:?}");
    assert_eq!(stats.stores, 1, "{stats:?}");
    // Deliberately distinguish executed branches AFTER cloning. This changes
    // only the original checked store: the fast clone must still store 77.
    f.inst_mut(checked_store).args[2] = slow_value;
    verify::verify(&f).unwrap();
    let fast_stores: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .filter(|&&i| f.inst(i).opcode == Opcode::VecSetInRange)
        .copied()
        .collect();
    assert_eq!(fast_stores.len(), 1);
    assert_eq!(f.inst(fast_stores[0]).args[2], fast_value);

    fn allocate_vector(length: usize) -> EgclVal {
        let bytes = 8 + length * 8;
        let body = gc::alloc_typed(bytes, type_id::SIMPLE_VECTOR).unwrap();
        // No Lisp allocation until the complete object is initialized/rooted.
        unsafe {
            body.cast::<u64>().write(length as u64);
            for i in 0..length {
                body.add(8 + i * 8)
                    .cast::<EgclVal>()
                    .write(EgclVal::from_fixnum(-1));
            }
            EgclVal::from_heap_ptr(body.sub(gc::body_header_offset(bytes)))
        }
    }
    fn element(vector: EgclVal, index: usize) -> EgclVal {
        unsafe {
            let header = vector.as_ptr();
            let offset = if (*header.cast::<ObjectHeader>()).is_large_object() {
                16
            } else {
                8
            };
            *header.add(offset + 8 + index * 8).cast::<EgclVal>()
        }
    }
    unsafe extern "C" fn poll(_activation: *mut u8) -> u64 {
        0
    }
    egcl_rt::rooted!(small = allocate_vector(8));
    egcl_rt::rooted!(large = allocate_vector(65_533));
    assert!(!unsafe { (*small.as_ptr().cast::<ObjectHeader>()).is_large_object() });
    assert!(unsafe { (*large.as_ptr().cast::<ObjectHeader>()).is_large_object() });
    for (vector, start, end, expected) in [
        (*small, 1, 4, 77),
        (*large, 1, 4, 88),
        (*small, 5, 5, -1),
        (*large, 5, 5, -1),
        (*small, 7, 6, -1),
        (*large, 7, 6, -1),
    ] {
        let length = if vector == *small { 8 } else { 65_533 };
        let before: Vec<_> = (0..length).map(|i| element(vector, i)).collect();
        // execute_with_poll requires emission of a real ordinary entry and
        // asserts the native return descriptor/result. Cold veneers return
        // a distinct sentinel, so fallback cannot make these assertions pass.
        execute_with_poll(
            &f,
            3,
            &[
                vector.0,
                EgclVal::from_fixnum(start).0,
                EgclVal::from_fixnum(end).0,
                0,
                0,
                0,
                0,
            ],
            vector.0,
            poll as *const () as u64,
        );
        if start < end {
            for i in start..end {
                assert_eq!(element(vector, i as usize).as_fixnum(), expected);
            }
            assert_eq!(element(vector, start as usize - 1).as_fixnum(), -1);
            assert_eq!(element(vector, end as usize).as_fixnum(), -1);
        } else {
            let after: Vec<_> = (0..length).map(|i| element(vector, i)).collect();
            assert_eq!(after, before, "zero/reversed interval must not write");
        }
    }
}
