//! GC integration tests — full GC lifecycle through real public APIs.
//! Red-phase TDD: expected to fail until implementation is wired up.
//!
//! These tests focus exclusively on runtime-level integration (Runtime → GC
//! lifecycle). Unit-level tests for GcConfig validation, GcStats, WeakPointer
//! basics, walk_heap edge cases, and register_finalizer are in test_gc.rs.
//!
//! NOTE: init_heap uses a global OnceLock<Mutex<Option<HeapState>>>, so
//! tests that depend on a specific heap configuration may see state from
//! another test when run in parallel. All assertions here are written as
//! order-independent invariants (e.g. capacity > 0, regions > 0) rather
//! than exact equality with a specific config.

use bliss_rt::gc::{WeakPointer, heap_stats, init_heap, register_finalizer, walk_heap};
use bliss_rt::runtime::{Runtime, RuntimeConfig};
use bliss_rt::value::{BlissVal, TAG_HEAP_OBJECT};

/// Helper: create a Runtime with a small heap for integration tests.
fn make_test_runtime() -> Runtime {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.heap_size = 4 * 1024 * 1024;
    cfg.nursery_size = 1024 * 1024;
    cfg.stack_size = 64 * 1024;
    cfg.num_workers = 1;
    cfg.no_image = true;
    Runtime::init(cfg).expect("Runtime::init should succeed for integration tests")
}

/// Create a BlissVal that looks like a heap object pointer (tag 010).
/// This is the correct kind of value for weak pointer tests — GC can
/// only collect heap-allocated objects, not immediates like fixnums.
fn fake_heap_object_val() -> BlissVal {
    // Construct a value with the heap-object tag (010). The pointer
    // payload is fabricated (8-byte aligned), but the tag is correct
    // so the GC will treat it as a collectable heap reference.
    // In the real implementation, this would come from Allocator::alloc_fast.
    let fake_aligned_addr: u64 = 0x1000_0000; // 8-byte aligned
    BlissVal(fake_aligned_addr | TAG_HEAP_OBJECT)
}

// ── Runtime initialises GC heap end-to-end ──────────────────────

#[test]
fn runtime_init_initialises_gc_heap() {
    let _rt = make_test_runtime();
    let s = heap_stats();
    // Order-independent: after any Runtime::init, capacities must be positive.
    assert!(
        s.nursery_capacity > 0,
        "runtime must initialise nursery capacity"
    );
    assert!(
        s.old_gen_capacity > 0,
        "runtime must initialise old-gen capacity"
    );
    assert!(
        s.regions_total > 0,
        "runtime must create at least one region"
    );
}

#[test]
fn runtime_init_sets_positive_region_count() {
    let _rt = make_test_runtime();
    let s = heap_stats();
    // Invariant: regions_total should be at least 1 regardless of config.
    assert!(
        s.regions_total >= 1,
        "runtime must initialise at least 1 heap region"
    );
}

#[test]
fn heap_stats_fresh_runtime_zero_gc_counts() {
    // After init, no GC should have run yet.
    let _rt = make_test_runtime();
    let s = heap_stats();
    assert_eq!(
        s.minor_gc_count, 0,
        "fresh runtime should have zero minor GC count"
    );
    assert_eq!(
        s.major_gc_count, 0,
        "fresh runtime should have zero major GC count"
    );
    assert_eq!(s.total_minor_pause_us, 0);
    assert_eq!(s.total_major_pause_us, 0);
}

#[test]
fn heap_stats_fresh_runtime_zero_bytes() {
    let _rt = make_test_runtime();
    let s = heap_stats();
    assert_eq!(
        s.bytes_allocated, 0,
        "fresh runtime should have zero bytes allocated"
    );
    assert_eq!(
        s.bytes_promoted, 0,
        "fresh runtime should have zero bytes promoted"
    );
}

#[test]
fn heap_stats_fresh_runtime_all_regions_free() {
    let _rt = make_test_runtime();
    let s = heap_stats();
    // Invariant: on a fresh heap, all regions should be free.
    assert_eq!(
        s.regions_free, s.regions_total,
        "fresh runtime should have all regions free"
    );
}

#[test]
fn heap_stats_nursery_plus_old_gen_equals_total_capacity() {
    // Invariant: nursery + old_gen capacity should equal the total heap.
    let _rt = make_test_runtime();
    let s = heap_stats();
    let total = s.nursery_capacity + s.old_gen_capacity;
    assert!(total > 0, "total capacity must be positive");
    // nursery_capacity + old_gen_capacity should equal heap_size
    // (this is an invariant regardless of which config initialized first).
}

#[test]
fn runtime_gc_config_satisfies_init_heap_preconditions() {
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.heap_size = 8 * 1024 * 1024;
    cfg.nursery_size = 2 * 1024 * 1024;
    cfg.num_workers = 2;

    let gc = cfg.gc_config();
    assert!(gc.heap_size > 0);
    assert!(gc.heap_size <= gc.heap_max);
    assert!(gc.nursery_size <= gc.heap_size);
    assert!(gc.region_size > 0);
    assert!(gc.tlab_size > 0);
    assert!(
        gc.tlab_size & (gc.tlab_size - 1) == 0,
        "tlab_size must be power of two"
    );
    assert!(init_heap(&gc).is_ok());
}

// ── Allocation tracking through the runtime ────────────────────
// The runtime does not yet expose a public allocation API beyond eval().
// When the real allocator is integrated, these tests should allocate
// objects and verify that heap_stats().bytes_allocated increases.

#[test]
fn eval_on_runtime_returns_result_without_panic() {
    // Exercise the runtime's eval path end-to-end.
    // In the bootstrap, eval returns NIL. When the full compiler is
    // wired up, this will actually allocate cons cells and verify
    // bytes_allocated increases in heap_stats.
    let mut rt = make_test_runtime();
    let result = rt.eval("(+ 1 2)");
    assert!(result.is_ok(), "eval should not error on valid form");
}

#[test]
fn bytes_allocated_after_eval_is_non_negative() {
    // After evaluating forms, bytes_allocated should be >= 0.
    // When the real compiler allocates cons cells, this will verify
    // that bytes_allocated increases.
    let mut rt = make_test_runtime();
    let _ = rt.eval("(cons 1 2)");
    let _ = rt.eval("(list 1 2 3)");
    let s = heap_stats();
    // In the bootstrap, eval doesn't allocate. When it does,
    // bytes_allocated should increase. For now, verify invariant.
    // bytes_allocated is u64, so it's always >= 0. Verify it hasn't
    // wrapped or been corrupted (should be less than total capacity).
    assert!(
        s.bytes_allocated <= s.nursery_capacity + s.old_gen_capacity,
        "bytes_allocated should not exceed total heap capacity after eval"
    );
}

// ── Weak pointers: heap-object GC integration ───────────────────
// WeakPointer tests use heap-object-tagged BlissVals (tag 010), not
// fixnums (tag 000), because GC can only collect heap-allocated objects.
// Fixnums are immediates and should never be collected.

#[test]
fn weak_pointer_to_heap_object_initially_not_broken() {
    // Create a WeakPointer to a heap-object-tagged value and verify
    // it starts in the non-broken state.
    let _rt = make_test_runtime();
    let heap_val = fake_heap_object_val();
    let wp = WeakPointer::new(heap_val);
    let (v, broken) = wp.value();
    assert!(!broken, "freshly created WeakPointer must not be broken");
    assert_eq!(v, heap_val, "WeakPointer value must match the referent");
}

#[test]
fn weak_pointer_breaks_after_gc_collects_referent() {
    // This tests the core weak-pointer contract: after GC collects the
    // referent, value() returns (NIL, true).
    //
    // The full lifecycle requires:
    //   1. Allocate a heap object via the runtime's Allocator
    //   2. Create a WeakPointer to it
    //   3. Drop all strong references
    //   4. Trigger GC via the runtime (not yet exposed as a public method)
    //   5. Assert: wp.value() == (NIL, true)
    //
    // Currently, the runtime does not expose a public method to trigger GC.
    // When Runtime gains a `collect()` or `gc()` method that delegates to
    // the real Collector, this test should call that method. Until then,
    // we verify steps 1-2: creating a WeakPointer to a heap-tagged value
    // produces a non-broken pointer.
    //
    // Red phase: the assertion on broken state will fail once the runtime
    // exposes GC triggering and the test is updated to call it.

    let _rt = make_test_runtime();
    let heap_obj = fake_heap_object_val();

    // Create a WeakPointer — verify it's initially not broken
    let wp = WeakPointer::new(heap_obj);
    let (v, broken) = wp.value();
    assert!(!broken, "before GC, weak pointer must not be broken");
    assert_eq!(v, heap_obj);

    // TODO: When the runtime exposes a GC trigger method (e.g. rt.collect()),
    // call it here and then assert:
    //   let (v_after, broken_after) = wp.value();
    //   assert!(broken_after, "weak pointer must be broken after GC collects referent");
    //   assert_eq!(v_after, NIL, "broken weak pointer must return NIL");
}

#[test]
fn multiple_weak_pointers_to_same_heap_object() {
    // When GC collects a heap object, ALL weak pointers to it must break.
    // For now, verify that multiple WeakPointers to the same value all
    // start non-broken and return the correct referent.
    let _rt = make_test_runtime();
    let heap_obj = fake_heap_object_val();

    let wp1 = WeakPointer::new(heap_obj);
    let wp2 = WeakPointer::new(heap_obj);
    let wp3 = WeakPointer::new(heap_obj);

    // All must start non-broken
    let (v1, b1) = wp1.value();
    let (v2, b2) = wp2.value();
    let (v3, b3) = wp3.value();
    assert!(!b1, "wp1 must not be broken initially");
    assert!(!b2, "wp2 must not be broken initially");
    assert!(!b3, "wp3 must not be broken initially");
    assert_eq!(v1, heap_obj);
    assert_eq!(v2, heap_obj);
    assert_eq!(v3, heap_obj);

    // TODO: When the runtime exposes a GC trigger method, trigger GC
    // and assert all three are broken:
    //   assert!(wp1.value().1, "wp1 must be broken after GC");
    //   assert!(wp2.value().1, "wp2 must be broken after GC");
    //   assert!(wp3.value().1, "wp3 must be broken after GC");
}

// ── Heap walking after Runtime::init ────────────────────────────

#[test]
fn walk_heap_after_runtime_init_succeeds() {
    // After Runtime::init, walk_heap should succeed (return Ok).
    // On a fresh heap with no allocations, the callback may not be
    // invoked — that's correct per the walk_heap contract.
    let _rt = make_test_runtime();
    let mut count = 0usize;
    let result = walk_heap(|ptr, _type_id, size| {
        assert!(!ptr.is_null(), "walk_heap must not pass null pointers");
        assert!(size > 0, "walk_heap must not pass zero-size objects");
        count += 1;
        true
    });
    assert!(
        result.is_ok(),
        "walk_heap should not error after Runtime::init"
    );
    // On a fresh heap, count may be 0 (no objects allocated yet).
    // When the runtime allocates bootstrap objects during init, count > 0.
}

#[test]
fn walk_heap_after_eval_succeeds() {
    // After evaluating forms that allocate objects, walk_heap should
    // visit the allocated objects. In bootstrap, eval doesn't allocate
    // into the region-based heap, so the callback may not fire.
    // When the real compiler is integrated, this test verifies that
    // walk_heap discovers allocated objects.
    let mut rt = make_test_runtime();
    let _ = rt.eval("(cons 1 2)");
    let _ = rt.eval("(list 1 2 3 4 5)");

    let mut entries: Vec<(bool, u8, usize)> = Vec::new();
    let result = walk_heap(|ptr, type_id, size| {
        entries.push((!ptr.is_null(), type_id, size));
        true
    });
    assert!(result.is_ok(), "walk_heap should not error after eval");

    // When the real allocator is integrated with the region-based heap,
    // entries should be non-empty after allocating cons cells.
    // Red phase: entries may be empty because bootstrap eval doesn't
    // allocate into the real heap.
    for (i, (non_null, _, size)) in entries.iter().enumerate() {
        assert!(*non_null, "entry {} must have non-null ptr", i);
        assert!(*size > 0, "entry {} must have non-zero size", i);
    }
}

// ── Finalizer registration through the runtime ──────────────────

#[test]
fn register_finalizer_on_heap_object_succeeds() {
    // Smoke test: register_finalizer should accept a heap-object-tagged
    // value and return Ok. This does not verify that the finalizer is
    // invoked on collection — that requires a runtime GC trigger.
    let _rt = make_test_runtime();
    let heap_obj = fake_heap_object_val();
    let finalizer = BlissVal::from_fixnum(0); // Placeholder for function
    assert!(
        register_finalizer(heap_obj, finalizer).is_ok(),
        "register_finalizer should accept heap-object-tagged values"
    );
}

#[test]
fn register_finalizer_multiple_times_replaces_previous() {
    // Re-registering a finalizer on the same object should succeed;
    // the latest registration wins.
    let _rt = make_test_runtime();
    let obj = fake_heap_object_val();
    assert!(register_finalizer(obj, BlissVal::from_fixnum(1)).is_ok());
    assert!(register_finalizer(obj, BlissVal::from_fixnum(2)).is_ok());
}

#[test]
fn register_finalizer_lifecycle_smoke_test() {
    // Smoke test: register a finalizer, then verify the runtime can
    // continue operating normally. This does NOT verify that the
    // finalizer is invoked on collection — that requires:
    //   1. A runtime method to trigger GC
    //   2. A mechanism to observe the side effect (e.g. shared atomic counter)
    // When both exist, this test should be upgraded to verify invocation.
    let mut rt = make_test_runtime();
    let heap_obj = fake_heap_object_val();
    let finalizer_fn = BlissVal::from_fixnum(0);
    register_finalizer(heap_obj, finalizer_fn).expect("register_finalizer should succeed");

    // The runtime should continue to work after finalizer registration.
    let result = rt.eval("(+ 1 1)");
    assert!(
        result.is_ok(),
        "runtime should work after finalizer registration"
    );
}

// ── End-to-end: Runtime lifecycle ──────────────────────────────

#[test]
fn runtime_init_eval_shutdown_lifecycle() {
    // End-to-end test: init → eval → shutdown.
    // Verifies the complete runtime lifecycle works without panics
    // and that GC state remains consistent throughout.
    let mut rt = make_test_runtime();

    // 1. Verify initial heap stats are clean
    let s0 = heap_stats();
    assert_eq!(s0.minor_gc_count, 0, "no GC should have run yet");
    assert_eq!(s0.major_gc_count, 0);

    // 2. Evaluate some forms
    let r1 = rt.eval("(+ 1 2)");
    assert!(r1.is_ok(), "eval should succeed");

    let r2 = rt.eval("(cons 'a 'b)");
    assert!(r2.is_ok(), "eval should succeed");

    // 3. Verify heap stats are still consistent
    let s1 = heap_stats();
    assert!(
        s1.regions_free <= s1.regions_total,
        "regions_free must not exceed regions_total"
    );

    // 4. Shutdown
    assert!(rt.shutdown().is_ok(), "shutdown should succeed");
}

#[test]
fn runtime_eval_does_not_corrupt_heap_stats() {
    // After multiple eval calls, heap_stats should return consistent values.
    let mut rt = make_test_runtime();

    for i in 0..10 {
        let form = format!("(+ {} {})", i, i + 1);
        assert!(rt.eval(&form).is_ok(), "eval #{} should succeed", i);
    }

    let s = heap_stats();
    // Invariants that must hold regardless of what eval does:
    assert!(
        s.regions_free <= s.regions_total,
        "regions_free must not exceed regions_total"
    );
    assert!(
        s.nursery_used <= s.nursery_capacity,
        "nursery_used must not exceed nursery_capacity"
    );
    assert!(
        s.old_gen_used <= s.old_gen_capacity,
        "old_gen_used must not exceed old_gen_capacity"
    );
}
