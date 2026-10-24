// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{emit::*, frame_state::*, ir::*};
use egcl_rt::{
    exec_context::{ExecutionKind, LispExecutionContext},
    jit::JitBuffer,
    value::NIL,
};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};

const STACK_SENTINEL: u64 = 0x1357_2468;
const GUARD_RESULT: u64 = 88 * 8;
const ARITY_RESULT: u64 = 99 * 8;

// The word at [call-SP-16] is below the return PC. A PUSH RBP changes it;
// a truly frameless return cannot. The probe is naked and owns its red zone.
#[unsafe(naked)]
unsafe extern "C" fn run(
    _entry: *const u8,
    _argument: u64,
    _nargs: u64,
    _out: *mut u64,
    _execution: *const LispExecutionContext,
) {
    core::arch::naked_asm!(
        "push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
        "sub rsp,40", "mov rbp,rsp", "mov r12,rcx", "mov r13,r8",
        "mov rbx,0x1111", "mov r14,0x2222", "mov r15,0x3333",
        "mov [r12+16],rbp", "mov r11,rdi", "mov rdi,rsi", "mov rcx,rdx",
        "mov rax,{sentinel}", "mov [rsp-16],rax", "mov rax,{nil}", "call r11",
        "mov [r12],rax", "setc byte ptr [r12+8]", "mov rax,[rsp-16]", "mov [r12+24],rax",
        "mov [r12+40],rbp", "mov [r12+48],rbx", "mov [r12+56],r14", "mov [r12+64],r15",
        "mov [r12+72],r13", "add rsp,40", "pop r15", "pop r14", "pop r13",
        "pop r12", "pop rbx", "pop rbp", "ret",
        sentinel = const STACK_SENTINEL, nil = const NIL.0,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn guard() {
    // OrdinaryGuardRequest.payload points to [symbol,bcp,nlocals,nstack,local n,...].
    core::arch::naked_asm!("mov [r12+32],rbp", "mov rax,[rdi+16]", "mov rax,[rax+32]",
        "mov [r12+80],rax", "mov rax,{value}", "clc", "ret", value = const GUARD_RESULT);
}
#[unsafe(naked)]
unsafe extern "C" fn wrong_arity() {
    core::arch::naked_asm!("mov [r12+32],rbp", "mov rax,{value}", "clc", "ret", value = const ARITY_RESULT);
}
#[unsafe(naked)]
unsafe extern "C" fn unexpected() {
    core::arch::naked_asm!("ud2");
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

fn fixture() -> Function {
    let mut f = Function::new("ordinary-general-early-return");
    let entry = f.entry();
    let base = f.make_block();
    let framed = f.make_block();
    let n = f.add_block_param(
        entry,
        IRType::of(TypeBits::FIXNUM),
        ValueRepresentation::Tagged,
    );
    let two = f
        .push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(2)),
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0];
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 77,
            bcp: 0,
            locals: vec![ValueSource::Value {
                value: n,
                repr: ValueRepresentation::Tagged,
            }],
            stack: vec![
                ValueSource::Value {
                    value: n,
                    repr: ValueRepresentation::Tagged,
                },
                ValueSource::Value {
                    value: two,
                    repr: ValueRepresentation::Tagged,
                },
            ],
        }],
        remat: vec![],
    });
    // As in the existing fib fixture, the framed emitter supplies the runtime
    // fixnum guard for an unchecked entry argument used by FixnumCmpLt.
    let mut compare = inst(Opcode::FixnumCmpLt, vec![n, two], AuxData::None);
    // Speculation retains deopt ordering on the typed comparison even though
    // its successful machine path is a pure comparison, not a runtime call.
    compare.flags.guard = true;
    compare.flags.effectful = true;
    compare.frame_state = Some(state);
    let predicate = f
        .push_inst(
            entry,
            compare,
            &[(IRType::of(TypeBits::TOP), ValueRepresentation::Tagged)],
        )
        .1[0];
    let mut branch = inst(Opcode::Brif, vec![predicate], AuxData::None);
    branch.targets = vec![
        BlockCall {
            block: base,
            args: vec![],
        },
        BlockCall {
            block: framed,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    f.set_terminator(base, inst(Opcode::Return, vec![n], AuxData::None));
    let answer = f
        .push_inst(
            framed,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(23)),
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0];
    f.set_terminator(framed, inst(Opcode::Return, vec![answer], AuxData::None));
    f
}

fn emit() -> FramedCode {
    let bytes = [AtomicU8::new(0), AtomicU8::new(0)];
    let words = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
    let f = fixture();
    egcl_compiler::t2::verify::verify(&f).unwrap();
    // No CALL or loop polls: flag addresses meet API admission but no emitted
    // instruction refers to them. This deliberately uses the general emitter.
    emit_framed_ordinary_function_with_poll(
        &f,
        OrdinaryLispEntryOptions {
            arity: 1,
            arity_error_veneer: wrong_arity as *const () as u64, binding: None,
        },
        &[],
        OrdinaryCallFlags {
            gc_requested: bytes[0].as_ptr() as u64,
            sigusr1_pending: bytes[1].as_ptr() as u64,
            posted_signal: words[0].as_ptr() as u64,
            native_errors: words[1].as_ptr() as u64,
            signal_epoch: words[2].as_ptr() as u64,
        },
        unexpected as *const () as u64,
        OrdinaryGuardOptions {
            veneer: guard as *const () as u64,
            multiple_values: unexpected as *const () as u64,
        },
        0,
    )
    .unwrap()
}

fn execute(code: &FramedCode, argument: u64, nargs: u64) -> [u64; 11] {
    let buffer = JitBuffer::new(&code.code).unwrap();
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
        expected_generation: 0,
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
    let mut out = [0; 11];
    unsafe {
        execution.with_active_native_control(std::pin::Pin::new_unchecked(&mut control), || {
            run(
                buffer.as_ptr().add(code.ordinary_entry.unwrap()),
                argument,
                nargs,
                out.as_mut_ptr(),
                execution.as_ptr(),
            );
        });
    }
    assert_eq!(
        out[1], 0,
        "all fixture paths return a single result with CF clear"
    );
    assert_eq!(out[5], out[2], "RBP restored");
    assert_eq!(
        &out[6..9],
        &[0x1111, 0x2222, 0x3333],
        "callee-saved registers preserved"
    );
    assert_eq!(
        out[9],
        execution.as_ptr() as u64,
        "R13 remains the owning execution"
    );
    assert_eq!(depth, 1);
    out
}

#[test]
fn ordinary_base_case_returns_before_frame_setup() {
    let code = emit();
    for n in [-3_i64, 0, 1] {
        let argument = (n << 3) as u64;
        let out = execute(&code, argument, 1);
        assert_eq!(out[0], argument);
        assert_eq!(
            out[3], STACK_SENTINEL,
            "base case must not push even the first RBP word"
        );
        assert_eq!(out[4], 0, "base case must not enter an adapter");
    }
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, Register};
    let instructions: Vec<_> = Decoder::new(
        64,
        &code.code[code.ordinary_entry.unwrap()..],
        DecoderOptions::NONE,
    )
    .into_iter()
    .collect();
    let frame = instructions
        .iter()
        .position(|i| i.mnemonic() == Mnemonic::Push && i.op0_register() == Register::RBP)
        .unwrap();
    assert!(
        instructions[..frame]
            .iter()
            .any(|i| i.mnemonic() == Mnemonic::Ret),
        "ordinary entry must contain the frameless return before its first prologue"
    );
    assert!(
        !instructions[..frame]
            .iter()
            .any(|i| i.mnemonic() == Mnemonic::Call),
        "frameless prefix must not call helpers"
    );
}

#[test]
fn ordinary_nonbase_and_failed_type_guard_use_original_frame_and_arity_gate() {
    let code = emit();
    let nonbase = execute(&code, 2 * 8, 1);
    assert_eq!(nonbase[0], 23 * 8);
    assert_eq!(
        nonbase[3], nonbase[2],
        "nonbase must save caller RBP in its frame"
    );
    assert_eq!(nonbase[4], 0);
    let failed_type = execute(&code, NIL.0, 1);
    assert_eq!(failed_type[0], GUARD_RESULT);
    assert_eq!(
        failed_type[10], NIL.0,
        "framed deopt payload must preserve the original argument"
    );
    assert_eq!(failed_type[3], failed_type[2]);
    assert_eq!(
        failed_type[4],
        failed_type[2] - 16,
        "failed prefix type check must enter original framed guard recovery"
    );
    // Even a would-be base argument must not bypass the public arity check.
    for nargs in [0, 2] {
        let wrong = execute(&code, 0, nargs);
        assert_eq!(wrong[0], ARITY_RESULT);
        assert_eq!(wrong[3], STACK_SENTINEL);
        assert_eq!(wrong[4], wrong[2], "arity adapter runs before any prologue");
    }
}
