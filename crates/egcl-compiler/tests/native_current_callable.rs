// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use egcl_compiler::t2::{emit::*, ir::*};
use egcl_rt::{
    exec_context::{ExecutionKind, LispExecutionContext},
    jit::JitBuffer,
    value::EgclVal,
};

// Seven actuals force both register and overflow transport. The hidden word is
// a mock tagged identity: this tests native transport, not closure construction.
#[unsafe(naked)]
unsafe extern "C" fn enter(
    _entry: *const u8,
    _hidden: u64,
    _execution: *const LispExecutionContext,
    _out: *mut u64,
) {
    core::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 40",
        "mov [rsp+32], rcx",
        "mov r11, rdi",
        "mov rax, rsi",
        "mov r13, rdx",
        "mov rcx, 7",
        "mov rdi, 8",
        "mov rsi, 16",
        "mov rdx, 24",
        "mov qword ptr [rsp], 32",
        "mov qword ptr [rsp+8], 40",
        "mov qword ptr [rsp+16], 48",
        "mov qword ptr [rsp+24], 56",
        "call r11",
        "mov r10, [rsp+32]",
        "mov [r10], rax",
        "setc byte ptr [r10+8]",
        "add rsp, 40",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    );
}

fn current_callable(arity: usize) -> FramedCode {
    let mut f = Function::new("native-current-callable");
    let entry = f.entry();
    for _ in 0..arity {
        f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    }
    let plain = |opcode| InstData {
        opcode,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    };
    let (_, value) = f.push_inst(
        entry,
        plain(Opcode::CurrentCallable),
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    f.set_terminator(
        entry,
        InstData {
            args: vec![value[0]],
            flags: InstFlags {
                terminator: true,
                ..Default::default()
            },
            ..plain(Opcode::Return)
        },
    );
    emit_framed_ordinary_function(
        &f,
        OrdinaryLispEntryOptions {
            require_callable_home: false,
            arity,
            arity_error_veneer: 1,
            binding: None,
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
    )
    .unwrap()
}

#[test]
fn native_hidden_callable_survives_prologue_and_argument_transport() {
    let code = current_callable(7);
    assert!(
        code.native_callable_slot.is_some(),
        "actual precise per-frame home is required"
    );
    let executable = JitBuffer::new(&code.code).unwrap();
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
    for hidden in [EgclVal::from_fixnum(71), EgclVal::from_fixnum(-41)] {
        let mut out = [0u64; 2];
        unsafe {
            enter(
                executable.as_ptr().add(code.ordinary_entry.unwrap()),
                hidden.0,
                &execution,
                out.as_mut_ptr(),
            );
        }
        assert_eq!(out[0], hidden.0);
        assert_eq!(out[1] & 0xff, 0, "single primary must clear carry");
    }
}

#[unsafe(naked)]
unsafe extern "C" fn enter_probe(
    _entry: *const u8,
    _execution: *const LispExecutionContext,
    _key: u64,
    _argument: u64,
    _out: *mut u64,
) {
    core::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 8",
        "mov [rsp], r8",
        "mov r11, rdi",
        "mov r13, rsi",
        "mov rdi, rdx",
        "mov rsi, rcx",
        "mov rcx, 2",
        "mov rax, -616",
        "call r11",
        "mov r10, [rsp]",
        "mov [r10], rax",
        "setc byte ptr [r10+8]",
        "add rsp, 8",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    );
}

#[test]
fn native_funcall_probe_commits_selected_word_only_after_all_weak_lease_checks() {
    use egcl_compiler::t2::native_transfer::*;
    use egcl_rt::call_table::*;
    use std::{
        mem::offset_of,
        pin::Pin,
        sync::atomic::{AtomicU64, Ordering},
    };
    let code = current_callable(1);
    let target = JitBuffer::new(&code.code).unwrap();
    let execution = LispExecutionContext::new(ExecutionKind::Fiber, 1, 2);
    // No runtime, allocation or poll is invoked by this kernel. Only the raw
    // stack bound is read; every field is an integer or raw pointer.
    let mut control: NativeRecursiveControl = unsafe { std::mem::zeroed() };
    control.execution = &execution;
    let revision = AtomicU64::new(7);
    let epoch = AtomicU64::new(11);
    let generations = AtomicU64::new(23);
    let mut descriptor = OrdinaryEntryDescriptor {
        kind: ORDINARY_X64_LISP_KIND,
        version: ORDINARY_X64_LISP_VERSION,
        revision: 7,
        entry: target.as_ptr() as usize + code.ordinary_entry.unwrap(),
        code_start: target.as_ptr() as usize,
        code_len: code.code.len(),
        arity: 1,
        environment: ORDINARY_ENV_CLOSURE,
        owner: 1,
        stack_required: code.ordinary_stack_required.unwrap(),
    };
    let key = EgclVal::from_fixnum(61).0;
    let selected = EgclVal::from_fixnum(55).0;
    let mut entries: [OrdinaryFuncallProbeEntry; ORDINARY_FUNCALL_PROBE_ENTRIES] =
        std::array::from_fn(|_| OrdinaryFuncallProbeEntry {
            function: 0,
            selected_callable: 0,
            epoch: u64::MAX,
            collections: u64::MAX,
            revision_address: 0,
            revision: u64::MAX,
            descriptor: 0,
        });
    entries[0] = OrdinaryFuncallProbeEntry {
        function: key,
        selected_callable: selected,
        epoch: 11,
        collections: 23,
        revision_address: revision.as_ptr() as u64,
        revision: 7,
        descriptor: std::ptr::from_ref(&descriptor) as u64,
    };
    let mut bytes = emit_ordinary_funcall_probe(OrdinaryFuncallProbe {
        table: entries.as_ptr() as u64,
        epoch: epoch.as_ptr() as u64,
        collections: generations.as_ptr() as u64,
        active_control: egcl_rt::exec_context::offsets::ACTIVE_NATIVE_CONTROL as i32,
        stack_limit: u8::try_from(offset_of!(NativeRecursiveControl, stack_limit)).unwrap(),
    });
    bytes.extend_from_slice(&[0xf8, 0xc3]); // miss: preserve incoming RAX, CLC, RET
    let probe = JitBuffer::new(&bytes).unwrap();
    let run = |control: &mut NativeRecursiveControl| {
        let mut out = [0u64; 2];
        unsafe {
            execution.with_active_native_control(Pin::new_unchecked(control), || {
                enter_probe(probe.as_ptr(), &execution, key, 8, out.as_mut_ptr());
            });
        }
        assert_eq!(out[1] & 0xff, 0);
        out[0]
    };
    let live = entries[0];
    let empty = entries[1];
    for index in [0, 1, 3] {
        entries.fill(empty);
        entries[index] = live;
        std::hint::black_box(&entries);
        assert_eq!(run(&mut control), selected, "selected slot {index}");
        descriptor.arity = 2;
        std::hint::black_box(&descriptor);
        assert_eq!(
            run(&mut control),
            EgclVal::from_fixnum(-77).0,
            "arity miss in slot {index} must preserve incoming carrier"
        );
        descriptor.arity = 1;
        std::hint::black_box(&descriptor);
        control.stack_limit = u64::MAX;
        assert_eq!(
            run(&mut control),
            EgclVal::from_fixnum(-77).0,
            "stack miss in slot {index} must preserve incoming carrier"
        );
        control.stack_limit = 0;
    }
    entries.fill(empty);
    entries[0] = live;
    generations.store(24, Ordering::Release);
    // These pointers are now deliberately unusable. A stale weak generation
    // must reject BEFORE reading either retained descriptor/revision storage.
    entries[0].revision_address = 1;
    entries[0].descriptor = 1;
    std::hint::black_box(&entries);
    assert_eq!(run(&mut control), EgclVal::from_fixnum(-77).0);
}
