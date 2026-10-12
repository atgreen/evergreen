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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

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
unsafe extern "C" fn rejected() {
    core::arch::naked_asm!("mov rax, -8", "clc", "ret");
}
unsafe extern "C" fn clear_mv(_primary: u64, _destination: *mut u64, _count: usize) -> u64 {
    0
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
                    revision_address: revision.as_ptr() as u64,
                },
            )
        })
        .collect();
    let code = emit_framed_ordinary_function(
        f,
        OrdinaryLispEntryOptions {
            arity,
            arity_error_veneer: rejected as *const () as u64,
        },
        &targets,
        flags,
        rejected as *const () as u64,
        OrdinaryGuardOptions {
            veneer: rejected as *const () as u64,
            multiple_values: clear_mv as *const () as u64,
        },
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
    slot.store(&descriptor as *const _ as usize, Ordering::Release);
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
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
    unsafe {
        execution.swap_active_native_control(&mut control as *mut _ as u64);
    }
    let mut out = [0; 4];
    unsafe {
        run(
            entry,
            args.as_ptr(),
            arity,
            execution.as_ptr(),
            out.as_mut_ptr(),
        );
    }
    assert_eq!(out[0], expected);
    assert_eq!(out[3], 0, "exactly one value returns with clear carry");
    assert_eq!(depth, 1, "every recursive child releases native depth");
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
    let result = value(&mut f, recurse, call);
    f.set_terminator(
        recurse,
        instruction(Opcode::Return, vec![result], AuxData::None),
    );
    execute(&f, 7, &[8, 16, 24, 32, 40, 48, 56], 16);
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
            arity_error_veneer: 1,
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
