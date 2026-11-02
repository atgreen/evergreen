// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Exercise full mapped entry initialization and GC maps, using one emitted
//! artifact with synthetic execution contexts, not shared TransferCode caches.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use egcl_compiler::t2::emit::{
    BaselineHeat, BaselineHeatCounter, NativePollControl, NativePollFastPath,
    NativePollInitialization, NativeRootPublication,
    emit_framed_native_handlers_with_entries_and_poll_fast_path,
};
use egcl_rt::bytecode::{BytecodeFunction, Instr};
use egcl_rt::exec_context::{ExecutionKind, LispExecutionContext};
use egcl_rt::execution_heat::HeatId;
use egcl_rt::value::{EgclVal, NIL};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
thread_local! {
    static CONTROL: Cell<*mut NativePollControl> = const { Cell::new(std::ptr::null_mut()) };
    static CREDITS: Cell<usize> = const { Cell::new(0) };
    static COLLECT: Cell<bool> = const { Cell::new(false) };
    static HEAT_ID: Cell<Option<HeatId>> = const { Cell::new(None) };
}
#[path = "native_execution_heat/saved_caller.rs"]
mod saved_caller;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
extern "C" fn initialize() -> *mut NativePollControl {
    CONTROL.with(Cell::get)
}
extern "C" fn initialize_with_heat(ordinal: usize) -> NativePollInitialization {
    let control = initialize();
    let heat = HEAT_ID
        .with(Cell::get)
        .and_then(|id| {
            if id.ordinal() != ordinal {
                return None;
            }
            let control = unsafe { control.as_ref() }?;
            let execution = unsafe { control.execution.as_ref() }?;
            execution.heat().counter(id).map(std::ptr::from_ref)
        })
        .unwrap_or(std::ptr::null());
    NativePollInitialization { control, heat }
}
extern "C" fn unrelated_helper(_: *mut u64) -> u64 {
    NIL.0
}
extern "C" fn poll(request: *mut u64) -> u64 {
    use egcl_rt::{Collector, HeapCollector};
    let bits = request as usize;
    if bits & 1 != 0 {
        CREDITS.with(|credits| credits.set(credits.get() + if bits & 4 != 0 { 4 } else { 1 }));
    }
    if COLLECT.with(Cell::get) {
        HeapCollector::new().minor_gc().unwrap();
    }
    NIL.0
}
#[test]
fn mapped_execution_heat_accumulates_across_activations_and_preserves_moving_roots() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    run_case(false);
}
#[test]
fn initialized_execution_heat_preserves_exact_bindings_fallback_and_moving_roots() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    run_case(true);
}
fn run_case(initialized: bool) {
    let a = LispExecutionContext::new(ExecutionKind::Fiber, 1, 1);
    let b = LispExecutionContext::new(ExecutionKind::Fiber, 2, 1);
    let missing = LispExecutionContext::new(ExecutionKind::Fiber, 3, 1);
    for execution in [&a, &b, &missing] {
        assert!(execution.begin_poll());
    }
    let id = HeatId::new().unwrap();
    HEAT_ID.with(|slot| slot.set(Some(id)));
    a.heat().install(id, 4);
    b.heat().install(id, 4);
    let mut control = NativePollControl {
        execution: &a,
        seen_carrier: 1,
        seen_signal_epoch: 0,
        deadline_armed: 0,
        reserved: [0; 7],
        test_observer: std::ptr::null(),
        test_skips: std::ptr::null_mut(),
    };
    CONTROL.with(|slot| slot.set(&mut control));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CONTROL.with(|slot| slot.set(std::ptr::null_mut()));
            COLLECT.with(|slot| slot.set(false));
            HEAT_ID.with(|slot| slot.set(None));
        }
    }
    let _reset = Reset;
    let gc = AtomicBool::new(false);
    let sigusr1 = AtomicBool::new(false);
    let posted = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);
    let epoch = AtomicU64::new(0);
    let fast = NativePollFastPath {
        initialize: if initialized {
            initialize_with_heat as *const () as u64
        } else {
            initialize as *const () as u64
        },
        initialize_with_execution_heat: initialized,
        gc_requested: &gc as *const _ as u64,
        sigusr1_pending: &sigusr1 as *const _ as u64,
        posted_signal: &posted as *const _ as u64,
        native_errors: &errors as *const _ as u64,
        signal_epoch: &epoch as *const _ as u64,
        observe_forces_cold: false,
        instrument_skips: false,
        baseline_heat: Some(BaselineHeat {
            counter: BaselineHeatCounter::ExecutionOwned(id),
            batch: 4,
        }),
    };
    let source = BytecodeFunction {
        self_call_sites: Vec::new(),
        notinline_sites: Vec::new(),
        code: vec![
            Instr::Br(1),
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Br(1),
            Instr::LoadLocal(0),
            Instr::Return,
        ],
        constants: vec![NIL],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 1,
        max_stack: 1,
        arity: 1,
        name: "GENERATED-POLL".into(),
        params_form: NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
        binder_cache: Default::default(),
        closure_template: Default::default(),
    };
    let mut function =
        egcl_compiler::t2::build::build_from_bytecode_for_transfers(&source).unwrap();
    for block in function.block_order().to_vec() {
        let instructions = function
            .block(block)
            .insts
            .iter()
            .copied()
            .filter(|&inst| function.inst(inst).opcode != egcl_compiler::t2::ir::Opcode::ClearMv)
            .collect();
        function.block_mut(block).insts = instructions;
    }
    let helper = unrelated_helper as *const () as u64;
    let poll_veneer = saved_caller::poll_veneer();
    let emit = |fast| {
        let (framed, _) = emit_framed_native_handlers_with_entries_and_poll_fast_path(
            &function,
            helper,
            source.num_slots(),
            helper,
            helper,
            helper,
            helper,
            helper,
            poll_veneer.as_ptr() as u64,
            None,
            None,
            &[],
            NativeRootPublication::ActivationShadows,
            Some(fast),
        )
        .unwrap();
        framed
    };
    let framed = emit(fast);
    assert!(
        matches!(saved_caller::selected_register(&framed), Some(12 | 14 | 3)),
        "this integration must execute the preserved-register arm"
    );
    let legacy_counter = std::sync::atomic::AtomicU32::new(4);
    let legacy_heat = BaselineHeat {
        counter: BaselineHeatCounter::CodeOwned(legacy_counter.as_ptr() as u64),
        batch: 4,
    };
    let legacy = emit(NativePollFastPath {
        baseline_heat: Some(legacy_heat),
        ..fast
    });
    assert_eq!(
        framed.native_spill_slots,
        legacy.native_spill_slots + 2,
        "cache pointer and fallback occupy exactly two raw frame words"
    );
    assert_eq!(
        framed.shadow_root_slots, legacy.shadow_root_slots,
        "raw cache words must not add tagged shadows"
    );
    for site in framed.native_calls.as_ref().unwrap().iter() {
        if let Some(egcl_compiler::t2::x64_calls::NativeCallValues::Frame(map)) = framed
            .native_calls
            .as_ref()
            .unwrap()
            .value_map(site.return_offset)
        {
            assert_eq!(
                map.layout().native_slots,
                framed.native_spill_slots,
                "precise map bounds include both raw words"
            );
        }
    }
    // Ordinary stack admission must include the same extra 16 bytes. The
    // actual mapped entry below also proves relocation with the new layout.
    let mut ordinary_function = function.clone();
    ordinary_function.osr_entries.clear();
    let emit_ordinary = |heat| {
        use egcl_compiler::t2::emit::{
            OrdinaryCallFlags, OrdinaryGuardOptions, OrdinaryLispEntryOptions,
            OrdinarySelfCallFacts, emit_framed_ordinary_function_with_self_call_facts,
        };
        emit_framed_ordinary_function_with_self_call_facts(
            &ordinary_function,
            OrdinaryLispEntryOptions {
                require_callable_home: false,
                arity: 1,
                arity_error_veneer: helper,
                binding: None,
            },
            &[],
            OrdinaryCallFlags {
                gc_requested: fast.gc_requested,
                sigusr1_pending: fast.sigusr1_pending,
                posted_signal: fast.posted_signal,
                native_errors: fast.native_errors,
                signal_epoch: fast.signal_epoch,
            },
            helper,
            OrdinaryGuardOptions {
                veneer: helper,
                multiple_values: helper,
            },
            poll as *const () as u64,
            &[],
            OrdinarySelfCallFacts {
                trusted_control: false,
                top_tier: false,
                baseline_heat: Some(heat),
            },
        )
        .unwrap()
    };
    let ordinary = emit_ordinary(fast.baseline_heat.unwrap());
    let ordinary_legacy = emit_ordinary(legacy_heat);
    assert_eq!(
        ordinary.native_spill_slots,
        ordinary_legacy.native_spill_slots + 2
    );
    assert!(saved_caller::selected_register(&ordinary).is_some());
    assert_eq!(saved_caller::selected_register(&ordinary_legacy), None);
    // Decode the actual push/pad/reservation stream independently of the
    // emitter's metadata. The extra preserved register can consume an old
    // padding word or require a new one; the two raw homes always add16bytes.
    fn physical_frame(code: &egcl_compiler::t2::emit::FramedCode) -> (usize, usize) {
        use iced_x86::{Decoder, DecoderOptions, Mnemonic, Register};
        let start = code.ordinary_entry.unwrap();
        let mut started = false;
        let mut bytes = 0;
        let mut saved = Vec::new();
        for instruction in Decoder::new(64, &code.code[start..], DecoderOptions::NONE) {
            if !started {
                if instruction.mnemonic() != Mnemonic::Push {
                    continue;
                }
                assert_eq!(instruction.op0_register(), Register::RBP);
                started = true;
            }
            match instruction.mnemonic() {
                Mnemonic::Push => {
                    saved.push(instruction.op0_register());
                    bytes += 8;
                }
                Mnemonic::Mov
                    if instruction.op0_register() == Register::RBP
                        && instruction.op1_register() == Register::RSP => {}
                Mnemonic::Sub if instruction.op0_register() == Register::RSP => {
                    bytes += instruction.immediate(1) as usize;
                }
                Mnemonic::Test if instruction.memory_base() == Register::RSP => {}
                _ => break,
            }
        }
        assert!(started);
        let recipe = code.native_calls.as_ref().unwrap().unwind_recipe().unwrap();
        assert_eq!(
            bytes,
            recipe.frame_bytes() as usize,
            "physical prologue and exact unwind geometry agree"
        );
        for (index, register) in saved.iter().enumerate() {
            let encoding = match register {
                Register::RBX => 3,
                Register::RBP => 5,
                Register::R12 => 12,
                Register::R13 => 13,
                Register::R14 => 14,
                Register::R15 => 15,
                _ => panic!("unexpected saved register"),
            };
            assert_eq!(
                recipe.saved_offset(encoding),
                Some((bytes - (index + 1) * 8) as u32)
            );
        }
        (bytes, saved.len())
    }
    let (new_bytes, new_saves) = physical_frame(&ordinary);
    let (old_bytes, old_saves) = physical_frame(&ordinary_legacy);
    assert_eq!(
        new_saves,
        old_saves + 1,
        "the raw register saves the caller's prior value"
    );
    let spill_bytes = |slots: u32| (slots as usize * 8 + 15) & !15;
    assert_eq!(
        spill_bytes(ordinary.native_spill_slots),
        spill_bytes(ordinary_legacy.native_spill_slots) + 16
    );
    let new_pad = new_bytes - spill_bytes(ordinary.native_spill_slots) - new_saves * 8;
    let old_pad = old_bytes - spill_bytes(ordinary_legacy.native_spill_slots) - old_saves * 8;
    assert!(matches!((old_pad, new_pad), (0, 8) | (8, 0)));
    assert_eq!(
        ordinary.ordinary_stack_required.unwrap()
            - ordinary_legacy.ordinary_stack_required.unwrap(),
        new_bytes - old_bytes,
        "stack admission includes the decoded raw homes, register save and padding delta"
    );
    // Locate the actual fallback address selected by the emitted ordinary
    // prologue, then ensure neither raw frame word appears in precise roots.
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, Register};
    let fallback_offsets: std::collections::HashSet<_> =
        Decoder::new(64, &ordinary.code, DecoderOptions::NONE)
            .into_iter()
            .filter(|insn| {
                insn.mnemonic() == Mnemonic::Lea
                    && insn.op0_register() == Register::RAX
                    && insn.memory_base() == Register::RSP
            })
            .map(|insn| insn.memory_displacement64() as i32)
            .collect();
    assert!(
        !fallback_offsets.is_empty(),
        "entry must initialize the stack fallback"
    );
    let mut checked_maps = 0;
    let calls = ordinary.native_calls.as_ref().unwrap();
    for site in calls.iter() {
        if let Some(egcl_compiler::t2::x64_calls::NativeCallValues::Frame(map)) =
            calls.value_map(site.return_offset)
        {
            checked_maps += 1;
            for &fallback in &fallback_offsets {
                assert!(fallback >= 8 && fallback < (ordinary.native_spill_slots * 8) as i32);
                for location in map.gc_locations() {
                    if let egcl_compiler::t2::x64_value_maps::NativeValueLocation::Register(
                        register,
                    ) = location
                    {
                        assert_ne!(
                            Some(*register),
                            saved_caller::selected_register(&ordinary),
                            "current raw heat register is not a tagged root"
                        );
                    }
                    if let egcl_compiler::t2::x64_value_maps::NativeValueLocation::Stack(offset) =
                        location
                    {
                        assert!(
                            *offset != fallback && *offset != fallback - 8,
                            "fallback countdown and cached pointer are never tagged roots"
                        );
                    }
                }
            }
        }
    }
    assert!(checked_maps > 0, "check actual ordinary poll maps");
    for execution in [&a, &b] {
        let address = execution.heat().counter(id).unwrap() as *const _ as usize;
        assert!(
            !framed
                .code
                .windows(8)
                .any(|bytes| bytes == address.to_le_bytes()),
            "no mutable countdown immediate"
        );
    }
    assert!(
        framed
            .native_calls
            .as_ref()
            .unwrap()
            .iter()
            .any(|site| matches!(
                site.origin,
                egcl_compiler::t2::x64_calls::NativeCallOrigin::LoopPoll(_)
            )),
        "must execute a mapped native loop poll"
    );
    let buffer = egcl_rt::jit::JitBuffer::new(&framed.code).unwrap();
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
    egcl_rt::rooted!(
        activation = vec![NIL; usize::from(source.num_slots() + framed.shadow_root_slots)]
    );
    activation[0] = EgclVal::from_fixnum(37);
    for _ in 0..3 {
        assert_eq!(run(activation.as_mut_ptr().cast()), activation[0].0);
    }
    assert_eq!(CREDITS.with(Cell::get), 0);
    assert_eq!(a.heat().counter(id).unwrap().load(Ordering::Relaxed), 1);
    control.execution = &b;
    assert_eq!(run(activation.as_mut_ptr().cast()), activation[0].0);
    assert_eq!(b.heat().counter(id).unwrap().load(Ordering::Relaxed), 3);
    control.execution = &a;
    assert_eq!(run(activation.as_mut_ptr().cast()), activation[0].0);
    assert_eq!(
        CREDITS.with(Cell::get),
        4,
        "four edges sampled exactly once"
    );
    a.set_carrier_id(9); // selector identity follows execution, not carrier
    a.begin_poll();
    assert_eq!(run(activation.as_mut_ptr().cast()), activation[0].0);
    assert_eq!(a.heat().counter(id).unwrap().load(Ordering::Relaxed), 3);
    control.execution = &missing;
    CREDITS.with(|c| c.set(0));
    for _ in 0..3 {
        assert_eq!(run(activation.as_mut_ptr().cast()), activation[0].0);
    }
    assert_eq!(
        CREDITS.with(Cell::get),
        3,
        "missing cell counts actual edges"
    );
    CONTROL.with(|slot| slot.set(std::ptr::null_mut()));
    assert_eq!(run(activation.as_mut_ptr().cast()), activation[0].0);
    assert_eq!(
        CREDITS.with(Cell::get),
        4,
        "null poll control counts one edge"
    );
    control.execution = &a;
    CONTROL.with(|slot| slot.set(&mut control));

    saved_caller::check(&framed, &buffer, &mut activation, &a);

    // Prove both raw cache words did not contaminate native GC maps.
    let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(123.5);
    }
    egcl_rt::rooted!(expected = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
    activation[0] = *expected;
    let old = expected.0;
    a.request_poll();
    COLLECT.with(|slot| slot.set(true));
    let result = run(activation.as_mut_ptr().cast());
    COLLECT.with(|slot| slot.set(false));
    assert_ne!(expected.0, old, "probe must relocate the live Lisp value");
    assert_eq!(result, expected.0, "return reloads the updated native home");
}
