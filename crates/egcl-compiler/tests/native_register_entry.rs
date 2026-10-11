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
        arity,
        arity_error_veneer: ordinary_arity_observer as *const () as u64,
    }
}

fn probe(
    code: &egcl_compiler::t2::emit::FramedCode,
    count: u64,
    closure: u64,
) -> ([u64; 8], [u64; 8]) {
    let buffer = JitBuffer::new(&code.code).unwrap();
    let mut observed = [0; 8];
    let mut output = [0; 8];
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
        output[2..],
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
