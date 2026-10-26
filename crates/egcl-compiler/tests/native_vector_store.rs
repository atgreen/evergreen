// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{emit::emit_framed, frame_state::*, ir::*};
use egcl_rt::{EgclVal, gc, jit::JitBuffer, object::type_id};
use gc::{Collector, GcConfig, HeapCollector};
use std::cell::Cell;

thread_local! { static DEOPTED: Cell<bool> = const { Cell::new(false) }; }
extern "C" fn deopt(_: u64, _: u64, _: *const u64) -> u64 {
    DEOPTED.with(|flag| flag.set(true));
    egcl_rt::value::NIL.0
}
extern "C" fn clear(_: u64, _: *mut EgclVal, _: u64) {}

fn store_code(constant: bool) -> JitBuffer {
    let mut f = Function::new("vector-store-barrier");
    let b = f.entry();
    let args: Vec<_> = (0..3)
        .map(|_| f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    let sources = args
        .iter()
        .map(|&value| ValueSource::Value {
            value,
            repr: ValueRepresentation::Tagged,
        })
        .collect::<Vec<_>>();
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp: 0,
            locals: sources.clone(),
            stack: sources,
        }],
        remat: vec![],
    });
    let value = if constant {
        f.push_inst(
            b,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(42),
                flags: Default::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0]
    } else {
        args[2]
    };
    let (_, result) = f.push_inst(
        b,
        InstData {
            opcode: Opcode::VecSet,
            args: vec![args[0], args[1], value],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: Some(state),
            source_pos: 0,
        },
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    let post = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp: 1,
            locals: args
                .iter()
                .map(|&value| ValueSource::Value {
                    value,
                    repr: ValueRepresentation::Tagged,
                })
                .collect(),
            stack: vec![ValueSource::Value {
                value: result[0],
                repr: ValueRepresentation::Tagged,
            }],
        }],
        remat: vec![],
    });
    // Match array_access specialization: its post-store reset also establishes
    // the precise, call-capable frame required by native array guards.
    f.push_inst(
        b,
        InstData {
            opcode: Opcode::ClearMv,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                call: true,
                safepoint: true,
                effectful: true,
                ..Default::default()
            },
            targets: vec![],
            frame_state: Some(post),
            source_pos: 1,
        },
        &[],
    );
    f.set_terminator(
        b,
        InstData {
            opcode: Opcode::Return,
            args: result,
            results: vec![],
            aux: AuxData::None,
            flags: Default::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 1,
        },
    );
    egcl_compiler::t2::verify::verify(&f).unwrap();
    let code = emit_framed(
        &f,
        deopt as *const () as u64,
        deopt as *const () as u64,
        0,
        0,
        0,
        0,
        0,
        clear as *const () as u64,
        None,
    )
    .unwrap();
    JitBuffer::new(&code.code).unwrap()
}

fn vector(length: usize) -> EgclVal {
    let body_size = 8 + length * 8;
    let body = gc::alloc_typed(body_size, type_id::SIMPLE_VECTOR).unwrap();
    // No allocating operation occurs between allocation and initialization.
    unsafe {
        body.cast::<u64>().write(length as u64);
        for i in 0..length {
            body.add(8 + i * 8)
                .cast::<EgclVal>()
                .write(EgclVal::from_fixnum(0));
        }
        EgclVal::from_heap_ptr(body.sub(gc::body_header_offset(body_size)))
    }
}

fn slot(vector: EgclVal) -> *mut EgclVal {
    // These small vectors have the ordinary eight-byte header.
    unsafe { vector.as_ptr().add(16).cast() }
}

fn run(code: &JitBuffer, vector: EgclVal, index: i64, value: EgclVal) -> EgclVal {
    let entry: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
    let mut frame = [egcl_rt::value::NIL.0; 128];
    frame[..3].copy_from_slice(&[vector.0, EgclVal::from_fixnum(index).0, value.0]);
    DEOPTED.with(|flag| flag.set(false));
    EgclVal(entry(frame.as_mut_ptr()))
}

#[test]
fn native_immediate_stores_bypass_helper_but_preserve_satb_and_young_references() {
    gc::init_heap(&GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 16 * 1024 * 1024,
        nursery_size: 1024 * 1024,
        region_size: 65536,
        tlab_size: 4096,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.9,
    })
    .unwrap();
    let dynamic = store_code(false);
    let constant = store_code(true);
    egcl_rt::rooted!(v = vector(2));
    egcl_rt::rooted!(old = vector(1));
    let before = *v;
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(
        *v, before,
        "the fixture must actually relocate, not merely enable stress"
    );
    struct Marking;
    impl Drop for Marking {
        fn drop(&mut self) {
            gc::set_gc_marking_in_progress(false);
        }
    }
    let _reset_marking = Marking;
    for code in [&dynamic, &constant] {
        unsafe {
            gc::store_ref(slot(*v), EgclVal::from_fixnum(0));
        }
        let _ = gc::drain_satb_log();
        gc::set_gc_marking_in_progress(true);
        assert_eq!(run(code, *v, 0, EgclVal::from_fixnum(42)).as_fixnum(), 42);
        assert_eq!(unsafe { *slot(*v) }.as_fixnum(), 42);
        assert!(
            gc::drain_satb_log().is_empty(),
            "the old helper logs even an immediate; empty proves direct native store"
        );
        gc::set_gc_marking_in_progress(false);
        unsafe {
            gc::store_ref(slot(*v), *old);
        }
        let _ = gc::drain_satb_log();
        gc::set_gc_marking_in_progress(true);
        assert_eq!(run(code, *v, 0, EgclVal::from_fixnum(42)).as_fixnum(), 42);
        assert_eq!(
            gc::drain_satb_log(),
            vec![*old],
            "overwritten heap reference must still use SATB helper"
        );
        gc::set_gc_marking_in_progress(false);
        let original = unsafe { *slot(*v) };
        let _ = run(code, *v, -1, EgclVal::from_fixnum(9));
        assert!(DEOPTED.with(Cell::get));
        assert_eq!(
            unsafe { *slot(*v) },
            original,
            "bounds failure must not write"
        );
    }
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(young = vector(1));
    assert_eq!(run(&dynamic, *v, 0, *young), *young);
    assert!(!DEOPTED.with(Cell::get));
    assert_eq!(
        gc::remembered_set_len(),
        1,
        "new heap reference must retain remembered-set barrier"
    );
    let young_before = *young;
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(*young, young_before, "young referent must actually move");
    assert_eq!(
        unsafe { *slot(*v) },
        *young,
        "remembered slot must follow relocation"
    );
}
