//! Stable handles to Lisp values across the Python boundary (bliss-lbkd0).
//!
//! No CPython here, deliberately: the handle table is the side of the bridge that
//! belongs entirely to TorCL's collector, and the interesting property — that a
//! handle survives its object being *moved* — is a GC property, testable without
//! an interpreter. (The Python-facing half lives in `python_embedding.rs`, which
//! cannot share a process with anything that also starts CPython.)

#![cfg(feature = "python")]

use std::sync::{Mutex, OnceLock};
use torcl_rt::python::{self, LispHandle};
use torcl_rt::value::{TAG_MASK, TorclVal};
use torcl_rt::{Allocator, Collector, GcConfig, HeapAllocator, HeapCollector, init_heap};

/// `init_heap` is global, so these tests cannot run concurrently.
fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn config() -> GcConfig {
    GcConfig {
        heap_size: 64 * 1024,
        heap_max: 128 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4 * 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    }
}

/// A heap object carrying a recognisable word, so it can be identified after the
/// collector has moved it somewhere else.
fn allocate(allocator: &mut HeapAllocator, marker: u64) -> TorclVal {
    let body = allocator
        .alloc_fast(16)
        .or_else(|| allocator.alloc_slow(16).ok())
        .expect("allocate probe");
    unsafe {
        *(body as *mut u64) = marker;
        TorclVal::from_heap_ptr(body.sub(8))
    }
}

fn marker(value: TorclVal) -> u64 {
    let header = (value.to_raw() & !TAG_MASK) as *const u8;
    unsafe { *(header.add(8) as *const u64) }
}

/// The property the whole design exists for: Python holds a number, the collector
/// moves the object, and the number still names it.
///
/// Handing Python a raw `TorclVal` instead would pass this test's first assertion
/// and fail its second by segfaulting or reading a poisoned word — which is why a
/// pointer can never cross this boundary.
#[test]
fn a_handle_survives_the_object_moving() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    let original = allocate(&mut allocator, 0x9111_0001);
    let handle = python::retain_lisp(original);
    assert_eq!(python::resolve_lisp(handle), Some(original));

    HeapCollector::new().minor_gc().expect("minor_gc");

    let moved = python::resolve_lisp(handle).expect("the handle still names the object");
    assert_ne!(moved, original, "the probe should have been relocated");
    assert_eq!(
        marker(moved),
        0x9111_0001,
        "the handle resolves to the same object at its new address"
    );
    python::release_lisp(handle);
}

/// A retained handle is a root in its own right. Nothing else refers to this
/// object, so if the scanner did not report it the collector would reclaim the
/// space and the marker would come back as something else (or not at all).
#[test]
fn retaining_keeps_an_otherwise_unreachable_object_alive() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    let handle = python::retain_lisp(allocate(&mut allocator, 0x9111_0002));
    // Allocate through the nursery a few times over, so a collector that had lost
    // the object would have handed its space to something else by now.
    for round in 0..200u64 {
        let _churn = allocate(&mut allocator, 0xDEAD_0000 + round);
    }
    HeapCollector::new().minor_gc().expect("minor_gc");

    let value = python::resolve_lisp(handle).expect("still held");
    assert_eq!(marker(value), 0x9111_0002, "the object was not reclaimed");
    python::release_lisp(handle);
}

/// Releasing and re-retaining reuses the slot, and the generation is what keeps
/// that from being a silent aliasing bug: without it the stale handle below would
/// resolve to the *new* object, and Python would be handed a value it never asked
/// for — the hardest possible bug to trace back to here.
#[test]
fn a_released_handle_does_not_resolve_to_its_slots_next_occupant() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    let first = python::retain_lisp(allocate(&mut allocator, 0x9111_0003));
    python::release_lisp(first);
    assert_eq!(
        python::resolve_lisp(first),
        None,
        "a released handle resolves to nothing"
    );

    let second = python::retain_lisp(allocate(&mut allocator, 0x9111_0004));
    assert_eq!(
        python::resolve_lisp(first),
        None,
        "the stale handle must not see the slot's new occupant"
    );
    assert_eq!(
        marker(python::resolve_lisp(second).expect("the new handle")),
        0x9111_0004
    );

    // Double release must not free the new occupant either.
    python::release_lisp(first);
    assert!(
        python::resolve_lisp(second).is_some(),
        "releasing a stale handle must not disturb the live one"
    );
    python::release_lisp(second);
}

/// A handle crosses into Python as one opaque word, so the round trip has to be
/// exact for both halves — a truncating conversion would lose the generation and
/// quietly reintroduce the aliasing bug above.
#[test]
fn a_handle_round_trips_through_its_word_form() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    // Cycle one slot so the generation is non-zero and a dropped high half shows.
    let handle = python::retain_lisp(allocate(&mut allocator, 0x9111_0005));
    python::release_lisp(handle);
    let reused = python::retain_lisp(allocate(&mut allocator, 0x9111_0006));
    assert_ne!(
        reused.to_bits() >> 32,
        0,
        "the reused slot should carry a bumped generation"
    );
    assert_eq!(LispHandle::from_bits(reused.to_bits()), reused);
    assert_eq!(
        python::resolve_lisp(LispHandle::from_bits(reused.to_bits())),
        python::resolve_lisp(reused)
    );
    python::release_lisp(reused);
}

/// Releasing is what balances retaining; the table must not grow without bound as
/// handles come and go.
#[test]
fn slots_are_reused_rather_than_accumulated() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    let before = python::held_lisp_handles();
    for round in 0..1000u64 {
        let handle = python::retain_lisp(allocate(&mut allocator, 0x9112_0000 + round));
        assert_eq!(python::held_lisp_handles(), before + 1);
        python::release_lisp(handle);
    }
    assert_eq!(
        python::held_lisp_handles(),
        before,
        "every handle was released"
    );
}
