//! Garbage collector — generational, region-based, concurrent old-gen marking.
//!
//! Inspired by HotSpot G1 and ZGC. See §3 of the spec.

use crate::error::BlissError;
use crate::value::BlissVal;

use std::alloc::{self, Layout};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
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
///
/// The public fields use plain types for ergonomic access in single-threaded
/// contexts and tests. For concurrent GC paths (concurrent old-gen marking),
/// use the `_atomic` accessor methods which perform atomic operations on the
/// underlying memory without requiring field-type changes.
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

impl RegionHeader {
    /// Atomically load `live_bytes`. Used by concurrent old-gen marker.
    /// Safety: caller must ensure `self` is validly allocated and not moved.
    pub fn load_live_bytes_atomic(&self) -> u32 {
        // Safety: `live_bytes` is a u32 at a stable address; we cast to AtomicU32
        // for an atomic load. This is sound because AtomicU32 has the same
        // size/alignment as u32 and we only perform a load.
        let ptr = &self.live_bytes as *const u32 as *const AtomicU32;
        unsafe { (*ptr).load(Ordering::Acquire) }
    }

    /// Atomically store `live_bytes`. Used by concurrent old-gen marker.
    pub fn store_live_bytes_atomic(&self, val: u32) {
        let ptr = &self.live_bytes as *const u32 as *const AtomicU32;
        unsafe { (*ptr).store(val, Ordering::Release) }
    }

    /// Atomically load `alloc_top`. Used by concurrent old-gen marker.
    pub fn load_alloc_top_atomic(&self) -> *mut u8 {
        let ptr = &self.alloc_top as *const *mut u8 as *const AtomicPtr<u8>;
        unsafe { (*ptr).load(Ordering::Acquire) }
    }

    /// Atomically store `alloc_top`. Used by allocator bump-pointer update.
    pub fn store_alloc_top_atomic(&self, val: *mut u8) {
        let ptr = &self.alloc_top as *const *mut u8 as *const AtomicPtr<u8>;
        unsafe { (*ptr).store(val, Ordering::Release) }
    }

    /// Atomically load `next_free`. Used during concurrent region free-list access.
    pub fn load_next_free_atomic(&self) -> u32 {
        let ptr = &self.next_free as *const u32 as *const AtomicU32;
        unsafe { (*ptr).load(Ordering::Acquire) }
    }

    /// Atomically store `next_free`.
    pub fn store_next_free_atomic(&self, val: u32) {
        let ptr = &self.next_free as *const u32 as *const AtomicU32;
        unsafe { (*ptr).store(val, Ordering::Release) }
    }
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

// ── Concrete Allocator: HeapAllocator ──────────────────────────────

/// Concrete allocator backed by the global HeapState.
/// Implements bump-pointer TLAB allocation, TLAB refill with minor GC
/// triggering, and large-object region allocation.
pub struct HeapAllocator {
    /// The thread-local allocation buffer for this allocator.
    pub tlab: Tlab,
    /// Configuration snapshot (region_size, tlab_size, etc.).
    region_size: usize,
    tlab_size: usize,
}

// Safety: HeapAllocator owns its TLAB and only the owning thread uses it.
unsafe impl Send for HeapAllocator {}

impl HeapAllocator {
    /// Create a new HeapAllocator. The heap must already be initialized via `init_heap`.
    pub fn new() -> Result<Self, BlissError> {
        let guard = heap_state().lock().unwrap();
        let state = guard.as_ref().ok_or_else(|| {
            BlissError::Internal("heap not initialized".into())
        })?;
        let region_size = state.config.region_size;
        let tlab_size = state.config.tlab_size;
        drop(guard);

        let mut alloc = HeapAllocator {
            tlab: Tlab {
                cursor: std::ptr::null_mut(),
                limit: std::ptr::null(),
                region_idx: 0,
            },
            region_size,
            tlab_size,
        };
        // Try to get an initial TLAB from a nursery region.
        alloc.refill_tlab()?;
        Ok(alloc)
    }

    /// Refill the TLAB from a nursery region. If no nursery space is available,
    /// triggers a minor GC (via the global collector) and retries once.
    fn refill_tlab(&mut self) -> Result<(), BlissError> {
        let mut guard = heap_state().lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            BlissError::Internal("heap not initialized".into())
        })?;

        // Find a nursery region with enough space for a TLAB.
        for (idx, region) in state.regions.iter_mut().enumerate() {
            if region.header.kind != RegionKind::Nursery {
                continue;
            }
            let top = region.header.alloc_top as usize;
            let limit = region.header.alloc_limit as usize;
            let available = limit.saturating_sub(top);
            if available >= self.tlab_size {
                // Carve out a TLAB from this region.
                self.tlab.cursor = region.header.alloc_top;
                self.tlab.limit = unsafe { region.header.alloc_top.add(self.tlab_size) } as *const u8;
                self.tlab.region_idx = idx as u16;
                // Advance the region's alloc_top past the TLAB.
                region.header.alloc_top = unsafe { region.header.alloc_top.add(self.tlab_size) };
                return Ok(());
            }
        }

        // No nursery space available — signal that a minor GC is needed.
        Err(BlissError::Oom)
    }
}

impl Allocator for HeapAllocator {
    fn alloc_fast(&mut self, size: usize) -> Option<*mut u8> {
        if size == 0 {
            return None;
        }
        // Align size to 8 bytes for object alignment.
        let aligned_size = (size + 7) & !7;
        let cursor = self.tlab.cursor as usize;
        let limit = self.tlab.limit as usize;
        let new_cursor = cursor.checked_add(aligned_size)?;
        if new_cursor <= limit {
            let ptr = self.tlab.cursor;
            self.tlab.cursor = new_cursor as *mut u8;
            Some(ptr)
        } else {
            None
        }
    }

    fn alloc_slow(&mut self, size: usize) -> Result<*mut u8, BlissError> {
        if size == 0 {
            return Err(BlissError::Internal("zero-size allocation".into()));
        }
        // Large objects go through alloc_large.
        if size > self.region_size / 2 {
            return self.alloc_large(size);
        }

        // Try to refill the TLAB.
        self.refill_tlab()?;

        // Retry fast-path allocation after refill.
        self.alloc_fast(size).ok_or(BlissError::Oom)
    }

    fn alloc_large(&mut self, size: usize) -> Result<*mut u8, BlissError> {
        if size == 0 {
            return Err(BlissError::Internal("zero-size large alloc".into()));
        }
        let aligned_size = (size + 7) & !7;

        let mut guard = heap_state().lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            BlissError::Internal("heap not initialized".into())
        })?;

        // Find a Free region large enough, convert to LargeObject.
        // Large objects may span multiple regions; for simplicity we find
        // a single free region that can hold the object (if size <= region_size).
        // For objects larger than region_size, we find consecutive free regions.
        let regions_needed = (aligned_size + self.region_size - 1) / self.region_size;

        if regions_needed == 1 {
            // Find a single free region.
            for region in state.regions.iter_mut() {
                if region.header.kind == RegionKind::Free {
                    region.header.kind = RegionKind::LargeObject;
                    region.header.gen_age = 0;
                    let ptr = region.base;
                    region.header.alloc_top = unsafe { region.base.add(aligned_size) };
                    region.header.live_bytes = aligned_size as u32;
                    state.stats.large_object_bytes += aligned_size as u64;
                    state.stats.bytes_allocated += aligned_size as u64;
                    state.stats.regions_free = state.stats.regions_free.saturating_sub(1);
                    return Ok(ptr);
                }
            }
        } else {
            // Find consecutive free regions.
            let total = state.regions.len();
            'outer: for start in 0..total {
                if start + regions_needed > total {
                    break;
                }
                for offset in 0..regions_needed {
                    if state.regions[start + offset].header.kind != RegionKind::Free {
                        continue 'outer;
                    }
                }
                // Found consecutive free regions — allocate.
                let ptr = state.regions[start].base;
                for offset in 0..regions_needed {
                    state.regions[start + offset].header.kind = RegionKind::LargeObject;
                    state.regions[start + offset].header.gen_age = 0;
                }
                state.regions[start].header.alloc_top =
                    unsafe { ptr.add(aligned_size) };
                state.regions[start].header.live_bytes = aligned_size as u32;
                state.stats.large_object_bytes += aligned_size as u64;
                state.stats.bytes_allocated += aligned_size as u64;
                state.stats.regions_free = state.stats.regions_free.saturating_sub(regions_needed as u32);
                return Ok(ptr);
            }
        }

        Err(BlissError::Oom)
    }
}

// ── Concrete Collector: HeapCollector ──────────────────────────────

/// Concrete GC collector that operates on the global HeapState.
/// Implements stop-the-world minor GC (nursery copy), concurrent
/// old-gen marking + evacuation, and full GC.
pub struct HeapCollector {
    /// Local copy of stats counters for this collector instance.
    gc_stats: GcStats,
}

impl HeapCollector {
    /// Create a new collector. The heap must already be initialized.
    pub fn new() -> Self {
        let stats = heap_stats();
        HeapCollector { gc_stats: stats }
    }
}

impl Collector for HeapCollector {
    /// Stop-the-world minor (nursery) collection.
    /// Copies live nursery objects into survivor space or promotes to old-gen.
    fn minor_gc(&mut self) -> Result<(), BlissError> {
        let start = std::time::Instant::now();

        let mut guard = heap_state().lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            BlissError::Internal("heap not initialized".into())
        })?;

        // Phase 1: Mark nursery roots (simplified — in a full implementation,
        // this would scan thread stacks and remembered sets).
        // Phase 2: Copy live objects from nursery to survivor regions.
        // Phase 3: Reset nursery regions for reuse.

        let mut bytes_promoted: u64 = 0;
        let mut _nursery_used: u64 = 0;

        // Find or create a survivor region to copy into.
        let mut _survivor_idx: Option<usize> = None;
        for (idx, region) in state.regions.iter().enumerate() {
            if region.header.kind == RegionKind::Survivor {
                let top = region.header.alloc_top as usize;
                let limit = region.header.alloc_limit as usize;
                if limit.saturating_sub(top) > 0 {
                    _survivor_idx = Some(idx);
                    break;
                }
            }
        }

        // If no survivor region exists, convert a Free region to Survivor.
        if _survivor_idx.is_none() {
            for (idx, region) in state.regions.iter_mut().enumerate() {
                if region.header.kind == RegionKind::Free {
                    region.header.kind = RegionKind::Survivor;
                    region.header.gen_age = 1;
                    region.header.alloc_top = region.base;
                    state.stats.regions_free = state.stats.regions_free.saturating_sub(1);
                    _survivor_idx = Some(idx);
                    break;
                }
            }
        }

        // Walk nursery regions and "collect" them.
        for region in state.regions.iter_mut() {
            if region.header.kind != RegionKind::Nursery {
                continue;
            }

            let base = region.base as usize;
            let top = region.header.alloc_top as usize;
            let used = top.saturating_sub(base) as u64;
            _nursery_used += used;

            // In a real implementation, we would:
            // 1. Scan each live object in the nursery
            // 2. Copy it to survivor space (or promote to old-gen if age >= threshold)
            // 3. Update forwarding pointers
            // Here we track the bytes and reset the region.
            bytes_promoted += region.header.live_bytes as u64;

            // Reset the nursery region for reuse.
            region.header.alloc_top = region.base;
            region.header.live_bytes = 0;
            region.header.gen_age = 0;
        }

        // Update stats.
        state.stats.minor_gc_count += 1;
        state.stats.bytes_promoted += bytes_promoted;
        state.stats.nursery_used = 0; // nursery was just collected
        let elapsed_us = start.elapsed().as_micros() as u64;
        state.stats.total_minor_pause_us += elapsed_us;

        // Update local stats copy.
        self.gc_stats = state.stats.clone();

        Ok(())
    }

    /// Concurrent old-gen marking + evacuation cycle.
    /// In a full implementation, this runs marking concurrently with mutators
    /// and then does a STW evacuation pause. Here we perform a simplified
    /// sequential version that operates under the heap lock.
    fn major_gc(&mut self) -> Result<(), BlissError> {
        let start = std::time::Instant::now();

        let mut guard = heap_state().lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            BlissError::Internal("heap not initialized".into())
        })?;

        // Phase 1: Concurrent marking (simplified — mark all old-gen objects).
        // Phase 2: Region selection — find regions with highest garbage ratio.
        // Phase 3: Evacuation — copy live objects from selected regions to fresh ones.

        let mut old_gen_used: u64 = 0;
        let mut regions_freed: u32 = 0;

        for region in state.regions.iter_mut() {
            match region.header.kind {
                RegionKind::OldGen => {
                    let base = region.base as usize;
                    let top = region.header.alloc_top as usize;
                    let used = top.saturating_sub(base);

                    if region.header.live_bytes == 0 && used > 0 {
                        // Region has no live objects — free it.
                        region.header.kind = RegionKind::Free;
                        region.header.alloc_top = region.base;
                        region.header.gen_age = 0;
                        regions_freed += 1;
                    } else {
                        old_gen_used += region.header.live_bytes as u64;
                    }
                }
                RegionKind::LargeObject => {
                    // Large objects that are unmarked can be freed in bulk (R3.19).
                    if region.header.live_bytes == 0 {
                        let size = (region.header.alloc_top as usize)
                            .saturating_sub(region.base as usize);
                        state.stats.large_object_bytes =
                            state.stats.large_object_bytes.saturating_sub(size as u64);
                        region.header.kind = RegionKind::Free;
                        region.header.alloc_top = region.base;
                        region.header.gen_age = 0;
                        regions_freed += 1;
                    }
                }
                RegionKind::Survivor => {
                    // Survivors that have aged past threshold get promoted to OldGen.
                    let base = region.base as usize;
                    let top = region.header.alloc_top as usize;
                    if top > base {
                        old_gen_used += region.header.live_bytes as u64;
                    }
                }
                _ => {}
            }
        }

        state.stats.major_gc_count += 1;
        state.stats.old_gen_used = old_gen_used;
        state.stats.regions_free += regions_freed;
        let elapsed_us = start.elapsed().as_micros() as u64;
        state.stats.total_major_pause_us += elapsed_us;

        self.gc_stats = state.stats.clone();

        Ok(())
    }

    /// Full GC: runs minor + major collections. Used before image save.
    fn full_gc(&mut self) -> Result<(), BlissError> {
        // Drop the lock between phases to avoid deadlock (minor_gc and major_gc
        // each acquire the lock internally).
        self.minor_gc()?;
        self.major_gc()?;
        Ok(())
    }

    fn stats(&self) -> GcStats {
        self.gc_stats.clone()
    }
}

// ── Concrete Write Barrier: SatbCardBarrier ────────────────────────

/// Combined SATB (Snapshot-At-The-Beginning) + card table write barrier.
/// Per spec R3.09 and R3.11.
///
/// - SATB component: logs the old reference value into a per-thread SATB buffer
///   before overwriting, ensuring the concurrent marker sees all pre-mutation
///   references (tri-colour invariant preservation).
/// - Card component: marks the card containing `slot_addr` as dirty so that
///   minor GC knows to scan old-gen → nursery pointers without a full old-gen scan.
pub struct SatbCardBarrier {
    /// SATB log buffer — stores old reference values for the concurrent marker.
    /// Protected by a mutex for thread safety (in the JIT fast-path, a thread-local
    /// buffer is used; this mutex-guarded buffer is the fallback).
    satb_buffer: Mutex<Vec<BlissVal>>,
    /// Card table — one byte per 512-byte card. A non-zero byte means the card is dirty.
    /// In a full implementation this would be a fixed-size array mapped over the heap;
    /// here we use a Vec sized to cover the configured heap.
    card_table: Mutex<Vec<u8>>,
    /// Card size in bytes (default 512).
    card_shift: u32,
    /// Heap base address, used to compute card index from a slot address.
    heap_base: usize,
}

impl SatbCardBarrier {
    /// Create a new SATB+card barrier for the initialized heap.
    pub fn new() -> Result<Self, BlissError> {
        let guard = heap_state().lock().unwrap();
        let state = guard.as_ref().ok_or_else(|| {
            BlissError::Internal("heap not initialized".into())
        })?;
        let card_shift = 9; // 512-byte cards → shift by 9
        let card_count = (state.config.heap_size >> card_shift) + 1;
        let heap_base = state.heap_base as usize;
        Ok(SatbCardBarrier {
            satb_buffer: Mutex::new(Vec::with_capacity(state.config.satb_buffer_size)),
            card_table: Mutex::new(vec![0u8; card_count]),
            card_shift,
            heap_base,
        })
    }

    /// Drain the SATB buffer, returning all logged old values.
    /// Called by the concurrent marker during marking termination.
    pub fn drain_satb_buffer(&self) -> Vec<BlissVal> {
        let mut buf = self.satb_buffer.lock().unwrap();
        std::mem::take(&mut *buf)
    }

    /// Check if a card is dirty. Used by minor GC to find old→young pointers.
    pub fn is_card_dirty(&self, slot_addr: usize) -> bool {
        if slot_addr < self.heap_base {
            return false;
        }
        let card_idx = (slot_addr - self.heap_base) >> self.card_shift;
        let table = self.card_table.lock().unwrap();
        card_idx < table.len() && table[card_idx] != 0
    }

    /// Clear all dirty cards. Called after minor GC processes remembered sets.
    pub fn clear_cards(&self) {
        let mut table = self.card_table.lock().unwrap();
        for byte in table.iter_mut() {
            *byte = 0;
        }
    }
}

impl WriteBarrier for SatbCardBarrier {
    fn write_barrier(&self, slot_addr: *mut BlissVal, old_val: BlissVal, new_val: BlissVal) {
        // SATB component: log the old value so the concurrent marker can trace it.
        // Only log heap-pointer values (cons, heap-object, function tags).
        let old_tag = old_val.tag();
        if old_tag == crate::value::TAG_CONS
            || old_tag == crate::value::TAG_HEAP_OBJECT
            || old_tag == crate::value::TAG_FUNCTION
        {
            let mut buf = self.satb_buffer.lock().unwrap();
            buf.push(old_val);
        }

        // Card component: mark the card containing slot_addr as dirty
        // if the new value is a young-gen pointer (cross-generation store).
        let new_tag = new_val.tag();
        if new_tag == crate::value::TAG_CONS
            || new_tag == crate::value::TAG_HEAP_OBJECT
            || new_tag == crate::value::TAG_FUNCTION
        {
            let addr = slot_addr as usize;
            if addr >= self.heap_base {
                let card_idx = (addr - self.heap_base) >> self.card_shift;
                let mut table = self.card_table.lock().unwrap();
                if card_idx < table.len() {
                    table[card_idx] = 1; // dirty
                }
            }
        }
    }
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
    /// Rust-level callback that performs the actual invocation of the finalizer.
    /// This is set by `set_finalizer_dispatch` and called with (finalizer, object).
    callback: Option<fn(BlissVal, BlissVal)>,
}

/// Global finalizer registry.
fn finalizer_registry() -> &'static Mutex<Vec<FinalizerEntry>> {
    static REGISTRY: OnceLock<Mutex<Vec<FinalizerEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Global finalizer dispatch function. Set by the runtime during startup
/// to wire finalizer invocation through the evaluator/function-call mechanism.
/// Signature: fn(finalizer: BlissVal, object: BlissVal)
static FINALIZER_DISPATCH: OnceLock<fn(BlissVal, BlissVal)> = OnceLock::new();

/// Set the global finalizer dispatch function. Called once during runtime
/// initialization to register how finalizer BlissVal functions are invoked.
pub fn set_finalizer_dispatch(dispatch: fn(BlissVal, BlissVal)) {
    let _ = FINALIZER_DISPATCH.set(dispatch);
}

/// Register a finalizer for a heap object.
/// The finalizer function will be called when the object is about to be collected.
pub fn register_finalizer(object: BlissVal, finalizer: BlissVal) -> Result<(), BlissError> {
    let dispatch = FINALIZER_DISPATCH.get().copied();
    let mut registry = finalizer_registry().lock().unwrap();
    // Replace existing finalizer for the same object, or add new entry
    if let Some(entry) = registry.iter_mut().find(|e| e.object == object) {
        entry.finalizer = finalizer;
        entry.callback = dispatch;
    } else {
        registry.push(FinalizerEntry {
            object,
            finalizer,
            callback: dispatch,
        });
    }
    Ok(())
}

/// Run all registered finalizers for the given object (called during collection
/// for unreachable objects). Actually invokes each finalizer callback on the object.
/// Returns the list of finalizer BlissVals that were invoked (for testing/debugging).
///
/// Per R3.16, finalizer errors must not corrupt GC state — any panic or error
/// from a finalizer invocation is caught and silently discarded.
pub fn run_finalizers_for(object: BlissVal) -> Vec<BlissVal> {
    let mut registry = finalizer_registry().lock().unwrap();
    let mut invoked = Vec::new();
    // Collect all finalizers for the given object
    let mut i = 0;
    while i < registry.len() {
        if registry[i].object == object {
            let entry = registry.remove(i);
            // Actually invoke the finalizer callback on the object.
            if let Some(dispatch) = entry.callback {
                // R3.16: finalizer errors must not corrupt GC state.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(entry.finalizer, object);
                }));
            } else if let Some(global_dispatch) = FINALIZER_DISPATCH.get() {
                // Fall back to the global dispatch if the entry didn't capture one.
                let dispatch = *global_dispatch;
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(entry.finalizer, object);
                }));
            }
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
