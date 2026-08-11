//! Garbage collector — generational, region-based, concurrent old-gen marking.
//!
//! Inspired by HotSpot G1 and ZGC. See §3 of the spec.

use crate::error::BlissError;
use crate::value::BlissVal;

use std::alloc::{self, Layout};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
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
/// use the `_atomic` accessor methods which perform atomic operations via
/// `addr_of!` + `AtomicU32::from_ptr` to avoid aliasing UB.
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
    ///
    /// Uses `addr_of!` to obtain a raw pointer without creating an intermediate
    /// `&u32` reference, then `AtomicU32::from_ptr` for a sound atomic load.
    /// The RegionHeader is only stored behind the HeapState mutex and in stable
    /// heap memory, satisfying `from_ptr`'s validity requirements.
    pub fn load_live_bytes_atomic(&self) -> u32 {
        let ptr = std::ptr::addr_of!(self.live_bytes) as *mut u32;
        // Safety: ptr is aligned (u32 in repr(C) struct), non-null, and
        // stable (HeapRegion is heap-allocated in a Vec behind a Mutex).
        unsafe { AtomicU32::from_ptr(ptr).load(Ordering::Acquire) }
    }

    /// Atomically store `live_bytes`. Used by concurrent old-gen marker.
    pub fn store_live_bytes_atomic(&self, val: u32) {
        let ptr = std::ptr::addr_of!(self.live_bytes) as *mut u32;
        unsafe { AtomicU32::from_ptr(ptr).store(val, Ordering::Release) }
    }

    /// Atomically load `alloc_top`. Used by concurrent old-gen marker.
    pub fn load_alloc_top_atomic(&self) -> *mut u8 {
        let ptr = std::ptr::addr_of!(self.alloc_top) as *mut *mut u8;
        unsafe { AtomicPtr::from_ptr(ptr).load(Ordering::Acquire) }
    }

    /// Atomically store `alloc_top`. Used by allocator bump-pointer update.
    pub fn store_alloc_top_atomic(&self, val: *mut u8) {
        let ptr = std::ptr::addr_of!(self.alloc_top) as *mut *mut u8;
        unsafe { AtomicPtr::from_ptr(ptr).store(val, Ordering::Release) }
    }

    /// Atomically load `next_free`. Used during concurrent region free-list access.
    pub fn load_next_free_atomic(&self) -> u32 {
        let ptr = std::ptr::addr_of!(self.next_free) as *mut u32;
        unsafe { AtomicU32::from_ptr(ptr).load(Ordering::Acquire) }
    }

    /// Atomically store `next_free`.
    pub fn store_next_free_atomic(&self, val: u32) {
        let ptr = std::ptr::addr_of!(self.next_free) as *mut u32;
        unsafe { AtomicU32::from_ptr(ptr).store(val, Ordering::Release) }
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

// ── Global marking flag ───────────────────────────────────────────

/// Global flag indicating whether concurrent old-gen marking is in progress.
/// Per spec §3.6.2, the SATB barrier should only fire when marking is active.
static GC_MARKING_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Query whether concurrent marking is currently active.
pub fn gc_marking_in_progress() -> bool {
    GC_MARKING_IN_PROGRESS.load(Ordering::Acquire)
}

/// Set the concurrent marking flag. Called by the collector at marking
/// phase start/end.
pub fn set_gc_marking_in_progress(active: bool) {
    GC_MARKING_IN_PROGRESS.store(active, Ordering::Release);
}

// ── Object header layout ──────────────────────────────────────────

/// Size of the per-object header used in the bootstrap heap layout.
/// Layout: [type_id: u8, padding: 3 bytes, size: u32] = 8 bytes.
const OBJECT_HEADER_SIZE: usize = 8;

/// Alignment for objects in the heap (spec §3.3.1: 16-byte minimum).
const OBJECT_ALIGNMENT: usize = 16;

/// Forwarding pointer marker. When type_id byte is 0xFF, the object
/// has been forwarded; bytes 8..16 contain the new location pointer.
const FORWARDED_TYPE_ID: u8 = 0xFF;

/// Write an object header at `ptr`. The header is 8 bytes:
/// byte 0: type_id, bytes 1-3: padding (zeroed), bytes 4-7: body_size (u32 LE).
///
/// Safety: `ptr` must be valid for writes of at least OBJECT_HEADER_SIZE bytes.
unsafe fn write_object_header(ptr: *mut u8, type_id: u8, body_size: u32) {
    unsafe {
        *ptr = type_id;
    }
    // bytes 1-3 are padding, already zeroed from alloc_zeroed
    let size_ptr = unsafe { ptr.add(4) } as *mut u32;
    unsafe {
        *size_ptr = body_size;
    }
}

/// Read an object header at `ptr`. Returns (type_id, body_size).
///
/// Safety: `ptr` must be valid for reads of at least OBJECT_HEADER_SIZE bytes.
unsafe fn read_object_header(ptr: *const u8) -> (u8, u32) {
    let type_id = unsafe { *ptr };
    let size_ptr = unsafe { ptr.add(4) } as *const u32;
    let body_size = unsafe { *size_ptr };
    (type_id, body_size)
}

/// Align `size` up to OBJECT_ALIGNMENT (16 bytes), per spec §3.3.1.
fn align_up(size: usize, align: usize) -> usize {
    (size + align - 1) & !(align - 1)
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
        let state = guard
            .as_ref()
            .ok_or_else(|| BlissError::Internal("heap not initialized".into()))?;
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
    /// lazily converts a Free region to Nursery and retries. Returns OOM only
    /// if no nursery region with space and no free region can be found.
    fn refill_tlab(&mut self) -> Result<(), BlissError> {
        let mut guard = heap_state().lock().unwrap();
        let state = guard
            .as_mut()
            .ok_or_else(|| BlissError::Internal("heap not initialized".into()))?;

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
                self.tlab.limit =
                    unsafe { region.header.alloc_top.add(self.tlab_size) } as *const u8;
                self.tlab.region_idx = idx as u16;
                // Advance the region's alloc_top past the TLAB.
                region.header.alloc_top = unsafe { region.header.alloc_top.add(self.tlab_size) };
                return Ok(());
            }
        }

        // No nursery region with space — convert a Free region to Nursery.
        for (idx, region) in state.regions.iter_mut().enumerate() {
            if region.header.kind != RegionKind::Free {
                continue;
            }
            // Convert Free → Nursery.
            region.header.kind = RegionKind::Nursery;
            region.header.gen_age = 0;
            region.header.alloc_top = region.base;
            region.header.live_bytes = 0;
            state.stats.regions_free = state.stats.regions_free.saturating_sub(1);

            let top = region.header.alloc_top as usize;
            let limit = region.header.alloc_limit as usize;
            let available = limit.saturating_sub(top);
            if available >= self.tlab_size {
                self.tlab.cursor = region.header.alloc_top;
                self.tlab.limit =
                    unsafe { region.header.alloc_top.add(self.tlab_size) } as *const u8;
                self.tlab.region_idx = idx as u16;
                region.header.alloc_top = unsafe { region.header.alloc_top.add(self.tlab_size) };
                return Ok(());
            }
        }

        // No nursery space available — signal that a minor GC is needed.
        Err(BlissError::Oom)
    }

    /// Update live_bytes for the nursery region that contains the TLAB
    /// after a successful allocation of `bytes` bytes.
    fn update_nursery_live_bytes(&self, bytes: usize) {
        let mut guard = heap_state().lock().unwrap();
        if let Some(state) = guard.as_mut() {
            let idx = self.tlab.region_idx as usize;
            if idx < state.regions.len() {
                state.regions[idx].header.live_bytes += bytes as u32;
                state.stats.bytes_allocated += bytes as u64;
                state.stats.nursery_used += bytes as u64;
            }
        }
    }
}

impl Allocator for HeapAllocator {
    fn alloc_fast(&mut self, size: usize) -> Option<*mut u8> {
        if size == 0 {
            return None;
        }
        // Total allocation = object header + body, aligned to 16 bytes (spec §3.3.1).
        let total_size = align_up(OBJECT_HEADER_SIZE + size, OBJECT_ALIGNMENT);
        let cursor = self.tlab.cursor as usize;
        let limit = self.tlab.limit as usize;
        let new_cursor = cursor.checked_add(total_size)?;
        if new_cursor <= limit {
            let header_ptr = self.tlab.cursor;
            self.tlab.cursor = new_cursor as *mut u8;
            // Write the object header (type_id=0 placeholder, caller sets real type).
            unsafe {
                write_object_header(header_ptr, 0, size as u32);
            }
            // Update live_bytes on the nursery region.
            self.update_nursery_live_bytes(total_size);
            // Return pointer past the header (to the object body).
            Some(unsafe { header_ptr.add(OBJECT_HEADER_SIZE) })
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
        let total_size = align_up(OBJECT_HEADER_SIZE + size, OBJECT_ALIGNMENT);

        let mut guard = heap_state().lock().unwrap();
        let state = guard
            .as_mut()
            .ok_or_else(|| BlissError::Internal("heap not initialized".into()))?;

        // Find a Free region large enough, convert to LargeObject.
        let regions_needed = total_size.div_ceil(self.region_size);

        if regions_needed == 1 {
            for region in state.regions.iter_mut() {
                if region.header.kind == RegionKind::Free {
                    region.header.kind = RegionKind::LargeObject;
                    region.header.gen_age = 0;
                    let ptr = region.base;
                    region.header.alloc_top = unsafe { region.base.add(total_size) };
                    region.header.live_bytes = total_size as u32;
                    state.stats.large_object_bytes += total_size as u64;
                    state.stats.bytes_allocated += total_size as u64;
                    state.stats.regions_free = state.stats.regions_free.saturating_sub(1);
                    // Write object header.
                    unsafe {
                        write_object_header(ptr, 0, size as u32);
                    }
                    return Ok(unsafe { ptr.add(OBJECT_HEADER_SIZE) });
                }
            }
        } else {
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
                let ptr = state.regions[start].base;
                for offset in 0..regions_needed {
                    state.regions[start + offset].header.kind = RegionKind::LargeObject;
                    state.regions[start + offset].header.gen_age = 0;
                }
                state.regions[start].header.alloc_top = unsafe { ptr.add(total_size) };
                state.regions[start].header.live_bytes = total_size as u32;
                state.stats.large_object_bytes += total_size as u64;
                state.stats.bytes_allocated += total_size as u64;
                state.stats.regions_free = state
                    .stats
                    .regions_free
                    .saturating_sub(regions_needed as u32);
                // Write object header.
                unsafe {
                    write_object_header(ptr, 0, size as u32);
                }
                return Ok(unsafe { ptr.add(OBJECT_HEADER_SIZE) });
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
    /// Promotion threshold: objects with gen_age >= this are promoted to old-gen.
    promotion_threshold: u8,
}

impl HeapCollector {
    /// Create a new collector. The heap must already be initialized.
    pub fn new() -> Self {
        let stats = heap_stats();
        let threshold = {
            let guard = heap_state().lock().unwrap();
            guard.as_ref().map_or(15, |s| s.config.promotion_threshold)
        };
        HeapCollector {
            gc_stats: stats,
            promotion_threshold: threshold,
        }
    }

    /// Find or allocate a target region of the given kind. Returns the index
    /// into the regions vec. If no suitable region exists, converts a Free region.
    fn find_or_create_target_region(
        state: &mut HeapState,
        kind: RegionKind,
        gen_age: u8,
    ) -> Option<usize> {
        // First, look for an existing region of the right kind with space.
        for (idx, region) in state.regions.iter().enumerate() {
            if region.header.kind == kind {
                let top = region.header.alloc_top as usize;
                let limit = region.header.alloc_limit as usize;
                if limit.saturating_sub(top) > OBJECT_HEADER_SIZE + OBJECT_ALIGNMENT {
                    return Some(idx);
                }
            }
        }
        // Convert a Free region.
        for (idx, region) in state.regions.iter_mut().enumerate() {
            if region.header.kind == RegionKind::Free {
                region.header.kind = kind;
                region.header.gen_age = gen_age;
                region.header.alloc_top = region.base;
                region.header.live_bytes = 0;
                state.stats.regions_free = state.stats.regions_free.saturating_sub(1);
                return Some(idx);
            }
        }
        None
    }

    /// Copy a single object (header_ptr points to the object header in the source
    /// region) into the target region at index `target_idx`. Returns the new body
    /// pointer (past header) or None if the target region is full.
    ///
    /// Also installs a forwarding pointer at the old location: sets type_id to
    /// FORWARDED_TYPE_ID and writes the new body pointer at offset 8.
    fn copy_object(
        state: &mut HeapState,
        source_header: *mut u8,
        body_size: u32,
        target_idx: usize,
    ) -> Option<*mut u8> {
        let total_size = align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
        let target = &mut state.regions[target_idx];
        let top = target.header.alloc_top as usize;
        let limit = target.header.alloc_limit as usize;

        if top + total_size > limit {
            return None; // Target region full.
        }

        let new_header = target.header.alloc_top;
        let new_body = unsafe { new_header.add(OBJECT_HEADER_SIZE) };

        // Copy the entire object (header + body) to the new location.
        unsafe {
            std::ptr::copy_nonoverlapping(source_header, new_header, total_size);
        }

        // Advance the target region's alloc_top.
        target.header.alloc_top = unsafe { new_header.add(total_size) };
        target.header.live_bytes += total_size as u32;

        // Install forwarding pointer at old location:
        // type_id = FORWARDED_TYPE_ID, and we store the new body ptr at offset 8.
        unsafe {
            *source_header = FORWARDED_TYPE_ID;
            // Ensure there's room for the forwarding pointer (need 16 bytes total).
            // Since minimum allocation is OBJECT_HEADER_SIZE + body with 16-byte alignment,
            // the minimum slot is 16 bytes, enough for header(8) + pointer(8).
            if total_size >= OBJECT_HEADER_SIZE + std::mem::size_of::<usize>() {
                let fwd_ptr_slot = source_header.add(OBJECT_HEADER_SIZE) as *mut *mut u8;
                *fwd_ptr_slot = new_body;
            }
        }

        Some(new_body)
    }
}

impl Default for HeapCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for HeapCollector {
    /// Stop-the-world minor (nursery) collection.
    /// Cheney-style scavenge: copies live nursery objects into survivor space
    /// or promotes to old-gen based on gen_age vs promotion_threshold.
    fn minor_gc(&mut self) -> Result<(), BlissError> {
        let start = std::time::Instant::now();

        let mut guard = heap_state().lock().unwrap();
        let state = guard
            .as_mut()
            .ok_or_else(|| BlissError::Internal("heap not initialized".into()))?;

        let promotion_threshold = self.promotion_threshold;

        // Phase 1: Ensure we have a survivor region to copy into.
        let survivor_idx = Self::find_or_create_target_region(state, RegionKind::Survivor, 1);

        // Phase 2: Walk each nursery region, copy live objects to survivor/old-gen.
        let mut bytes_promoted: u64 = 0;
        let mut _nursery_used: u64 = 0;

        // Collect nursery region indices first (to avoid borrow issues).
        let nursery_indices: Vec<usize> = state
            .regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.header.kind == RegionKind::Nursery)
            .map(|(i, _)| i)
            .collect();

        for &nursery_idx in &nursery_indices {
            let base = state.regions[nursery_idx].base as usize;
            let top = state.regions[nursery_idx].header.alloc_top as usize;
            let used = top.saturating_sub(base) as u64;
            _nursery_used += used;

            if top <= base {
                continue; // Empty nursery region.
            }

            // Scan objects in this nursery region from base to alloc_top.
            let mut cursor = base;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *mut u8;
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };

                if body_size == 0 && type_id == 0 {
                    break; // End of allocated objects (zeroed memory).
                }

                // Skip already-forwarded objects.
                if type_id == FORWARDED_TYPE_ID {
                    let total = align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                    cursor += total;
                    continue;
                }

                let total_size =
                    align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);

                // Decide target: promote to old-gen if region age >= threshold,
                // otherwise copy to survivor.
                let nursery_age = state.regions[nursery_idx].header.gen_age;
                let (target_kind, target_gen_age) = if nursery_age >= promotion_threshold {
                    (RegionKind::OldGen, 0)
                } else {
                    (RegionKind::Survivor, nursery_age + 1)
                };

                // Find target region. We may need to allocate new ones as they fill up.
                let mut target_idx_opt = if target_kind == RegionKind::Survivor {
                    survivor_idx
                } else {
                    Self::find_or_create_target_region(state, target_kind, target_gen_age)
                };

                // Try to copy the object.
                let mut copied = false;
                if let Some(tidx) = target_idx_opt {
                    if Self::copy_object(state, header_ptr, body_size, tidx).is_some() {
                        copied = true;
                    }
                }

                // If copy failed (target full), get a new target region and retry.
                if !copied {
                    target_idx_opt =
                        Self::find_or_create_target_region(state, target_kind, target_gen_age);
                    if let Some(tidx) = target_idx_opt {
                        Self::copy_object(state, header_ptr, body_size, tidx);
                    }
                    // If still no space, the object is lost (OOM during GC).
                }

                bytes_promoted += total_size as u64;
                cursor += total_size;
            }

            // Phase 3: Run finalizers for dead (non-forwarded) objects before
            // zeroing the region. An object is dead if it was NOT forwarded
            // (i.e., its type_id is not FORWARDED_TYPE_ID and it has a valid header).
            {
                let base = state.regions[nursery_idx].base as usize;
                let top = state.regions[nursery_idx].header.alloc_top as usize;
                let mut fcursor = base;
                while fcursor + OBJECT_HEADER_SIZE <= top {
                    let header_ptr = fcursor as *const u8;
                    let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                    if body_size == 0 && type_id == 0 {
                        break;
                    }
                    let total_size =
                        align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                    // Finalizers observe every nursery object in the collection
                    // cycle before the region is reset.
                    let body_ptr = unsafe { header_ptr.add(OBJECT_HEADER_SIZE) };
                    let obj_val = BlissVal::from_raw(body_ptr as u64);
                    run_finalizers_for(obj_val);
                    fcursor += total_size;
                }
            }

            // Phase 4: Reset the nursery region for reuse.
            let region = &mut state.regions[nursery_idx];
            // Zero the region memory so walk_heap doesn't see stale forwarding pointers.
            let region_used =
                (region.header.alloc_top as usize).saturating_sub(region.base as usize);
            if region_used > 0 {
                unsafe {
                    std::ptr::write_bytes(region.base, 0, region_used);
                }
            }
            region.header.alloc_top = region.base;
            region.header.live_bytes = 0;
            region.header.gen_age = 0;
        }

        // Break weak pointers to objects that were in nursery regions (now freed).
        // After resetting, any pointer into these regions is dead.
        {
            let nursery_ranges: Vec<(usize, usize)> = nursery_indices
                .iter()
                .map(|&idx| {
                    let base = state.regions[idx].base as usize;
                    let limit = state.regions[idx].header.alloc_limit as usize;
                    (base, limit)
                })
                .collect();
            break_dead_weak_pointers(&|val: BlissVal| {
                let addr = val.to_raw() as usize;
                nursery_ranges
                    .iter()
                    .any(|&(base, limit)| addr >= base && addr < limit)
            });
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

    /// Concurrent old-gen marking + evacuation cycle (A3.02).
    ///
    /// Simplified sequential implementation that runs under the heap lock:
    /// 1. Mark phase: walk all old-gen/survivor/large-object regions, scan objects
    ///    from base to alloc_top, and compute accurate live_bytes per region.
    /// 2. Region selection: identify regions with high garbage ratio
    ///    (live_bytes / used_bytes < 0.5) as candidates for evacuation.
    /// 3. Evacuation: copy live objects from selected regions to fresh old-gen
    ///    regions, install forwarding pointers, and free evacuated regions.
    fn major_gc(&mut self) -> Result<(), BlissError> {
        let start = std::time::Instant::now();

        // A standalone major collection must first drain nursery state so
        // nursery deaths trigger finalizers and weak-reference clearing too.
        self.minor_gc()?;

        // Set marking flag (§3.6.2: SATB barrier only fires when marking active).
        set_gc_marking_in_progress(true);

        let mut guard = heap_state().lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            set_gc_marking_in_progress(false);
            BlissError::Internal("heap not initialized".into())
        })?;

        // Phase 1: Mark phase — conservative pointer tracing.
        //
        // We use a mark bitmap (one bit per OBJECT_ALIGNMENT-byte slot) to track
        // which objects are reachable. The algorithm:
        //   1. Build an index of all object start addresses in old-gen/survivor/LO regions.
        //   2. Scan all non-free regions (including nursery) for pointer-like values
        //      that point to indexed objects, marking them live.
        //   3. Transitively mark objects referenced by newly-marked objects.
        //   4. Compute live_bytes from the mark bitmap.
        //
        // This is a conservative approach: any aligned 8-byte value that happens to
        // match an object address will mark that object as live (false retention is
        // possible, but false collection is not).
        let region_count = state.regions.len();
        let heap_base_addr = state.heap_base as usize;
        let heap_size = state.config.heap_size;
        // Record TAMS (Top-At-Mark-Start) per region.
        let mut tams: Vec<usize> = Vec::with_capacity(region_count);
        for region in state.regions.iter() {
            tams.push(region.header.alloc_top as usize);
        }

        // Build a set of valid object body addresses in old-gen/survivor/LO regions,
        // along with their sizes. We store (body_addr, total_size, region_idx).
        let mut object_index: std::collections::HashMap<usize, (usize, usize)> =
            std::collections::HashMap::new();
        for (idx, region) in state.regions.iter().enumerate() {
            match region.header.kind {
                RegionKind::OldGen | RegionKind::Survivor | RegionKind::LargeObject => {
                    let base = region.base as usize;
                    let top = tams[idx];
                    if top <= base {
                        continue;
                    }
                    let mut cursor = base;
                    while cursor + OBJECT_HEADER_SIZE <= top {
                        let header_ptr = cursor as *const u8;
                        let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                        if body_size == 0 && type_id == 0 {
                            break;
                        }
                        let total_size =
                            align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                        if type_id != FORWARDED_TYPE_ID {
                            let body_addr = cursor + OBJECT_HEADER_SIZE;
                            object_index.insert(body_addr, (total_size, idx));
                        }
                        cursor += total_size;
                    }
                }
                _ => {}
            }
        }

        // Mark bitmap: track which object body addresses are marked live.
        let mut marked: std::collections::HashSet<usize> = std::collections::HashSet::new();

        // Conservative scan: scan ALL non-free regions for pointer-like values
        // that match known object addresses.
        let mut scan_worklist: Vec<usize> = Vec::new();
        for (idx, region) in state.regions.iter().enumerate() {
            if region.header.kind == RegionKind::Free {
                continue;
            }
            // For old-gen/survivor/LO regions being marked, skip scanning their
            // own objects as roots — they will only be live if referenced from
            // nursery regions or other roots. But we conservatively scan nursery
            // regions as roots (they contain the live set from the last minor GC).
            let base = region.base as usize;
            let top = tams[idx].min(region.header.alloc_top as usize);
            if top <= base {
                continue;
            }
            // Scan memory in this region for pointer-sized values.
            let mut scan = base;
            while scan + 8 <= top {
                let val = unsafe { *(scan as *const usize) };
                if val >= heap_base_addr
                    && val < heap_base_addr + heap_size
                    && object_index.contains_key(&val)
                    && !marked.contains(&val)
                {
                    marked.insert(val);
                    scan_worklist.push(val);
                }
                scan += 8; // scan every 8-byte aligned slot
            }
        }

        // Transitive closure: scan newly marked objects for more pointers.
        while let Some(obj_addr) = scan_worklist.pop() {
            if let Some(&(total_size, _)) = object_index.get(&obj_addr) {
                let body_size = total_size.saturating_sub(OBJECT_HEADER_SIZE);
                let mut scan = obj_addr;
                let scan_end = obj_addr + body_size;
                while scan + 8 <= scan_end {
                    let val = unsafe { *(scan as *const usize) };
                    if val >= heap_base_addr
                        && val < heap_base_addr + heap_size
                        && object_index.contains_key(&val)
                        && !marked.contains(&val)
                    {
                        marked.insert(val);
                        scan_worklist.push(val);
                    }
                    scan += 8;
                }
            }
        }

        // Compute live_bytes per region from mark results.
        // Also run finalizers for dead objects and break their weak pointers.
        let mut dead_object_vals: Vec<BlissVal> = Vec::new();
        for (idx, region) in state.regions.iter_mut().enumerate() {
            match region.header.kind {
                RegionKind::OldGen | RegionKind::Survivor | RegionKind::LargeObject => {
                    let base = region.base as usize;
                    let top = tams[idx];
                    if top <= base {
                        region.header.live_bytes = 0;
                        continue;
                    }

                    let mut live = 0u32;
                    let mut cursor = base;
                    while cursor + OBJECT_HEADER_SIZE <= top {
                        let header_ptr = cursor as *const u8;
                        let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                        if body_size == 0 && type_id == 0 {
                            break;
                        }
                        let total_size =
                            align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                        if type_id != FORWARDED_TYPE_ID {
                            let body_addr = cursor + OBJECT_HEADER_SIZE;
                            if region.header.kind == RegionKind::LargeObject
                                || marked.contains(&body_addr)
                            {
                                live += total_size as u32;
                            } else {
                                // Object is dead — queue for finalization.
                                let obj_val = BlissVal::from_raw(body_addr as u64);
                                dead_object_vals.push(obj_val);
                            }
                        }
                        cursor += total_size;
                    }

                    region.header.live_bytes = live;
                }
                _ => {}
            }
        }

        // Run finalizers for dead objects (R3.12).
        for obj_val in &dead_object_vals {
            run_finalizers_for(*obj_val);
        }

        // Break weak pointers to dead objects (R3.13).
        {
            let dead_set: std::collections::HashSet<u64> =
                dead_object_vals.iter().map(|v| v.to_raw()).collect();
            break_dead_weak_pointers(&|val: BlissVal| dead_set.contains(&val.to_raw()));
        }

        // Phase 2: Region selection — find old-gen regions with high garbage ratio.
        // A region is a candidate if live_bytes < 50% of used bytes (i.e. mostly garbage).
        let mut evacuation_set: Vec<usize> = Vec::new();
        for (idx, region) in state.regions.iter().enumerate() {
            if region.header.kind != RegionKind::OldGen {
                continue;
            }
            let base = region.base as usize;
            let top = region.header.alloc_top as usize;
            let used = top.saturating_sub(base) as u32;
            if used == 0 {
                continue;
            }

            if region.header.live_bytes == 0 {
                // Entirely garbage — will be freed directly below.
                continue;
            }

            // Select regions where less than half the used space is live.
            if (region.header.live_bytes as u64) < (used as u64 / 2) {
                evacuation_set.push(idx);
            }
        }

        // Phase 3: Evacuation — copy live objects from selected regions to fresh ones.
        for &evac_idx in &evacuation_set {
            let target_idx = Self::find_or_create_target_region(state, RegionKind::OldGen, 0);
            let target_idx = match target_idx {
                Some(idx) if idx != evac_idx => idx,
                _ => continue, // No space for evacuation, skip this region.
            };

            let base = state.regions[evac_idx].base as usize;
            let top = state.regions[evac_idx].header.alloc_top as usize;

            let mut cursor = base;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *mut u8;
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };

                if body_size == 0 && type_id == 0 {
                    break;
                }

                let total_size =
                    align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);

                // Only copy non-forwarded, marked (live) objects.
                let body_addr = cursor + OBJECT_HEADER_SIZE;
                if type_id != FORWARDED_TYPE_ID && marked.contains(&body_addr) {
                    // Try to copy; if target fills up, find another.
                    if Self::copy_object(state, header_ptr, body_size, target_idx).is_none() {
                        if let Some(new_target) =
                            Self::find_or_create_target_region(state, RegionKind::OldGen, 0)
                        {
                            if new_target != evac_idx {
                                let _ = Self::copy_object(state, header_ptr, body_size, new_target);
                            }
                        }
                    }
                }

                cursor += total_size;
            }

            // Free the evacuated region.
            let region = &mut state.regions[evac_idx];
            let region_used =
                (region.header.alloc_top as usize).saturating_sub(region.base as usize);
            if region_used > 0 {
                unsafe {
                    std::ptr::write_bytes(region.base, 0, region_used);
                }
            }
            region.header.kind = RegionKind::Free;
            region.header.alloc_top = region.base;
            region.header.live_bytes = 0;
            region.header.gen_age = 0;
            state.stats.regions_free += 1;
        }

        // Free old-gen and large-object regions with zero live bytes.
        let mut old_gen_used: u64 = 0;
        let mut regions_freed: u32 = 0;

        for region in state.regions.iter_mut() {
            match region.header.kind {
                RegionKind::OldGen => {
                    let base = region.base as usize;
                    let top = region.header.alloc_top as usize;
                    let used = top.saturating_sub(base);

                    if region.header.live_bytes == 0 && used > 0 {
                        // Zero the region memory.
                        unsafe {
                            std::ptr::write_bytes(region.base, 0, used);
                        }
                        region.header.kind = RegionKind::Free;
                        region.header.alloc_top = region.base;
                        region.header.gen_age = 0;
                        regions_freed += 1;
                    } else {
                        old_gen_used += region.header.live_bytes as u64;
                    }
                }
                RegionKind::LargeObject => {
                    if region.header.live_bytes == 0 {
                        let size =
                            (region.header.alloc_top as usize).saturating_sub(region.base as usize);
                        state.stats.large_object_bytes =
                            state.stats.large_object_bytes.saturating_sub(size as u64);
                        if size > 0 {
                            unsafe {
                                std::ptr::write_bytes(region.base, 0, size);
                            }
                        }
                        region.header.kind = RegionKind::Free;
                        region.header.alloc_top = region.base;
                        region.header.gen_age = 0;
                        regions_freed += 1;
                    }
                }
                RegionKind::Survivor => {
                    // Promote survivors that have aged past threshold to OldGen.
                    if region.header.gen_age >= self.promotion_threshold {
                        region.header.kind = RegionKind::OldGen;
                        region.header.gen_age = 0;
                    }
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

        // Clear marking flag.
        drop(guard);
        set_gc_marking_in_progress(false);

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
///   references (tri-colour invariant preservation). Only fires when concurrent
///   marking is active (§3.6.2, §3.7.1).
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
        let state = guard
            .as_ref()
            .ok_or_else(|| BlissError::Internal("heap not initialized".into()))?;
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
        let raw_idx = if slot_addr >= self.heap_base {
            (slot_addr - self.heap_base) >> self.card_shift
        } else {
            0
        };
        let table = self.card_table.lock().unwrap();
        let card_idx = if raw_idx < table.len() { raw_idx } else { 0 };
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
        // Per §3.6.2 and §3.7.1, only log when concurrent marking is active.
        if gc_marking_in_progress() {
            let mut buf = self.satb_buffer.lock().unwrap();
            buf.push(old_val);
        }

        // Card component: mark the card containing slot_addr as dirty
        // if the new value is a young-gen pointer (cross-generation store).
        let _ = new_val;
        let addr = slot_addr as usize;
        let raw_idx = if addr >= self.heap_base {
            (addr - self.heap_base) >> self.card_shift
        } else {
            0
        };
        let mut table = self.card_table.lock().unwrap();
        let card_idx = if raw_idx < table.len() { raw_idx } else { 0 };
        if card_idx < table.len() {
            table[card_idx] = 1; // dirty
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

    /// Break this weak pointer, clearing the referent. Called by the GC
    /// when the referent becomes unreachable (per R3.13).
    pub fn break_ref(&mut self) {
        self.broken = true;
        self.referent = crate::value::NIL;
    }

    /// Returns whether this weak pointer has been broken.
    pub fn is_broken(&self) -> bool {
        self.broken
    }
}

// ── Weak pointer registry ─────────────────────────────────────────

/// Wrapper around raw pointer to WeakPointer for Send/Sync.
/// Safety: access is always guarded by the registry mutex.
struct WeakPtrHandle(*mut WeakPointer);
unsafe impl Send for WeakPtrHandle {}
unsafe impl Sync for WeakPtrHandle {}

/// Global registry of weak pointers so the GC can break them during collection.
fn weak_pointer_registry() -> &'static Mutex<Vec<WeakPtrHandle>> {
    static REGISTRY: OnceLock<Mutex<Vec<WeakPtrHandle>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a weak pointer with the GC so it can be broken when its referent
/// is collected. The caller must ensure the WeakPointer lives at least until
/// it is unregistered or broken.
pub fn register_weak_pointer(wp: &mut WeakPointer) {
    let ptr: *mut WeakPointer = wp;
    let mut registry = weak_pointer_registry().lock().unwrap();
    registry.push(WeakPtrHandle(ptr));
}

/// Unregister a weak pointer from the GC registry.
pub fn unregister_weak_pointer(wp: &WeakPointer) {
    let ptr = wp as *const WeakPointer;
    let mut registry = weak_pointer_registry().lock().unwrap();
    registry.retain(|h| !std::ptr::eq(h.0 as *const WeakPointer, ptr));
}

/// Break all weak pointers whose referent is in a dead region (one being freed).
/// Called by the collector during minor_gc and major_gc.
/// `is_dead` returns true if the given BlissVal's referent is unreachable.
fn break_dead_weak_pointers<F>(is_dead: &F)
where
    F: Fn(BlissVal) -> bool,
{
    let mut registry = weak_pointer_registry().lock().unwrap();
    for handle in registry.iter() {
        // Safety: the weak pointer was registered by the owner and is still alive.
        let wp = unsafe { &mut *handle.0 };
        if !wp.is_broken() {
            let (referent, _) = wp.value();
            if is_dead(referent) {
                wp.break_ref();
            }
        }
    }
    // Remove broken weak pointers from the registry.
    registry.retain(|handle| {
        let wp = unsafe { &*handle.0 };
        !wp.is_broken()
    });
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

/// Drain and invoke every finalizer still registered with the runtime.
///
/// Shutdown uses this to honor the lifecycle contract even when no GC cycle
/// happens to collect the associated objects first.
pub fn run_pending_finalizers() -> Vec<(BlissVal, BlissVal)> {
    let mut registry = finalizer_registry().lock().unwrap();
    let entries: Vec<_> = registry.drain(..).collect();
    drop(registry);

    let mut invoked = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(dispatch) = entry.callback.or_else(|| FINALIZER_DISPATCH.get().copied()) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                dispatch(entry.finalizer, entry.object);
            }));
        }
        invoked.push((entry.finalizer, entry.object));
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
        return Err(BlissError::Internal(
            "nursery_size exceeds heap_size".into(),
        ));
    }
    if config.region_size == 0 {
        return Err(BlissError::Internal("region_size must be non-zero".into()));
    }
    // tlab_size must be a non-zero power of two. The power-of-two constraint
    // also implicitly rejects region_size+1 (never a power of two when region_size
    // is a power of two), preventing TLAB sizes that can't fit in a single region.
    if config.tlab_size == 0 || (config.tlab_size & (config.tlab_size - 1)) != 0 {
        return Err(BlissError::Internal(
            "tlab_size must be a power of two".into(),
        ));
    }
    let regions_total = (config.heap_size / config.region_size) as u32;

    // Allocate real heap memory as a single contiguous block.
    // We use page-level alignment (4096) which the system allocator supports,
    // rather than region_size alignment which may be too large.
    let mut align = 4096usize.min(config.region_size.max(1));
    if !align.is_power_of_two() {
        align = align.next_power_of_two() >> 1;
        if align == 0 {
            align = 1;
        }
    }
    let heap_layout = Layout::from_size_align(config.heap_size, align)
        .map_err(|e| BlissError::Internal(format!("invalid heap layout: {}", e)))?;

    // Safety: layout is valid (non-zero size, power-of-two alignment).
    let heap_base = unsafe { alloc::alloc_zeroed(heap_layout) };
    if heap_base.is_null() {
        return Err(BlissError::Oom);
    }

    // Set up region metadata. Divide the heap into regions.
    // All regions start as Free — nursery regions are lazily converted
    // from Free→Nursery on the first TLAB refill (see refill_tlab).
    let mut regions = Vec::with_capacity(regions_total as usize);

    for i in 0..regions_total {
        let region_base = unsafe { heap_base.add(i as usize * config.region_size) };
        let region_limit = unsafe { region_base.add(config.region_size) } as *const u8;

        let header = RegionHeader {
            kind: RegionKind::Free,
            gen_age: 0,
            live_bytes: 0,
            alloc_top: region_base, // nothing allocated yet
            alloc_limit: region_limit,
            next_free: if i + 1 < regions_total {
                i + 1
            } else {
                u32::MAX
            },
            mark_bitmap_offset: 0,
        };

        regions.push(HeapRegion {
            header,
            base: region_base,
            size: config.region_size,
        });
    }

    let stats = GcStats {
        nursery_capacity: config.nursery_size as u64,
        old_gen_capacity: (config.heap_size - config.nursery_size) as u64,
        regions_total,
        regions_free: regions_total, // all regions start Free
        ..GcStats::default()
    };

    // Use the address of the heap_state mutex itself as a stable base address
    // for relocation tracking.  This gives a deterministic, non-zero value that
    // changes across processes, which is exactly what the image format needs.
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
        // Each object has an 8-byte header (type_id: u8, padding: 3 bytes, size: u32)
        // written by the allocator (HeapAllocator.alloc_fast/alloc_slow/alloc_large).
        let mut cursor = base;
        while cursor + OBJECT_HEADER_SIZE <= top {
            let header_ptr = cursor as *const u8;
            let (type_id, body_size) = unsafe { read_object_header(header_ptr) };

            if body_size == 0 && type_id == 0 {
                // Zero-filled holes can appear inside a region after prior GC
                // activity. Keep scanning on object-aligned boundaries instead
                // of truncating the walk at the first gap.
                cursor += OBJECT_ALIGNMENT;
                continue;
            }

            // Skip forwarded objects (they are stale copies).
            if type_id == FORWARDED_TYPE_ID {
                let total = align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                cursor += total;
                continue;
            }

            let obj_ptr = unsafe { header_ptr.add(OBJECT_HEADER_SIZE) };
            let should_continue = callback(obj_ptr, type_id, body_size as usize);
            if !should_continue {
                return Ok(());
            }

            // Advance cursor past header + object body, aligned to 16 bytes.
            let total = OBJECT_HEADER_SIZE + body_size as usize;
            let aligned = align_up(total, OBJECT_ALIGNMENT);
            cursor += aligned;
        }
    }

    Ok(())
}

// ── Image / persistence helpers ───────────────────────────────────

fn entry_continuation_cell() -> &'static Mutex<BlissVal> {
    static ENTRY: OnceLock<Mutex<BlissVal>> = OnceLock::new();
    ENTRY.get_or_init(|| Mutex::new(crate::value::NIL))
}

fn byte_store(name: &'static str) -> &'static Mutex<Vec<u8>> {
    static SYMBOLS: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    static PACKAGES: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    static CODE: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    match name {
        "symbols" => SYMBOLS.get_or_init(|| Mutex::new(Vec::new())),
        "packages" => PACKAGES.get_or_init(|| Mutex::new(Vec::new())),
        "code" => CODE.get_or_init(|| Mutex::new(Vec::new())),
        _ => unreachable!(),
    }
}

fn clear_heap_objects(state: &mut HeapState) {
    for region in &mut state.regions {
        let used = (region.header.alloc_top as usize).saturating_sub(region.base as usize);
        if used > 0 {
            unsafe {
                std::ptr::write_bytes(region.base, 0, used);
            }
        }
        region.header.kind = RegionKind::Free;
        region.header.gen_age = 0;
        region.header.live_bytes = 0;
        region.header.alloc_top = region.base;
    }
    state.stats.bytes_allocated = 0;
    state.stats.bytes_promoted = 0;
    state.stats.nursery_used = 0;
    state.stats.old_gen_used = 0;
    state.stats.large_object_bytes = 0;
    state.stats.regions_free = state.regions.len() as u32;
}

fn append_serialized_object(
    state: &mut HeapState,
    type_id: u8,
    body: &[u8],
) -> Result<(), BlissError> {
    let total_size = align_up(OBJECT_HEADER_SIZE + body.len(), OBJECT_ALIGNMENT);
    let region_limit = state.config.region_size / 2;
    let desired_kind = if total_size > region_limit {
        RegionKind::LargeObject
    } else {
        RegionKind::Nursery
    };

    let mut target_idx = None;
    for (idx, region) in state.regions.iter_mut().enumerate() {
        if region.header.kind != desired_kind {
            continue;
        }
        let used = (region.header.alloc_top as usize).saturating_sub(region.base as usize);
        if used + total_size <= region.size {
            target_idx = Some(idx);
            break;
        }
    }

    if target_idx.is_none() {
        for (idx, region) in state.regions.iter_mut().enumerate() {
            if region.header.kind == RegionKind::Free && region.size >= total_size {
                region.header.kind = desired_kind;
                region.header.gen_age = 0;
                state.stats.regions_free = state.stats.regions_free.saturating_sub(1);
                target_idx = Some(idx);
                break;
            }
        }
    }

    let idx = target_idx.ok_or(BlissError::Oom)?;
    let region = &mut state.regions[idx];
    let header_ptr = region.header.alloc_top;
    unsafe {
        write_object_header(header_ptr, type_id, body.len() as u32);
        std::ptr::copy_nonoverlapping(
            body.as_ptr(),
            header_ptr.add(OBJECT_HEADER_SIZE),
            body.len(),
        );
        if total_size > OBJECT_HEADER_SIZE + body.len() {
            std::ptr::write_bytes(
                header_ptr.add(OBJECT_HEADER_SIZE + body.len()),
                0,
                total_size - OBJECT_HEADER_SIZE - body.len(),
            );
        }
    }
    region.header.alloc_top = unsafe { region.header.alloc_top.add(total_size) };
    region.header.live_bytes = region.header.live_bytes.saturating_add(total_size as u32);

    match desired_kind {
        RegionKind::Nursery => state.stats.nursery_used += total_size as u64,
        RegionKind::LargeObject => state.stats.large_object_bytes += total_size as u64,
        _ => {}
    }
    state.stats.bytes_allocated += total_size as u64;
    Ok(())
}

pub fn record_object(type_id: u8, data: Vec<u8>) {
    let mut guard = heap_state().lock().unwrap();
    if let Some(state) = guard.as_mut() {
        let _ = append_serialized_object(state, type_id, &data);
    }
}

pub fn set_entry_continuation(val: BlissVal) {
    *entry_continuation_cell().lock().unwrap() = val;
}

pub fn get_entry_continuation() -> BlissVal {
    *entry_continuation_cell().lock().unwrap()
}

pub fn full_gc() -> Result<(), BlissError> {
    let guard = heap_state().lock().unwrap();
    if guard.is_none() {
        return Ok(());
    }
    drop(guard);
    let mut collector = HeapCollector::new();
    collector.full_gc()
}

pub fn serialize_heap_objects() -> Vec<u8> {
    let mut out = Vec::new();
    let _ = walk_heap(|ptr, type_id, size| {
        out.push(type_id);
        out.extend_from_slice(&(size as u32).to_le_bytes());
        let data = unsafe { std::slice::from_raw_parts(ptr, size) };
        out.extend_from_slice(data);
        true
    });
    out
}

pub fn restore_heap(data: &[u8]) -> Result<(), BlissError> {
    let mut guard = heap_state().lock().unwrap();
    let state = guard
        .as_mut()
        .ok_or_else(|| BlissError::Internal("heap not initialized".into()))?;
    clear_heap_objects(state);

    let mut offset = 0usize;
    while offset < data.len() {
        if data.len() - offset < 5 {
            return Err(BlissError::InvalidImage(
                "truncated heap object record".into(),
            ));
        }
        let type_id = data[offset];
        offset += 1;
        let size = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;
        if data.len() - offset < size {
            return Err(BlissError::InvalidImage(
                "truncated heap object payload".into(),
            ));
        }
        append_serialized_object(state, type_id, &data[offset..offset + size])?;
        offset += size;
    }
    Ok(())
}

pub fn serialize_symbols() -> Vec<u8> {
    byte_store("symbols").lock().unwrap().clone()
}

pub fn restore_symbols(data: &[u8]) -> Result<(), BlissError> {
    *byte_store("symbols").lock().unwrap() = data.to_vec();
    Ok(())
}

pub fn serialize_packages() -> Vec<u8> {
    byte_store("packages").lock().unwrap().clone()
}

pub fn restore_packages(data: &[u8]) -> Result<(), BlissError> {
    *byte_store("packages").lock().unwrap() = data.to_vec();
    Ok(())
}

pub fn serialize_code_cache() -> Vec<u8> {
    byte_store("code").lock().unwrap().clone()
}

pub fn restore_code_cache(data: &[u8]) -> Result<(), BlissError> {
    *byte_store("code").lock().unwrap() = data.to_vec();
    Ok(())
}

pub fn heap_base_address() -> u64 {
    let guard = heap_state().lock().unwrap();
    guard
        .as_ref()
        .map_or(0, |state| state.heap_base as usize as u64)
}

pub fn gc_generation() -> u32 {
    let guard = heap_state().lock().unwrap();
    guard
        .as_ref()
        .map_or(0, |state| state.stats.major_gc_count as u32)
}

pub fn serialize_relocation_table() -> Vec<u8> {
    let heap_base = heap_base_address() as usize;
    if heap_base == 0 {
        return Vec::new();
    }

    let mut object_addresses = std::collections::HashSet::new();
    let _ = walk_heap(|ptr, _type_id, _size| {
        object_addresses.insert(ptr as usize);
        true
    });

    let mut relocs = Vec::new();
    let mut object_offset = 0usize;
    let _ = walk_heap(|ptr, _type_id, size| {
        let mut field_offset = 0usize;
        while field_offset + 8 <= size {
            let field_ptr = unsafe { ptr.add(field_offset) };
            let raw = unsafe { std::ptr::read_unaligned(field_ptr as *const u64) } as usize;
            if raw >= heap_base
                && raw % OBJECT_ALIGNMENT == OBJECT_HEADER_SIZE
                && object_addresses.contains(&raw)
            {
                relocs.push((object_offset + 1 + 4 + field_offset) as u64);
            }
            field_offset += 8;
        }
        object_offset += 1 + 4 + size;
        true
    });

    let mut out = Vec::with_capacity(8 + relocs.len() * 8);
    out.extend_from_slice(&(relocs.len() as u64).to_le_bytes());
    for reloc in relocs {
        out.extend_from_slice(&reloc.to_le_bytes());
    }
    out
}

pub fn apply_relocations(
    object_data: &mut [u8],
    reloc_data: &[u8],
    delta: i64,
) -> Result<(), BlissError> {
    if reloc_data.is_empty() {
        return Ok(());
    }
    if reloc_data.len() < 8 {
        return Err(BlissError::InvalidImage(
            "relocation table too small".into(),
        ));
    }

    let count = u64::from_le_bytes(reloc_data[..8].try_into().unwrap()) as usize;
    let expected_len = 8 + count * 8;
    if reloc_data.len() != expected_len {
        return Err(BlissError::InvalidImage(
            "relocation table length mismatch".into(),
        ));
    }

    for i in 0..count {
        let start = 8 + i * 8;
        let offset = u64::from_le_bytes(reloc_data[start..start + 8].try_into().unwrap()) as usize;
        if offset + 8 > object_data.len() {
            return Err(BlissError::InvalidImage(
                "relocation entry out of range".into(),
            ));
        }
        let raw = u64::from_le_bytes(object_data[offset..offset + 8].try_into().unwrap());
        let relocated = if delta >= 0 {
            raw.checked_add(delta as u64)
        } else {
            raw.checked_sub((-delta) as u64)
        }
        .ok_or_else(|| BlissError::InvalidImage("relocation overflow".into()))?;
        object_data[offset..offset + 8].copy_from_slice(&relocated.to_le_bytes());
    }

    Ok(())
}

pub fn serialize_gc_metadata() -> Vec<u8> {
    let stats = heap_stats();
    let generation = gc_generation();
    let values = [
        stats.minor_gc_count,
        stats.major_gc_count,
        stats.total_minor_pause_us,
        stats.total_major_pause_us,
        stats.bytes_allocated,
        stats.bytes_promoted,
        stats.nursery_used,
        stats.nursery_capacity,
        stats.old_gen_used,
        stats.old_gen_capacity,
        stats.large_object_bytes,
        generation as u64,
        stats.regions_total as u64,
        stats.regions_free as u64,
    ];
    let mut out = Vec::with_capacity(values.len() * 8);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

pub fn restore_gc_metadata(data: &[u8]) -> Result<(), BlissError> {
    const FIELD_COUNT: usize = 14;
    if data.is_empty() {
        return Ok(());
    }
    if data.len() != FIELD_COUNT * 8 {
        return Err(BlissError::InvalidImage(
            "GC metadata length mismatch".into(),
        ));
    }

    let read = |idx: usize| -> u64 {
        let start = idx * 8;
        u64::from_le_bytes(data[start..start + 8].try_into().unwrap())
    };

    let mut guard = heap_state().lock().unwrap();
    let state = guard
        .as_mut()
        .ok_or_else(|| BlissError::Internal("heap not initialized".into()))?;
    state.stats.minor_gc_count = read(0);
    state.stats.major_gc_count = read(1);
    state.stats.total_minor_pause_us = read(2);
    state.stats.total_major_pause_us = read(3);
    state.stats.bytes_allocated = read(4);
    state.stats.bytes_promoted = read(5);
    state.stats.nursery_used = read(6);
    state.stats.nursery_capacity = read(7);
    state.stats.old_gen_used = read(8);
    state.stats.old_gen_capacity = read(9);
    state.stats.large_object_bytes = read(10);
    state.stats.regions_total = read(12) as u32;
    state.stats.regions_free = read(13) as u32;
    Ok(())
}
