// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::deopt::rebox_f64_payload;
use egcl_compiler::t2::emit::{
    NativeCallStorage, NativeRootPublication, TransferDeoptRequest,
    emit_framed_native_handlers_with_entries_and_storage,
};
use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
use egcl_compiler::t2::ir::{
    AuxData, BlockCall, Function, IRType, InstData, InstFlags, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use egcl_compiler::t2::native_transfer::{SysvTransferCapture, emit_published_helper_veneer};
use egcl_compiler::t2::x64_calls::{NativeCallSites, NativeCallValues};
use egcl_compiler::t2::x64_value_maps::NativeValueLocation;
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeReturnValues};
use egcl_rt::value::{EgclVal, NIL};
use egcl_rt::{Collector, EgclStack, HeapCollector};
use std::cell::Cell;
use std::sync::Arc;

struct Probe {
    calls: Arc<NativeCallSites>,
    base: usize,
    raw: Value,
    held: Value,
    raw_bits: u64,
    guard: *mut u64,
    polls: usize,
    deopts: usize,
    wrong_type: bool,
    input_bits: u64,
}
thread_local! { static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) }; }

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

fn fixture(guard: *mut u64) -> (Function, Value, Value) {
    let mut f = Function::new("raw-f64-poll-deopt");
    let entry = f.entry();
    let header = f.make_block();
    let boxed = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let held = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp: 0,
            locals: vec![boxed, held]
                .into_iter()
                .map(|value| ValueSource::Value {
                    value,
                    repr: ValueRepresentation::Tagged,
                })
                .collect(),
            stack: vec![],
        }],
        remat: vec![],
    });
    let mut unbox = instruction(Opcode::UnboxFloat, vec![boxed]);
    unbox.flags.guard = true;
    unbox.flags.effectful = true;
    unbox.frame_state = Some(state);
    let (_, raw) = f.push_inst(
        entry,
        unbox,
        &[(
            IRType::of(TypeBits::DOUBLE_FLOAT),
            ValueRepresentation::UnboxedF64,
        )],
    );
    let raw = raw[0];
    let mut jump = instruction(Opcode::Jump, vec![]);
    jump.targets.push(BlockCall {
        block: header,
        args: vec![],
    });
    f.set_terminator(entry, jump.clone());
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp: 1,
            locals: vec![
                ValueSource::Value {
                    value: raw,
                    repr: ValueRepresentation::UnboxedF64,
                },
                ValueSource::Value {
                    value: held,
                    repr: ValueRepresentation::Tagged,
                },
            ],
            stack: vec![],
        }],
        remat: vec![],
    });
    let mut check = instruction(Opcode::Guard, vec![]);
    check.aux = AuxData::BuiltinBinding {
        address: guard as usize,
        expected: 0,
    };
    check.flags.guard = true;
    check.flags.effectful = true;
    check.frame_state = Some(state);
    f.push_inst(header, check, &[]);
    f.set_terminator(header, jump);
    egcl_compiler::t2::verify::verify(&f).unwrap();
    (f, raw, held)
}

unsafe fn physical(
    image: *mut SysvTransferCapture,
    body: *mut u8,
    location: NativeValueLocation,
) -> *mut EgclVal {
    match location {
        NativeValueLocation::Stack(offset) => unsafe { body.offset(offset as isize).cast() },
        NativeValueLocation::Register(register) => {
            let index = [3, 5, 12, 13, 14, 15]
                .iter()
                .position(|&r| r == register)
                .unwrap();
            unsafe {
                std::ptr::addr_of_mut!((*image).preserved)
                    .cast::<u64>()
                    .add(index)
                    .cast()
            }
        }
        other => panic!("native published home required, got {other:?}"),
    }
}

unsafe extern "C" fn poll(_: *mut u8, out: *mut NativeOutcome, image: *mut SysvTransferCapture) {
    let probe = unsafe { &mut *PROBE.with(Cell::get) };
    let offset = unsafe { (*image).return_pc as usize } - probe.base;
    let site = probe.calls.get(offset).unwrap();
    let Some(NativeCallValues::Frame(map)) = probe.calls.value_map(offset) else {
        panic!("executed poll must have an authoritative map")
    };
    let body = unsafe {
        (*image)
            .caller_sp
            .cast::<u8>()
            .add(site.stack_adjust as usize)
            .cast_mut()
    };
    let raw = map.ssa_value(probe.raw).unwrap();
    assert_eq!(raw.repr, ValueRepresentation::UnboxedF64);
    assert!(!raw.may_reference_heap());
    let raw_home = unsafe { physical(image, body, raw.locations()[0]) }.cast::<u64>();
    let held = map.ssa_value(probe.held).unwrap();
    let held_home = unsafe { physical(image, body, held.locations()[0]) };
    let before = unsafe { held_home.read() };
    assert_eq!(unsafe { raw_home.read() }, probe.raw_bits);
    struct Roots(Vec<*mut EgclVal>);
    impl egcl_rt::gc::TraceHostRoots for Roots {
        fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
            for &root in &self.0 {
                visit(root);
            }
        }
    }
    {
        let mut roots = Roots(
            map.gc_locations()
                .map(|&location| unsafe { physical(image, body, location) })
                .collect(),
        );
        roots.0.sort_unstable();
        roots.0.dedup();
        assert!(!roots.0.contains(&raw_home.cast()));
        egcl_rt::rooted_ref!(_roots = &mut roots);
        HeapCollector::new().minor_gc().unwrap();
        assert_ne!(
            unsafe { held_home.read() },
            before,
            "native root must actually move"
        );
        assert_eq!(unsafe { held_home.read() }.as_double_float(), 73.5);
        assert_eq!(unsafe { raw_home.read() }, probe.raw_bits);
    }
    probe.polls += 1;
    unsafe {
        probe.guard.write(1);
        out.write(NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        });
    }
}

unsafe extern "C" fn deopt(
    request: *mut u8,
    out: *mut NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    let probe = unsafe { &mut *PROBE.with(Cell::get) };
    let offset = unsafe { (*image).return_pc as usize } - probe.base;
    let Some(NativeCallValues::Deoptimizing { f64_slots, .. }) = probe.calls.value_map(offset)
    else {
        panic!("executed deopt must retain its exact payload representation")
    };
    assert_eq!(
        f64_slots.as_ref(),
        if probe.wrong_type { &[][..] } else { &[0][..] }
    );
    let request = unsafe { &*request.cast::<TransferDeoptRequest>() };
    assert_eq!((request.n_scopes, request.n_words), (1, 6));
    let words = unsafe { std::slice::from_raw_parts_mut(request.words.cast_mut(), 6) };
    assert_eq!(&words[..4], &[0, u64::from(!probe.wrong_type), 2, 0]);
    assert_eq!(
        words[4],
        if probe.wrong_type {
            probe.input_bits
        } else {
            probe.raw_bits
        }
    );
    // Consume the descriptor belonging to the actual executed return PC.
    unsafe {
        rebox_f64_payload(&mut words[4..], f64_slots, |value| {
            HeapCollector::new().minor_gc().unwrap();
            egcl_rt::gc::alloc_double_float(value)
        })
    }
    .unwrap();
    if probe.wrong_type {
        assert_eq!(words[4], probe.input_bits);
    } else {
        assert_eq!(
            EgclVal(words[4]).as_double_float().to_bits(),
            probe.raw_bits
        );
    }
    assert_eq!(EgclVal(words[5]).as_double_float(), 73.5);
    probe.deopts += 1;
    unsafe {
        out.write(NativeOutcome {
            value: EgclVal(words[4]),
            exit: NativeExit::Returned,
        });
    }
}

unsafe extern "C" fn single_values(_: u64, dst: *mut EgclVal, operation: u64) {
    if operation == native_transfer::SNAPSHOT_NATIVE_RETURN_VALUES {
        unsafe {
            dst.cast::<NativeReturnValues>()
                .write(NativeReturnValues::SINGLE);
        }
    } else {
        assert_eq!(operation, 0);
    }
}

#[test]
fn raw_f64_survives_generated_collecting_poll_and_precise_deopt() {
    exercise_boundary(0);
}

#[test]
fn non_double_unbox_restores_the_tagged_pre_unbox_state() {
    exercise_boundary(1);
    exercise_boundary(2);
}

fn exercise_boundary(input_kind: u8) {
    let wrong_type = input_kind != 0;
    let mut guard = Box::new(0u64);
    let (ir, raw, held) = fixture(&mut *guard);
    let poll = JitBuffer::new(&emit_published_helper_veneer(poll, std::ptr::null())).unwrap();
    let deopt = JitBuffer::new(&emit_published_helper_veneer(deopt, std::ptr::null())).unwrap();
    let (emitted, _) = emit_framed_native_handlers_with_entries_and_storage(
        &ir,
        1,
        2,
        1,
        1,
        single_values as *const () as u64,
        1,
        1,
        poll.as_ptr() as u64,
        None,
        Some(deopt.as_ptr() as u64),
        &[],
        NativeRootPublication::PublishedHomes,
        NativeCallStorage::NativeStack,
    )
    .expect("mapped native emitter must admit guarded raw F64 homes");
    let code = JitBuffer::new(&emitted.code).unwrap();
    let stack = EgclStack::new(64 * 1024);
    let (mut activation, raw_bits) = {
        egcl_rt::rooted!(sentinel = egcl_rt::gc::alloc_double_float(73.5));
        let raw_bits = sentinel.0;
        let boxed = match input_kind {
            0 => egcl_rt::gc::alloc_double_float(f64::from_bits(raw_bits)),
            1 => EgclVal::from_fixnum(11),
            2 => egcl_rt::packed_f64::make(1, 0.0).unwrap(),
            _ => unreachable!(),
        };
        let mut activation = vec![NIL; 2 + usize::from(emitted.shadow_root_slots)];
        activation[0] = boxed;
        activation[1] = *sentinel;
        (activation, raw_bits)
    };
    let mut probe = Probe {
        calls: Arc::new(emitted.native_calls.clone().unwrap()),
        base: code.as_ptr() as usize,
        raw,
        held,
        raw_bits,
        guard: &mut *guard,
        polls: 0,
        deopts: 0,
        wrong_type,
        input_bits: activation[0].0,
    };
    struct Reset(*mut Probe);
    impl Drop for Reset {
        fn drop(&mut self) {
            PROBE.with(|slot| slot.set(self.0));
        }
    }
    let _reset = Reset(PROBE.with(|slot| slot.replace(&mut probe)));
    let outcome = unsafe {
        native_transfer::invoke_native_segment(
            code.as_ptr(),
            activation.as_mut_ptr().cast(),
            &stack,
        )
    }
    .unwrap();
    assert_eq!(outcome.exit, NativeExit::Returned);
    if wrong_type {
        assert_eq!(outcome.value.0, probe.input_bits);
        if input_kind == 2 {
            assert_eq!(egcl_rt::packed_f64::len(outcome.value), Some(1));
        }
    } else {
        assert_eq!(outcome.value.as_double_float().to_bits(), raw_bits);
    }
    assert_eq!((probe.polls, probe.deopts), (usize::from(!wrong_type), 1));
}

#[test]
fn raw_f64_cannot_cross_an_implicit_tagged_boundary() {
    for shape in [
        "return",
        "entry",
        "state",
        "managed",
        "no-deopt",
        "result-type",
    ] {
        let mut guard = 0;
        let (mut ir, raw, _) = fixture(&mut guard);
        let header = ir.block_order()[1];
        let mut storage = NativeCallStorage::NativeStack;
        let mut deopt = Some(1);
        match shape {
            "return" => {
                let terminator = ir.terminator(header).unwrap();
                *ir.inst_mut(terminator) = instruction(Opcode::Return, vec![raw]);
                ir.inst_mut(terminator).flags.terminator = true;
            }
            "entry" => {
                let parameter = ir.block(ir.entry()).params[0];
                ir.set_repr(parameter, ValueRepresentation::UnboxedF64);
            }
            "state" => {
                let check = ir.block(header).insts[0];
                let state = ir.inst(check).frame_state.unwrap();
                ir.frame_states.get_mut(state).scopes[0].locals[0] = ValueSource::Value {
                    value: raw,
                    repr: ValueRepresentation::Tagged,
                };
            }
            "managed" => storage = NativeCallStorage::ManagedActivation,
            "no-deopt" => deopt = None,
            "result-type" => ir.refine_type(raw, IRType::of(TypeBits::SINGLE_FLOAT)),
            _ => unreachable!(),
        }
        egcl_compiler::t2::verify::verify(&ir).unwrap();
        assert!(
            emit_framed_native_handlers_with_entries_and_storage(
                &ir,
                1,
                2,
                1,
                1,
                1,
                1,
                1,
                1,
                None,
                deopt,
                &[],
                NativeRootPublication::PublishedHomes,
                storage,
            )
            .is_err(),
            "raw boundary must decline: {shape}"
        );
    }
}
