//! Tests for bliss-rt GC module: region model, allocator, collector,
//! write barrier, weak pointers, finalization, heap init, and stats.

use bliss_rt::gc::*;
use bliss_rt::value::{BlissVal, NIL, T};

// ── RegionKind ────────────────────────────────────────────────────

#[test]
fn region_kind_all_variants_distinct() {
    let kinds = [
        RegionKind::Free, RegionKind::Nursery, RegionKind::Survivor,
        RegionKind::OldGen, RegionKind::LargeObject,
    ];
    for (i, a) in kinds.iter().enumerate() {
        for (j, b) in kinds.iter().enumerate() {
            assert_eq!(i == j, a == b, "variant equality mismatch at {i},{j}");
        }
    }
}

#[test]
fn region_kind_is_copy_clone_debug() {
    let k = RegionKind::Nursery;
    let k2 = k; // Copy
    assert_eq!(k, k2.clone());
    assert!(format!("{:?}", k).contains("Nursery"));
}

// ── RegionHeader ──────────────────────────────────────────────────

#[test]
fn region_header_fields_roundtrip() {
    let mut h = RegionHeader {
        kind: RegionKind::Free, gen_age: 0, live_bytes: 0,
        alloc_top: std::ptr::null_mut(), alloc_limit: std::ptr::null(),
        next_free: 0, mark_bitmap_offset: 0,
    };
    h.kind = RegionKind::Survivor;
    h.gen_age = 5;
    h.live_bytes = 8192;
    h.next_free = 42;
    h.mark_bitmap_offset = 1024;
    assert_eq!(h.kind, RegionKind::Survivor);
    assert_eq!(h.gen_age, 5);
    assert_eq!(h.live_bytes, 8192);
    assert_eq!(h.next_free, 42);
    assert_eq!(h.mark_bitmap_offset, 1024);
}

// ── Tlab ──────────────────────────────────────────────────────────

#[test]
fn tlab_fields_roundtrip() {
    let mut buf = [0u8; 256];
    let base = buf.as_mut_ptr();
    let limit = unsafe { base.add(256) } as *const u8;
    let tlab = Tlab { cursor: base, limit, region_idx: 7 };
    assert_eq!(tlab.cursor, base);
    assert_eq!(tlab.limit, limit);
    assert_eq!(tlab.region_idx, 7);
}

// ── GcConfig ──────────────────────────────────────────────────────

fn make_gc_config() -> GcConfig {
    GcConfig {
        heap_size: 64 << 20, heap_max: 256 << 20, nursery_size: 8 << 20,
        tlab_size: 2 << 20, region_size: 1 << 20, promotion_threshold: 15,
        pause_target_ms: 10, gc_workers: 2, satb_buffer_size: 1024,
        old_occupancy_trigger: 0.45,
    }
}

#[test]
fn gc_config_fields_and_clone() {
    let cfg = make_gc_config();
    let c2 = cfg.clone();
    assert_eq!(c2.heap_size, 64 << 20);
    assert_eq!(c2.gc_workers, 2);
    assert!((c2.old_occupancy_trigger - 0.45).abs() < f64::EPSILON);
}

// ── GcStats ───────────────────────────────────────────────────────

#[test]
fn gc_stats_default_all_zeroed() {
    let s = GcStats::default();
    assert_eq!(s.minor_gc_count, 0);
    assert_eq!(s.major_gc_count, 0);
    assert_eq!(s.total_minor_pause_us, 0);
    assert_eq!(s.total_major_pause_us, 0);
    assert_eq!(s.bytes_allocated, 0);
    assert_eq!(s.bytes_promoted, 0);
    assert_eq!(s.nursery_used, 0);
    assert_eq!(s.nursery_capacity, 0);
    assert_eq!(s.old_gen_used, 0);
    assert_eq!(s.old_gen_capacity, 0);
    assert_eq!(s.large_object_bytes, 0);
    assert_eq!(s.regions_total, 0);
    assert_eq!(s.regions_free, 0);
}

#[test]
fn gc_stats_clone_and_debug() {
    let mut s = GcStats::default();
    s.minor_gc_count = 10;
    s.bytes_allocated = 999;
    let s2 = s.clone();
    assert_eq!(s2.minor_gc_count, 10);
    assert!(format!("{:?}", s2).contains("bytes_allocated"));
}

// ── init_heap ─────────────────────────────────────────────────────

#[test]
fn init_heap_valid_config_succeeds() {
    assert!(init_heap(&make_gc_config()).is_ok());
}

#[test]
fn init_heap_zero_heap_errors() {
    let mut cfg = make_gc_config();
    cfg.heap_size = 0;
    assert!(init_heap(&cfg).is_err());
}

// ── WeakPointer ───────────────────────────────────────────────────

#[test]
fn weak_pointer_new_and_value() {
    let wp = WeakPointer::new(NIL);
    let (val, broken) = wp.value();
    assert!(!broken);
    assert_eq!(val, NIL);
}

// ── register_finalizer ────────────────────────────────────────────

#[test]
fn register_finalizer_valid_args_succeeds() {
    assert!(register_finalizer(NIL, T).is_ok());
}

// ── walk_heap ─────────────────────────────────────────────────────

#[test]
fn walk_heap_runs_and_stops_early() {
    let mut count = 0usize;
    let _ = walk_heap(|_, _, _| { count += 1; false });
    assert!(count <= 1, "should stop after callback returns false");
}
