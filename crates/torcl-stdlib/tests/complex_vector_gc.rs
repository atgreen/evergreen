//! GC-rooting regressions for the COMPLEX_ARRAY grow paths (bliss-ez7w).
//!
//! `vector_push_extend` and `adjust_complex_vector` replace a complex vector's
//! backing storage by calling `build_vector`, which allocates and can therefore
//! fire a minor GC. A minor GC is a Cheney scavenge: it *copies* live nursery
//! objects into survivor space, so the complex vector itself moves. Any Rust
//! local still holding the pre-move address is stale, and writing the fresh
//! storage (or the fill pointer) through it is lost — the surviving array keeps
//! its old, smaller storage and subsequent element writes run off its end.
//!
//! These tests pin that down by running with `TORCL_GC_STRESS=1`, so the
//! allocation inside `build_vector` collects every time, and by growing a
//! vector that is still nursery-fresh (an object only relocates on the first
//! collection that reaches it; once it lands in survivor space it stops moving,
//! which is why the same code path looks clean in a long-running REPL).

use torcl_rt::gc::{GcConfig, init_heap};
use torcl_rt::value::{NIL, TorclVal};

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 256,
        region_size: 1024,
        promotion_threshold: 15,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    }
}

/// Must run before the first allocation: the stride is cached in a `OnceLock`.
/// This is the only test in this binary, so the process is still single-
/// threaded and nothing has allocated yet.
fn enable_gc_stress_and_init() {
    unsafe { std::env::set_var("TORCL_GC_STRESS", "1") };
    init_heap(&gc_config()).expect("init_heap");
}

#[test]
fn grow_paths_survive_a_minor_gc_fired_by_their_own_storage_allocation() {
    enable_gc_stress_and_init();

    // ── A complex vector really does relocate on the first minor GC that
    //    reaches it. If this stops holding, the rest of the test is vacuous. ──
    let probe = torcl_stdlib::build_complex_vector(&[], 4, 0, true, false, false, true);
    let probe_before = unsafe { probe.as_ptr() } as usize;
    torcl_rt::rooted!(probe = probe);
    torcl_rt::gc::collect_t0_minor().expect("minor gc");
    assert_ne!(
        unsafe { (*probe).as_ptr() } as usize,
        probe_before,
        "precondition: a nursery complex vector must move across a minor GC, \
         otherwise this test cannot exercise the stale-pointer hazard"
    );

    // ── VECTOR-PUSH-EXTEND on a full (capacity 0) vector: the grow calls
    //    build_vector, whose allocation collects and moves `v`. ──
    let v = torcl_stdlib::build_complex_vector(&[], 0, 0, true, false, false, true);
    torcl_rt::rooted!(v = v);
    let elem = TorclVal::from_fixnum(42);
    torcl_stdlib::vector_push_extend(*v, elem, None).expect("vector-push-extend");
    assert_eq!(
        torcl_stdlib::cvec_fill_pointer(*v),
        1,
        "fill pointer must be written through the post-GC address"
    );
    assert!(
        torcl_stdlib::cvec_capacity(*v) >= 1,
        "the freshly built storage must be installed in the post-GC object, \
         not the pre-move one"
    );
    assert_eq!(
        torcl_stdlib::elt(*v, 0).expect("elt 0"),
        TorclVal::from_fixnum(42),
        "the pushed element must be readable back"
    );

    // ── ADJUST-ARRAY's grow path has the same shape. ──
    let a = torcl_stdlib::build_complex_vector(
        &[TorclVal::from_fixnum(7)],
        1,
        1,
        true,
        false,
        false,
        false,
    );
    torcl_rt::rooted!(a = a);
    let grown = torcl_stdlib::adjust_complex_vector(*a, 64, None, NIL).expect("adjust-array");
    torcl_rt::rooted!(grown = grown);
    assert!(
        torcl_stdlib::cvec_capacity(*grown) >= 64,
        "grown storage must be installed in the post-GC object"
    );
    assert_eq!(
        torcl_stdlib::cvec_fill_pointer(*grown),
        64,
        "fill pointer must be written through the post-GC address"
    );
    assert_eq!(
        torcl_stdlib::elt(*grown, 0).expect("elt 0"),
        TorclVal::from_fixnum(7),
        "existing elements must be carried into the new storage"
    );
}
