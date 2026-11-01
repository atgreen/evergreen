// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::emit::{OrdinaryLispEntryOptions, emit_framed_ordinary_leaf};
use egcl_compiler::t2::ir::{
    AuxData, BlockCall, Function, IRType, InstData, InstFlags, Opcode, Value, ValueRepresentation,
};
use egcl_rt::{jit::JitBuffer, value::NIL};

// A SysV harness enters the real Lisp ABI and records the return protocol and
// preserved registers. The fourth argument is above the machine return address.
// Neither harness nor arity observer allocates, safepoints, or calls Rust.
core::arch::global_asm!(
    r#"
.text
.globl ordinary_entry_probe
ordinary_entry_probe:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 24
    mov [rsp + 8], r9
    mov r11, rdi
    mov r10, rsi
    mov r13, r8
    mov rax, rcx
    mov rcx, rdx
    mov rdi, [r10]
    mov rsi, [r10 + 8]
    mov rdx, [r10 + 16]
    mov r10, [r10 + 24]
    mov [rsp], r10
    mov rbp, 0x5151
    mov rbx, 0x5252
    mov r12, 0x5353
    mov r14, 0x5454
    mov r15, 0x5555
    stc
    call r11
    mov r10, [rsp + 8]
    mov [r10], rax
    mov [r10 + 64], rdx
    mov [r10 + 72], rcx
    setc byte ptr [r10 + 8]
    mov [r10 + 16], rbp
    mov [r10 + 24], rbx
    mov [r10 + 32], r12
    mov [r10 + 40], r13
    mov [r10 + 48], r14
    mov [r10 + 56], r15
    add rsp, 24
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
.globl ordinary_arity_observer
ordinary_arity_observer:
    mov [r13], rdi
    mov [r13 + 8], rsi
    mov [r13 + 16], rdx
    mov r10, [rsp + 8]
    mov [r13 + 24], r10
    mov [r13 + 32], rcx
    mov [r13 + 40], rax
    mov r10, rsp
    and r10, 15
    mov [r13 + 48], r10
    mov [r13 + 56], rbp
    mov rax, 0x6161
    stc
    ret
"#
);

unsafe extern "C" {
    fn ordinary_entry_probe(
        entry: *const u8,
        args: *const u64,
        count: u64,
        closure: u64,
        execution: *mut u64,
        output: *mut u64,
    );
    fn ordinary_arity_observer();
}

fn instruction(opcode: Opcode, args: Vec<Value>) -> InstData {
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

fn leaf(arity: usize, selected: usize, permuted: bool) -> Function {
    let mut f = Function::new("ordinary-leaf");
    let entry = f.entry();
    let args: Vec<_> = (0..arity)
        .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    if arity == 0 {
        let (_, values) = f.push_inst(
            entry,
            instruction(Opcode::ConstNil, vec![]),
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(entry, instruction(Opcode::Return, vec![values[0]]));
    } else if permuted {
        let body = f.make_block();
        let params: Vec<_> = (0..arity)
            .map(|_| f.add_block_param(body, IRType::TOP, ValueRepresentation::Tagged))
            .collect();
        let mut jump = instruction(Opcode::Jump, vec![]);
        jump.targets.push(BlockCall {
            block: body,
            args: args.into_iter().rev().collect(),
        });
        f.set_terminator(entry, jump);
        f.set_terminator(body, instruction(Opcode::Return, vec![params[selected]]));
    } else {
        f.set_terminator(entry, instruction(Opcode::Return, vec![args[selected]]));
    }
    f
}

fn options(arity: usize) -> OrdinaryLispEntryOptions {
    OrdinaryLispEntryOptions {
        require_callable_home: false,
        arity,
        arity_error_veneer: ordinary_arity_observer as *const () as u64,
        binding: None,
    }
}

fn probe(
    code: &egcl_compiler::t2::emit::FramedCode,
    count: u64,
    closure: u64,
) -> ([u64; 10], [u64; 8]) {
    let buffer = JitBuffer::new(&code.code).unwrap();
    let mut observed = [0; 8];
    let mut output = [0; 10];
    let args = [0x110, 0x220, 0x330, 0x440];
    unsafe {
        ordinary_entry_probe(
            buffer
                .as_ptr()
                .add(code.ordinary_entry.expect("distinct ordinary entry")),
            args.as_ptr(),
            count,
            closure,
            observed.as_mut_ptr(),
            output.as_mut_ptr(),
        );
    }
    assert_eq!(
        output[2..8],
        [
            0x5151,
            0x5252,
            0x5353,
            observed.as_ptr() as u64,
            0x5454,
            0x5555
        ]
    );
    (output, observed)
}

#[test]
fn ordinary_leaf_executes_zero_through_four_arguments_and_permutations() {
    for arity in 0..=4 {
        for selected in 0..arity.max(1) {
            for permuted in [false, true] {
                let code =
                    emit_framed_ordinary_leaf(&leaf(arity, selected, permuted), options(arity))
                        .unwrap();
                assert_eq!(
                    code.frame_entry,
                    Some(0),
                    "leaf retains its distinct slot entry"
                );
                assert!(code.native_recursive_entry.is_none());
                assert_ne!(code.ordinary_entry, Some(code.compiled_entry));
                assert_eq!(code.shadow_root_slots, 0);
                assert_eq!(code.emitted_safepoints, 0);
                assert_eq!(
                    code.native_calls.as_ref().unwrap().iter().count(),
                    0,
                    "bounded leaf must not contain runtime helper calls"
                );
                let (output, observed) = probe(&code, arity as u64, NIL.0);
                let expected = if arity == 0 {
                    NIL.0
                } else {
                    [0x110, 0x220, 0x330, 0x440][if permuted {
                        arity - 1 - selected
                    } else {
                        selected
                    }]
                };
                assert_eq!(
                    output[0], expected,
                    "arity={arity} selected={selected} permuted={permuted}"
                );
                assert_eq!(output[1], 0, "single return must clear carry");
                assert_eq!(
                    observed, [0; 8],
                    "correct arity must not visit error veneer"
                );
            }
        }
    }
}

#[test]
fn ordinary_wrong_arity_tail_handoff_preserves_supplied_arguments_and_context() {
    let code = emit_framed_ordinary_leaf(&leaf(2, 0, false), options(2)).unwrap();
    for actual in [0, 1, 3, 4] {
        let (output, observed) = probe(&code, actual, 0x7777);
        assert_eq!(output[0], 0x6161);
        assert_eq!(output[1], 1, "error veneer owns the result protocol");
        assert_eq!(
            observed,
            [0x110, 0x220, 0x330, 0x440, actual, 0x7777, 8, 0x5151]
        );
    }
}

#[test]
fn ordinary_leaf_declines_unsupported_shapes_and_missing_error_handoff() {
    assert!(emit_framed_ordinary_leaf(&leaf(5, 4, false), options(5)).is_err());
    assert!(emit_framed_ordinary_leaf(&leaf(1, 0, false), options(2)).is_err());
    let mut opts = options(1);
    opts.arity_error_veneer = 0;
    assert!(emit_framed_ordinary_leaf(&leaf(1, 0, false), opts).is_err());
    let mut variadic = leaf(1, 0, false);
    variadic.set_variadic(true);
    assert!(emit_framed_ordinary_leaf(&variadic, options(1)).is_err());
    let mut call = leaf(1, 0, false);
    let ret = call.terminator(call.entry()).unwrap();
    call.inst_mut(ret).opcode = Opcode::TailCall;
    assert!(emit_framed_ordinary_leaf(&call, options(1)).is_err());
}

#[test]
fn ordinary_leaf_has_an_rbp_chain_aligned_body_and_reserved_execution_register() {
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, Register};
    let code = emit_framed_ordinary_leaf(&leaf(4, 3, true), options(4)).unwrap();
    let recipe = code.native_calls.as_ref().unwrap().unwind_recipe().unwrap();
    assert_eq!(
        recipe.frame_bytes() % 16,
        8,
        "body RSP must be aligned after CALL"
    );
    assert_eq!(
        recipe.saved_offset(5),
        Some(recipe.frame_bytes() - 8),
        "RBP is the first saved word"
    );
    assert_eq!(
        recipe.saved_offset(13),
        None,
        "R13 is reserved, not a value home"
    );
    let mut decoder = Decoder::new(
        64,
        &code.code[code.ordinary_entry.unwrap()..],
        DecoderOptions::NONE,
    );
    let mut chain = false;
    let mut instructions = Vec::new();
    let mut prior = None;
    while decoder.can_decode() {
        let inst = decoder.decode();
        instructions.push(inst);
        if inst.mnemonic() == Mnemonic::Mov
            && inst.op0_register() == Register::RBP
            && inst.op1_register() == Register::RSP
        {
            assert_eq!(prior, Some((Mnemonic::Push, Register::RBP)));
            chain = true;
        }
        assert_ne!(
            inst.op0_register(),
            Register::R13,
            "ordinary code must not overwrite the execution pointer"
        );
        prior = Some((inst.mnemonic(), inst.op0_register()));
        if inst.mnemonic() == Mnemonic::Ret {
            break;
        }
    }
    assert!(
        chain,
        "ordinary prologue must construct the frame-pointer chain"
    );
    let mut tail = instructions.iter().rev();
    assert_eq!(tail.next().unwrap().mnemonic(), Mnemonic::Ret);
    assert_eq!(tail.next().unwrap().mnemonic(), Mnemonic::Clc);
    for inst in tail {
        if !matches!(
            inst.mnemonic(),
            Mnemonic::Pop | Mnemonic::Lea | Mnemonic::Add | Mnemonic::Sub
        ) {
            break;
        }
        assert!(
            !matches!(inst.mnemonic(), Mnemonic::Add | Mnemonic::Sub),
            "ordinary frame teardown must preserve the return flags"
        );
    }
}

#[test]
fn ordinary_stack_bound_reserves_phi_snapshot_headroom() {
    for arity in 0..=4 {
        let code = emit_framed_ordinary_leaf(&leaf(arity, 0, true), options(arity)).unwrap();
        let fixed = code
            .native_calls
            .as_ref()
            .unwrap()
            .unwind_recipe()
            .unwrap()
            .frame_bytes() as usize;
        let move_bytes = (arity * 8 + 15) & !15;
        assert_eq!(code.ordinary_stack_required, Some(fixed + move_bytes));
    }
}

static ORDINARY_COLD_BODY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ORDINARY_COLD_SLICE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn ordinary_caller(arity: usize, reverse: bool) -> Function {
    let mut f = Function::new("ordinary-caller");
    let entry = f.entry();
    let mut args: Vec<_> = (0..arity)
        .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    if reverse {
        args.reverse();
    }
    let mut call = instruction(Opcode::Call, args);
    call.aux = AuxData::CallTarget(77);
    call.flags.call = true;
    call.flags.safepoint = true;
    let (_, result) = f.push_inst(entry, call, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    f.set_terminator(entry, instruction(Opcode::Return, result));
    f
}

static ORDINARY_COLD_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ORDINARY_COLD_PC: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ORDINARY_COLD_CLOSURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ORDINARY_COLD_ARGS: [std::sync::atomic::AtomicU64; 4] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 4];

#[unsafe(naked)]
unsafe extern "C" fn ordinary_cold_observer() {
    core::arch::naked_asm!(
        "push rbp", "mov rsi, [rsp + 8]", "call {observe}", "pop rbp",
        "mov rdx, 3", "mov rcx, 0x789", "stc", "ret",
        observe = sym observe_ordinary_cold,
    );
}

unsafe extern "C" fn observe_ordinary_cold(
    request: *const egcl_compiler::t2::emit::OrdinaryColdRequest,
    pc: usize,
) -> u64 {
    use std::sync::atomic::Ordering::Relaxed;
    let request = unsafe { &*request };
    ORDINARY_COLD_COUNT.fetch_add(1, Relaxed);
    ORDINARY_COLD_PC.store(pc, Relaxed);
    ORDINARY_COLD_BODY.store(
        request as *const _ as usize + std::mem::size_of_val(request),
        Relaxed,
    );
    ORDINARY_COLD_SLICE.store(request.args as usize, Relaxed);
    ORDINARY_COLD_CLOSURE.store(request.closure.0, Relaxed);
    for (i, slot) in ORDINARY_COLD_ARGS.iter().enumerate().take(request.nargs) {
        slot.store(unsafe { (*request.args.add(i)).0 }, Relaxed);
    }
    0xc01d
}

#[test]
fn ordinary_call_crosses_code_owners_with_exact_native_maps() {
    use egcl_compiler::t2::emit::{
        NativeRecursiveControl, OrdinaryCallFlags, OrdinaryCallTarget, emit_framed_ordinary_call,
    };
    use egcl_rt::{
        call_table::*,
        exec_context::{ExecutionKind, LispExecutionContext},
    };
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};
    let callee = emit_framed_ordinary_leaf(&leaf(4, 3, false), options(4)).unwrap();
    let callee_buffer = JitBuffer::new(&callee.code).unwrap();
    let mut descriptor = OrdinaryEntryDescriptor {
        kind: ORDINARY_X64_LISP_KIND,
        version: ORDINARY_X64_LISP_VERSION,
        revision: 7,
        entry: callee_buffer.as_ptr() as usize + callee.ordinary_entry.unwrap(),
        code_start: callee_buffer.as_ptr() as usize,
        code_len: callee.code.len(),
        arity: 4,
        environment: ORDINARY_ENV_NONE,
        owner: &callee as *const _ as usize,
        stack_required: callee.ordinary_stack_required.unwrap(),
    };
    let slot = AtomicUsize::new(&descriptor as *const _ as usize);
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
    let target = OrdinaryCallTarget {
        symbol: 77,
        descriptor_slot: &slot as *const _ as u64,
        revision_address: &revision as *const _ as u64,
        heat_slot: 0,
        function_cell: 0,
    };
    let f = ordinary_caller(4, false);
    let caller = emit_framed_ordinary_call(
        &f,
        options(4),
        target,
        flags,
        ordinary_cold_observer as *const () as u64,
    )
    .unwrap();
    assert_eq!(caller.compiled_entry, 0);
    assert_eq!(&caller.code[..2], &[0x0f, 0x0b]);
    assert!(caller.native_recursive_entry.is_none());
    assert!(
        caller.native_entry_kind_slot.is_none(),
        "ordinary frames have no legacy managed identity"
    );
    assert!(
        caller.frame_entry.is_none(),
        "call-only artifact cannot advertise its UD2 entry"
    );
    let buffer = JitBuffer::new(&caller.code).unwrap();
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
    // The runtime consumes the initially armed poll before entering native code.
    execution.begin_poll();
    let mut depth = 0;
    let mv_active = AtomicBool::new(false);
    let mut control = NativeRecursiveControl {
        execution: execution.as_ptr(),
        depth: &mut depth,
        reserved_counter: 0,
        mv_active: mv_active.as_ptr(),
        expected_generation: 999,
        seen_carrier: 2,
        seen_signal_epoch: 0,
        depth_limit: 8,
        anchor_depth: 0,
        deadline_armed: 0,
        reserved: [0; 7],
        test_entries: std::ptr::null_mut(),
        test_observer: std::ptr::null(),
        stack_limit: 0,
    };
    unsafe {
        execution.swap_active_native_control(&mut control as *mut _ as u64);
    }
    let args = [0x110, 0x220, 0x330, 0x440];
    let run = || {
        let mut output = [0; 10];
        unsafe {
            ordinary_entry_probe(
                buffer.as_ptr().add(caller.ordinary_entry.unwrap()),
                args.as_ptr(),
                4,
                NIL.0,
                execution.as_ptr() as *mut u64,
                output.as_mut_ptr(),
            );
        }
        assert_eq!(
            output[2..8],
            [
                0x5151,
                0x5252,
                0x5353,
                execution.as_ptr() as u64,
                0x5454,
                0x5555
            ]
        );
        output
    };
    ORDINARY_COLD_COUNT.store(0, Relaxed);
    let output = run();
    assert_eq!(output[0], args[3]);
    assert_eq!(output[1], 0);
    assert_eq!(depth, 0, "direct call balances shared depth");
    assert_eq!(output[5], execution.as_ptr() as u64);
    let maps = caller.native_calls.as_ref().unwrap();
    assert_eq!(maps.iter().count(), 2, "direct and bounded cold return PCs");
    assert_eq!(
        maps.iter()
            .map(|site| site.stack_adjust)
            .collect::<Vec<_>>(),
        [16, 64]
    );
    assert!(
        caller.ordinary_stack_required.unwrap()
            >= maps.unwind_recipe().unwrap().frame_bytes() as usize + 72
    );
    for site in maps.iter() {
        let egcl_compiler::t2::x64_calls::NativeCallValues::Frame(values) =
            maps.value_map(site.return_offset).unwrap()
        else {
            panic!("exact call values")
        };
        assert_eq!(values.layout().activation_slots, 0);
        assert!(values.layout().activation_base_slot.is_none());
        assert!(values.gc_locations().count() >= 4);
    }
    let original = descriptor;
    assert!(
        original.is_compatible(),
        "published baseline descriptor is valid"
    );
    // These are immutable descriptor invariants, enforced before publication,
    // not repeated at each generated call. Check the same compatibility gate
    // here; call_table's publication test also proves rejected replacements
    // preserve the previous entry. Never inject malformed published metadata.
    for case in [0, 1, 6, 7, 20, 21, 22] {
        let mut invalid = original;
        match case {
            0 => invalid.kind ^= 1,
            1 => invalid.version += 1,
            6 => invalid.stack_required = 0,
            7 => invalid.stack_required = usize::MAX,
            20 => invalid.owner = 0,
            21 => invalid.code_start = invalid.entry + 1,
            22 => invalid.code_len = usize::MAX,
            _ => unreachable!(),
        }
        assert!(
            !invalid.is_compatible(),
            "case {case}: reject before publication"
        );
    }
    let control_address = &mut control as *mut _ as u64;
    // Native emitters consume one local poll; producer-specific handshakes
    // belong to runtime tests, not synthetic global cells in this fixture.
    // This call site's incoming multiple-value state is proven empty. Dynamic
    // MV rejection is exercised after a real producer in
    // native_ordinary_mv_admission; forging mv_active here violates ingress.
    for case in [2, 3, 4, 5, 8, 11, 12, 18, 19, 23] {
        match case {
            2 => descriptor.arity = 3,
            3 => descriptor.environment = ORDINARY_ENV_CLOSURE,
            4 => revision.store(8, Relaxed),
            5 => slot.store(0, Relaxed),
            8 => control.stack_limit = u64::MAX,
            11 => execution.request_poll(),
            12 => execution.set_carrier_id(3),
            18 => execution.request_yield(),
            19 => unsafe {
                execution.swap_active_native_control(0);
            },
            23 => control.execution = std::ptr::null(),
            _ => unreachable!(),
        }
        // Generated code reads these records through the published raw addresses.
        std::hint::black_box(&descriptor);
        std::hint::black_box(&control);
        let before = ORDINARY_COLD_COUNT.load(Relaxed);
        let output = run();
        assert_eq!(
            ORDINARY_COLD_COUNT.load(Relaxed),
            before + 1,
            "case {case}: bounded cold handoff"
        );
        assert_eq!(
            [output[0], output[1], output[8], output[9]],
            [0xc01d, 1, 3, 0x789],
            "case {case}: full cold tuple"
        );
        assert_eq!(depth, 0, "case {case}: cold path owns no child depth");
        assert_eq!(ORDINARY_COLD_CLOSURE.load(Relaxed), NIL.0);
        assert_eq!(ORDINARY_COLD_ARGS.each_ref().map(|x| x.load(Relaxed)), args);
        let cold_offset = ORDINARY_COLD_PC.load(Relaxed) - buffer.as_ptr() as usize;
        assert!(
            maps.value_map(cold_offset).is_some(),
            "actual cold return PC has roots"
        );
        let egcl_compiler::t2::x64_calls::NativeCallValues::Frame(values) =
            maps.value_map(cold_offset).unwrap()
        else {
            unreachable!()
        };
        let body = ORDINARY_COLD_BODY.load(Relaxed);
        let slice = ORDINARY_COLD_SLICE.load(Relaxed);
        for argument in 0..4 {
            assert!(
                values.gc_locations().any(|location| matches!(location,
                egcl_compiler::t2::x64_value_maps::NativeValueLocation::Stack(offset)
                    if body.wrapping_add_signed(*offset as isize) == slice + argument * 8)),
                "case {case}: actual outgoing argument {argument} has a writable map location"
            );
        }
        descriptor = original;
        revision.store(7, Relaxed);
        slot.store(&descriptor as *const _ as usize, Relaxed);
        control.stack_limit = 0;
        control.execution = execution.as_ptr();
        control.depth_limit = 8;
        control.deadline_armed = 0;
        mv_active.store(false, Relaxed);
        execution.set_carrier_id(2);
        execution.take_yield_request();
        execution.begin_poll();
        for flag in &bytes {
            flag.store(0, Relaxed);
        }
        for flag in &words {
            flag.store(0, Relaxed);
        }
        unsafe {
            execution.swap_active_native_control(control_address);
        }
    }
    std::hint::black_box(&control);
    std::hint::black_box(&descriptor);
    assert_eq!(run()[0], args[3], "restored target is direct again");
    let cold_count = ORDINARY_COLD_COUNT.load(Relaxed);
    // A distinct ordinary callee observes depth while it is active, then
    // returns a machine tuple with an opaque payload token. No Rust shim sets its flags.
    let tuple_code = [
        0x55, 0x48, 0x89, 0xe5, // push rbp; mov rbp,rsp
        0x4d, 0x8b, 0x5d, 0x20, // r11=[r13+ACTIVE_NATIVE_CONTROL]
        0x4d, 0x8b, 0x5b, 0x08, // r11=control.depth
        0x41, 0x8b, 0x03, // eax=*depth
        0x48, 0xc7, 0xc2, 3, 0, 0, 0, // rdx=count
        0x48, 0xc7, 0xc1, 0x89, 7, 0, 0, // rcx=payload token
        0x5d, 0xf9, 0xc3, // pop rbp; stc; ret
    ];
    let tuple_buffer = JitBuffer::new(&tuple_code).unwrap();
    descriptor.code_start = tuple_buffer.as_ptr() as usize;
    descriptor.code_len = tuple_code.len();
    descriptor.entry = descriptor.code_start;
    descriptor.owner = &tuple_buffer as *const _ as usize;
    descriptor.stack_required = 8;
    std::hint::black_box(&descriptor);
    let tuple = run();
    assert_eq!(
        [tuple[0], tuple[1], tuple[8], tuple[9]],
        [0, 1, 3, 0x789],
        "ordinary machine calls do not increment Rust/native reentry depth"
    );
    assert_eq!(depth, 0);
    assert_eq!(
        ORDINARY_COLD_COUNT.load(Relaxed),
        cold_count,
        "tuple callee took the direct edge"
    );
    for arity in 0..4 {
        let callee =
            emit_framed_ordinary_leaf(&leaf(arity, arity.saturating_sub(1), false), options(arity))
                .unwrap();
        let callee_buffer = JitBuffer::new(&callee.code).unwrap();
        descriptor = original;
        descriptor.code_start = callee_buffer.as_ptr() as usize;
        descriptor.code_len = callee.code.len();
        descriptor.entry = descriptor.code_start + callee.ordinary_entry.unwrap();
        descriptor.arity = arity;
        descriptor.owner = &callee as *const _ as usize;
        descriptor.stack_required = callee.ordinary_stack_required.unwrap();
        let caller = emit_framed_ordinary_call(
            &ordinary_caller(arity, true),
            options(arity),
            target,
            flags,
            ordinary_cold_observer as *const () as u64,
        )
        .unwrap();
        let buffer = JitBuffer::new(&caller.code).unwrap();
        let mut output = [0; 10];
        std::hint::black_box(&descriptor);
        unsafe {
            ordinary_entry_probe(
                buffer.as_ptr().add(caller.ordinary_entry.unwrap()),
                args.as_ptr(),
                arity as u64,
                NIL.0,
                execution.as_ptr() as *mut u64,
                output.as_mut_ptr(),
            );
        }
        assert_eq!(
            output[0],
            if arity == 0 { NIL.0 } else { args[0] },
            "arity {arity}: reversed arguments"
        );
        assert_eq!(output[1], 0);
        assert_eq!(depth, 0);
    }
    assert_eq!(ORDINARY_COLD_COUNT.load(Relaxed), cold_count);
    let mut unsupported = ordinary_caller(4, false);
    unsupported.set_variadic(true);
    assert!(
        emit_framed_ordinary_call(
            &unsupported,
            options(4),
            target,
            flags,
            ordinary_cold_observer as *const () as u64
        )
        .is_err()
    );
    unsafe {
        execution.swap_active_native_control(0);
    }
}

fn two_call_caller() -> Function {
    let mut f = Function::new("ordinary-two-calls");
    let b = f.entry();
    let args: Vec<_> = (0..4)
        .map(|_| f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    for (symbol, operands) in [(77, args.clone()), (88, args.into_iter().rev().collect())] {
        if symbol == 88 {
            let mut clear = instruction(Opcode::ClearMv, vec![]);
            clear.flags.call = true;
            clear.flags.safepoint = true;
            clear.flags.effectful = true;
            f.push_inst(b, clear, &[]);
        }
        let mut call = instruction(Opcode::Call, operands);
        call.aux = AuxData::CallTarget(symbol);
        call.flags.call = true;
        call.flags.safepoint = true;
        let (_, result) = f.push_inst(b, call, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        if symbol == 88 {
            f.set_terminator(b, instruction(Opcode::Return, result));
        }
    }
    f
}

static TWO_CALL_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TWO_CALL_FIRST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static TWO_CALL_SECOND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static TWO_CALL_CLEARS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[unsafe(naked)]
unsafe extern "C" fn two_call_zero() {
    core::arch::naked_asm!(
        "inc qword ptr [rip + {first}]", "mov byte ptr [rip + {active}], 1",
        "mov rax, {nil}", "xor edx, edx", "xor ecx, ecx", "stc", "ret",
        first = sym TWO_CALL_FIRST, active = sym TWO_CALL_ACTIVE, nil = const NIL.0,
    );
}
#[unsafe(naked)]
unsafe extern "C" fn two_call_many() {
    core::arch::naked_asm!(
        "inc qword ptr [rip + {second}]", "mov rax, rdi",
        "mov rdx, 3", "mov rcx, 0x987", "stc", "ret", second = sym TWO_CALL_SECOND,
    );
}
extern "C" fn two_call_clear(primary: u64, destination: *mut u64, count: u64) {
    assert_eq!((primary, destination as usize, count), (0, 0, 0));
    assert!(TWO_CALL_ACTIVE.swap(false, std::sync::atomic::Ordering::Relaxed));
    TWO_CALL_CLEARS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn ordinary_two_calls_normalize_zero_before_distinct_second_target_and_forward_tuple() {
    use egcl_compiler::t2::emit::{
        NativeRecursiveControl, OrdinaryCallFlags, OrdinaryCallTarget,
        emit_framed_ordinary_two_calls,
    };
    use egcl_rt::{
        call_table::*,
        exec_context::{ExecutionKind, LispExecutionContext},
    };
    use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};
    TWO_CALL_ACTIVE.store(false, Relaxed);
    TWO_CALL_FIRST.store(0, Relaxed);
    TWO_CALL_SECOND.store(0, Relaxed);
    TWO_CALL_CLEARS.store(0, Relaxed);
    let descriptors = [
        two_call_zero as *const () as usize,
        two_call_many as *const () as usize,
    ]
    .map(|entry| OrdinaryEntryDescriptor {
        kind: ORDINARY_X64_LISP_KIND,
        version: ORDINARY_X64_LISP_VERSION,
        revision: 7,
        entry,
        code_start: entry,
        code_len: 128,
        arity: 4,
        environment: ORDINARY_ENV_NONE,
        owner: entry,
        stack_required: 8,
    });
    let slots = descriptors
        .each_ref()
        .map(|d| AtomicUsize::new(d as *const _ as usize));
    let revisions = [AtomicU64::new(7), AtomicU64::new(7)];
    let targets = std::array::from_fn(|i| OrdinaryCallTarget {
        symbol: [77, 88][i],
        descriptor_slot: slots[i].as_ptr() as u64,
        revision_address: revisions[i].as_ptr() as u64,
        heat_slot: 0,
        function_cell: 0,
    });
    let byte = AtomicU8::new(0);
    let word = AtomicU64::new(0);
    let flags = OrdinaryCallFlags {
        gc_requested: byte.as_ptr() as u64,
        sigusr1_pending: byte.as_ptr() as u64,
        posted_signal: word.as_ptr() as u64,
        native_errors: word.as_ptr() as u64,
        signal_epoch: word.as_ptr() as u64,
    };
    let f = two_call_caller();
    let code = emit_framed_ordinary_two_calls(
        &f,
        options(4),
        targets,
        flags,
        ordinary_cold_observer as *const () as u64,
        two_call_clear as *const () as u64,
    )
    .unwrap();
    assert!(code.frame_entry.is_none());
    let maps = code.native_calls.as_ref().unwrap();
    assert_eq!(
        maps.iter().count(),
        4,
        "each call owns direct and cold return maps"
    );
    let buffer = JitBuffer::new(&code.code).unwrap();
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
    // The runtime consumes the initially armed poll before entering native code.
    execution.begin_poll();
    let mut depth = 0;
    let mut control = NativeRecursiveControl {
        execution: execution.as_ptr(),
        depth: &mut depth,
        reserved_counter: 0,
        mv_active: TWO_CALL_ACTIVE.as_ptr(),
        expected_generation: 0,
        seen_carrier: 2,
        seen_signal_epoch: 0,
        depth_limit: 8,
        anchor_depth: 0,
        deadline_armed: 0,
        reserved: [0; 7],
        test_entries: std::ptr::null_mut(),
        test_observer: std::ptr::null(),
        stack_limit: 0,
    };
    unsafe {
        execution.swap_active_native_control(&mut control as *mut _ as u64);
    }
    let args = [0x110, 0x220, 0x330, 0x440];
    let mut output = [0; 10];
    unsafe {
        ordinary_entry_probe(
            buffer.as_ptr().add(code.ordinary_entry.unwrap()),
            args.as_ptr(),
            4,
            NIL.0,
            execution.as_ptr() as *mut u64,
            output.as_mut_ptr(),
        );
    }
    assert_eq!(
        [output[0], output[1], output[8], output[9]],
        [args[3], 1, 3, 0x987]
    );
    assert_eq!(TWO_CALL_FIRST.load(Relaxed), 1);
    assert_eq!(
        TWO_CALL_SECOND.load(Relaxed),
        1,
        "second descriptor is selected, not the first reused"
    );
    assert_eq!(TWO_CALL_CLEARS.load(Relaxed), 1);
    assert!(!TWO_CALL_ACTIVE.load(Relaxed));
    assert_eq!(depth, 0);
}

#[test]
fn ordinary_two_calls_reject_missing_reset_targets_and_other_shapes() {
    use egcl_compiler::t2::emit::{
        OrdinaryCallFlags, OrdinaryCallTarget, emit_framed_ordinary_two_calls,
    };
    let flags = OrdinaryCallFlags {
        gc_requested: 1,
        sigusr1_pending: 1,
        posted_signal: 1,
        native_errors: 1,
        signal_epoch: 1,
    };
    let targets = [77, 88].map(|symbol| OrdinaryCallTarget {
        symbol,
        descriptor_slot: 1,
        revision_address: 1,
        heat_slot: 0,
        function_cell: 0,
    });
    let compile =
        |f: &Function, targets| emit_framed_ordinary_two_calls(f, options(4), targets, flags, 1, 1);
    let mut no_reset = two_call_caller();
    let b = no_reset.entry();
    let reset = no_reset
        .block(b)
        .insts
        .iter()
        .copied()
        .find(|&i| no_reset.inst(i).opcode == Opcode::ClearMv)
        .unwrap();
    no_reset.block_mut(b).insts.retain(|&i| i != reset);
    assert!(compile(&no_reset, targets).is_err());
    let mut wrong = targets;
    wrong[1].symbol = 99;
    assert!(compile(&two_call_caller(), wrong).is_err());
    assert!(compile(&ordinary_caller(4, false), targets).is_err());
    let mut allocating = two_call_caller();
    let block = allocating.entry();
    let mut allocation = instruction(Opcode::Call, vec![Value(0), Value(1)]);
    allocation.aux = AuxData::CallTarget(99); // an additional allocating helper
    allocation.flags.call = true;
    allocation.flags.safepoint = true;
    let (extra, _) = allocating.push_inst(
        block,
        allocation,
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    allocating
        .block_mut(block)
        .insts
        .retain(|&inst| inst != extra);
    allocating.block_mut(block).insts.insert(2, extra);
    assert!(compile(&allocating, targets).is_err());
    let mut cfg = two_call_caller();
    let block = cfg.make_block();
    let (_, nil) = cfg.push_inst(
        block,
        instruction(Opcode::ConstNil, vec![]),
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    cfg.set_terminator(block, instruction(Opcode::Return, nil));
    assert!(compile(&cfg, targets).is_err());
    let mut raw = two_call_caller();
    raw.set_repr(Value(0), ValueRepresentation::UnboxedF64);
    assert!(compile(&raw, targets).is_err());
}
