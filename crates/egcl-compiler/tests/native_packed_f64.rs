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

#[derive(Clone, Copy, Debug, PartialEq)]
enum Input {
    Valid,
    ConstantIndex,
    Large,
    Negative,
    Bounds,
    IndexType,
    DestinationBounds,
    SourceLayout,
    DestinationLayout,
    SourceElementType,
    DestinationElementType,
}

struct Probe {
    calls: Arc<NativeCallSites>,
    base: usize,
    scale: Value,
    source: Value,
    destination: Value,
    guard: *mut u64,
    input: Input,
    polls: usize,
    deopts: usize,
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

fn state(
    f: &mut Function,
    bcp: u32,
    locals: Vec<ValueSource>,
    stack: Vec<ValueSource>,
) -> egcl_compiler::t2::frame_state::FrameStateId {
    f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp,
            locals,
            stack,
        }],
        remat: vec![],
    })
}

fn fixture(guard: *mut u64) -> (Function, Value, Value, Value) {
    let mut f = Function::new("packed-f64-relocation-loop");
    let entry = f.entry();
    let header = f.make_block();
    let source = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let destination = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let index = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let boxed_scale = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let tagged = |value| ValueSource::Value {
        value,
        repr: ValueRepresentation::Tagged,
    };
    let raw = |value| ValueSource::Value {
        value,
        repr: ValueRepresentation::UnboxedF64,
    };
    let before = state(
        &mut f,
        0,
        vec![
            tagged(source),
            tagged(destination),
            tagged(index),
            tagged(boxed_scale),
        ],
        vec![],
    );
    let mut unbox = instruction(Opcode::UnboxFloat, vec![boxed_scale]);
    unbox.flags.guard = true;
    unbox.flags.effectful = true;
    unbox.frame_state = Some(before);
    let (_, scale) = f.push_inst(
        entry,
        unbox,
        &[(
            IRType::of(TypeBits::DOUBLE_FLOAT),
            ValueRepresentation::UnboxedF64,
        )],
    );
    let scale = scale[0];
    let mut jump = instruction(Opcode::Jump, vec![]);
    jump.targets.push(BlockCall {
        block: header,
        args: vec![],
    });
    let locals = vec![
        tagged(source),
        tagged(destination),
        tagged(index),
        raw(scale),
    ];
    // The mapped emitter polls on entry to the loop header. Execute one
    // access in the preheader, then require another access after that poll.
    for block in [entry, header] {
        let before_load = state(&mut f, 1, locals.clone(), vec![]);
        let mut load = instruction(Opcode::PackedF64Ref, vec![source, index]);
        load.flags.guard = true;
        load.flags.effectful = true;
        load.frame_state = Some(before_load);
        let (_, value) = f.push_inst(
            block,
            load,
            &[(
                IRType::of(TypeBits::DOUBLE_FLOAT),
                ValueRepresentation::UnboxedF64,
            )],
        );
        let multiply = instruction(Opcode::FloatMul, vec![value[0], scale]);
        let (_, result) = f.push_inst(
            block,
            multiply,
            &[(
                IRType::of(TypeBits::DOUBLE_FLOAT),
                ValueRepresentation::UnboxedF64,
            )],
        );
        let add = instruction(Opcode::FloatAdd, vec![result[0], value[0]]);
        let (_, result) = f.push_inst(
            block,
            add,
            &[(
                IRType::of(TypeBits::DOUBLE_FLOAT),
                ValueRepresentation::UnboxedF64,
            )],
        );
        let result = result[0];
        let before_store = state(&mut f, 2, locals.clone(), vec![raw(result)]);
        let mut write = instruction(Opcode::PackedF64Set, vec![destination, index, result]);
        write.flags.guard = true;
        write.flags.effectful = true;
        write.frame_state = Some(before_store);
        f.push_inst(block, write, &[]);
        let after = state(&mut f, 3, locals.clone(), vec![raw(result)]);
        let mut check = instruction(Opcode::Guard, vec![]);
        check.aux = AuxData::BuiltinBinding {
            address: guard as usize,
            expected: 0,
        };
        check.flags.guard = true;
        check.flags.effectful = true;
        check.frame_state = Some(after);
        f.push_inst(block, check, &[]);
        f.set_terminator(block, jump.clone());
    }
    egcl_compiler::t2::verify::verify(&f).unwrap();
    (f, scale, source, destination)
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
    assert!(matches!(probe.input, Input::Valid | Input::ConstantIndex));
    assert_eq!(
        probe.polls, 0,
        "guard must stop after one relocating iteration"
    );
    let offset = unsafe { (*image).return_pc as usize } - probe.base;
    let site = probe.calls.get(offset).unwrap();
    let Some(NativeCallValues::Frame(map)) = probe.calls.value_map(offset) else {
        panic!("exact poll map")
    };
    let body = unsafe {
        (*image)
            .caller_sp
            .cast::<u8>()
            .add(site.stack_adjust as usize)
            .cast_mut()
    };
    let scale = map.ssa_value(probe.scale).unwrap();
    assert_eq!(scale.repr, ValueRepresentation::UnboxedF64);
    assert!(!scale.may_reference_heap());
    let scale_home = unsafe { physical(image, body, scale.locations()[0]) }.cast::<u64>();
    let source_home = unsafe {
        physical(
            image,
            body,
            map.ssa_value(probe.source).unwrap().locations()[0],
        )
    };
    let destination_home = unsafe {
        physical(
            image,
            body,
            map.ssa_value(probe.destination).unwrap().locations()[0],
        )
    };
    let (source_before, destination_before) =
        unsafe { (source_home.read(), destination_home.read()) };
    assert_eq!(egcl_rt::packed_f64::get(destination_before, 0), Some(3.75));
    struct Roots(Vec<*mut EgclVal>);
    impl egcl_rt::gc::TraceHostRoots for Roots {
        fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
            for &root in &self.0 {
                visit(root);
            }
        }
    }
    let mut roots = Roots(
        map.gc_locations()
            .map(|&location| unsafe { physical(image, body, location) })
            .collect(),
    );
    roots.0.sort_unstable();
    roots.0.dedup();
    assert!(!roots.0.contains(&scale_home.cast()));
    egcl_rt::rooted_ref!(_roots = &mut roots);
    HeapCollector::new().minor_gc().unwrap();
    let (source_after, destination_after) =
        unsafe { (source_home.read(), destination_home.read()) };
    assert_ne!(source_before, source_after, "source must relocate");
    assert_ne!(
        destination_before, destination_after,
        "destination must relocate"
    );
    assert_eq!(unsafe { scale_home.read() }, 2.0_f64.to_bits());
    // Invalidate the first iteration's answer. The second iteration must both
    // reload the moved source and store into the moved destination.
    assert!(egcl_rt::packed_f64::set(source_after, 0, 3.0));
    assert!(egcl_rt::packed_f64::set(destination_after, 0, -99.0));
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
        panic!("exact deopt map")
    };
    let request = unsafe { &*request.cast::<TransferDeoptRequest>() };
    let words = unsafe {
        std::slice::from_raw_parts_mut(request.words.cast_mut(), request.n_words as usize)
    };
    let expected_bcp = match probe.input {
        Input::Valid | Input::ConstantIndex | Input::Large => 3,
        Input::DestinationLayout | Input::DestinationBounds | Input::DestinationElementType => 2,
        _ => 1,
    };
    assert_eq!(words[1], expected_bcp);
    assert_eq!(
        f64_slots.as_ref(),
        if expected_bcp == 1 {
            &[3][..]
        } else {
            &[3, 4][..]
        }
    );
    unsafe {
        rebox_f64_payload(&mut words[4..], f64_slots, |value| {
            HeapCollector::new().minor_gc().unwrap();
            egcl_rt::gc::alloc_double_float(value)
        })
    }
    .unwrap();
    let destination = EgclVal(words[5]);
    if probe.input == Input::DestinationLayout {
        assert_eq!(destination.as_double_float(), 41.0);
    } else if probe.input == Input::DestinationElementType {
        assert_eq!(egcl_rt::types::bit_vector_ref(destination, 0), Some(1));
    } else if probe.input == Input::Large {
        assert_eq!(egcl_rt::packed_f64::len(destination), Some(65536));
        assert_eq!(egcl_rt::packed_f64::get(destination, 0), Some(0.0));
        assert_eq!(egcl_rt::packed_f64::get(destination, 65535), Some(3.75));
    } else if probe.input == Input::DestinationBounds {
        assert_eq!(egcl_rt::packed_f64::len(destination), Some(0));
    } else {
        assert_eq!(
            egcl_rt::packed_f64::get(destination, 0),
            Some(
                if matches!(probe.input, Input::Valid | Input::ConstantIndex) {
                    9.0
                } else {
                    0.0
                }
            )
        );
    }
    assert_eq!(EgclVal(words[7]).as_double_float(), 2.0);
    probe.deopts += 1;
    unsafe {
        out.write(NativeOutcome {
            value: destination,
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
fn packed_f64_loop_recomputes_data_addresses_after_collection() {
    exercise(Input::Valid);
    exercise(Input::ConstantIndex);
}

#[test]
fn packed_f64_access_honors_the_extended_object_header() {
    exercise(Input::Large);
}

#[test]
fn packed_f64_type_and_bounds_guards_deopt_before_memory_effects() {
    for input in [
        Input::Negative,
        Input::Bounds,
        Input::IndexType,
        Input::DestinationBounds,
        Input::SourceLayout,
        Input::DestinationLayout,
        Input::SourceElementType,
        Input::DestinationElementType,
    ] {
        exercise(input);
    }
}

#[test]
fn packed_f64_ir_requires_exact_operand_and_recovery_contracts() {
    for malformed in 0..4 {
        let mut guard = 0;
        let (mut ir, _, _, _) = fixture(&mut guard);
        let opcode = if malformed == 3 {
            Opcode::PackedF64Set
        } else {
            Opcode::PackedF64Ref
        };
        let inst = ir
            .block_order()
            .iter()
            .flat_map(|&block| ir.block(block).insts.iter().copied())
            .find(|&inst| ir.inst(inst).opcode == opcode)
            .unwrap();
        match malformed {
            0 => ir.inst_mut(inst).flags.guard = false,
            1 => {
                ir.inst_mut(inst).args.pop();
            }
            2 => {
                let result = ir.inst(inst).results[0];
                ir.set_repr(result, ValueRepresentation::Tagged);
            }
            3 => {
                let value = ir.inst(inst).args[2];
                ir.refine_type(value, IRType::of(TypeBits::SINGLE_FLOAT));
            }
            _ => unreachable!(),
        }
        let errors = egcl_compiler::t2::verify::verify(&ir)
            .expect_err("malformed packed access must be rejected before lowering");
        assert!(
            errors
                .iter()
                .any(|error| error.check == "V8 packed-f64-contract")
        );
    }
}

fn exercise(input: Input) {
    let mut guard = Box::new(u64::from(input == Input::Large));
    let (mut ir, scale, source, destination) = fixture(&mut *guard);
    if input == Input::ConstantIndex {
        let entry = ir.entry();
        let mut constant = instruction(Opcode::ConstFixnum, vec![]);
        constant.aux = AuxData::FixnumImm(0);
        let (inst, values) = ir.push_inst(
            entry,
            constant,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        assert_eq!(ir.block_mut(entry).insts.pop(), Some(inst));
        ir.block_mut(entry).insts.insert(0, inst);
        for block in ir.block_order().to_vec() {
            for inst in ir.block(block).insts.clone() {
                if matches!(
                    ir.inst(inst).opcode,
                    Opcode::PackedF64Ref | Opcode::PackedF64Set
                ) {
                    ir.inst_mut(inst).args[1] = values[0];
                }
            }
        }
        egcl_compiler::t2::verify::verify(&ir).unwrap();
    }
    let poll = JitBuffer::new(&emit_published_helper_veneer(poll, std::ptr::null())).unwrap();
    let deopt = JitBuffer::new(&emit_published_helper_veneer(deopt, std::ptr::null())).unwrap();
    let (emitted, _) = emit_framed_native_handlers_with_entries_and_storage(
        &ir,
        1,
        5,
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
    .expect("native packed double operations");
    let code = JitBuffer::new(&emitted.code).unwrap();
    let stack = EgclStack::new(64 * 1024);
    let mut activation = {
        egcl_rt::rooted!(
            source_value = if input == Input::SourceLayout {
                EgclVal::from_fixnum(17)
            } else if input == Input::SourceElementType {
                egcl_rt::types::make_bit_vector(&[1])
            } else {
                egcl_rt::packed_f64::make(if input == Input::Large { 65536 } else { 1 }, 1.25)
                    .unwrap()
            }
        );
        egcl_rt::rooted!(
            destination_value = if input == Input::DestinationLayout {
                egcl_rt::gc::alloc_double_float(41.0)
            } else if input == Input::DestinationElementType {
                egcl_rt::types::make_bit_vector(&[1])
            } else {
                egcl_rt::packed_f64::make(
                    if input == Input::Large {
                        65536
                    } else {
                        usize::from(input != Input::DestinationBounds)
                    },
                    0.0,
                )
                .unwrap()
            }
        );
        if input == Input::Large {
            for array in [*source_value, *destination_value] {
                let header = unsafe {
                    array
                        .as_ptr()
                        .cast::<egcl_rt::object::ObjectHeader>()
                        .read()
                };
                assert!(header.is_large_object(), "exercise the 16-byte header");
            }
        }
        let boxed_scale = egcl_rt::gc::alloc_double_float(2.0);
        let mut activation = vec![NIL; 5 + usize::from(emitted.shadow_root_slots)];
        activation[..4].copy_from_slice(&[
            *source_value,
            *destination_value,
            match input {
                Input::Negative => EgclVal::from_fixnum(-1),
                Input::Bounds => EgclVal::from_fixnum(1),
                Input::Large => EgclVal::from_fixnum(65535),
                Input::IndexType => EgclVal::from_single_float(0.0),
                _ => EgclVal::from_fixnum(0),
            },
            boxed_scale,
        ]);
        activation
    };
    let mut probe = Probe {
        calls: Arc::new(emitted.native_calls.clone().unwrap()),
        base: code.as_ptr() as usize,
        scale,
        source,
        destination,
        guard: &mut *guard,
        input,
        polls: 0,
        deopts: 0,
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
    assert_eq!(
        (probe.polls, probe.deopts),
        (
            usize::from(matches!(input, Input::Valid | Input::ConstantIndex)),
            1
        )
    );
    if matches!(input, Input::Valid | Input::ConstantIndex) {
        assert_eq!(egcl_rt::packed_f64::get(outcome.value, 0), Some(9.0));
    }
}
