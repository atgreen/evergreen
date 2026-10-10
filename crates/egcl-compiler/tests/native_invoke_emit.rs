#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::t2::build::build_from_bytecode_for_transfers;
use egcl_compiler::t2::emit::{emit_framed, emit_framed_transfers};
use egcl_rt::bytecode::{BytecodeFunction, Instr};
use egcl_rt::value::NIL;
use egcl_compiler::t2::x64_calls::{NativeCallSites, NativeCallValues};
use egcl_compiler::t2::x64_value_maps::NativeValueLocation;

fn body() -> BytecodeFunction {
    BytecodeFunction {
        code: vec![
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: 123456,
                nargs: 1,
            },
            Instr::CallNamed {
                sym: 123457,
                nargs: 1,
            },
            Instr::Return,
        ],
        constants: vec![],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        names: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 2,
        max_stack: 2,
        arity: 2,
        name: "native-invoke-emission".into(),
        params_form: NIL,
        min_args: 2,
        max_args: Some(2),
        variadic: false,
    }
}

#[test]
fn rich_emitter_produces_one_exact_recovery_site_per_invoke() {
    let ir = build_from_bytecode_for_transfers(&body()).unwrap();
    assert!(emit_framed(&ir, 0, 0, 0, 0, 0, 0, 0, 0, None).is_err());
    let (emitted, table) = emit_framed_transfers(&ir, 0x12345678, 4).unwrap();
    assert_eq!(table.sites().count(), 2);
    assert!(!emitted.has_deopt);
    assert_eq!(emitted.emitted_safepoints, 2);
    for site in table.sites() {
        let offset = site.return_offset() as usize;
        assert_eq!(&emitted.code[offset - 2..offset], &[0xff, 0xd0]);
        assert!(table.lookup(0x1000, 0x1000 + offset).is_some());
        assert!(!site.map().roots.is_empty());
    }
}

#[test]
fn transfer_entry_refuses_missing_adapter_storage_and_raw_argument_representation() {
    let mut ir = build_from_bytecode_for_transfers(&body()).unwrap();
    assert!(emit_framed_transfers(&ir, 0, 4).is_err());
    assert!(emit_framed_transfers(&ir, 0x12345678, 1).is_err());
    let argument = ir.block(ir.entry()).params[0];
    ir.set_repr(
        argument,
        egcl_compiler::t2::ir::ValueRepresentation::UnboxedFixnum,
    );
    assert!(emit_framed_transfers(&ir, 0x12345678, 4).is_err());
}

use std::cell::Cell;
use egcl_compiler::t2::emit::TransferCallRequest;
use egcl_compiler::t2::native_transfer::{
    SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
};
use egcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable};
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeSegment};
use egcl_rt::value::EgclVal;
use egcl_rt::{Collector, HeapCollector, EgclStack};

struct RunState {
    anchor: *mut NativeSegment,
    table: *const SysvTransferTable,
    value_maps: *const NativeCallSites,
    call_capture: *const [usize; 2],
    entry_capture: *const [usize; 8],
    code_base: usize,
    snapshots: *mut u8,
    snapshot_count: usize,
    activation_len: usize,
    expected: *const EgclVal,
    expected_len: usize,
    first_nargs: usize,
    transfer_bcp: u32,
    calls: usize,
    drops: usize,
    recovered: usize,
    exit: NativeExit,
}
thread_local! { static RUN: Cell<*mut RunState> = const { Cell::new(std::ptr::null_mut()) }; }
struct Finished(*mut usize);
impl Drop for Finished {
    fn drop(&mut self) {
        unsafe {
            *self.0 += 1;
        }
    }
}
unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let state = unsafe { &mut *RUN.with(Cell::get) };
    let request = unsafe { &*request.cast::<TransferCallRequest>() };
    state.calls += 1;
    let _finished = Finished(&mut state.drops);
    assert_eq!(
        request.symbol,
        if state.calls == 1 { 123456 } else { 123457 }
    );
    assert_eq!(
        request.nargs,
        if state.calls == 1 {
            state.first_nargs
        } else {
            1
        }
    );
    state.anchor = native_transfer::current_segment();
    let calls = unsafe { &*state.value_maps };
    let [return_pc, call_rsp] = unsafe { *state.call_capture };
    let offset = return_pc - state.code_base;
    let site = calls.get(offset).expect("actual primary return PC must be indexed");
    assert_eq!(site.stack_adjust, 64);
    let Some(NativeCallValues::Frame(map)) = calls.value_map(offset) else {
        panic!("collecting Invoke must have an authoritative map");
    };
    let entry = unsafe { *state.entry_capture };
    let cursor = egcl_compiler::t2::x64_unwind::NativeFrameCursor {
        pc: return_pc, call_sp: call_rsp, registers: [None; 6],
    };
    let step = calls.unwind(state.code_base, &cursor, call_rsp..entry[1], |address| {
        Some(unsafe { (address as *const usize).read() })
    }).expect("emitted frame must unwind at the actual Invoke PC");
    assert_eq!((step.caller.pc, step.caller.call_sp), (entry[0], entry[1]));
    for (index, location) in step.caller.registers.iter().enumerate() {
        if let Some(location) = location {
            assert_eq!(unsafe { (*location as *const usize).read() }, entry[index + 2]);
        }
    }
    assert_eq!(step.body_sp, Some(call_rsp + site.stack_adjust as usize));
    let body_rsp = (call_rsp + site.stack_adjust as usize) as *const usize;
    let activation = unsafe { body_rsp.add(map.layout().activation_base_slot.unwrap() as usize).read() };
    assert_eq!(activation, request.activation as usize);
    // This compiler fixture bypasses the evaluator's root publication. Consume
    // its exact native maps explicitly, including the newly described aliases.
    struct MappedRoots(Vec<*mut EgclVal>);
    impl egcl_rt::gc::TraceHostRoots for MappedRoots {
        fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
            for &slot in &self.0 { visit(slot); }
        }
    }
    let copies: Vec<Vec<*mut EgclVal>> = map.values().iter()
        .filter(|value| value.may_reference_heap())
        .map(|value| value.locations().iter().map(|location| match *location {
            NativeValueLocation::Activation(slot) => unsafe { request.activation.add(slot as usize) },
            NativeValueLocation::Stack(offset) =>
                (body_rsp as usize).wrapping_add_signed(offset as isize) as *mut EgclVal,
            _ => panic!("this fixture expects stack-homed roots"),
        }).collect()).collect();
    let mut roots = MappedRoots(copies.iter().flatten().copied().collect());
    egcl_rt::rooted_ref!(_roots = &mut roots);
    for slots in &copies {
        assert!(slots.iter().all(|slot| unsafe { **slot == *slots[0] }),
            "native and shadow copies must agree before relocation");
    }
    HeapCollector::new().minor_gc().unwrap();
    for slots in &copies {
        assert!(slots.iter().all(|slot| unsafe { **slot == *slots[0] }),
            "every alias must be updated by GC");
        let expected = unsafe { std::slice::from_raw_parts(state.expected, state.expected_len) };
        assert!(expected.contains(unsafe { &*slots[0] }), "map must describe a relocated live value");
    }
    for index in 0..request.nargs {
        let outgoing_slot = unsafe { request.args.add(index).offset_from(request.activation) } as u16;
        assert!(map.gc_locations().any(|location| *location == NativeValueLocation::Activation(outgoing_slot)),
            "every outgoing argument copy must be rooted");
        assert_eq!(unsafe { request.args.add(index).read() }, unsafe {
            state
                .expected
                .add(if state.calls == 1 { index } else { 1 })
                .read()
        });
    }
    // Return a different value than argument zero. Normal root restoration
    // must not overwrite this result with the old argument's shadow.
    let value = unsafe { state.expected.add(1).read() };
    unsafe {
        out.write(NativeOutcome {
            value,
            exit: if state.calls == 1 {
                NativeExit::Returned
            } else {
                state.exit
            },
        });
    }
}
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let state = unsafe { &mut *RUN.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    let request = unsafe { &*capture.request.cast::<TransferCallRequest>() };
    let table = unsafe { &*state.table };
    let site = table
        .lookup(state.code_base, capture.return_pc as usize)
        .unwrap();
    assert_eq!(
        site.map().origin_bcp,
        state.transfer_bcp,
        "recover second call, never replay first"
    );
    let snapshots = unsafe {
        std::slice::from_raw_parts_mut(
            state.snapshots.cast::<(u32, SysvSiteSnapshot)>(),
            state.snapshot_count,
        )
    };
    let snapshot = &mut snapshots
        .iter_mut()
        .find(|(offset, _)| *offset == site.return_offset())
        .unwrap()
        .1;
    egcl_rt::rooted!(payload = capture.value);
    unsafe {
        snapshot
            .capture_from_activation(
                state.code_base,
                capture,
                std::slice::from_raw_parts(request.activation, state.activation_len),
            )
            .unwrap();
    }
    egcl_rt::rooted_ref!(_snapshot = &mut *snapshot);
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(
        frames = snapshot
            .reconstruct(|_| panic!("no unboxed floats"))
            .unwrap()
    );
    // At the second call all original locals are dead: only the first call's
    // result on the operand stack belongs to this continuation. Keep the live
    // payload/relocation assertions below instead of retaining dead roots.
    assert_eq!(frames[0].locals.len(), state.expected_len);
    assert!(frames[0].locals.iter().all(|value| *value == egcl_rt::value::UNBOUND));
    assert_eq!(frames[0].stack.as_slice(), &[*payload]);
    unsafe {
        snapshot.write_back(state.code_base, capture).unwrap();
    }
    state.recovered += 1;
    capture.value = *payload;
    capture.request = state.anchor.cast();
}
#[unsafe(naked)]
unsafe extern "C" fn dispatch(_anchor: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!("endbr64", "jmp {leave}", leave = sym native_transfer::leave_native_segment);
}
fn double(value: f64) -> EgclVal {
    let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(value);
        EgclVal::from_heap_ptr(body.sub(8))
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn compiler_emitted_calls_return_or_capture_after_collecting_helpers() {
    assert!(native_transfer::is_supported());
    for nargs in [0usize, 1, 4, 8] {
        for exit in [
            NativeExit::Returned,
            NativeExit::Transfer,
            NativeExit::Deopt,
        ] {
            eprintln!("compiler invoke nargs={nargs} exit={exit:?}");
            let mut source = body();
            let locals = nargs.max(2);
            source.n_locals = locals as u16;
            source.arity = locals as u16;
            source.min_args = locals as u16;
            source.max_args = Some(locals as u16);
            source.max_stack = nargs.max(1) as u16;
            source.code = (0..nargs).map(|i| Instr::LoadLocal(i as u16)).collect();
            source.code.extend([
                Instr::CallNamed {
                    sym: 123456,
                    nargs: nargs as u16,
                },
                Instr::CallNamed {
                    sym: 123457,
                    nargs: 1,
                },
                Instr::Return,
            ]);
            let activation_slots = source.n_locals + source.max_stack;
            let ir = build_from_bytecode_for_transfers(&source).unwrap();
            let capture =
                JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8)).unwrap();
            let veneer = JitBuffer::new(&emit_helper_veneer(helper, capture.as_ptr())).unwrap();
            let mut call_capture = Box::new([0usize; 2]);
            // Tail-jump after observing the actual primary CALL coordinates.
            let probe = native_unwind::capture_call(veneer.as_ptr(), call_capture.as_mut_ptr(), false);
            let (emitted, table) =
                emit_framed_transfers(&ir, probe.as_ptr() as u64, activation_slots).unwrap();
            assert!(emitted.native_calls.as_ref().unwrap().unwind_recipe().is_some(), "SysV emitted code needs its exact prologue recipe");
            let code = JitBuffer::new(&emitted.code).unwrap();
            let mut entry_capture = Box::new([0usize; 8]);
            let entry_probe = native_unwind::capture_call(code.as_ptr(), entry_capture.as_mut_ptr(), true);
            let mut snapshots: Vec<_> = table
                .sites()
                .map(|site| (site.return_offset(), site.reserve_snapshot().unwrap()))
                .collect();
            egcl_rt::rooted!(expected = vec![NIL; locals]);
            for i in 1..locals {
                expected[i] = double((i + 1) as f64 * 101.0);
            }
            expected[0] = double(101.0);
            let original = expected[0].to_raw();
            egcl_rt::rooted!(
                activation = vec![
                    NIL;
                    usize::from(activation_slots)
                        + usize::from(emitted.shadow_root_slots)
                ]
            );
            activation[..locals].copy_from_slice(&expected);
            let mut state = RunState {
                anchor: std::ptr::null_mut(),
                table: &table,
                value_maps: emitted.native_calls.as_ref().unwrap(),
                call_capture: &*call_capture,
                entry_capture: &*entry_capture,
                code_base: code.as_ptr() as usize,
                snapshots: snapshots.as_mut_ptr().cast(),
                snapshot_count: snapshots.len(),
                activation_len: activation.len(),
                expected: expected.as_ptr(),
                expected_len: locals,
                first_nargs: nargs,
                transfer_bcp: nargs as u32 + 1,
                calls: 0,
                drops: 0,
                recovered: 0,
                exit,
            };
            RUN.with(|slot| slot.set(&mut state));
            let stack = EgclStack::new(64 * 1024);
            let outcome = unsafe {
                native_transfer::invoke_native_segment(
                    entry_probe.as_ptr(),
                    activation.as_mut_ptr().cast(),
                    &stack,
                )
            }
            .unwrap();
            RUN.with(|slot| slot.set(std::ptr::null_mut()));
            assert_eq!(outcome.exit, exit);
            assert_eq!(outcome.value, expected[1]);
            assert_eq!(outcome.value.as_double_float(), 202.0);
            assert_eq!((state.calls, state.drops), (2, 2));
            assert_eq!(state.recovered, usize::from(exit != NativeExit::Returned));
            assert_ne!(
                original,
                expected[0].to_raw(),
                "helper really relocated the argument"
            );
            assert!(native_transfer::current_segment().is_null());
        }
    }
}

unsafe extern "C" fn complete_leaf_guard(request: *mut u8, out: *mut NativeOutcome) {
    native_unwind::check_deopt_cursor();
    let request = unsafe { &*request.cast::<egcl_compiler::t2::emit::TransferDeoptRequest>() };
    assert_eq!((request.n_scopes, request.n_words), (1, 5));
    assert_eq!(
        unsafe { request.words.add(4).read() },
        EgclVal::from_single_float(1.5).0
    );
    unsafe {
        out.write(NativeOutcome {
            value: EgclVal::from_fixnum(42),
            exit: NativeExit::Returned,
        });
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn guarded_leaf_without_poll_has_a_call_capable_deopt_frame() {
    use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use egcl_compiler::t2::ir::{
        AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
    };
    let mut ir = Function::new("guard-only");
    let entry = ir.entry();
    let input = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
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
            opcode: Opcode::Guard,
            args: vec![input],
            results: vec![],
            aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
            flags: InstFlags {
                effectful: true,
                guard: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: Some(state),
            source_pos: 0,
        },
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    ir.set_terminator(
        entry,
        InstData {
            opcode: Opcode::Return,
            args: vec![checked[0]],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                terminator: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
    );
    let veneer =
        JitBuffer::new(&emit_helper_veneer(complete_leaf_guard, std::ptr::null())).unwrap();
    let mut top = Box::new([0usize; 2]);
    let probe = native_unwind::capture_call(veneer.as_ptr(), top.as_mut_ptr(), false);
    let (emitted, _) = egcl_compiler::t2::emit::emit_framed_native_handlers_with_recursion(
        &ir,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        0,
        None,
        Some(probe.as_ptr() as u64),
    )
    .unwrap();
    assert!(emitted.has_deopt);
    // One scope header (four words), one local, and the native request area:
    // 40 + 32 bytes rounded to 16-byte call alignment = 80 bytes.
    let deopts: Vec<_> = emitted.native_calls.as_ref().unwrap().iter().filter(|site|
        matches!(site.origin, egcl_compiler::t2::x64_calls::NativeCallOrigin::Deopt(_))).collect();
    assert_eq!(deopts.len(), 1);
    assert_eq!(deopts[0].stack_adjust, 80);
    assert!(matches!(emitted.native_calls.as_ref().unwrap().value_map(deopts[0].return_offset),
        Some(NativeCallValues::Deoptimizing { .. })));
    assert_eq!(&emitted.code[deopts[0].return_offset - 2..deopts[0].return_offset], &[0xff, 0xd0]);
    let legacy = egcl_compiler::t2::emit::emit_framed_with_direct_natives(
        &ir, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 0, 1, None, &[], 1, &[], Default::default(),
    ).unwrap();
    let legacy_deopts: Vec<_> = legacy.native_calls.as_ref().unwrap().iter().filter(|site|
        matches!(site.origin, egcl_compiler::t2::x64_calls::NativeCallOrigin::Deopt(_))).collect();
    assert_eq!(legacy_deopts.len(), 1);
    assert_eq!(legacy_deopts[0].stack_adjust, 48, "legacy serialization has no native request");
    let code = JitBuffer::new(&emitted.code).unwrap();
    let mut entry = Box::new([0usize; 8]);
    let entry_probe = native_unwind::capture_call(code.as_ptr(), entry.as_mut_ptr(), true);
    let mut walk = native_unwind::DeoptWalk {
        calls: emitted.native_calls.as_ref().unwrap(),
        code_base: code.as_ptr() as usize,
        top: &*top, entry: &*entry, checked: 0,
    };
    native_unwind::DEOPT_WALK.with(|slot| slot.set(&mut walk));
    let stack = EgclStack::new(64 * 1024);
    for (input, expected) in [
        (EgclVal::from_fixnum(7), EgclVal::from_fixnum(7)),
        (EgclVal::from_single_float(1.5), EgclVal::from_fixnum(42)),
    ] {
        let mut activation = vec![NIL; 1 + usize::from(emitted.shadow_root_slots)];
        activation[0] = input;
        let result = unsafe {
            native_transfer::invoke_native_segment(
                entry_probe.as_ptr(),
                activation.as_mut_ptr().cast(),
                &stack,
            )
        }
        .unwrap();
        assert_eq!(result.exit, NativeExit::Returned);
        assert_eq!(result.value, expected);
    }
    native_unwind::DEOPT_WALK.with(|slot| slot.set(std::ptr::null_mut()));
    assert_eq!(walk.checked, 1);
}

#[path = "support/native_unwind.rs"]
mod native_unwind;
