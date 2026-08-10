//! Garbage collector — generational, region-based, concurrent old-gen marking.
//!
//! Inspired by HotSpot G1 and ZGC. See §3 of the spec.

use crate::error::BlissError;
use crate::value::BlissVal;

use std::sync::Mutex;

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
        WeakPointer { referent, broken: false }
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

/// An entry in the finalization queue: (object, finalizer_fn).
struct FinalizerEntry {
    object: BlissVal,
    finalizer: BlissVal,
}

/// Global finalization queue consulted during GC collection.
static FINALIZER_QUEUE: std::sync::LazyLock<Mutex<Vec<FinalizerEntry>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Register a finalizer for a heap object.
/// The finalizer function will be called when the object is about to be collected.
pub fn register_finalizer(object: BlissVal, finalizer: BlissVal) -> Result<(), BlissError> {
    let mut queue = FINALIZER_QUEUE.lock().map_err(|e| {
        BlissError::Internal(format!("finalizer queue lock poisoned: {}", e))
    })?;
    queue.push(FinalizerEntry { object, finalizer });
    Ok(())
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

// ── Heap internals ────────────────────────────────────────────────

/// Remembered set: tracks cross-region references for generational GC.
/// Stores (source_slot_addr, target_addr) pairs so the minor collector
/// can find old→nursery pointers without scanning the entire old-gen.
struct RememberedSet {
    entries: Vec<(usize, usize)>,
}

impl RememberedSet {
    fn new() -> Self {
        RememberedSet { entries: Vec::new() }
    }

    fn add(&mut self, slot_addr: usize, target_addr: usize) {
        self.entries.push((slot_addr, target_addr));
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Card table for write barrier tracking. Each card covers 512 bytes of heap.
/// A dirty card indicates that region may contain cross-generational pointers.
struct CardTable {
    cards: Vec<u8>,
    card_shift: u32,
}

impl CardTable {
    fn new(heap_size: usize) -> Self {
        let card_size = 512usize;
        let card_shift = card_size.trailing_zeros();
        let num_cards = (heap_size + card_size - 1) / card_size;
        CardTable {
            cards: vec![0u8; num_cards],
            card_shift,
        }
    }

    fn dirty(&mut self, offset: usize) {
        let idx = offset >> self.card_shift;
        if idx < self.cards.len() {
            self.cards[idx] = 1;
        }
    }

    fn _is_dirty(&self, offset: usize) -> bool {
        let idx = offset >> self.card_shift;
        idx < self.cards.len() && self.cards[idx] != 0
    }

    fn clear(&mut self) {
        self.cards.fill(0);
    }
}

/// A single heap region's runtime data: the backing memory and its header.
struct HeapRegion {
    header: RegionHeader,
    /// Backing memory for this region.
    memory: Vec<u8>,
}

/// Internal TLAB descriptor using offsets (Send-safe, unlike raw-pointer Tlab).
struct TlabDescriptor {
    region_idx: u16,
    offset: usize,
    size: usize,
}

/// The global heap structure, initialized by `init_heap`.
struct Heap {
    /// All regions (side array of metadata + memory).
    regions: Vec<HeapRegion>,
    /// Indices of regions designated as nursery.
    nursery_indices: Vec<usize>,
    /// Free-list of region indices.
    free_indices: Vec<usize>,
    /// TLAB pool: pre-created TLABs for thread-local allocation (offset-based).
    tlab_pool: Vec<TlabDescriptor>,
    /// Remembered set for cross-region references.
    remembered_set: RememberedSet,
    /// Card table for write barrier fast-path.
    card_table: CardTable,
    /// Copy of the configuration used to create this heap.
    config: GcConfig,
    /// Running statistics.
    stats: GcStats,
}

// Safety: Heap's raw pointers (in RegionHeader) are derived from owned Vec<u8>
// allocations and are only accessed under the HEAP mutex.
unsafe impl Send for Heap {}

static HEAP: std::sync::LazyLock<Mutex<Option<Heap>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Initialize the GC heap. Called once during runtime startup.
/// Validates configuration and sets up the region-based heap structure.
pub fn init_heap(config: &GcConfig) -> Result<(), BlissError> {
    if config.heap_size == 0 {
        return Err(BlissError::Internal("heap_size must be > 0".into()));
    }
    if config.heap_size > config.heap_max {
        return Err(BlissError::Internal("heap_size exceeds heap_max".into()));
    }
    if config.nursery_size > config.heap_size {
        return Err(BlissError::Internal("nursery_size exceeds heap_size".into()));
    }
    if config.region_size == 0 || !config.region_size.is_power_of_two() {
        return Err(BlissError::Internal("region_size must be a positive power of 2".into()));
    }
    if config.tlab_size == 0 || !config.tlab_size.is_power_of_two() {
        return Err(BlissError::Internal("tlab_size must be a positive power of 2".into()));
    }
    // Validate tlab_size vs region_size: if tlab exceeds region and isn't
    // a clean power-of-two multiple of region_size, reject it.
    // A tlab > region that is also a valid power of two is allowed (multi-region TLAB).
    // A tlab > region that is NOT a power of two is invalid — caught above.
    // This explicit check catches edge cases where tlab_size passes the power-of-two
    // check but is not a valid multiple of region_size.
    if config.tlab_size > config.region_size && (config.tlab_size % config.region_size != 0) {
        return Err(BlissError::Internal("tlab_size exceeds region_size and is not a clean multiple".into()));
    }

    // Issue #4: Actually allocate the heap, regions, nursery, etc.
    let num_regions = config.heap_size / config.region_size;
    let nursery_regions = config.nursery_size / config.region_size;

    let mut regions = Vec::with_capacity(num_regions);
    let mut nursery_indices = Vec::with_capacity(nursery_regions);
    let mut free_indices = Vec::new();

    for i in 0..num_regions {
        let kind = if i < nursery_regions {
            nursery_indices.push(i);
            RegionKind::Nursery
        } else {
            free_indices.push(i);
            RegionKind::Free
        };

        let mut memory = vec![0u8; config.region_size];
        let base = memory.as_mut_ptr();
        let limit = unsafe { base.add(config.region_size) } as *const u8;

        regions.push(HeapRegion {
            header: RegionHeader {
                kind,
                gen_age: 0,
                live_bytes: 0,
                alloc_top: base,
                alloc_limit: limit,
                next_free: if i + 1 < num_regions { (i + 1) as u32 } else { u32::MAX },
                mark_bitmap_offset: 0,
            },
            memory,
        });
    }

    // Create initial TLAB pool from nursery regions (using offset-based descriptors)
    let tlabs_per_region = config.region_size / config.tlab_size;
    let mut tlab_pool = Vec::new();
    for &ni in &nursery_indices {
        for t in 0..tlabs_per_region {
            let offset = t * config.tlab_size;
            tlab_pool.push(TlabDescriptor {
                region_idx: ni as u16,
                offset,
                size: config.tlab_size,
            });
        }
    }

    let remembered_set = RememberedSet::new();
    let card_table = CardTable::new(config.heap_size);

    let stats = GcStats {
        nursery_capacity: config.nursery_size as u64,
        old_gen_capacity: (config.heap_size - config.nursery_size) as u64,
        regions_total: num_regions as u32,
        regions_free: free_indices.len() as u32,
        ..GcStats::default()
    };

    let heap = Heap {
        regions,
        nursery_indices,
        free_indices,
        tlab_pool,
        remembered_set,
        card_table,
        config: config.clone(),
        stats,
    };

    let mut global = HEAP.lock().map_err(|e| {
        BlissError::Internal(format!("heap lock poisoned: {}", e))
    })?;
    *global = Some(heap);
    Ok(())
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
    let global = HEAP.lock().map_err(|e| {
        BlissError::Internal(format!("heap lock poisoned: {}", e))
    })?;
    let heap = match global.as_ref() {
        Some(h) => h,
        None => return Ok(()), // Heap not yet initialised — nothing to walk.
    };

    for region in &heap.regions {
        if region.header.kind == RegionKind::Free {
            continue;
        }
        // Walk from the base of the region's memory up to alloc_top.
        // Each object is prefixed with a minimal header: [type_id: u8][size: u32 LE].
        let base = region.memory.as_ptr();
        let top = region.header.alloc_top as *const u8;
        let mut cursor = base;
        while cursor < top {
            // Need at least 5 bytes for (type_id + size)
            let remaining = unsafe { top.offset_from(cursor) } as usize;
            if remaining < 5 {
                break;
            }
            let type_id = unsafe { *cursor };
            let size_bytes: [u8; 4] = unsafe {
                [*cursor.add(1), *cursor.add(2), *cursor.add(3), *cursor.add(4)]
            };
            let obj_size = u32::from_le_bytes(size_bytes) as usize;
            if obj_size == 0 {
                break; // No more objects.
            }
            let obj_ptr = unsafe { cursor.add(5) };
            if !callback(obj_ptr, type_id, obj_size) {
                return Ok(()); // Caller asked to stop.
            }
            // Advance past header + object body, aligned to 8 bytes.
            let total = 5 + obj_size;
            let aligned = (total + 7) & !7;
            cursor = unsafe { cursor.add(aligned) };
        }
    }

    Ok(())
}
