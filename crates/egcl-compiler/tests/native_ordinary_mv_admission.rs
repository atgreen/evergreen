// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{emit::*, ir::*};
use egcl_rt::{
    call_table::*,
    exec_context::{ExecutionKind, LispExecutionContext},
    jit::JitBuffer,
    value::NIL,
};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize};

#[repr(C)]
struct Control {
    native: NativeRecursiveControl,
    clears: u64,
    cold_calls: u64,
    second_calls: u64,
}

static TWO_VALUES: [u64; 2] = [8, 16];

#[unsafe(naked)]
unsafe extern "C" fn first() {
    core::arch::naked_asm!(
        "mov r11, [r13+{control}]", "mov r11, [r11+{active}]",
        "mov byte ptr [r11], 1", "mov rax, 8", "mov rdx, 2",
        "lea rcx, [rip+{values}]", "stc", "ret",
        control = const egcl_rt::exec_context::offsets::ACTIVE_NATIVE_CONTROL,
        active = const std::mem::offset_of!(NativeRecursiveControl, mv_active),
        values = sym TWO_VALUES,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn second() {
    core::arch::naked_asm!(
        "mov r11, [r13+{control}]", "inc qword ptr [r11+{calls}]",
        "mov rax, 40", "clc", "ret",
        control = const egcl_rt::exec_context::offsets::ACTIVE_NATIVE_CONTROL,
        calls = const std::mem::offset_of!(Control, second_calls),
    );
}

#[unsafe(naked)]
unsafe extern "C" fn clear_mv() {
    core::arch::naked_asm!(
        "mov r11, [r13+{control}]", "inc qword ptr [r11+{clears}]",
        "mov r11, [r11+{active}]", "mov byte ptr [r11], 0", "xor eax,eax", "ret",
        control = const egcl_rt::exec_context::offsets::ACTIVE_NATIVE_CONTROL,
        clears = const std::mem::offset_of!(Control, clears),
        active = const std::mem::offset_of!(NativeRecursiveControl, mv_active),
    );
}

#[unsafe(naked)]
unsafe extern "C" fn cold() {
    core::arch::naked_asm!(
        "mov r11, [rdi+{control}]", "inc qword ptr [r11+{calls}]",
        "mov r11, [r11+{active}]", "mov byte ptr [r11],0",
        "mov rax,24", "clc", "ret",
        control = const std::mem::offset_of!(OrdinaryColdRequest, control),
        calls = const std::mem::offset_of!(Control, cold_calls),
        active = const std::mem::offset_of!(NativeRecursiveControl, mv_active),
    );
}

#[unsafe(naked)]
unsafe extern "C" fn rejected() {
    core::arch::naked_asm!("ud2");
}

#[unsafe(naked)]
unsafe extern "C" fn run(
    _entry: *const u8,
    _execution: *const LispExecutionContext,
    _out: *mut u64,
) {
    core::arch::naked_asm!(
        "push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
        "sub rsp,24", "mov [rsp],rdx", "mov r11,rdi", "mov r13,rsi",
        "xor ecx,ecx", "mov rax,{nil}", "call r11",
        "mov r10,[rsp]", "mov [r10],rax", "setc byte ptr [r10+8]",
        "add rsp,24", "pop r15", "pop r14", "pop r13", "pop r12", "pop rbx", "pop rbp", "ret",
        nil = const NIL.0,
    );
}

fn inst(opcode: Opcode, args: Vec<Value>, aux: AuxData) -> InstData {
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

fn exercise(clear_between: bool) {
    let mut f = Function::new("ordinary-mv-admission");
    let entry = f.entry();
    let mut sites = Vec::new();
    let mut last = None;
    for symbol in [71, 72] {
        if symbol == 72 && clear_between {
            let mut clear = inst(Opcode::ClearMv, vec![], AuxData::None);
            clear.flags = InstFlags { call: true, safepoint: true, effectful: true, ..InstFlags::default() };
            f.push_inst(entry, clear, &[]);
        }
        let mut call = inst(Opcode::Call, vec![], AuxData::CallTarget(symbol));
        call.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..InstFlags::default()
        };
        let (site, results) = f.push_inst(
            entry,
            call,
            &[(IRType::of(TypeBits::TOP), ValueRepresentation::Tagged)],
        );
        sites.push(site);
        last = Some(results[0]);
    }
    f.set_terminator(
        entry,
        inst(Opcode::Return, vec![last.unwrap()], AuxData::None),
    );
    let normalized = egcl_compiler::t2::opt_clear_mv::calls_with_discarded_multiple_values(&f);
    assert_eq!(normalized.contains(&sites[0]), clear_between);
    let known = egcl_compiler::t2::opt_clear_mv::single_value_state_after_normalized_calls(
        &f,
        true,
        &normalized,
    );
    assert!(known.contains(&sites[0]));
    assert_eq!(known.contains(&sites[1]), clear_between);

    let descriptors =
        [first as *const (), second as *const ()].map(|entry| OrdinaryEntryDescriptor {
            kind: ORDINARY_X64_LISP_KIND,
            version: ORDINARY_X64_LISP_VERSION,
            revision: 7,
            entry: entry as usize,
            code_start: entry as usize,
            code_len: 128,
            arity: 0,
            environment: ORDINARY_ENV_NONE,
            owner: entry as usize,
            stack_required: 8,
        });
    let slots = descriptors
        .each_ref()
        .map(|descriptor| AtomicUsize::new(descriptor as *const _ as usize));
    let revisions = [AtomicU64::new(7), AtomicU64::new(7)];
    let bytes = [AtomicU8::new(0), AtomicU8::new(0)];
    let words = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
    let flags = OrdinaryCallFlags {
        gc_requested: bytes[0].as_ptr() as u64,
        sigusr1_pending: bytes[1].as_ptr() as u64,
        posted_signal: words[0].as_ptr() as u64,
        native_errors: words[1].as_ptr() as u64,
        signal_epoch: words[2].as_ptr() as u64,
    };
    let targets: Vec<_> = sites
        .iter()
        .enumerate()
        .map(|(i, &site)| {
            (
                site,
                OrdinaryCallTarget {
                    symbol: 71 + i as u32,
                    descriptor_slot: slots[i].as_ptr() as u64,
                    revision_address: revisions[i].as_ptr() as u64,
                    heat_slot: 0,
                },
            )
        })
        .collect();
    let emitted = emit_framed_ordinary_function_with_poll(
        &f,
        OrdinaryLispEntryOptions {
            arity: 0,
            arity_error_veneer: rejected as *const () as u64,
        },
        &targets,
        flags,
        cold as *const () as u64,
        OrdinaryGuardOptions {
            veneer: rejected as *const () as u64,
            multiple_values: clear_mv as *const () as u64,
        },
        0,
    )
    .unwrap();
    let buffer = JitBuffer::new(&emitted.code).unwrap();
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
    let mut depth = 1;
    let active = AtomicBool::new(false);
    let mut control = Control {
        native: NativeRecursiveControl {
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
        },
        clears: 0,
        cold_calls: 0,
        second_calls: 0,
    };
    let mut result = [0_u64; 2];
    unsafe {
        execution.with_active_native_control(
            std::pin::Pin::new_unchecked(&mut control.native),
            || {
                run(
                    buffer.as_ptr().add(emitted.ordinary_entry.unwrap()),
                    execution.as_ptr(),
                    result.as_mut_ptr(),
                );
            },
        );
    }
    assert_eq!(result, [if clear_between { 40 } else { 24 }, 0]);
    assert_eq!(depth, 1, "admitted children must balance native depth");
    assert!(!active.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(control.clears, u64::from(clear_between));
    assert_eq!(control.second_calls, u64::from(clear_between));
    assert_eq!(
        control.cold_calls,
        u64::from(!clear_between),
        "unknown active MV must take the cold path, normalized MV must admit second callee"
    );

    // Execution proves the retained check matters; decoding proves the known
    // empty cases did not retain a redundant load of control.mv_active.
    let mv_pointer_loads =
        iced_x86::Decoder::new(64, &emitted.code, iced_x86::DecoderOptions::NONE)
            .into_iter()
            .filter(|instruction| {
                instruction.mnemonic() == iced_x86::Mnemonic::Mov
                    && instruction.op1_kind() == iced_x86::OpKind::Memory
                    && instruction.memory_base() == iced_x86::Register::RDI
                    && instruction.memory_displacement64()
                        == std::mem::offset_of!(NativeRecursiveControl, mv_active) as u64
            })
            .count();
    assert_eq!(
        mv_pointer_loads,
        usize::from(!clear_between),
        "only the unproven second call may read control.mv_active"
    );
}

#[test]
fn known_empty_call_admission_omits_mv_load_after_real_tuple_cleanup() {
    exercise(true);
}

#[test]
fn unknown_call_admission_keeps_mv_check_and_rejects_active_tuple() {
    exercise(false);
}
