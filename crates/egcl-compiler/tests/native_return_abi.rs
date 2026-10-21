// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{
    build::build_from_bytecode_for_native_cleanups,
    emit::{FramedCode, emit_framed_native_handlers_with_recursion},
    frame_state::{FrameScope, FrameState, ValueSource},
    ir::{
        AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, Value,
        ValueRepresentation,
    },
    native_transfer::emit_helper_veneer,
};
use egcl_rt::{
    EgclStack,
    bytecode::{BytecodeFunction, Instr},
    jit::JitBuffer,
    native_transfer::{
        NativeExit, NativeOutcome, NativeReturnValues, SNAPSHOT_NATIVE_RETURN_VALUES,
    },
    value::{EgclVal, NIL},
};
use std::cell::Cell;

thread_local! {
    static COUNT: Cell<usize> = const { Cell::new(1) };
    static SNAPSHOTS: Cell<usize> = const { Cell::new(0) };
    static COMPLETIONS: Cell<usize> = const { Cell::new(0) };
}
static PAYLOAD: [EgclVal; 3] = [EgclVal(336), EgclVal(344), EgclVal(352)];

// The fixture owns a static tuple, so these helpers never allocate, collect,
// suspend or require a runtime Env. The tested code is the actual framed emitter.
unsafe extern "C" fn snapshot(_: u64, dst: *mut EgclVal, operation: u64) {
    assert_eq!(operation, SNAPSHOT_NATIVE_RETURN_VALUES);
    SNAPSHOTS.with(|calls| calls.set(calls.get() + 1));
    let count = COUNT.with(Cell::get);
    unsafe {
        dst.cast::<NativeReturnValues>().write(NativeReturnValues {
            count,
            payload: if count > 1 {
                PAYLOAD.as_ptr()
            } else {
                std::ptr::null()
            },
        });
    }
}

unsafe extern "C" fn complete(_: *mut u8, out: *mut NativeOutcome) {
    COMPLETIONS.with(|calls| calls.set(calls.get() + 1));
    unsafe {
        out.write(NativeOutcome {
            value: if COUNT.with(Cell::get) == 0 {
                NIL
            } else {
                PAYLOAD[0]
            },
            exit: NativeExit::Returned,
        });
    }
}

#[repr(C)]
#[derive(Default, Debug)]
struct Returned {
    primary: u64,
    count: usize,
    payload: *const EgclVal,
    carry: u8,
}

// No C helper or outer adapter stands between the body's RET and this capture.
#[unsafe(naked)]
unsafe extern "C" fn observe(
    _entry: *const u8,
    _slots: *mut EgclVal,
    _stack: *const EgclStack,
    _out: *mut Returned,
    _incoming_carry: u64,
) {
    core::arch::naked_asm!(
        "push rbx",
        "mov rbx, rcx",
        "mov r11, rdi",
        "mov rdi, rsi",
        "mov rsi, rdx",
        "xor edx, edx",
        "bt r8, 0",
        "call r11",
        "setc byte ptr [rbx + 24]",
        "mov [rbx], rax",
        "mov [rbx + 8], rdx",
        "mov [rbx + 16], rcx",
        "pop rbx",
        "ret",
    );
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

fn leaf(guarded: bool) -> Function {
    let mut ir = Function::new("native-return-leaf");
    let entry = ir.entry();
    let input = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let value = if guarded {
        let state = ir.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: input,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let (_, checked) = ir.push_inst(
            entry,
            InstData {
                aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                flags: InstFlags {
                    effectful: true,
                    guard: true,
                    ..Default::default()
                },
                frame_state: Some(state),
                ..instruction(Opcode::Guard, vec![input])
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        checked[0]
    } else {
        input
    };
    ir.set_terminator(entry, instruction(Opcode::Return, vec![value]));
    ir
}

fn call_body() -> Function {
    let body = BytecodeFunction {
        self_call_sites: Vec::new(),
        name: "native-return-call".into(),
        code: vec![Instr::CallNamed { sym: 1, nargs: 0 }, Instr::Return],
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
        params_form: NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
        binder_cache: Default::default(),
    };
    build_from_bytecode_for_native_cleanups(&body).unwrap()
}

fn emit(ir: &Function, helper: &JitBuffer, deopt: bool) -> FramedCode {
    emit_framed_native_handlers_with_recursion(
        ir,
        helper.as_ptr() as u64,
        1,
        1,
        1,
        snapshot as *const () as u64,
        1,
        1,
        0,
        None,
        deopt.then_some(helper.as_ptr() as u64),
    )
    .unwrap()
    .0
}

fn check_dynamic(emitted: &FramedCode, input: EgclVal) {
    let code = JitBuffer::new(&emitted.code).unwrap();
    let stack = EgclStack::new(64 * 1024);
    for count in [0, 1, 3] {
        for carry in [0, 1] {
            COUNT.with(|slot| slot.set(count));
            SNAPSHOTS.with(|slot| slot.set(0));
            COMPLETIONS.with(|slot| slot.set(0));
            let mut slots = vec![NIL; 1 + usize::from(emitted.shadow_root_slots)];
            slots[0] = input;
            let mut returned = Returned::default();
            unsafe {
                observe(
                    code.as_ptr(),
                    slots.as_mut_ptr(),
                    &stack,
                    &mut returned,
                    carry,
                );
            }
            assert_eq!(
                COMPLETIONS.with(Cell::get),
                1,
                "the intended call/deopt must run"
            );
            assert_eq!(
                SNAPSHOTS.with(Cell::get),
                1,
                "the body return must snapshot its result"
            );
            assert_eq!(
                returned.primary,
                if count == 0 { NIL.0 } else { PAYLOAD[0].0 }
            );
            assert_eq!(
                returned.carry,
                u8::from(count != 1),
                "count {count}; {returned:?}"
            );
            if count != 1 {
                assert_eq!(returned.count, count);
                assert_eq!(
                    returned.payload,
                    if count == 0 {
                        std::ptr::null()
                    } else {
                        PAYLOAD.as_ptr()
                    }
                );
            }
        }
    }
}

#[test]
fn framed_known_single_return_clears_carry_without_snapshot() {
    let helper = JitBuffer::new(&emit_helper_veneer(complete, std::ptr::null())).unwrap();
    let emitted = emit(&leaf(false), &helper, false);
    let code = JitBuffer::new(&emitted.code).unwrap();
    let stack = EgclStack::new(64 * 1024);
    for carry in [0, 1] {
        SNAPSHOTS.with(|slot| slot.set(0));
        COMPLETIONS.with(|slot| slot.set(0));
        let mut slots = vec![EgclVal::from_fixnum(42); 1 + usize::from(emitted.shadow_root_slots)];
        let mut returned = Returned::default();
        unsafe {
            observe(
                code.as_ptr(),
                slots.as_mut_ptr(),
                &stack,
                &mut returned,
                carry,
            );
        }
        assert_eq!(returned.primary, EgclVal::from_fixnum(42).0);
        assert_eq!(returned.carry, 0, "incoming carry {carry}");
        assert_eq!(SNAPSHOTS.with(Cell::get), 0);
        assert_eq!(COMPLETIONS.with(Cell::get), 0);
    }
}

#[test]
fn framed_unknown_return_snapshots_zero_one_many() {
    let helper = JitBuffer::new(&emit_helper_veneer(complete, std::ptr::null())).unwrap();
    check_dynamic(&emit(&call_body(), &helper, false), NIL);
}

#[test]
fn framed_precise_deopt_return_snapshots_zero_one_many() {
    let helper = JitBuffer::new(&emit_helper_veneer(complete, std::ptr::null())).unwrap();
    let emitted = emit(&leaf(true), &helper, true);
    assert!(emitted.has_deopt);
    check_dynamic(&emitted, EgclVal::from_single_float(1.5));
}
