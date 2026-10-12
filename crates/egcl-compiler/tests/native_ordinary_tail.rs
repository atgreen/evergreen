// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::emit::{
    NativeRecursiveControl, OrdinaryCallFlags, OrdinaryCallTarget, OrdinaryLispEntryOptions,
    emit_framed_ordinary_tail_call,
};
use egcl_compiler::t2::ir::{
    AuxData, Function, IRType, InstData, InstFlags, Opcode, ValueRepresentation,
};
use egcl_rt::{
    call_table::*,
    exec_context::{ExecutionKind, LispExecutionContext},
    jit::JitBuffer,
    value::NIL,
};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize};

// One test owns these machine-only observation words. Neither entry allocates
// or calls Rust; tuple payloads are opaque tokens, never dereferenced.
#[unsafe(no_mangle)]
static mut TAIL_OBSERVED: [u64; 10] = [0; 10];
#[unsafe(no_mangle)]
static mut TAIL_RESULT: [u64; 4] = [0; 4];
core::arch::global_asm!(
    r#"
.text
.globl tail_entry_probe
tail_entry_probe:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 24
    mov [rsp + 8], r9
    mov [rsp + 16], rcx
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
    lea r10, [rsp - 8]
    mov [r9 + 80], r10
    lea r10, [rip + tail_probe_return]
    mov [r9 + 88], r10
    cmp qword ptr [r9 + 96], 0
    je tail_probe_limit_ready
    lea r10, [rsp - 8]
    sub r10, [r9 + 96]
    mov rax, [r13 + 32]
    mov [rax + 88], r10
    mov rax, [rsp + 16]
tail_probe_limit_ready:
    mov rbp, 0x5151
    mov rbx, 0x5252
    mov r12, 0x5353
    mov r14, 0x5454
    mov r15, 0x5555
    call r11
tail_probe_return:
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
.globl tail_target_probe
tail_target_probe:
    lea r10, [rip + TAIL_OBSERVED]
    mov [r10], rdi
    mov [r10 + 8], rsi
    mov [r10 + 16], rdx
    mov r11, [rsp + 8]
    mov [r10 + 24], r11
    mov [r10 + 32], rcx
    mov [r10 + 40], rax
    mov [r10 + 48], rsp
    mov r11, [rsp]
    mov [r10 + 56], r11
    mov [r10 + 64], rbp
    mov r11, [r13 + 32]
    mov r11, [r11 + 8]
    mov r11d, [r11]
    mov [r10 + 72], r11
    lea r10, [rip + TAIL_RESULT]
    mov rax, [r10]
    mov rdx, [r10 + 8]
    mov rcx, [r10 + 16]
    bt qword ptr [r10 + 24], 0
    ret
.globl tail_cold_probe
tail_cold_probe:
    lea r10, [rip + TAIL_OBSERVED]
    mov r11, [rsp]
    mov [r10 + 56], r11
    mov rax, 0xc01d
    mov rdx, 3
    mov rcx, 0x789
    stc
    ret
"#
);
unsafe extern "C" {
    fn tail_entry_probe(
        entry: *const u8,
        args: *const u64,
        count: u64,
        closure: u64,
        execution: *mut u64,
        output: *mut u64,
    );
    fn tail_target_probe();
    fn tail_cold_probe();
}

fn forwarder(arity: usize, indices: &[usize]) -> Function {
    let mut f = Function::new("ordinary-tail-forwarder");
    let block = f.entry();
    let params: Vec<_> = (0..arity)
        .map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    let mut call = InstData {
        opcode: Opcode::Call,
        args: indices.iter().map(|&i| params[i]).collect(),
        results: vec![],
        aux: AuxData::CallTarget(77),
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    };
    call.flags.call = true;
    call.flags.safepoint = true;
    let (_, result) = f.push_inst(block, call, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    f.set_terminator(
        block,
        InstData {
            opcode: Opcode::Return,
            args: result,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
    );
    f
}

#[test]
fn ordinary_tail_reuses_frame_and_preserves_moves_depth_and_result_descriptor() {
    let target_entry = tail_target_probe as *const () as usize;
    let mut descriptor = OrdinaryEntryDescriptor {
        kind: ORDINARY_X64_LISP_KIND,
        version: ORDINARY_X64_LISP_VERSION,
        revision: 7,
        entry: target_entry,
        code_start: target_entry,
        code_len: 128,
        arity: 4,
        environment: ORDINARY_ENV_NONE,
        owner: target_entry,
        stack_required: 8,
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
        revision_address: &revision as *const _ as u64, heat_slot: 0,
    };
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
    let mut depth = 3;
    let active = AtomicBool::new(false);
    let mut control = NativeRecursiveControl {
        execution: execution.as_ptr(),
        depth: &mut depth,
        reserved_counter: 0,
        mv_active: active.as_ptr(),
        expected_generation: 999,
        seen_carrier: 2,
        seen_signal_epoch: 0,
        depth_limit: 8,
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
    let args = [0x110, 0x220, 0x330, 0x440];
    for (arity, indices, tail) in [
        (4, vec![3, 0, 3, 1], true),
        (4, vec![3, 2, 1, 0], true),
        (3, vec![2, 0, 1], true),
        (2, vec![1, 0], true),
        (1, vec![0], true),
        (0, vec![], true),
        (3, vec![2, 0, 1, 2], false),
    ] {
        descriptor.arity = indices.len();
        // A replacement frame is legal at the depth cap; capacity fallback
        // still performs a real child call and therefore needs spare depth.
        control.depth_limit = if tail { depth } else { depth + 1 };
        std::hint::black_box(&control);
        std::hint::black_box(&descriptor);
        let code = emit_framed_ordinary_tail_call(
            &forwarder(arity, &indices),
            OrdinaryLispEntryOptions {
                arity,
                arity_error_veneer: tail_cold_probe as *const () as u64,
            },
            target,
            flags,
            tail_cold_probe as *const () as u64,
        )
        .unwrap();
        let buffer = JitBuffer::new(&code.code).unwrap();
        assert_eq!(
            code.native_calls.as_ref().unwrap().iter().count(),
            if tail { 1 } else { 2 },
            "only actual CALL return PCs get maps"
        );
        for result in [
            [0x1234, 0, 0, 0],
            [NIL.0, 0, 0, 1],
            [0x4567, 3, 0x789, 1],
            [0x5678, 0x1_0000_0003, 0x123456789, 1],
        ] {
            unsafe {
                std::ptr::addr_of_mut!(TAIL_RESULT).write(result);
            }
            let mut output = [0u64; 13];
            unsafe {
                tail_entry_probe(
                    buffer.as_ptr().add(code.ordinary_entry.unwrap()),
                    args.as_ptr(),
                    arity as u64,
                    NIL.0,
                    execution.as_ptr() as *mut u64,
                    output.as_mut_ptr(),
                );
            }
            let observed = unsafe { std::ptr::addr_of!(TAIL_OBSERVED).read() };
            for (i, &source) in indices.iter().enumerate() {
                assert_eq!(observed[i], args[source]);
            }
            assert_eq!(observed[4], indices.len() as u64);
            assert_eq!(observed[5], NIL.0);
            assert_eq!(observed[9], if tail { 3 } else { 4 });
            if tail {
                assert_eq!(observed[6..9], [output[10], output[11], 0x5151]);
            } else {
                assert!(observed[6] < output[10]);
                assert_ne!(observed[7], output[11]);
            }
            assert_eq!([output[0], output[8], output[9], output[1]], result);
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
            assert_eq!(depth, 3);
        }
        if tail && arity == 4 {
            // Set the limit from the harness's actual target-entry RSP. Nine
            // bytes admit an eight-byte target bound; equality must refuse.
            // Checking from the unretired caller body would reject both.
            for gap in [9, 8] {
                let mut output = [0u64; 13];
                output[12] = gap;
                unsafe {
                    tail_entry_probe(
                        buffer.as_ptr().add(code.ordinary_entry.unwrap()),
                        args.as_ptr(),
                        arity as u64,
                        NIL.0,
                        execution.as_ptr() as *mut u64,
                        output.as_mut_ptr(),
                    );
                }
                if gap == 9 {
                    assert_eq!(output[0], 0x5678, "bound uses restored entry RSP");
                } else {
                    assert_eq!(output[0], 0xc01d, "bound equality takes rooted cold path");
                }
                control.stack_limit = 0;
                std::hint::black_box(&control);
            }
            use std::sync::atomic::Ordering::Relaxed;
            for guard in 0..4 {
                match guard {
                    0 => control.stack_limit = u64::MAX,
                    1 => active.store(true, Relaxed),
                    2 => revision.store(8, Relaxed),
                    3 => bytes[0].store(1, Relaxed),
                    _ => unreachable!(),
                }
                std::hint::black_box(&control);
                unsafe {
                    std::ptr::addr_of_mut!(TAIL_OBSERVED).write([0; 10]);
                }
                let mut output = [0u64; 13];
                unsafe {
                    tail_entry_probe(
                        buffer.as_ptr().add(code.ordinary_entry.unwrap()),
                        args.as_ptr(),
                        arity as u64,
                        NIL.0,
                        execution.as_ptr() as *mut u64,
                        output.as_mut_ptr(),
                    );
                }
                assert_eq!(
                    [output[0], output[8], output[9], output[1]],
                    [0xc01d, 3, 0x789, 1]
                );
                assert_eq!(depth, 3, "cold admission cannot retire the caller's depth");
                let observed = unsafe { std::ptr::addr_of!(TAIL_OBSERVED).read() };
                assert_eq!(observed[6], 0, "rejected admission must not enter target");
                let offset = observed[7] as usize - buffer.as_ptr() as usize;
                assert!(
                    code.native_calls
                        .as_ref()
                        .unwrap()
                        .value_map(offset)
                        .is_some(),
                    "cold return PC retains caller roots"
                );
                control.stack_limit = 0;
                active.store(false, Relaxed);
                revision.store(7, Relaxed);
                bytes[0].store(0, Relaxed);
            }
        }
    }
}
