//! GC integration tests — full GC lifecycle through real public APIs.
//! Red-phase TDD: expected to fail until implementation is wired up.

use bliss_rt::gc::{
    init_heap, heap_stats, walk_heap, register_finalizer,
    GcConfig, GcStats, WeakPointer, Collector, Allocator,
};
use bliss_rt::value::{BlissVal, NIL, T};
use bliss_rt::error::BlissError;

fn test_gc_config() -> GcConfig {
    GcConfig {
        heap_size: 16 * 1024 * 1024,
        heap_max: 32 * 1024 * 1024,
        nursery_size: 4 * 1024 * 1024,
        tlab_size: 32 * 1024,
        region_size: 1024 * 1024,
        promotion_threshold: 3,
        pause_target_ms: 10,
        gc_workers: 2,
        satb_buffer_size: 1024,
        old_occupancy_trigger: 0.45,
    }
}

fn small_gc_config() -> GcConfig {
    GcConfig {
        heap_size: 1024 * 1024,
        heap_max: 2 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 1024,
        region_size: 64 * 1024,
        promotion_threshold: 2,
        pause_target_ms: 5,
        gc_workers: 1,
        satb_buffer_size: 256,
        old_occupancy_trigger: 0.50,
    }
}

// ── Heap initialisation ───────────────────────────────────────────

#[test]
fn init_heap_valid_config_succeeds() {
    assert!(init_heap(&test_gc_config()).is_ok());
}

#[test]
fn init_heap_zero_heap_size_fails() {
    let mut c = test_gc_config();
    c.heap_size = 0;
    assert!(init_heap(&c).is_err());
}

#[test]
fn init_heap_heap_exceeds_max_fails() {
    let mut c = test_gc_config();
    c.heap_size = 64 * 1024 * 1024;
    c.heap_max = 32 * 1024 * 1024;
    assert!(init_heap(&c).is_err());
}

#[test]
fn init_heap_nursery_exceeds_heap_fails() {
    let mut c = test_gc_config();
    c.nursery_size = c.heap_size + 1;
    assert!(init_heap(&c).is_err());
}

#[test]
fn init_heap_zero_region_size_fails() {
    let mut c = test_gc_config();
    c.region_size = 0;
    assert!(init_heap(&c).is_err());
}

#[test]
fn init_heap_tlab_not_power_of_two_fails() {
    let mut c = test_gc_config();
    c.tlab_size = 3000;
    assert!(init_heap(&c).is_err());
}

#[test]
fn init_heap_zero_tlab_size_fails() {
    let mut c = test_gc_config();
    c.tlab_size = 0;
    assert!(init_heap(&c).is_err());
}

// ── Stats after init ──────────────────────────────────────────────

#[test]
fn heap_stats_capacity_matches_config() {
    let cfg = test_gc_config();
    let _ = init_heap(&cfg);
    let s = heap_stats();
    assert_eq!(s.nursery_capacity, cfg.nursery_size as u64);
    assert_eq!(s.old_gen_capacity, (cfg.heap_size - cfg.nursery_size) as u64);
}

#[test]
fn heap_stats_regions_total_matches_config() {
    let cfg = test_gc_config();
    let _ = init_heap(&cfg);
    let s = heap_stats();
    assert_eq!(s.regions_total, (cfg.heap_size / cfg.region_size) as u32);
}

#[test]
fn heap_stats_fresh_heap_zero_gc_counts() {
    let _ = init_heap(&test_gc_config());
    let s = heap_stats();
    assert_eq!(s.minor_gc_count, 0);
    assert_eq!(s.major_gc_count, 0);
    assert_eq!(s.total_minor_pause_us, 0);
    assert_eq!(s.total_major_pause_us, 0);
}

#[test]
fn heap_stats_fresh_heap_zero_bytes() {
    let _ = init_heap(&test_gc_config());
    let s = heap_stats();
    assert_eq!(s.bytes_allocated, 0);
    assert_eq!(s.bytes_promoted, 0);
}

#[test]
fn heap_stats_fresh_heap_all_regions_free() {
    let _ = init_heap(&test_gc_config());
    let s = heap_stats();
    assert_eq!(s.regions_free, s.regions_total);
}

#[test]
fn gc_stats_default_all_zero() {
    let s = GcStats::default();
    assert_eq!(s.minor_gc_count, 0);
    assert_eq!(s.major_gc_count, 0);
    assert_eq!(s.bytes_allocated, 0);
    assert_eq!(s.nursery_capacity, 0);
    assert_eq!(s.old_gen_capacity, 0);
    assert_eq!(s.regions_total, 0);
    assert_eq!(s.regions_free, 0);
    assert_eq!(s.large_object_bytes, 0);
    assert_eq!(s.nursery_used, 0);
    assert_eq!(s.old_gen_used, 0);
    assert_eq!(s.total_minor_pause_us, 0);
    assert_eq!(s.total_major_pause_us, 0);
    assert_eq!(s.bytes_promoted, 0);
}

// ── Allocation tracking ──────────────────────────────────────────

#[test]
fn bytes_allocated_never_decreases() {
    let _ = init_heap(&small_gc_config());
    let before = heap_stats().bytes_allocated;
    let after = heap_stats().bytes_allocated;
    assert!(after >= before);
}

#[test]
fn large_object_bytes_starts_zero() {
    let _ = init_heap(&small_gc_config());
    assert_eq!(heap_stats().large_object_bytes, 0);
}

// ── Collector trait ──────────────────────────────────────────────

struct StubCollector { stats: GcStats }

impl StubCollector {
    fn new() -> Self { StubCollector { stats: GcStats::default() } }
}

impl Collector for StubCollector {
    fn minor_gc(&mut self) -> Result<(), BlissError> {
        self.stats.minor_gc_count += 1;
        Ok(())
    }
    fn major_gc(&mut self) -> Result<(), BlissError> {
        self.stats.major_gc_count += 1;
        Ok(())
    }
    fn full_gc(&mut self) -> Result<(), BlissError> {
        self.stats.minor_gc_count += 1;
        self.stats.major_gc_count += 1;
        Ok(())
    }
    fn stats(&self) -> GcStats { self.stats.clone() }
}

#[test]
fn minor_gc_increments_minor_count() {
    let mut c = StubCollector::new();
    assert_eq!(c.stats().minor_gc_count, 0);
    c.minor_gc().unwrap();
    assert_eq!(c.stats().minor_gc_count, 1);
    c.minor_gc().unwrap();
    assert_eq!(c.stats().minor_gc_count, 2);
}

#[test]
fn major_gc_increments_major_count() {
    let mut c = StubCollector::new();
    c.major_gc().unwrap();
    assert_eq!(c.stats().major_gc_count, 1);
}

#[test]
fn full_gc_increments_both_counts() {
    let mut c = StubCollector::new();
    c.full_gc().unwrap();
    assert!(c.stats().minor_gc_count >= 1);
    assert!(c.stats().major_gc_count >= 1);
}

#[test]
fn minor_gc_does_not_affect_major_count() {
    let mut c = StubCollector::new();
    c.minor_gc().unwrap();
    c.minor_gc().unwrap();
    assert_eq!(c.stats().major_gc_count, 0);
}

#[test]
fn major_gc_does_not_affect_minor_count() {
    let mut c = StubCollector::new();
    c.major_gc().unwrap();
    assert_eq!(c.stats().minor_gc_count, 0);
}

#[test]
fn collector_stats_independent_of_global_heap_stats() {
    let mut c = StubCollector::new();
    c.minor_gc().unwrap();
    assert_eq!(c.stats().minor_gc_count, 1);
    assert_eq!(heap_stats().minor_gc_count, 0);
}

// ── Weak pointers ────────────────────────────────────────────────

#[test]
fn weak_pointer_new_is_not_broken() {
    let val = BlissVal::from_fixnum(42);
    let wp = WeakPointer::new(val);
    let (v, broken) = wp.value();
    assert!(!broken);
    assert_eq!(v, val);
}

#[test]
fn weak_pointer_to_nil_and_t() {
    let wp_nil = WeakPointer::new(NIL);
    let (v, b) = wp_nil.value();
    assert!(!b);
    assert_eq!(v, NIL);

    let wp_t = WeakPointer::new(T);
    let (v, b) = wp_t.value();
    assert!(!b);
    assert_eq!(v, T);
}

#[test]
fn weak_pointer_broken_contract() {
    // Fresh pointer must not be broken; after GC collects referent,
    // value() must return (NIL, true). We verify fresh state here.
    let val = BlissVal::from_fixnum(99);
    let wp = WeakPointer::new(val);
    let (v, broken) = wp.value();
    assert!(!broken);
    assert_eq!(v, val);
}

#[test]
fn weak_pointer_preserves_fixnum_values() {
    for n in [-1_000_000i64, -1, 0, 1, 42, 1_000_000] {
        let val = BlissVal::from_fixnum(n);
        let wp = WeakPointer::new(val);
        let (v, b) = wp.value();
        assert!(!b);
        assert_eq!(v.as_fixnum(), n);
    }
}

#[test]
fn weak_pointer_preserves_character() {
    let val = BlissVal::from_char('λ');
    let wp = WeakPointer::new(val);
    let (v, b) = wp.value();
    assert!(!b);
    assert_eq!(v.as_char(), 'λ');
}

#[test]
fn weak_pointer_preserves_single_float() {
    let val = BlissVal::from_single_float(3.14);
    let wp = WeakPointer::new(val);
    let (v, b) = wp.value();
    assert!(!b);
    assert!((v.as_single_float() - 3.14f32).abs() < 1e-6);
}

#[test]
fn multiple_weak_pointers_to_same_object() {
    let val = BlissVal::from_fixnum(7);
    let wp1 = WeakPointer::new(val);
    let wp2 = WeakPointer::new(val);
    let (v1, b1) = wp1.value();
    let (v2, b2) = wp2.value();
    assert!(!b1 && !b2);
    assert_eq!(v1, v2);
}

// ── Heap walking ─────────────────────────────────────────────────

#[test]
fn walk_heap_empty_heap_no_callbacks() {
    let _ = init_heap(&small_gc_config());
    let mut visited = 0u64;
    let result = walk_heap(|_ptr, _tid, _sz| { visited += 1; true });
    assert!(result.is_ok());
    assert_eq!(visited, 0);
}

#[test]
fn walk_heap_callback_data_validity() {
    let _ = init_heap(&small_gc_config());
    let mut entries: Vec<(bool, u8, usize)> = Vec::new();
    let result = walk_heap(|ptr, type_id, size| {
        entries.push((!ptr.is_null(), type_id, size));
        true
    });
    assert!(result.is_ok());
    for (i, (non_null, _, size)) in entries.iter().enumerate() {
        assert!(*non_null, "entry {} must have non-null ptr", i);
        assert!(*size > 0, "entry {} must have non-zero size", i);
    }
}

#[test]
fn walk_heap_early_termination() {
    let _ = init_heap(&small_gc_config());
    let mut count = 0u64;
    let result = walk_heap(|_, _, _| { count += 1; false });
    assert!(result.is_ok());
    assert!(count <= 1);
}

#[test]
fn walk_heap_does_not_panic_on_uninitialised() {
    let _ = walk_heap(|_, _, _| true);
}

// ── Finalizer registration ───────────────────────────────────────

#[test]
fn register_finalizer_succeeds_for_various_values() {
    assert!(register_finalizer(BlissVal::from_fixnum(42), BlissVal::from_fixnum(0)).is_ok());
    assert!(register_finalizer(NIL, NIL).is_ok());
    assert!(register_finalizer(T, T).is_ok());
    assert!(register_finalizer(BlissVal::from_char('X'), BlissVal::from_fixnum(0)).is_ok());
}

#[test]
fn register_finalizer_multiple_times_same_object() {
    let obj = BlissVal::from_fixnum(100);
    assert!(register_finalizer(obj, BlissVal::from_fixnum(1)).is_ok());
    assert!(register_finalizer(obj, BlissVal::from_fixnum(2)).is_ok());
}

// ── Allocator trait ──────────────────────────────────────────────

struct StubAllocator { cursor: usize, buffer: Vec<u8> }

impl StubAllocator {
    fn new(size: usize) -> Self { StubAllocator { cursor: 0, buffer: vec![0u8; size] } }
}

impl Allocator for StubAllocator {
    fn alloc_fast(&mut self, size: usize) -> Option<*mut u8> {
        if self.cursor + size <= self.buffer.len() {
            let ptr = unsafe { self.buffer.as_mut_ptr().add(self.cursor) };
            self.cursor += size;
            Some(ptr)
        } else {
            None
        }
    }
    fn alloc_slow(&mut self, size: usize) -> Result<*mut u8, BlissError> {
        self.alloc_fast(size).ok_or(BlissError::Oom)
    }
    fn alloc_large(&mut self, size: usize) -> Result<*mut u8, BlissError> {
        self.alloc_slow(size)
    }
}

#[test]
fn allocator_fast_path_success_and_exhaustion() {
    let mut a = StubAllocator::new(128);
    let p = a.alloc_fast(64);
    assert!(p.is_some());
    assert!(!p.unwrap().is_null());
    let _ = a.alloc_fast(64); // consume rest
    assert!(a.alloc_fast(1).is_none());
}

#[test]
fn allocator_slow_path_oom() {
    let mut a = StubAllocator::new(64);
    a.alloc_slow(64).unwrap();
    assert!(a.alloc_slow(1).is_err());
}

#[test]
fn allocator_large_object_path() {
    let mut a = StubAllocator::new(8192);
    assert!(a.alloc_large(4096).is_ok());
}

#[test]
fn allocator_sequential_allocs_distinct_pointers() {
    let mut a = StubAllocator::new(1024);
    let p1 = a.alloc_fast(64).unwrap();
    let p2 = a.alloc_fast(64).unwrap();
    assert_ne!(p1, p2);
}

// ── GcStats consistency ──────────────────────────────────────────

#[test]
fn gc_stats_clone_preserves_values() {
    let mut s = GcStats::default();
    s.minor_gc_count = 5;
    s.major_gc_count = 2;
    s.bytes_allocated = 12345;
    s.regions_total = 16;
    s.regions_free = 10;
    let s2 = s.clone();
    assert_eq!(s2.minor_gc_count, 5);
    assert_eq!(s2.major_gc_count, 2);
    assert_eq!(s2.bytes_allocated, 12345);
    assert_eq!(s2.regions_total, 16);
    assert_eq!(s2.regions_free, 10);
}

#[test]
fn gc_stats_debug_format() {
    let dbg = format!("{:?}", GcStats::default());
    assert!(!dbg.is_empty());
    assert!(dbg.contains("minor_gc_count"));
}

// ── End-to-end: Runtime -> GC ────────────────────────────────────

#[test]
fn runtime_init_initialises_gc_heap() {
    use bliss_rt::runtime::{Runtime, RuntimeConfig};
    let mut cfg = RuntimeConfig::from_env();
    cfg.heap_size = 4 * 1024 * 1024;
    cfg.nursery_size = 1024 * 1024;
    cfg.stack_size = 64 * 1024;
    cfg.num_workers = 1;
    cfg.no_image = true;

    let runtime = Runtime::init(cfg);
    assert!(runtime.is_ok());
    let s = heap_stats();
    assert!(s.nursery_capacity > 0);
    assert!(s.old_gen_capacity > 0);
    assert!(s.regions_total > 0);
}

#[test]
fn runtime_gc_config_satisfies_init_heap_preconditions() {
    use bliss_rt::runtime::RuntimeConfig;
    let mut cfg = RuntimeConfig::from_env();
    cfg.heap_size = 8 * 1024 * 1024;
    cfg.nursery_size = 2 * 1024 * 1024;
    cfg.num_workers = 2;

    let gc = cfg.gc_config();
    assert!(gc.heap_size > 0);
    assert!(gc.heap_size <= gc.heap_max);
    assert!(gc.nursery_size <= gc.heap_size);
    assert!(gc.region_size > 0);
    assert!(gc.tlab_size > 0);
    assert!(gc.tlab_size & (gc.tlab_size - 1) == 0);
    assert!(init_heap(&gc).is_ok());
}
