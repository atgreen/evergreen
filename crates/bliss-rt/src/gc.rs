//! Garbage collector — generational, region-based, concurrent old-gen marking.
//!
//! Inspired by HotSpot G1 and ZGC. See §3 of the spec.

use crate::error::BlissError;
use crate::value::BlissVal;

use std::alloc::{self, Layout};
use std::sync::{Mutex, OnceLock};

// ── Region model ───────────────────────────────────────────────────

/// The kind of a heap region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind {
    Free,
    Nursery,
    Survivor,
    OldGen,
    LargeObject,
}

/// Per-region metadata (stored in side array, not in-region). D3.01.
#[repr(C)]
pub struct RegionHeader {
    pub kind: RegionKind,
    pub gen_age: u8,
    pub live_bytes: u32,
    pub alloc_top: *mut u8,
    pub alloc_limit: *const u8,
    pub next_free: u32,
    pub mark_bitmap_offset: u32,
}

/// Thread-Local Allocation Buffer. D3.02.
pub struct Tlab {
    pub cursor: *mut u8,
    pub limit: *const u8,
    pub region_idx: u16,
}

// ── Allocator trait ────────────────────────────────────────────────

/// Public allocator interface for the runtime.
pub trait Allocator {
    /// Fast-path bump-pointer allocation (inlined by JIT).
    /// Returns `None` if TLAB is exhausted (caller must use slow path).
    fn alloc_fast(&mut self, size: usize) -> Option<*mut u8>;

    /// Slow-path allocation: refill TLAB, trigger minor GC if needed,
    /// or allocate in old-gen for large objects.
    fn alloc_slow(&mut self, size: usize) -> Result<*mut u8, BlissError>;

    /// Allocate a large object directly in old-gen large-object regions.
    fn alloc_large(&mut self, size: usize) -> Result<*mut u8, BlissError>;
}

// ── Collector trait ────────────────────────────────────────────────

/// Public GC interface.
pub trait Collector {
    /// Trigger a minor (nursery) collection. Stop-the-world.
    fn minor_gc(&mut self) -> Result<(), BlissError>;

    /// Trigger a major (old-gen) collection cycle.
    /// Initiates concurrent marking followed by evacuation.
    fn major_gc(&mut self) -> Result<(), BlissError>;

    /// Request a full GC (minor + major). Used before image save.
    fn full_gc(&mut self) -> Result<(), BlissError>;

    /// Query current GC statistics.
    fn stats(&self) -> GcStats;
}

// ── Write barrier ──────────────────────────────────────────────────

/// Write barrier interface emitted by the compiler at every reference store.
pub trait WriteBarrier {
    /// Combined SATB + card barrier.
    /// Called by JIT-generated code at every reference store.
    fn write_barrier(&self, slot_addr: *mut BlissVal, old_val: BlissVal, new_val: BlissVal);
}

// ── Weak references ────────────────────────────────────────────────

/// A weak pointer that is cleared when its referent is collected.
pub struct WeakPointer {
    referent: BlissVal,
    broken: bool,
}

impl WeakPointer {
    /// Create a new weak pointer to `referent`.
    pub fn new(referent: BlissVal) -> Self {
        WeakPointer {
            referent,
            broken: false,
        }
    }

    /// Get the referent value. Returns `(value, broken)`.
    /// If the weak pointer has been broken by GC, returns `(NIL, true)`.
    pub fn value(&self) -> (BlissVal, bool) {
        if self.broken {
            (crate::value::NIL, true)
        } else {
            (self.referent, false)
        }
    }
}

// ── Finalization ───────────────────────────────────────────────────

/// Entry in the finalizer registry: maps an object to its finalizer callback.
struct FinalizerEntry {
    object: BlissVal,
    finalizer: BlissVal,
}

/// Global finalizer registry.
fn finalizer_registry() -> &'static Mutex<Vec<FinalizerEntry>> {
    static REGISTRY: OnceLock<Mutex<Vec<FinalizerEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a finalizer for a heap object.
/// The finalizer function will be called when the object is about to be collected.
pub fn register_finalizer(object: BlissVal, finalizer: BlissVal) -> Result<(), BlissError> {
    let mut registry = finalizer_registry().lock().unwrap();
    // Replace existing finalizer for the same object, or add new entry
    if let Some(entry) = registry.iter_mut().find(|e| e.object == object) {
        entry.finalizer = finalizer;
    } else {
        registry.push(FinalizerEntry { object, finalizer });
    }
    Ok(())
}

/// Run all registered finalizers (called during collection for unreachable objects).
/// Returns the list of finalizer BlissVals that were invoked (for testing/debugging).
pub fn run_finalizers_for(object: BlissVal) -> Vec<BlissVal> {
    let mut registry = finalizer_registry().lock().unwrap();
    let mut invoked = Vec::new();
    // Collect all finalizers for the given object
    let mut i = 0;
    while i < registry.len() {
        if registry[i].object == object {
            let entry = registry.remove(i);
            invoked.push(entry.finalizer);
        } else {
            i += 1;
        }
    }
    invoked
}

// ── GC statistics ──────────────────────────────────────────────────

/// GC statistics for introspection (`ROOM`, profiling).
#[derive(Clone, Debug, Default)]
pub struct GcStats {
    pub minor_gc_count: u64,
    pub major_gc_count: u64,
    pub total_minor_pause_us: u64,
    pub total_major_pause_us: u64,
    pub bytes_allocated: u64,
    pub bytes_promoted: u64,
    pub nursery_used: u64,
    pub nursery_capacity: u64,
    pub old_gen_used: u64,
    pub old_gen_capacity: u64,
    pub large_object_bytes: u64,
    pub regions_total: u32,
    pub regions_free: u32,
}

// ── Heap initialization ────────────────────────────────────────────

/// GC configuration parameters (from env vars / CLI flags).
#[derive(Clone, Debug)]
pub struct GcConfig {
    pub heap_size: usize,
    pub heap_max: usize,
    pub nursery_size: usize,
    pub tlab_size: usize,
    pub region_size: usize,
    pub promotion_threshold: u8,
    pub pause_target_ms: u32,
    pub gc_workers: u32,
    pub satb_buffer_size: usize,
    pub old_occupancy_trigger: f64,
}

/// A single heap region backed by real memory.
struct HeapRegion {
    header: RegionHeader,
    /// Base pointer of the region's backing memory.
    base: *mut u8,
    /// Size of the backing memory allocation (retained for dealloc/walk).
    #[allow(dead_code)]
    size: usize,
}

// Safety: HeapRegion is only accessed under the HeapState mutex.
unsafe impl Send for HeapRegion {}

/// Bootstrap heap state — stores the GC configuration, allocated regions,
/// and stats so that queries can report capacity values after initialization.
struct HeapState {
    #[allow(dead_code)]
    config: GcConfig,
    stats: GcStats,
    /// All heap regions, backed by real allocated memory.
    regions: Vec<HeapRegion>,
    /// Base pointer of the contiguous heap allocation.
    heap_base: *mut u8,
    /// Layout used for the heap allocation (needed for dealloc).
    heap_layout: Layout,
}

// Safety: HeapState is only accessed under the global mutex.
unsafe impl Send for HeapState {}

impl Drop for HeapState {
    fn drop(&mut self) {
        if !self.heap_base.is_null() {
            // Safety: heap_base was allocated with heap_layout in init_heap.
            unsafe {
                alloc::dealloc(self.heap_base, self.heap_layout);
            }
            self.heap_base = std::ptr::null_mut();
        }
    }
}

/// Global heap state, initialized by `init_heap`.
fn heap_state() -> &'static Mutex<Option<HeapState>> {
    static STATE: OnceLock<Mutex<Option<HeapState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// Initialize the GC heap. Called once during runtime startup.
/// Validates configuration and sets up the region-based heap structure.
pub fn init_heap(config: &GcConfig) -> Result<(), BlissError> {
    if config.heap_size == 0 {
        return Err(BlissError::Internal("heap_size must be non-zero".into()));
    }
    if config.heap_size > config.heap_max {
        return Err(BlissError::Internal("heap_size exceeds heap_max".into()));
    }
    if config.nursery_size > config.heap_size {
        return Err(BlissError::Internal("nursery_size exceeds heap_size".into()));
    }
    if config.region_size == 0 {
        return Err(BlissError::Internal("region_size must be non-zero".into()));
    }
    if config.tlab_size == 0 || (config.tlab_size & (config.tlab_size - 1)) != 0 {
        return Err(BlissError::Internal("tlab_size must be a power of two".into()));
    }
    let regions_total = (config.heap_size / config.region_size) as u32;

    // Allocate real heap memory as a single contiguous block.
    // We use page-level alignment (4096) which the system allocator supports,
    // rather than region_size alignment which may be too large.
    let align = 4096.min(config.region_size);
    let heap_layout = Layout::from_size_align(config.heap_size, align)
        .map_err(|e| BlissError::Internal(format!("invalid heap layout: {}", e)))?;

    // Safety: layout is valid (non-zero size, power-of-two alignment).
    let heap_base = unsafe { alloc::alloc_zeroed(heap_layout) };
    if heap_base.is_null() {
        return Err(BlissError::Oom);
    }

    // Set up region metadata. Divide the heap into regions.
    let nursery_regions = (config.nursery_size / config.region_size) as u32;
    let mut regions = Vec::with_capacity(regions_total as usize);

    for i in 0..regions_total {
        let region_base = unsafe { heap_base.add(i as usize * config.region_size) };
        let region_limit = unsafe { region_base.add(config.region_size) } as *const u8;

        let kind = if i < nursery_regions {
            RegionKind::Nursery
        } else {
            RegionKind::Free
        };

        let header = RegionHeader {
            kind,
            gen_age: 0,
            live_bytes: 0,
            alloc_top: region_base, // nothing allocated yet
            alloc_limit: region_limit,
            next_free: if i + 1 < regions_total { i + 1 } else { u32::MAX },
            mark_bitmap_offset: 0,
        };

        regions.push(HeapRegion {
            header,
            base: region_base,
            size: config.region_size,
        });
    }

    let free_regions = regions.iter().filter(|r| r.header.kind == RegionKind::Free).count() as u32;

    let mut stats = GcStats::default();
    stats.nursery_capacity = config.nursery_size as u64;
    stats.old_gen_capacity = (config.heap_size - config.nursery_size) as u64;
    stats.regions_total = regions_total;
    stats.regions_free = free_regions;

    let state = HeapState {
        config: config.clone(),
        stats,
        regions,
        heap_base,
        heap_layout,
    };
    *heap_state().lock().unwrap() = Some(state);

    Ok(())
}

/// Query the current heap stats. Returns default (zeroed) stats if the
/// heap has not been initialized yet.
pub fn heap_stats() -> GcStats {
    let guard = heap_state().lock().unwrap();
    match &*guard {
        Some(state) => state.stats.clone(),
        None => GcStats::default(),
    }
}

/// Walk all live heap objects. Used for image serialisation and debugging.
/// The callback receives (object_ptr, type_id, size) and returns true to continue.
///
/// Iterates over all allocated (non-Free) regions and walks each live object
/// within the region (from region base up to alloc_top). If the heap has not
/// been initialised yet or no objects have been allocated, returns Ok(())
/// with no callbacks invoked.
pub fn walk_heap<F>(mut callback: F) -> Result<(), BlissError>
where
    F: FnMut(*const u8, u8, usize) -> bool,
{
    let guard = heap_state().lock().unwrap();
    let state = match &*guard {
        Some(s) => s,
        None => return Ok(()), // No heap initialized, nothing to walk.
    };

    // Walk all non-Free regions that have allocated data (alloc_top > base).
    for region in &state.regions {
        if region.header.kind == RegionKind::Free {
            continue;
        }

        let base = region.base as usize;
        let top = region.header.alloc_top as usize;

        if top <= base {
            continue; // No objects allocated in this region.
        }

        // Walk objects from base to alloc_top.
        // Each object is at least 8 bytes (one tagged word). In the bootstrap
        // heap, objects are laid out contiguously with an 8-byte header
        // containing (type_id: u8, padding: 3 bytes, size: u32).
        let mut cursor = base;
        while cursor + OBJECT_HEADER_SIZE <= top {
            let header_ptr = cursor as *const u8;
            // Read the object header: first byte is type_id, bytes 4..8 are size (u32 LE).
            let type_id = unsafe { *header_ptr };
            let size = unsafe {
                let size_ptr = (cursor + 4) as *const u32;
                *size_ptr as usize
            };

            if size == 0 {
                break; // No more objects (zero-filled memory).
            }

            let obj_ptr = unsafe { header_ptr.add(OBJECT_HEADER_SIZE) };
            let should_continue = callback(obj_ptr, type_id, size);
            if !should_continue {
                return Ok(());
            }

            // Advance cursor past header + object body, aligned to 8 bytes.
            let total = OBJECT_HEADER_SIZE + size;
            let aligned = (total + 7) & !7;
            cursor += aligned;
        }
    }

    Ok(())
}

/// Size of the per-object header used in the bootstrap heap layout.
const OBJECT_HEADER_SIZE: usize = 8;
