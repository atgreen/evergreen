// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Garbage collector — generational, region-based, concurrent old-gen marking.
//!
//! Inspired by HotSpot G1 and ZGC. See §3 of the spec.

#[path = "gc_minor_phase.rs"]
mod minor_phase;
use minor_phase::{MinorPhaseLog, Phase as MinorPhase};
#[path = "gc_root_kind.rs"]
mod root_kind;
#[path = "gc_root_phase.rs"]
mod root_phase;
pub use root_kind::{ExternalRootScanKind, external_root_scan_kind};

use crate::error::EgclError;
use crate::image_relocation::ImageRelocations;
use crate::value::EgclVal;

use std::alloc::Layout;
#[cfg(debug_assertions)]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, RwLock};
use std::thread::ThreadId;
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
    fn alloc_slow(&mut self, size: usize) -> Result<*mut u8, EgclError>;

    /// Allocate a large object directly in old-gen large-object regions.
    fn alloc_large(&mut self, size: usize) -> Result<*mut u8, EgclError>;
}

// ── Collector trait ────────────────────────────────────────────────

/// Public GC interface.
pub trait Collector {
    /// Trigger a minor (nursery) collection. Stop-the-world.
    fn minor_gc(&mut self) -> Result<(), EgclError>;

    /// Trigger a major (old-gen) collection cycle.
    /// Initiates concurrent marking followed by evacuation.
    fn major_gc(&mut self) -> Result<(), EgclError>;

    /// Request a full GC (minor + major). Used before image save.
    fn full_gc(&mut self) -> Result<(), EgclError>;

    /// Query current GC statistics.
    fn stats(&self) -> GcStats;
}

// ── Write barrier ──────────────────────────────────────────────────

/// Write barrier interface emitted by the compiler at every reference store.
pub trait WriteBarrier {
    /// Combined SATB + card barrier.
    /// Called by JIT-generated code at every reference store.
    fn write_barrier(&self, slot_addr: *mut EgclVal, old_val: EgclVal, new_val: EgclVal);
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

// ── Object header layout (spec §1.3) ──────────────────────────────
//
// Every GC-managed object begins with the 8-byte `ObjectHeader` defined in
// `crate::object`: type_id (63:56) · gc_bits (55:48) · hash (47:16) · size
// (15:0, the total object footprint in 8-byte units **including** the header).
// Objects whose footprint exceeds the inline size field use the large-object
// extension: size = 0xFFFF and the true total byte size is stored as a u64
// immediately after the header, with the payload beginning at offset 16.
//
// Forwarding uses the FORWARDED gc-bit (bit 53) with the new body address
// written into the object's first payload word; type_id and size are left
// intact so the heap walker can still stride over an evacuated object (R1.09).
//
// This is the single header contract shared by the allocator, heap walker,
// tracer, and evacuator (bliss-jtc.19). Header access goes through the helpers
// below rather than reading raw bytes.

use crate::lock_order::{LockLevel, OrderedMutex};
use crate::object::{ObjectHeader, gc_bit};

/// Size of the base object header (spec §1.3).
const OBJECT_HEADER_SIZE: usize = 8;

/// Alignment for objects in the heap (spec §3.3.1: 16-byte minimum).
const OBJECT_ALIGNMENT: usize = 16;

/// `size` field value reserved to signal the large-object extension (§1.3).
const LARGE_SIZE_SENTINEL: u16 = 0xFFFF;

/// Payload offset for large objects: base header (8) + u64 true-size (8).
const LARGE_OBJECT_PAYLOAD_OFFSET: usize = 16;

/// Bound the aligned footprint as well as the exact body length: heap walkers
/// return padded body sizes as u32, and region live-byte counters are u32 too.
const MAX_OBJECT_BODY_SIZE: usize =
    (u32::MAX as usize & !(OBJECT_ALIGNMENT - 1)) - LARGE_OBJECT_PAYLOAD_OFFSET;

#[inline]
fn allocation_body_size(size: usize) -> Option<u32> {
    if size == 0 || size > MAX_OBJECT_BODY_SIZE {
        None
    } else {
        Some(size as u32)
    }
}

/// Total object footprint in bytes (header + body, 16-byte aligned) for a
/// requested `body_size`, plus whether the large-object extension is required.
fn object_footprint(body_size: usize) -> (usize, bool) {
    let inline = align_up(OBJECT_HEADER_SIZE + body_size, OBJECT_ALIGNMENT);
    if inline / 8 >= LARGE_SIZE_SENTINEL as usize {
        (
            align_up(LARGE_OBJECT_PAYLOAD_OFFSET + body_size, OBJECT_ALIGNMENT),
            true,
        )
    } else {
        (inline, false)
    }
}

/// Read the `ObjectHeader` at `ptr`.
///
/// Safety: `ptr` must point at a valid object header.
#[inline]
unsafe fn header_at(ptr: *const u8) -> ObjectHeader {
    unsafe { *(ptr as *const ObjectHeader) }
}

/// Byte offset from the header to the object payload (8, or 16 for large
/// objects). Safety: `ptr` must point at a valid object header.
#[inline]
unsafe fn body_offset(ptr: *const u8) -> usize {
    if unsafe { header_at(ptr) }.size_units() == LARGE_SIZE_SENTINEL {
        LARGE_OBJECT_PAYLOAD_OFFSET
    } else {
        OBJECT_HEADER_SIZE
    }
}

/// Total object footprint in bytes, for striding the heap. Reads the
/// large-object extension when present. Safety: valid header at `ptr`.
#[inline]
unsafe fn header_total_bytes(ptr: *const u8) -> usize {
    let h = unsafe { header_at(ptr) };
    if h.size_units() == LARGE_SIZE_SENTINEL {
        unsafe { *(ptr.add(OBJECT_HEADER_SIZE) as *const u64) as usize }
    } else {
        h.size_units() as usize * 8
    }
}

/// True if the slot holds no object (zeroed header). Zero-filled holes appear
/// inside a region after a prior GC. Safety: valid readable `ptr`.
#[inline]
unsafe fn header_is_free(ptr: *const u8) -> bool {
    unsafe { header_at(ptr) }.0 == 0
}

/// True if the object at `ptr` has been evacuated (FORWARDED gc-bit set).
/// Safety: valid header at `ptr`.
#[inline]
unsafe fn header_is_forwarded(ptr: *const u8) -> bool {
    (unsafe { header_at(ptr) }.gc_bits() & (1 << gc_bit::FORWARDED)) != 0
}

/// True if the object at `ptr` is pinned (must not be moved) (bliss-jtc.18).
/// Safety: valid header at `ptr`.
#[inline]
unsafe fn header_is_pinned(ptr: *const u8) -> bool {
    (unsafe { header_at(ptr) }.gc_bits() & (1 << gc_bit::PINNED)) != 0
}

/// The forwarding address (new body pointer) of an evacuated object.
/// Safety: `ptr` must be a forwarded object.
#[inline]
unsafe fn header_forwarding_addr(ptr: *const u8) -> *mut u8 {
    unsafe { *(ptr.add(body_offset(ptr)) as *const *mut u8) }
}

/// Install a forwarding pointer at `ptr`: set the FORWARDED gc-bit (leaving
/// type_id and size intact) and store `new_body` in the first payload word.
/// Safety: `ptr` must point at a live object being evacuated, with room for a
/// pointer in its payload (always true — minimum object is 16 bytes).
#[inline]
unsafe fn header_set_forwarded(ptr: *mut u8, new_body: *mut u8) {
    unsafe {
        (*(ptr as *mut ObjectHeader)).set_forwarded();
        *(ptr.add(body_offset(ptr)) as *mut *mut u8) = new_body;
    }
}

/// Write a spec `ObjectHeader` for a `body_size`-byte body and return the
/// payload offset (8, or 16 for large objects).
///
/// The header `size` field records the total footprint in 8-byte units, which
/// is all the GC needs to stride and evacuate. The exact logical body length is
/// stashed in the otherwise-unused `hash` field so the image serializer and
/// heap walker can reproduce sub-8-byte bodies byte-exactly (R7.01). This reuse
/// is a bootstrap-era mechanism: once real Lisp objects carry their own length
/// fields (§1.6) and `SXHASH` caching is wired for heap objects, exact length
/// comes from the payload and the `hash` field reverts to identity hashing.
///
/// Safety: `ptr` must be valid for writes of the object's full footprint.
unsafe fn write_object_header(ptr: *mut u8, type_id: u8, body_size: u32) -> usize {
    let (total, large) = object_footprint(body_size as usize);
    let mut hdr = if large {
        ObjectHeader::new(type_id, LARGE_SIZE_SENTINEL)
    } else {
        ObjectHeader::new(type_id, (total / 8) as u16)
    };
    hdr.set_hash(body_size);
    unsafe {
        *(ptr as *mut ObjectHeader) = hdr;
        if large {
            *(ptr.add(OBJECT_HEADER_SIZE) as *mut u64) = total as u64;
            LARGE_OBJECT_PAYLOAD_OFFSET
        } else {
            OBJECT_HEADER_SIZE
        }
    }
}

/// The exact logical body length recorded at allocation (see
/// `write_object_header`), falling back to the footprint body size for headers
/// written without one. Safety: valid header at `ptr`.
#[inline]
unsafe fn header_exact_body_len(ptr: *const u8) -> usize {
    let h = unsafe { header_at(ptr) };
    let exact = h.hash() as usize;
    if exact != 0 {
        exact
    } else {
        unsafe { header_total_bytes(ptr) - body_offset(ptr) }
    }
}

/// Read an object header at `ptr`. Returns `(type_id, body_size_bytes)` where
/// the body size is the payload footprint (total minus header/extension),
/// rounded up to the object alignment. A zeroed slot reads as `(0, 0)`.
///
/// Safety: `ptr` must be valid for reads of at least OBJECT_HEADER_SIZE bytes.
unsafe fn read_object_header(ptr: *const u8) -> (u8, u32) {
    #[cfg(test)]
    nursery_scan_tests::HEADER_READS.with(|count| count.set(count.get() + 1));
    if unsafe { header_is_free(ptr) } {
        return (0, 0);
    }
    let type_id = unsafe { header_at(ptr) }.type_id();
    let body = unsafe { header_total_bytes(ptr) } - unsafe { body_offset(ptr) };
    (type_id, body as u32)
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
    /// Bytes allocated from the current TLAB and not yet added to its
    /// region's `live_bytes` and the heap stats. Counting them here keeps the
    /// global heap lock off the allocation fast path; [`Self::flush_pending`]
    /// publishes them when the TLAB retires (bliss-xyt4l).
    pending_bytes: usize,
}

// Safety: HeapAllocator owns its TLAB and only the owning thread uses it.
unsafe impl Send for HeapAllocator {}

impl HeapAllocator {
    /// Create a new HeapAllocator. The heap must already be initialized via `init_heap`.
    pub fn new() -> Result<Self, EgclError> {
        let mut alloc = Self::without_tlab()?;
        alloc.refill_tlab()?;
        Ok(alloc)
    }

    /// Initialize allocation state without reserving nursery space. T0 uses
    /// this so its first allocation reaches the collecting slow path when a
    /// fresh thread (or an invalidated allocator) finds the nursery full.
    fn without_tlab() -> Result<Self, EgclError> {
        let guard = heap_state().lock().unwrap();
        let state = guard
            .as_ref()
            .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;
        let region_size = state.config.region_size;
        let tlab_size = state.config.tlab_size;
        drop(guard);

        Ok(HeapAllocator {
            tlab: Tlab {
                cursor: std::ptr::null_mut(),
                limit: std::ptr::null(),
                region_idx: 0,
            },
            region_size,
            tlab_size,
            pending_bytes: 0,
        })
    }

    /// Refill the TLAB from a nursery region. If the configured nursery budget
    /// has room, lazily converts a Free region to Nursery. Returns OOM once the
    /// budget is full; the collecting slow path uses that result as its trigger.
    fn refill_tlab(&mut self) -> Result<(), EgclError> {
        self.refill_tlab_within(false)
    }

    /// [`refill_tlab`](Self::refill_tlab), optionally past the nursery budget
    /// into the regions [`emergency_reserve_regions`] holds back. Only the
    /// allocation that a short collection failed uses the reserve, so the
    /// mutator can reach its next poll and signal STORAGE-CONDITION.
    fn refill_tlab_within(&mut self, emergency: bool) -> Result<(), EgclError> {
        let mut guard = heap_state().lock().unwrap();
        let state = guard
            .as_mut()
            .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;

        // Find a nursery region with enough space for a TLAB.
        for (idx, region) in state.regions.iter_mut().enumerate() {
            if region.header.kind != RegionKind::Nursery {
                continue;
            }
            let top = region.header.alloc_top as usize;
            let limit = region.header.alloc_limit as usize;
            let available = limit.saturating_sub(top);
            if available >= self.tlab_size {
                // Carve out a TLAB from this region, zeroing what a minor
                // collection left in it (bliss-xyt4l).
                // SAFETY: the TLAB span lies above alloc_top, holding no object.
                unsafe { region.zero_for_reuse(top, top + self.tlab_size) };
                self.tlab.cursor = region.header.alloc_top;
                self.tlab.limit =
                    unsafe { region.header.alloc_top.add(self.tlab_size) } as *const u8;
                self.tlab.region_idx = idx as u16;
                // Advance the region's alloc_top past the TLAB.
                region.header.alloc_top = unsafe { region.header.alloc_top.add(self.tlab_size) };
                return Ok(());
            }
        }

        // No nursery region with space. Commit another region only while doing
        // so stays within the configured nursery budget; the rest of the heap
        // is survivor/old-generation reserve, not an extension of the nursery.
        let nursery_regions = state
            .regions
            .iter()
            .filter(|region| region.header.kind == RegionKind::Nursery)
            .count();
        let nursery_region_limit = state.config.nursery_size.div_ceil(self.region_size);
        // Growth stops short of the emergency reserve, which only the
        // allocation that a collection could not satisfy may take.
        if emergency
            || (nursery_regions < nursery_region_limit
                && free_region_count(state) > emergency_reserve_regions(state))
        {
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
                    // SAFETY: as above. A Free region is already zero, so this
                    // only guards that invariant.
                    unsafe { region.zero_for_reuse(top, top + self.tlab_size) };
                    self.tlab.cursor = region.header.alloc_top;
                    self.tlab.limit =
                        unsafe { region.header.alloc_top.add(self.tlab_size) } as *const u8;
                    self.tlab.region_idx = idx as u16;
                    region.header.alloc_top =
                        unsafe { region.header.alloc_top.add(self.tlab_size) };
                    return Ok(());
                }
            }
        }

        // No nursery space available — signal that a minor GC is needed.
        Err(EgclError::Oom)
    }

    /// Turn the unused tail of the current TLAB into one inert object.
    ///
    /// Region walkers advance object-by-object and treat a zero header as the
    /// end of allocated space. Without a filler, an under-filled TLAB leaves a
    /// zero gap before the next TLAB carved from the same region, hiding every
    /// later object from marking and relocation.
    fn publish_tlab_tail(&mut self) {
        let cursor = self.tlab.cursor as usize;
        let limit = self.tlab.limit as usize;
        let remaining = limit.saturating_sub(cursor);
        if cursor != 0 && remaining >= OBJECT_ALIGNMENT {
            // Type zero with a non-zero body is an internal, reference-free
            // filler. Its aligned footprint exactly consumes the TLAB tail.
            let normal_body = remaining - OBJECT_HEADER_SIZE;
            let (_, needs_large_header) = object_footprint(normal_body);
            let body_size = if needs_large_header {
                remaining - LARGE_OBJECT_PAYLOAD_OFFSET
            } else {
                normal_body
            };
            debug_assert_eq!(object_footprint(body_size).0, remaining);
            unsafe {
                write_object_header(self.tlab.cursor, 0, body_size as u32);
            }
        }
    }

    fn retire_tlab(&mut self) {
        self.flush_pending();
        self.publish_tlab_tail();
        self.tlab.cursor = self.tlab.limit as *mut u8;
    }

    /// Count `bytes` allocated from the current TLAB. The region and the heap
    /// stats learn of them in [`Self::flush_pending`]: taking the global heap
    /// lock here, once per allocation, cost more than the allocation itself
    /// and serialised allocating threads (bliss-xyt4l).
    #[inline]
    fn update_nursery_live_bytes(&mut self, bytes: usize) {
        self.pending_bytes += bytes;
    }

    /// Add the pending byte count to the TLAB's nursery region and the heap
    /// stats. Called while the TLAB's region is still the one it was carved
    /// from: when the TLAB retires, and when stats are read.
    pub(crate) fn flush_pending(&mut self) {
        if self.pending_bytes == 0 {
            return;
        }
        let bytes = std::mem::take(&mut self.pending_bytes);
        let mut guard = heap_state().lock().unwrap();
        if let Some(state) = guard.as_mut() {
            let idx = self.tlab.region_idx as usize;
            if idx < state.regions.len() {
                let region = &mut state.regions[idx].header;
                region.live_bytes = region.live_bytes.saturating_add(bytes as u32);
                state.stats.bytes_allocated += bytes as u64;
                state.stats.nursery_used += bytes as u64;
            }
        }
    }

    /// Drop the pending count of an allocator whose nursery has since been
    /// collected: its region was reset, so only the cumulative
    /// `bytes_allocated` still wants the bytes.
    fn discard_pending_after_collection(&mut self) {
        if self.pending_bytes == 0 {
            return;
        }
        let bytes = std::mem::take(&mut self.pending_bytes);
        if let Some(state) = heap_state().lock().unwrap().as_mut() {
            state.stats.bytes_allocated += bytes as u64;
        }
    }
}

impl Allocator for HeapAllocator {
    fn alloc_fast(&mut self, size: usize) -> Option<*mut u8> {
        let body_size = allocation_body_size(size)?;
        // Total allocation = header + body, 16-byte aligned (spec §3.3.1), with
        // the large-object extension when the footprint exceeds the size field.
        let (total_size, _) = object_footprint(size);
        let cursor = self.tlab.cursor as usize;
        let limit = self.tlab.limit as usize;
        let new_cursor = cursor.checked_add(total_size)?;
        if new_cursor <= limit {
            let header_ptr = self.tlab.cursor;
            self.tlab.cursor = new_cursor as *mut u8;
            // Write the object header (type_id=0 placeholder, caller sets real type).
            let body_off = unsafe { write_object_header(header_ptr, 0, body_size) };
            // Update live_bytes on the nursery region.
            self.update_nursery_live_bytes(total_size);
            // Return pointer past the header (to the object body).
            Some(unsafe { header_ptr.add(body_off) })
        } else {
            None
        }
    }

    fn alloc_slow(&mut self, size: usize) -> Result<*mut u8, EgclError> {
        if size == 0 {
            return Err(EgclError::Internal("zero-size allocation".into()));
        }
        allocation_body_size(size).ok_or(EgclError::Oom)?;
        // alloc_typed can hold the carrier's T0_ALLOCATOR borrow here. Waiting
        // for collector admission must still acknowledge GC, but must not
        // suspend that borrow or resume it on another carrier.
        let _carrier_pin = crate::thread::FiberPin::current();
        // Anything that cannot fit a fresh TLAB goes through alloc_large. The old
        // `size > region_size / 2` threshold stranded the "medium" range
        // (tlab_size, region_size/2]: alloc_fast fails (footprint exceeds a TLAB),
        // and refill_tlab only ever carves another tlab_size TLAB, so the retry
        // Oom'd and alloc_typed panicked "GC heap unavailable" — e.g.
        // (make-string 100000), a ~400 KiB character string, with the 256 KiB
        // TLAB / 1 MiB region defaults (egcl-medobj). A fresh TLAB is exactly
        // tlab_size, so route on the object's total footprint vs tlab_size.
        if object_footprint(size).0 > self.tlab_size {
            match self.alloc_large(size) {
                Ok(body) => return Ok(body),
                Err(EgclError::Oom) => {}
                Err(error) => return Err(error),
            }
            // The failed attempt released the heap lock. An object larger than
            // the entire heap cannot fit after collection either.
            let fits_heap = {
                let guard = heap_state().lock().unwrap();
                guard.as_ref().is_some_and(|state| {
                    object_footprint(size).0.div_ceil(self.region_size) <= state.regions.len()
                })
            };
            if !fits_heap {
                return Err(EgclError::Oom);
            }
            // Large allocations can exhaust Free regions without filling the
            // nursery, so the ordinary TLAB-refill trigger may never run.
            // Retire this borrowed allocator ourselves: the collector's T0
            // safepoint hook cannot borrow it again. Publish the tail filler
            // before any heap walk and invalidate pointers before movement.
            self.retire_tlab();
            self.tlab.cursor = std::ptr::null_mut();
            self.tlab.limit = std::ptr::null();
            // Minor GC already retries through KeepNursery major collection
            // when no evacuation space is available. Full GC also reclaims
            // dead large objects when the nursery itself is nearly empty.
            HeapCollector::new().full_gc()?;
            // One bounded retry; genuinely retained/fragmented heaps still OOM.
            return self.alloc_large(size);
        }

        // Close the old TLAB before carving another one so region walks can
        // cross its unused tail.
        self.retire_tlab();

        // Try to refill the TLAB. Once the configured nursery budget is full,
        // collect it and retry against the reset nursery regions.
        if let Err(err) = self.refill_tlab() {
            if !matches!(err, EgclError::Oom) {
                return Err(err);
            }
            self.tlab.cursor = std::ptr::null_mut();
            self.tlab.limit = std::ptr::null();
            match HeapCollector::new()
                .minor_gc()
                .and_then(|()| self.refill_tlab())
            {
                // The heap has room again: a later exhaustion signals afresh.
                Ok(()) => HEAP_EXHAUSTION_SIGNALLED.with(|signalled| signalled.set(false)),
                Err(EgclError::Oom) => {
                    // The collection could not place the nursery's survivors
                    // and left the heap as it was, or it succeeded but only the
                    // emergency reserve is left for the nursery (GitHub #471).
                    // Have this execution signal STORAGE-CONDITION at its next
                    // poll, and let it get there on the emergency reserve;
                    // allocation sites cannot all report failure themselves.
                    // Signal once until the heap recovers: the handler, and the
                    // unwinding that drops the data, also run on the reserve,
                    // and a second signal there would escape the handler.
                    if !HEAP_EXHAUSTION_SIGNALLED.with(|signalled| signalled.replace(true)) {
                        crate::thread::post_current_pending_signal(
                            crate::thread::PendingSignal::HeapExhausted,
                        );
                    }
                    self.refill_tlab_within(true)?;
                }
                Err(err) => return Err(err),
            }
        }

        // Retry fast-path allocation after refill.
        self.alloc_fast(size).ok_or(EgclError::Oom)
    }

    fn alloc_large(&mut self, size: usize) -> Result<*mut u8, EgclError> {
        if size == 0 {
            return Err(EgclError::Internal("zero-size large alloc".into()));
        }
        let body_size = allocation_body_size(size).ok_or(EgclError::Oom)?;
        let (total_size, _) = object_footprint(size);

        let mut guard = heap_state().lock().unwrap();
        let state = guard
            .as_mut()
            .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;

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
                    let body_off = unsafe { write_object_header(ptr, 0, body_size) };
                    return Ok(unsafe { ptr.add(body_off) });
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
                let body_off = unsafe { write_object_header(ptr, 0, body_size) };
                return Ok(unsafe { ptr.add(body_off) });
            }
        }

        Err(EgclError::Oom)
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

/// Exact object starts and liveness for one nursery collection. Two bits per
/// object-alignment granule replace per-object hash entries. Region offsets
/// keep the bitmaps proportional to the nursery, not the reserved heap size.
/// Body addresses are normalized to header addresses before indexing, just as
/// the old nursery index keyed every object by header + OBJECT_HEADER_SIZE.
///
/// A minor collection indexes regions lazily ([`Self::reset_lazy`]): a
/// region's objects are walked only when a candidate reference first lands in
/// it, so a nursery of dead objects costs nothing to index (bliss-xyt4l).
#[derive(Default)]
struct NurseryObjectMap {
    heap_base: usize,
    region_size: usize,
    region_offsets: Vec<usize>,
    starts: Vec<u64>,
    marked: Vec<u64>,
    /// Per region index, the `alloc_top` of a region whose starts are still to
    /// be indexed by [`Self::ensure_indexed`]; 0 once indexed (or eager).
    pending_tops: Vec<usize>,
    /// Regions walked by [`Self::ensure_indexed`] during this collection.
    lazily_indexed: u64,
}

impl NurseryObjectMap {
    fn reset(
        &mut self,
        heap_base: usize,
        region_size: usize,
        region_count: usize,
        nursery: &[usize],
    ) {
        self.heap_base = heap_base;
        self.region_size = region_size;
        self.region_offsets.resize(region_count, usize::MAX);
        self.region_offsets.fill(usize::MAX);
        let words_per_region = (region_size / OBJECT_ALIGNMENT).div_ceil(64);
        for (ordinal, &region) in nursery.iter().enumerate() {
            self.region_offsets[region] = ordinal * words_per_region;
        }
        let words = nursery.len() * words_per_region;
        self.starts.resize(words, 0);
        self.starts.fill(0);
        self.marked.resize(words, 0);
        self.marked.fill(0);
        self.pending_tops.clear();
        self.lazily_indexed = 0;
    }

    /// Like [`Self::reset`], but defer each region's object walk until a
    /// candidate reference first falls inside it. Its bitmap words are cleared
    /// then, too, so only the regions that live references reach are touched.
    ///
    /// # Safety
    /// Until the next reset, every region in `nursery` must be stopped,
    /// writable nursery memory holding valid objects up to its `tops` entry,
    /// separated only by zero-filled gaps, exactly as for
    /// [`Self::prepare_region`], and its headers must stay intact until it is
    /// indexed. Indexing happens from [`Self::mark`] and
    /// [`Self::ensure_indexed`], so marking must finish before evacuation
    /// rewrites any header.
    unsafe fn reset_lazy(
        &mut self,
        heap_base: usize,
        region_size: usize,
        region_count: usize,
        nursery: &[usize],
        tops: &[usize],
    ) {
        debug_assert_eq!(nursery.len(), tops.len());
        self.heap_base = heap_base;
        self.region_size = region_size;
        self.region_offsets.resize(region_count, usize::MAX);
        self.region_offsets.fill(usize::MAX);
        self.pending_tops.resize(region_count, 0);
        self.pending_tops.fill(0);
        let words_per_region = (region_size / OBJECT_ALIGNMENT).div_ceil(64);
        for (ordinal, (&region, &top)) in nursery.iter().zip(tops).enumerate() {
            self.region_offsets[region] = ordinal * words_per_region;
            self.pending_tops[region] = top;
        }
        // Stale words are cleared per region when it is indexed. A region
        // never indexed is never read: lookups in a pending region fail.
        let words = nursery.len() * words_per_region;
        if self.starts.len() < words {
            self.starts.resize(words, 0);
            self.marked.resize(words, 0);
        }
        self.lazily_indexed = 0;
    }

    fn clear_region_words(&mut self, region: usize) {
        let words = (self.region_size / OBJECT_ALIGNMENT).div_ceil(64);
        let start = self.region_offsets[region];
        self.starts[start..start + words].fill(0);
        self.marked[start..start + words].fill(0);
    }

    /// Index `region` now if [`Self::reset_lazy`] deferred it. Returns whether
    /// it holds a pinned object (false for a region already indexed).
    fn ensure_indexed(&mut self, region: usize) -> bool {
        if !self.is_pending(region) {
            return false;
        }
        let top = std::mem::replace(&mut self.pending_tops[region], 0);
        self.lazily_indexed += 1;
        self.clear_region_words(region);
        // SAFETY: `reset_lazy`'s contract: this region is stopped, walkable
        // nursery memory through `top` whose headers are still intact.
        unsafe { self.prepare_region(region, top) }
    }

    /// Whether a lazily reset region has not been indexed yet.
    #[inline]
    fn is_pending(&self, region: usize) -> bool {
        self.pending_tops.get(region).is_some_and(|&top| top != 0)
    }

    #[inline]
    fn position(&self, body: usize) -> Option<(usize, u64)> {
        let offset = body
            .checked_sub(self.heap_base)?
            .checked_sub(OBJECT_HEADER_SIZE)?;
        if offset % OBJECT_ALIGNMENT != 0 || self.region_size == 0 {
            return None;
        }
        let &words = self.region_offsets.get(offset / self.region_size)?;
        if words == usize::MAX {
            return None;
        }
        let bit = (offset % self.region_size) / OBJECT_ALIGNMENT;
        Some((words + bit / 64, 1u64 << (bit % 64)))
    }

    fn insert(&mut self, body: usize) {
        let (word, bit) = self
            .position(body)
            .expect("nursery object outside indexed regions");
        self.starts[word] |= bit;
    }

    /// The region index of a `body` that [`Self::position`] accepted.
    #[inline]
    fn region_of(&self, body: usize) -> usize {
        (body - self.heap_base - OBJECT_HEADER_SIZE) / self.region_size
    }

    #[inline]
    fn contains(&self, body: usize) -> bool {
        self.position(body).is_some_and(|(word, bit)| {
            !self.is_pending(self.region_of(body)) && self.starts[word] & bit != 0
        })
    }

    /// Mark the exact object start `body`, first indexing its region if
    /// [`Self::reset_lazy`] deferred it.
    #[inline]
    fn mark(&mut self, body: usize) -> bool {
        let Some((word, bit)) = self.position(body) else {
            return false;
        };
        let region = self.region_of(body);
        if self.is_pending(region) {
            self.ensure_indexed(region);
        }
        if self.starts[word] & bit == 0 || self.marked[word] & bit != 0 {
            return false;
        }
        self.marked[word] |= bit;
        true
    }

    #[inline]
    fn is_marked(&self, body: usize) -> bool {
        self.position(body).is_some_and(|(word, bit)| {
            !self.is_pending(self.region_of(body)) && self.marked[word] & bit != 0
        })
    }

    /// Repair unretired TLAB tails, index exact starts, and detect pinning in
    /// one walk. A zero gap must not hide objects in a later TLAB (bliss-bw3t).
    /// Persistent forwarding stubs are excluded, as in the mark worklist.
    ///
    /// # Safety
    /// The indexed region through `top` must be writable, stopped nursery
    /// memory containing valid objects separated only by zero-filled gaps.
    unsafe fn prepare_region(&mut self, region: usize, top: usize) -> bool {
        let base = self.heap_base + region * self.region_size;
        let mut cursor = base;
        let mut pinned = false;
        while cursor + OBJECT_HEADER_SIZE <= top {
            let header = cursor as *mut u8;
            let (type_id, body_size) = unsafe { read_object_header(header) };
            if type_id == 0 && body_size == 0 {
                let mut end = cursor + OBJECT_ALIGNMENT;
                while end + OBJECT_HEADER_SIZE <= top {
                    if unsafe { read_object_header(end as *const u8) } != (0, 0) {
                        break;
                    }
                    end += OBJECT_ALIGNMENT;
                }
                let end = end.min(top);
                let span = end - cursor;
                if span >= OBJECT_ALIGNMENT {
                    // Extended headers need their extra word subtracted too,
                    // so the filler occupies exactly the unused TLAB span.
                    let normal_body = span - OBJECT_HEADER_SIZE;
                    let (_, large) = object_footprint(normal_body);
                    let filler_body = if large {
                        span - LARGE_OBJECT_PAYLOAD_OFFSET
                    } else {
                        normal_body
                    };
                    debug_assert_eq!(object_footprint(filler_body).0, span);
                    unsafe { write_object_header(header, 0, filler_body as u32) };
                    self.insert(cursor + OBJECT_HEADER_SIZE);
                } else if gc_verify_enabled() {
                    panic!(
                        "gc-verify(tlab-gap): nursery region {region} has an unfilled \
                         gap at {cursor:#x} before alloc_top {top:#x} (base {base:#x})"
                    );
                }
                cursor = end;
                continue;
            }
            if !unsafe { header_is_forwarded(header) } {
                self.insert(cursor + OBJECT_HEADER_SIZE);
                pinned |= unsafe { header_is_pinned(header) };
            }
            cursor += align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
        }
        pinned
    }

    /// Enumerate only live bodies, in the same address order as the old stride
    /// walk. Bits were set only for exact, non-forwarded object starts, before
    /// evacuation changed any header. No scratch allocation is needed.
    fn marked_bodies(&self, region: usize) -> impl Iterator<Item = usize> + '_ {
        // A region never indexed holds no marks; its bitmap words are stale.
        let words = if self.is_pending(region) {
            0
        } else {
            (self.region_size / OBJECT_ALIGNMENT).div_ceil(64)
        };
        let start = self.region_offsets[region];
        let base = self.heap_base + region * self.region_size;
        self.marked[start..start + words]
            .iter()
            .enumerate()
            .flat_map(move |(word, &bits)| {
                let mut remaining = bits;
                std::iter::from_fn(move || {
                    if remaining == 0 {
                        return None;
                    }
                    let bit = remaining.trailing_zeros() as usize;
                    remaining &= remaining - 1;
                    Some(base + (word * 64 + bit) * OBJECT_ALIGNMENT + OBJECT_HEADER_SIZE)
                })
            })
    }
}

#[cfg(test)]
mod nursery_object_map_tests {
    use super::*;

    const BASE: usize = 0x10000;
    const REGION: usize = 4096;

    #[test]
    fn nursery_map_marks_only_indexed_object_bodies_once() {
        let mut map = NurseryObjectMap::default();
        map.reset(BASE, REGION, 5, &[1, 3]);
        let body = BASE + REGION + OBJECT_HEADER_SIZE;
        map.insert(body);
        assert!(map.contains(body));
        assert!(!map.is_marked(body));
        assert!(map.mark(body));
        assert!(map.is_marked(body));
        assert!(!map.mark(body));
        // Interior addresses, headers, unindexed objects, and old regions
        // must never become roots merely because they fall inside the heap.
        for invalid in [
            0,
            BASE - 1,
            body - 8,
            body + 1,
            body + 16,
            BASE + OBJECT_HEADER_SIZE,
            BASE + 5 * REGION + OBJECT_HEADER_SIZE,
            usize::MAX,
        ] {
            assert!(!map.contains(invalid), "{invalid:#x}");
            assert!(!map.mark(invalid), "{invalid:#x}");
        }
        let last = BASE + 4 * REGION - OBJECT_ALIGNMENT + OBJECT_HEADER_SIZE;
        map.insert(last);
        assert!(map.mark(last));
    }

    #[test]
    fn nursery_map_reset_drops_previous_objects_and_marks() {
        let mut map = NurseryObjectMap::default();
        let body = BASE + REGION + OBJECT_HEADER_SIZE;
        map.reset(BASE, REGION, 5, &[1, 3]);
        map.insert(body);
        assert!(map.mark(body));
        map.reset(BASE, REGION, 5, &[1]);
        assert!(!map.contains(body));
        assert!(!map.is_marked(body));
        map.insert(body);
        assert!(map.mark(body));
        map.reset(BASE + 8 * REGION, REGION * 2, 2, &[0]);
        assert!(!map.contains(body));
        let moved_heap_body = BASE + 8 * REGION + OBJECT_HEADER_SIZE;
        map.insert(moved_heap_body);
        assert!(map.mark(moved_heap_body));
    }

    #[test]
    fn nursery_map_storage_tracks_nursery_not_heap_capacity() {
        let mut map = NurseryObjectMap::default();
        map.reset(BASE, REGION, 10000, &[2, 9999]);
        assert_eq!(map.starts.len(), 2 * (REGION / OBJECT_ALIGNMENT / 64));
        assert_eq!(map.marked.len(), map.starts.len());
        map.reset(BASE, REGION, 10000, &[]);
        assert!(!map.mark(BASE + 2 * REGION + OBJECT_HEADER_SIZE));
        assert!(map.starts.is_empty());
    }

    #[test]
    fn live_body_iteration_preserves_address_order_and_region_boundaries() {
        let mut map = NurseryObjectMap::default();
        // Deliberately use a different bitmap order from physical region order.
        map.reset(BASE, REGION, 5, &[3, 1]);
        for region in [1, 3] {
            let bodies: Vec<usize> = [0, 1, 63, 64, 255]
                .map(|bit| BASE + region * REGION + bit * OBJECT_ALIGNMENT + OBJECT_HEADER_SIZE)
                .into();
            for &body in bodies.iter().rev() {
                map.insert(body);
                assert!(map.mark(body));
            }
            map.insert(BASE + region * REGION + 32 * OBJECT_ALIGNMENT + OBJECT_HEADER_SIZE);
            assert_eq!(map.marked_bodies(region).collect::<Vec<_>>(), bodies);
        }
        map.reset(BASE, REGION, 5, &[1]);
        assert_eq!(map.marked_bodies(1).count(), 0);
        // Small regions have a partial final bitmap word.
        map.reset(BASE, 32, 2, &[0, 1]);
        map.insert(BASE + 16 + OBJECT_HEADER_SIZE);
        map.mark(BASE + 16 + OBJECT_HEADER_SIZE);
        assert_eq!(
            map.marked_bodies(0).collect::<Vec<_>>(),
            [BASE + 16 + OBJECT_HEADER_SIZE]
        );
        assert_eq!(map.marked_bodies(1).count(), 0);
    }

    #[test]
    fn preparation_fills_tlab_gaps_without_indexing_forwarded_stubs() {
        let mut memory = vec![0u128; REGION / OBJECT_ALIGNMENT];
        let ptr = memory.as_mut_ptr() as *mut u8;
        let base = ptr as usize;
        let mut map = NurseryObjectMap::default();
        map.reset(base, REGION, 1, &[0]);
        unsafe {
            for offset in [0, 128, 160] {
                write_object_header(ptr.add(offset), crate::object::type_id::CONS, 16);
            }
            (*(ptr.add(128) as *mut ObjectHeader)).set_pinned();
            (*(ptr.add(160) as *mut ObjectHeader)).set_pinned();
            header_set_forwarded(ptr.add(160), ptr.add(128 + OBJECT_HEADER_SIZE));
            assert!(map.prepare_region(0, base + 256));
            assert_eq!(header_total_bytes(ptr.add(32)), 96);
            assert_eq!(header_total_bytes(ptr.add(192)), 64);
        }
        for offset in [0, 32, 128, 192] {
            assert!(map.contains(base + offset + OBJECT_HEADER_SIZE));
        }
        for offset in [16, 48, 96, 144, 160, 176, 208, 256] {
            assert!(!map.contains(base + offset + OBJECT_HEADER_SIZE));
        }
        // Pinning on a forwarding stub must not retain the region by itself.
        unsafe { (*(ptr.add(128) as *mut ObjectHeader)).clear_pinned() };
        map.reset(base, REGION, 1, &[0]);
        assert!(!unsafe { map.prepare_region(0, base + 256) });
    }

    #[test]
    fn preparation_preserves_large_filler_footprints_and_later_objects() {
        const REGION: usize = 2 * 1024 * 1024;
        const LATER: usize = 1024 * 1024;
        let mut memory = vec![0u128; REGION / OBJECT_ALIGNMENT];
        let ptr = memory.as_mut_ptr() as *mut u8;
        let base = ptr as usize;
        let mut map = NurseryObjectMap::default();
        map.reset(base, REGION, 1, &[0]);
        unsafe {
            write_object_header(ptr, crate::object::type_id::CONS, 16);
            write_object_header(ptr.add(LATER), crate::object::type_id::CONS, 16);
            assert!(!map.prepare_region(0, base + LATER + 32));
            assert_eq!(body_offset(ptr.add(32)), LARGE_OBJECT_PAYLOAD_OFFSET);
            assert_eq!(header_total_bytes(ptr.add(32)), LATER - 32);
        }
        assert!(map.contains(base + LATER + OBJECT_HEADER_SIZE));
        assert!(!map.contains(base + LATER - 16 + OBJECT_HEADER_SIZE));
    }

    #[test]
    fn lazy_reset_indexes_only_regions_that_marking_reaches() {
        let mut memory = vec![0u128; 2 * REGION / OBJECT_ALIGNMENT];
        let ptr = memory.as_mut_ptr() as *mut u8;
        let base = ptr as usize;
        let mut map = NurseryObjectMap::default();
        // Leave stale bits from an earlier eager use: a lazy region must clear
        // them before answering for it.
        map.reset(base, REGION, 2, &[0, 1]);
        map.insert(base + 64 + OBJECT_HEADER_SIZE);
        map.insert(base + REGION + 64 + OBJECT_HEADER_SIZE);
        unsafe {
            write_object_header(ptr, crate::object::type_id::CONS, 16);
            write_object_header(ptr.add(REGION), crate::object::type_id::CONS, 16);
            map.reset_lazy(base, REGION, 2, &[0, 1], &[base + 32, base + REGION + 32]);
        }
        let first = base + OBJECT_HEADER_SIZE;
        let second = base + REGION + OBJECT_HEADER_SIZE;
        assert!(map.is_pending(0) && map.is_pending(1));
        assert!(!map.contains(first) && !map.is_marked(first));
        assert_eq!(map.marked_bodies(0).count(), 0);
        // A non-start address still indexes the region, and is rejected.
        assert!(!map.mark(second + 16));
        assert!(!map.is_pending(1) && map.is_pending(0));
        assert_eq!(map.lazily_indexed, 1);
        assert!(!map.contains(base + REGION + 64 + OBJECT_HEADER_SIZE));
        assert!(map.mark(second));
        assert!(!map.mark(second));
        assert_eq!(map.marked_bodies(1).collect::<Vec<_>>(), [second]);
        // The other region was never read.
        assert!(map.is_pending(0));
        assert_eq!(map.lazily_indexed, 1);
        // Explicit indexing reports pins, once.
        unsafe { (*(ptr as *mut ObjectHeader)).set_pinned() };
        assert!(map.ensure_indexed(0));
        assert!(!map.ensure_indexed(0));
        assert!(!map.contains(base + 64 + OBJECT_HEADER_SIZE));
        assert!(map.contains(first));
        // An eager reset leaves nothing pending.
        map.reset(base, REGION, 2, &[0, 1]);
        assert!(!map.is_pending(0) && !map.is_pending(1));
    }

    #[test]
    fn reuse_zeroes_only_the_stale_part_of_a_handed_out_span() {
        let mut memory = vec![0xA5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5u128; 64];
        let ptr = memory.as_mut_ptr() as *mut u8;
        let base = ptr as usize;
        let mut region = HeapRegion {
            header: RegionHeader {
                kind: RegionKind::Nursery,
                gen_age: 0,
                live_bytes: 0,
                alloc_top: ptr,
                alloc_limit: unsafe { ptr.add(1024) },
                next_free: u32::MAX,
                mark_bitmap_offset: 0,
            },
            base: ptr,
            size: 1024,
            dirty_end: base + 512,
        };
        let bytes = |memory: &[u128]| -> Vec<u8> {
            memory.iter().flat_map(|word| word.to_ne_bytes()).collect()
        };
        unsafe { region.zero_for_reuse(base, base + 256) };
        let now = bytes(&memory);
        assert!(now[..256].iter().all(|&b| b == 0));
        assert!(now[256..].iter().all(|&b| b == 0xA5));
        assert_eq!(region.dirty_end, base + 512);
        // A span crossing the stale end zeroes up to it and clears the record.
        unsafe { region.zero_for_reuse(base + 256, base + 768) };
        let now = bytes(&memory);
        assert!(now[..512].iter().all(|&b| b == 0));
        assert!(
            now[512..].iter().all(|&b| b == 0xA5),
            "fresh bytes untouched"
        );
        assert_eq!(region.dirty_end, base);
        // Nothing stale: a no-op.
        unsafe { region.zero_for_reuse(base + 768, base + 1024) };
        assert!(bytes(&memory)[768..].iter().all(|&b| b == 0xA5));
        // The tail above alloc_top is zeroed when the region leaves the nursery.
        region.header.alloc_top = unsafe { ptr.add(128) };
        region.dirty_end = base + 1024;
        region.zero_stale_tail();
        assert!(bytes(&memory).iter().all(|&b| b == 0));
        assert_eq!(region.dirty_end, base);
    }
}

thread_local! {
    /// Persistent minor-GC scratch (bliss-5i3f). `HeapCollector`s are created
    /// ad-hoc per collection, so retain bitmap/worklist capacity between
    /// collections. The STW body is single-threaded under the heap lock:
    /// take the scratch, reset it, and return it before resuming mutators.
    /// No early return or `?` runs between take and restore.
    static MINOR_OBJECT_MAP: RefCell<NurseryObjectMap> = RefCell::new(NurseryObjectMap::default());
    static MINOR_WORKLIST: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The same scratch for the MAJOR mark phase, kept separate from the minor
    /// one so the bitmaps do not thrash between nursery-sized and whole-old-gen
    /// sizes on every cycle (a major collection runs a minor first).
    static MAJOR_OBJECT_MAP: RefCell<NurseryObjectMap> = RefCell::new(NurseryObjectMap::default());
    static MAJOR_WORKLIST: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// True if `v` is a tagged heap reference (cons, heap object, or function
/// pointer) whose referent the GC must trace. Immediates — fixnums, chars,
/// single-floats, symbols-by-id, NIL/T — are not references.
/// Public: code generators must know which values may MOVE under the minor GC
/// (a movable value can never be embedded as a raw immediate in native code —
/// it must be loaded through a GC-visible slot; bliss-d0b T1 constants).
/// Live heap bounds, published at init for lock-free membership tests.
/// (0, 0) until the heap exists. Read with Relaxed: the values are written once
/// per heap lifetime, before any allocation is possible.
static HEAP_RANGE_BASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HEAP_RANGE_END: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Bounds-checked object type read for an ARBITRARY value (bliss-334): `None`
/// unless `v` is heap-tagged and its object header lies inside the managed
/// heap, else the header's type_id. Reading a header anywhere inside the
/// mapping is memory-safe (the heap is one live mmap), so this can classify
/// untrusted values without a liveness side-table. A DANGLING in-heap value
/// yields whatever occupies that address now — the same staleness contract as
/// every heap reference under a moving collector.
pub fn heap_object_type_id(v: EgclVal) -> Option<u8> {
    if !is_heap_ref(v) {
        return None;
    }
    let base = HEAP_RANGE_BASE.load(std::sync::atomic::Ordering::Relaxed);
    let end = HEAP_RANGE_END.load(std::sync::atomic::Ordering::Relaxed);
    let body = ref_body_addr(v);
    if base == 0 || body < base + OBJECT_HEADER_SIZE || body >= end {
        return None;
    }
    let header = (body - OBJECT_HEADER_SIZE) as *const u8;
    // SAFETY: header lies inside the live heap mapping (bounds above).
    let (type_id, _) = unsafe { read_object_header(header) };
    Some(type_id)
}

/// Install a PERSISTENT forwarding pointer from `old` to `new` (both values of
/// the same heap-object kind), using the collector's own forwarding layout —
/// FORWARDED gc-bit plus the new BODY address in the first payload word. Used
/// by CHANGE-CLASS / class-redefinition instance migration (bliss-334).
///
/// Layout compatibility is the point: `relocate_slot` and the mark pass treat
/// these stubs exactly like evacuation forwarding, so every reachable slot
/// holding `old` is rewritten to the final target at the next collection and
/// the stub then dies. The previous CLOS-private scheme stored a TAGGED
/// `EgclVal` in that word, which `relocate_slot` (reading an untagged body
/// pointer) would have turned into a wild pointer.
///
/// # Safety
/// `old` and `new` must reference live objects; `old` must not be reachable as
/// a plain object afterwards (its payload word is overwritten).
pub unsafe fn forward_object_to(old: EgclVal, new: EgclVal) {
    let old_body = ref_body_addr(old);
    let new_body = ref_body_addr(new);
    let header = (old_body - OBJECT_HEADER_SIZE) as *mut u8;
    unsafe { header_set_forwarded(header, new_body as *mut u8) };
}

/// Follow forwarding (persistent CHANGE-CLASS stubs and, transiently during a
/// collection, evacuation forwarding) from `v` to the final live object,
/// preserving `v`'s tag. Returns `v` unchanged when it is not a forwarded
/// in-heap reference.
pub fn resolve_forwarded(v: EgclVal) -> EgclVal {
    let base = HEAP_RANGE_BASE.load(std::sync::atomic::Ordering::Relaxed);
    let end = HEAP_RANGE_END.load(std::sync::atomic::Ordering::Relaxed);
    if base == 0 || !is_heap_ref(v) {
        return v;
    }
    let tag = v.0 & crate::value::TAG_MASK;
    let mut body = ref_body_addr(v);
    let offset = body - ((v.0 & !crate::value::TAG_MASK) as usize);
    loop {
        if body < base + OBJECT_HEADER_SIZE || body >= end {
            return EgclVal((body - offset) as u64 | tag);
        }
        let header = (body - OBJECT_HEADER_SIZE) as *const u8;
        if !unsafe { header_is_forwarded(header) } {
            return EgclVal((body - offset) as u64 | tag);
        }
        body = unsafe { header_forwarding_addr(header) } as usize;
    }
}

#[inline]
pub fn is_heap_ref(v: EgclVal) -> bool {
    matches!(
        v.0 & crate::value::TAG_MASK,
        crate::value::TAG_CONS | crate::value::TAG_HEAP_OBJECT | crate::value::TAG_FUNCTION
    )
}

/// The body address (object header + OBJECT_HEADER_SIZE) of the object a heap
/// reference points at. Conses point at their body directly; every other heap
/// object's value points at the object header, so its body is 8 bytes further on.
/// Normalizing to the body address gives the GC one object identity for indexing,
/// marking, forwarding, and relocation, regardless of the pointer convention.
/// Safety-neutral: pure address arithmetic on a tagged value.
#[inline]
fn ref_body_addr(v: EgclVal) -> usize {
    let ptr = (v.0 & !crate::value::TAG_MASK) as usize;
    if (v.0 & crate::value::TAG_MASK) == crate::value::TAG_CONS {
        ptr
    } else {
        ptr + OBJECT_HEADER_SIZE
    }
}

/// If `slot` holds a reference to an object that has been evacuated — its header
/// carries a forwarding pointer — rewrite the slot to the object's new location,
/// preserving the tag. Bounds-checked against `[heap_base, heap_end)` so a slot
/// pointing outside the managed heap is never dereferenced. This is the single
/// relocation primitive shared by every root and object-field update pass
/// (bliss-jtc.17): CL-stack frames, the entry continuation, the remembered set,
/// and object reference fields.
///
/// # Safety
/// `slot` must point at a readable/writable `EgclVal`.
#[inline]
unsafe fn relocate_slot(slot: *mut EgclVal, heap_base: usize, heap_end: usize) {
    let v = unsafe { *slot };
    if !is_heap_ref(v) {
        return;
    }
    let tag = v.0 & crate::value::TAG_MASK;
    let v_ptr = (v.0 & !crate::value::TAG_MASK) as usize;
    // Normalize to the object body address (tag-aware: conses point at their
    // body, other heap objects at their header), so the header and forwarding
    // logic below is uniform for every object kind.
    let body = ref_body_addr(v);
    if body < heap_base + OBJECT_HEADER_SIZE || body >= heap_end {
        return;
    }
    let mut body = body;
    let offset = body - v_ptr;
    let mut hops = 0usize;
    loop {
        let header = (body - OBJECT_HEADER_SIZE) as *const u8;
        // SAFETY: `body` lies within the managed heap; its header precedes it.
        if !unsafe { header_is_forwarded(header) } {
            break;
        }
        // Follow CHAINS, not just one hop: a persistent CHANGE-CLASS stub
        // (bliss-334) can point at an object that was itself evacuated this
        // cycle, so stub -> old target -> new target must resolve fully or the
        // slot is rewritten to an address about to be reset.
        body = unsafe { header_forwarding_addr(header) } as usize;
        hops += 1;
        debug_assert!(hops < 64, "forwarding chain too long — cycle?");
        if body < heap_base + OBJECT_HEADER_SIZE || body >= heap_end {
            break;
        }
    }
    if hops > 0 {
        // Preserve the value's offset from the object header (0 for a cons that
        // points at its body, OBJECT_HEADER_SIZE for a header-pointing object).
        unsafe { *slot = EgclVal(((body - offset) as u64) | tag) };
    }
}

/// Relocate the saved entry-continuation root if it points at an evacuated
/// object (bliss-jtc.17).
fn relocate_entry_continuation(heap_base: usize, heap_end: usize) {
    let cell = entry_continuation_cell();
    let mut guard = cell.lock().unwrap();
    let mut v = *guard;
    // SAFETY: `&mut v` is a valid local EgclVal slot.
    unsafe { relocate_slot(&mut v as *mut EgclVal, heap_base, heap_end) };
    *guard = v;
}

/// Enumerate the `EgclVal` reference fields of a heap object precisely
/// (bliss-jtc.20). `body` points past the object header; `body_len` is the
/// object's exact body length in bytes; `type_id` selects the field layout
/// (spec §1.5–§1.16). Only genuine reference slots are visited — numeric limbs,
/// string characters, double-float bits, raw code/entry pointers, and CLOS
/// wrapper pointers are never handed to `visit`, so pointer-shaped payload data
/// can never be mistaken for a live reference. The `visit` closure decides which
/// visited values are actual heap references.
///
/// The `visit` closure receives a mutable pointer to each reference slot, so it
/// can both read (marking) and rewrite (evacuation relocation) the field.
///
/// Safety: `body` must point at a live object body of at least `body_len` bytes.
unsafe fn trace_object(
    body: *mut u8,
    type_id: u8,
    body_len: usize,
    mut visit: impl FnMut(*mut EgclVal),
) {
    use crate::object::type_id as tid;
    let words = body_len / 8;
    // Read body word `i` as a EgclVal. Safety: `i < words` keeps it in bounds.
    let word = |i: usize| unsafe { EgclVal(*(body as *const u64).add(i)) };
    let mut visit_word = |i: usize| {
        if i < words {
            // Safety: `i < words`, so the slot is within the object body.
            visit(unsafe { (body as *mut EgclVal).add(i) });
        }
    };
    match type_id {
        tid::MUTEX => {
            if words >= 3 && word(1).to_raw() & crate::object::mutex_flags::HAS_NAME_SLOT != 0 {
                visit_word(2);
            }
        }
        // ── Reference-free leaves: numbers and byte/character payloads. Their
        //    bodies are raw bits and must never be scanned for pointers. ──
        tid::BIGNUM
        | tid::DOUBLE_FLOAT
        | tid::RANDOM_STATE
        | tid::FOREIGN_POINTER
        | tid::FOREIGN_LIBRARY
        | tid::FOREIGN_CALLBACK
        | tid::PYTHON_OBJECT
        | tid::SIMPLE_BASE_STRING
        | tid::SIMPLE_CHARACTER_STRING => {}

        // Packed specialised arrays contain only an element tag, length and
        // raw numeric/bit payload. Pointer-shaped float bits are not roots.
        tid::SIMPLE_ARRAY => {}

        // ── Fixed reference pairs. ──
        tid::CONS => {
            // Real conses are headerless (§1.5.1); this covers a headered
            // cons-shaped object. car @0, cdr @1.
            visit_word(0);
            visit_word(1);
        }
        tid::RATIO => {
            visit_word(0); // numerator
            visit_word(1); // denominator
        }
        tid::COMPLEX => {
            visit_word(0); // realpart
            visit_word(1); // imagpart
        }

        // ── Simple vector: [length, elements...]. `build_vector` stores the
        //    length word RAW (a plain element count, not a fixnum-tagged value),
        //    so read it raw here — reading it as a fixnum would under-count and
        //    leave live elements untraced. ──
        tid::SIMPLE_VECTOR => {
            let n = if words >= 1 { word(0).0 as usize } else { 0 };
            for i in 0..n.min(words.saturating_sub(1)) {
                visit_word(1 + i);
            }
        }
        // ── Complex (fill-pointer / adjustable) vector: [storage-ref |
        //    fill-pointer | adjustable]. Only word 0 (the backing SIMPLE_VECTOR)
        //    is a heap reference; the fill pointer and flag are immediates. ──
        tid::COMPLEX_ARRAY => {
            visit_word(0);
        }
        // ── Multidimensional array: [storage-ref | dims-ref | rank]. Words 0
        //    (row-major SIMPLE_VECTOR) and 1 (dims SIMPLE_VECTOR) are heap
        //    references; word 2 (rank) is an immediate fixnum. ──
        tid::MD_ARRAY => {
            visit_word(0);
            visit_word(1);
        }

        // ── Symbol: name/value/function/plist/package are references; the
        //    trailing flags/binding_guard words are raw (§1.7). ──
        tid::SYMBOL => {
            for i in 0..5 {
                visit_word(i);
            }
        }

        // ── Interpreted function: lambda_list/body/env/name (§1.11.1); the
        //    counters, definition index and raw closure-state word follow. ──
        tid::FUNCTION_INTERPRETED => {
            for i in 0..FUNCTION_HEAP_REFERENCE_WORDS {
                visit_word(i);
            }
            if words >= 7 {
                let offset = core::mem::offset_of!(crate::object::FunctionData, definition_index)
                    - core::mem::size_of::<ObjectHeader>();
                let index = unsafe { *body.add(offset).cast::<u32>() };
                if index != 0 {
                    crate::symbols::trace_symbol_index(index, &mut visit);
                }
            }
            // The evaluator traces what the object owns off the heap: its
            // closure state, and (for a definition) its bytecode body. The
            // raw state word itself is never visited as a reference.
            if words * 8 >= FUNCTION_BODY_BYTES {
                if let Some(trace) = FUNCTION_CAPTURE_TRACE_FN.get() {
                    // SAFETY: a FUNCTION_INTERPRETED object is never large, so
                    // its header immediately precedes the body.
                    let function = unsafe { EgclVal::from_heap_ptr(body.sub(OBJECT_HEADER_SIZE)) };
                    trace(function, &mut visit);
                }
            }
            let _ = word;
        }
        // ── Compiled function (§1.11.2): entry_point and code_size are raw;
        //    name/lambda_list/constants are references. ──
        tid::COMPILED_FUNCTION => {
            visit_word(2); // name
            visit_word(3); // lambda_list
            visit_word(5); // constants
        }
        // ── Closure: function slot + the trailing closed-over variables. ──
        tid::CLOSURE => {
            for i in 0..words {
                visit_word(i);
            }
        }

        // ── Pathname: all six components are references (§1.14). ──
        tid::PATHNAME => {
            for i in 0..6 {
                visit_word(i);
            }
        }
        // ── Readtable (§1.15): word 0 is case_mode+padding; the four tables
        //    are references. ──
        tid::READTABLE => {
            for i in 1..5 {
                visit_word(i);
            }
        }
        // ── Restart: name/function/report/interactive/test (§1.16). ──
        tid::RESTART => {
            for i in 0..5 {
                visit_word(i);
            }
        }

        // ── CLOS instance: word 0 is the raw wrapper pointer (not a GC
        //    reference); every remaining inline word is a slot value (§1.10). ──
        tid::STANDARD_OBJECT => {
            for i in 1..words {
                visit_word(i);
            }
        }

        // ── Package: name, internal/external symbol tables, use-list, and
        //    nicknames are tagged references; the trailing lock is raw. ──
        tid::PACKAGE => {
            for i in 0..5 {
                visit_word(i);
            }
        }

        // ── Stream handle: body word 0 is a raw pointer to an off-heap block;
        //    its Lisp-visible component references live in that block and are
        //    traced by the stdlib-registered hook (egcl-jtc.7a). ──
        tid::STREAM => {
            if let Some(f) = STREAM_TRACE_FN.get() {
                f(body, &mut visit);
            }
        }

        // ── Kinds whose runtime layout interleaves references with raw fields
        //    or side storage (structures, conditions, hash-tables, specialised
        //    arrays) are traced by dedicated callbacks once
        //    real instances are constructed on the GC heap (jtc.1/jtc.2). None
        //    are allocated here yet, so visiting nothing is safe and precise —
        //    never a conservative pointer scan. ──
        _ => {}
    }
}

// ── Old-generation scan for minor collections (bliss-3uwzg) ───────
//
// A minor collection must find every older slot that refers into the nursery.
// Not every reference store runs the write barrier, so without help the
// collector has to trace the whole old generation (GitHub #471: ~2.9 ms per MB
// of live conses per minor collection). With kernel soft-dirty tracking armed
// (`gc_soft_dirty`), only pages written since the previous minor collection can
// hold such a slot: the scan visits the objects overlapping those pages and,
// within them, only the slots on dirty pages. Objects whose trace reaches
// off-heap storage (a stream's state block, an interpreted function's captured
// environment) can change without a heap write, so they are traced on every
// collection from a per-region list kept between collections.

/// Leading body words of an interpreted function that hold heap references
/// (lambda_list, body, env, name); the rest are counters and raw metadata.
const FUNCTION_HEAP_REFERENCE_WORDS: usize = 4;

/// Body bytes of a current interpreted function object.
const FUNCTION_BODY_BYTES: usize =
    core::mem::size_of::<crate::object::FunctionData>() - core::mem::size_of::<ObjectHeader>();

/// Body offset of an interpreted function's raw closure-state word.
const FUNCTION_CLOSURE_STATE_OFFSET: usize =
    core::mem::offset_of!(crate::object::FunctionData, closure_state)
        - core::mem::size_of::<ObjectHeader>();

/// Visit only the heap reference words of an interpreted function, leaving its
/// off-heap captures to the remembered-capture scanner (bliss-gg3i1.7.9).
///
/// # Safety
/// `body` must be the body of a live FUNCTION_INTERPRETED object of
/// `body_len` bytes.
unsafe fn trace_function_heap_words(
    body: *mut u8,
    body_len: usize,
    mut visit: impl FnMut(*mut EgclVal),
) {
    for i in 0..FUNCTION_HEAP_REFERENCE_WORDS.min(body_len / 8) {
        visit(unsafe { (body as *mut EgclVal).add(i) });
    }
}

/// Whether `trace_object` reaches slots outside the object's own heap words for
/// this type — storage whose changes no heap page records.
#[inline]
fn traces_off_heap(type_id: u8) -> bool {
    use crate::object::type_id as tid;
    matches!(type_id, tid::STREAM | tid::FUNCTION_INTERPRETED)
}

/// Older-generation scan state kept across minor collections.
#[derive(Default)]
struct OldGenScan {
    dirty: crate::gc_soft_dirty::DirtyPages,
    ranges: Vec<(usize, usize)>,
    /// Older slots (heap or off-heap) that referred into the nursery; the
    /// relocation pass rewrites exactly these.
    slots: Vec<usize>,
    /// Headers of older forwarding stubs (CHANGE-CLASS) whose target is young.
    stubs: Vec<usize>,
    /// Each region's `alloc_top` before evacuation; objects above it were
    /// copied by this collection and need their fields relocated.
    pre_tops: Vec<usize>,
    /// Per region: headers of `traces_off_heap` objects, and the (kind, top)
    /// the list was built for. Valid only for the heap in `cache_heap`.
    off_heap: Vec<Vec<usize>>,
    seen: Vec<Option<(RegionKind, usize)>>,
    cache_heap: (usize, usize),
    /// Diagnostics for `EGCL_GC_REGION_LOG`: regions walked and objects traced.
    walked_regions: u64,
    traced_objects: u64,
}

/// Global, not thread-local: whichever thread collects next must see the lists
/// the previous collection left.
static OLD_GEN_SCAN: std::sync::Mutex<Option<OldGenScan>> = std::sync::Mutex::new(None);

/// Bytes a whole-generation minor scan would walk.
fn old_generation_bytes(state: &HeapState) -> usize {
    state
        .regions
        .iter()
        .filter(|r| is_old_scan_kind(r.header.kind))
        .map(|r| (r.header.alloc_top as usize).saturating_sub(r.base as usize))
        .sum()
}

/// Exact nonempty older-region extents, including unused tails. An allocation
/// into an existing older region must dirty a previously armed page. Old region
/// recycling happens only during major/reset paths, which disarm tracking; a
/// kind transition/new extent is a new token and initially reads fully dirty.
fn older_tracking_regions(state: &HeapState) -> Vec<crate::gc_async_wp::Range> {
    state
        .regions
        .iter()
        .filter_map(|region| {
            (is_old_scan_kind(region.header.kind)
                && region.header.alloc_top as usize > region.base as usize)
                .then_some(crate::gc_async_wp::Range {
                    start: region.base as usize,
                    // Large-object heads can extend over continuation regions
                    // whose alloc_top is empty. Cover the entire used tail too.
                    end: (region.header.alloc_limit as usize).max(
                        (region.header.alloc_top as usize)
                            .saturating_add(crate::syscall::page_size() - 1)
                            & !(crate::syscall::page_size() - 1),
                    ),
                    kind: region.header.kind as u8,
                })
        })
        .collect()
}

#[inline]
fn is_old_scan_kind(kind: RegionKind) -> bool {
    matches!(
        kind,
        RegionKind::OldGen | RegionKind::Survivor | RegionKind::LargeObject
    )
}

impl OldGenScan {
    fn take() -> OldGenScan {
        OLD_GEN_SCAN
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_default()
    }

    /// Rebuild every region's list after a major collection moved older
    /// objects, so the next minor collection need not walk the whole old
    /// generation to find them again. One header walk, in the major pause.
    fn rebuild_regions(state: &HeapState) {
        let mut guard = OLD_GEN_SCAN.lock().unwrap_or_else(|e| e.into_inner());
        let scan = guard.get_or_insert_with(OldGenScan::default);
        scan.cache_heap = (state.heap_base as usize, state.config.heap_size);
        scan.off_heap.resize_with(state.regions.len(), Vec::new);
        scan.seen.resize(state.regions.len(), None);
        for (idx, region) in state.regions.iter().enumerate() {
            let kind = region.header.kind;
            if !is_old_scan_kind(kind) {
                scan.forget_region(idx);
                continue;
            }
            scan.start_region(idx);
            let top = region.header.alloc_top as usize;
            let mut cursor = region.base as usize;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *const u8;
                // SAFETY: walking valid object headers below the region top.
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                if body_size == 0 && type_id == 0 {
                    break;
                }
                if !unsafe { header_is_forwarded(header_ptr) } {
                    scan.note_object(idx, cursor, type_id);
                }
                cursor += unsafe { header_total_bytes(header_ptr) };
            }
            scan.finish_region(idx, kind, top);
        }
    }

    fn put_back(self) {
        *OLD_GEN_SCAN.lock().unwrap_or_else(|e| e.into_inner()) = Some(self);
    }

    /// Reset per-collection scratch and size the per-region lists. Returns
    /// true when the dirty pages of every older region are known, i.e. the
    /// scan may be limited to them.
    fn begin(&mut self, state: &HeapState) -> bool {
        let heap = (state.heap_base as usize, state.config.heap_size);
        let count = state.regions.len();
        self.slots.clear();
        self.stubs.clear();
        self.walked_regions = 0;
        self.traced_objects = 0;
        if self.cache_heap != heap || self.seen.len() != count {
            self.cache_heap = heap;
            self.off_heap.clear();
            self.off_heap.resize_with(count, Vec::new);
            self.seen.clear();
            self.seen.resize(count, None);
        }
        self.ranges.clear();
        for region in state.regions.iter() {
            let base = region.base as usize;
            let top = region.header.alloc_top as usize;
            if !is_old_scan_kind(region.header.kind) || top <= base {
                continue;
            }
            match self.ranges.last_mut() {
                Some((_, end)) if *end == base => *end = top,
                _ => self.ranges.push((base, top)),
            }
        }
        crate::gc_soft_dirty::read_dirty_pages(
            heap.0,
            heap.1,
            &self.ranges,
            &older_tracking_regions(state),
            &mut self.dirty,
        )
    }

    /// Record the off-heap-tracing object at `header` in region `idx`.
    fn note_object(&mut self, idx: usize, header: usize, type_id: u8) {
        if traces_off_heap(type_id) {
            self.off_heap[idx].push(header);
        }
    }

    /// Start (re)building region `idx`'s list during a walk of the region.
    fn start_region(&mut self, idx: usize) {
        self.off_heap[idx].clear();
    }

    fn finish_region(&mut self, idx: usize, kind: RegionKind, top: usize) {
        self.seen[idx] = Some((kind, top));
    }

    /// Forget every region the scan did not walk or validate.
    fn forget_region(&mut self, idx: usize) {
        self.off_heap[idx].clear();
        self.seen[idx] = None;
    }

    /// After evacuation: extend each region's list over the objects this
    /// collection copied into it, so a region that only received copies (which
    /// leave no dirty page) is not walked again by the next collection.
    fn note_copies(&mut self, state: &HeapState) {
        for (idx, region) in state.regions.iter().enumerate() {
            let kind = region.header.kind;
            let base = region.base as usize;
            let top = region.header.alloc_top as usize;
            let Some(&before) = self.pre_tops.get(idx) else {
                continue;
            };
            if !is_old_scan_kind(kind) || top <= before {
                continue;
            }
            match self.seen[idx] {
                Some((seen_kind, seen_top)) if seen_kind == kind && seen_top == before => {}
                None if before == base => self.off_heap[idx].clear(),
                _ => continue,
            }
            let mut cursor = before;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *const u8;
                // SAFETY: objects this collection copied below the region top.
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                if body_size == 0 && type_id == 0 {
                    break;
                }
                self.note_object(idx, cursor, type_id);
                cursor += unsafe { header_total_bytes(header_ptr) };
            }
            self.seen[idx] = Some((kind, top));
        }
    }

    fn snapshot_tops(&mut self, state: &HeapState) {
        self.pre_tops.clear();
        self.pre_tops.extend(state.regions.iter().map(|r| {
            if r.header.kind == RegionKind::Free {
                r.base as usize
            } else {
                r.header.alloc_top as usize
            }
        }));
    }
}

impl HeapCollector {
    /// Precise CL-stack roots (nmq.3): mark exactly the heap references held in
    /// every live CL frame of every green thread. Each frame slot is a tagged
    /// `EgclVal`, so references are found by tag and non-references are never
    /// pinned. The current thread uses its live frame pointer; parked threads
    /// use the frame pointer they published at their safepoint.
    fn scan_cl_stack_roots(mut visit: impl FnMut(EgclVal)) {
        let mut mark_from = |fp: *const crate::stack::Frame| {
            // SAFETY: `fp` is a valid frame chain (live or published).
            unsafe {
                crate::stack::visit_stack_refs(fp, |slot| {
                    visit(*slot);
                });
            }
        };
        let cur = crate::thread::current_thread_id();
        let current_fiber = crate::thread::current_fiber_id();
        mark_from(crate::thread::current_stack().fp());
        for id in crate::thread::all_thread_ids() {
            if id == cur {
                continue;
            }
            if let Some(fp) = crate::thread::thread_published_fp(id) {
                mark_from(fp);
            }
        }
        for id in crate::thread::all_fiber_ids() {
            if Some(id) == current_fiber {
                continue;
            }
            if let Some(fp) = crate::thread::fiber_published_fp(id) {
                mark_from(fp);
            }
        }
    }

    /// Relocate CL-stack references to evacuated objects (nmq.3): walk every CL
    /// frame and, for each heap reference whose object has been forwarded,
    /// rewrite the frame slot to the object's new location (preserving the tag).
    /// Must run after copying installs forwarding pointers and before the source
    /// regions are zeroed. `heap_base`/`heap_size` bound the check so a frame
    /// slot pointing outside the managed heap is never dereferenced.
    fn relocate_cl_stack_refs(heap_base: usize, heap_size: usize) {
        let heap_end = heap_base + heap_size;
        let mut chase = |slot: &mut crate::value::EgclVal| {
            // SAFETY: `slot` is a live frame slot.
            unsafe { relocate_slot(slot as *mut EgclVal, heap_base, heap_end) };
        };
        let mut relocate_from = |fp: *const crate::stack::Frame| {
            // SAFETY: `fp` is a valid frame chain.
            unsafe { crate::stack::visit_stack_refs(fp, &mut chase) };
        };
        let cur = crate::thread::current_thread_id();
        let current_fiber = crate::thread::current_fiber_id();
        relocate_from(crate::thread::current_stack().fp());
        for id in crate::thread::all_thread_ids() {
            if id == cur {
                continue;
            }
            if let Some(fp) = crate::thread::thread_published_fp(id) {
                relocate_from(fp);
            }
        }
        for id in crate::thread::all_fiber_ids() {
            if Some(id) == current_fiber {
                continue;
            }
            if let Some(fp) = crate::thread::fiber_published_fp(id) {
                relocate_from(fp);
            }
        }
    }

    /// Soft-dirty half of a minor collection's old-generation scan
    /// (bliss-3uwzg): hand `visit` every older reference slot that may have
    /// been written since the previous minor collection. Regions with no
    /// dirty page cost only their off-heap-tracing objects; in a region with
    /// dirty pages, objects that overlap one are traced, restricted to the
    /// slots on dirty pages. Young targets of forwarding stubs on dirty pages
    /// are marked through `visit` as well, and the stubs recorded for
    /// relocation.
    fn scan_dirty_old_generation(
        state: &HeapState,
        scan: &mut OldGenScan,
        captures_remembered: bool,
        mut visit: impl FnMut(*mut EgclVal, &mut OldGenScan),
    ) {
        // With remembered captures, an old interpreted function on a clean
        // page has nothing to visit: its heap words are unwritten and the
        // evaluator's scanner covers every capture written this epoch.
        let skip_off_heap = |type_id: u8| {
            captures_remembered && type_id == crate::object::type_id::FUNCTION_INTERPRETED
        };
        let dirty = std::mem::take(&mut scan.dirty);
        for (idx, region) in state.regions.iter().enumerate() {
            let kind = region.header.kind;
            if !is_old_scan_kind(kind) {
                scan.forget_region(idx);
                continue;
            }
            let base = region.base as usize;
            let top = region.header.alloc_top as usize;
            if scan.seen[idx] == Some((kind, top)) && !dirty.any_dirty(base, top) {
                // Unwritten since the last scan: only off-heap storage can
                // have changed.
                for i in 0..scan.off_heap[idx].len() {
                    let header_ptr = scan.off_heap[idx][i] as *const u8;
                    // SAFETY: recorded live object headers in an unwritten region.
                    let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                    if skip_off_heap(type_id) {
                        continue;
                    }
                    let body = unsafe { header_ptr.add(body_offset(header_ptr)) } as *mut u8;
                    unsafe {
                        trace_object(body, type_id, body_size as usize, |slot| visit(slot, scan))
                    };
                }
                continue;
            }
            scan.start_region(idx);
            scan.walked_regions += 1;
            let mut cursor = base;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *const u8;
                // SAFETY: walking valid object headers below the region top.
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                if body_size == 0 && type_id == 0 {
                    break;
                }
                let total_size = unsafe { header_total_bytes(header_ptr) };
                let written = dirty.any_dirty(cursor, cursor + total_size);
                if unsafe { header_is_forwarded(header_ptr) } {
                    if written {
                        // A CHANGE-CLASS stub: its forwarding word is a raw
                        // body pointer that may name a young object.
                        let mut stub = EgclVal(cursor as u64 | crate::value::TAG_HEAP_OBJECT);
                        let before = scan.slots.len();
                        visit(&mut stub as *mut EgclVal, scan);
                        if scan.slots.len() > before {
                            scan.slots.truncate(before);
                            scan.stubs.push(cursor);
                        }
                    }
                } else if written && skip_off_heap(type_id) {
                    // Its captures are the remembered scanner's; only the
                    // written heap words can hold a new young reference.
                    scan.note_object(idx, cursor, type_id);
                    scan.traced_objects += 1;
                    let body = unsafe { header_ptr.add(body_offset(header_ptr)) } as *mut u8;
                    unsafe {
                        trace_function_heap_words(body, body_size as usize, |slot| {
                            if dirty.is_dirty(slot as usize) {
                                visit(slot, scan);
                            }
                        })
                    };
                } else {
                    scan.note_object(idx, cursor, type_id);
                    let off_heap = traces_off_heap(type_id) && !skip_off_heap(type_id);
                    if written || off_heap {
                        scan.traced_objects += 1;
                        let body = unsafe { header_ptr.add(body_offset(header_ptr)) } as *mut u8;
                        unsafe {
                            trace_object(body, type_id, body_size as usize, |slot| {
                                if off_heap || dirty.is_dirty(slot as usize) {
                                    visit(slot, scan);
                                }
                            })
                        };
                    }
                }
                cursor += total_size;
            }
            scan.finish_region(idx, kind, top);
        }
        scan.dirty = dirty;
    }

    /// Relocation half of a soft-dirty minor collection (bliss-3uwzg): rewrite
    /// the older slots and stubs the scan found, the fields of every object
    /// this collection copied, and the fields of retained (pinned) regions —
    /// the only places outside the roots that can still refer into the nursery.
    fn relocate_minor_targets(
        state: &HeapState,
        scan: &OldGenScan,
        pinned: &[usize],
        heap_base: usize,
        heap_end: usize,
        captures_remembered: bool,
    ) {
        for &slot in &scan.slots {
            // SAFETY: slots recorded by this collection's scan; the world is
            // stopped, so they are still live reference slots.
            unsafe { relocate_slot(slot as *mut EgclVal, heap_base, heap_end) };
        }
        for &stub in &scan.stubs {
            let header_ptr = stub as *mut u8;
            // SAFETY: a forwarded older object recorded by the scan.
            unsafe {
                let target = header_forwarding_addr(header_ptr) as usize;
                let mut v =
                    EgclVal((target - OBJECT_HEADER_SIZE) as u64 | crate::value::TAG_HEAP_OBJECT);
                relocate_slot(&mut v, heap_base, heap_end);
                *(header_ptr.add(body_offset(header_ptr)) as *mut usize) = ref_body_addr(v);
            }
        }
        let relocate_range = |start: usize, top: usize| {
            let mut cursor = start;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *mut u8;
                // SAFETY: objects copied or retained by this collection.
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                if body_size == 0 && type_id == 0 {
                    break;
                }
                let total = unsafe { header_total_bytes(header_ptr) };
                if !unsafe { header_is_forwarded(header_ptr) } {
                    let body = unsafe { header_ptr.add(body_offset(header_ptr)) };
                    if captures_remembered
                        && type_id == crate::object::type_id::FUNCTION_INTERPRETED
                    {
                        // As in the scan: the remembered-capture scanner
                        // relocates every capture written this epoch, and an
                        // older one refers to no nursery object. Tracing the
                        // captures here as well walked the frames and bodies
                        // of every function in a retained or newly filled
                        // region on each minor collection (bliss-gg3i1.7.12).
                        unsafe {
                            trace_function_heap_words(body, body_size as usize, |slot| {
                                relocate_slot(slot, heap_base, heap_end);
                            });
                        }
                    } else {
                        unsafe {
                            trace_object(body, type_id, body_size as usize, |slot| {
                                relocate_slot(slot, heap_base, heap_end);
                            });
                        }
                    }
                }
                cursor += total;
            }
        };
        for (idx, region) in state.regions.iter().enumerate() {
            if !is_old_scan_kind(region.header.kind) {
                continue;
            }
            let top = region.header.alloc_top as usize;
            if pinned.contains(&idx) {
                relocate_range(region.base as usize, top);
            } else if let Some(&before) = scan.pre_tops.get(idx) {
                if top > before {
                    relocate_range(before, top);
                }
            }
        }
    }

    /// Rewrite every reference field of every surviving object to its referent's
    /// forwarded location (bliss-jtc.17). Walks all old-gen/survivor/large-object
    /// regions — which after evacuation hold the live survivors *and* the fresh
    /// copies — and, for each non-forwarded object, traces its reference fields
    /// (precisely, by type_id) and relocates any that point at an evacuated
    /// object. Forwarded (stale) originals are skipped; their storage is about to
    /// be reclaimed. Must run after all copying installs forwarding and before
    /// any region is freed, so no live field is left pointing at a stale location.
    /// `skip_remembered_captures`: a minor collection whose remembered-capture
    /// scanner relocates every capture written this epoch (an older capture
    /// refers to no nursery object) relocates only interpreted functions' heap
    /// words, as its old-generation scan marks only them. A major collection
    /// moves older objects too and must pass `false`.
    fn relocate_object_fields(
        state: &HeapState,
        heap_base: usize,
        heap_end: usize,
        skip_remembered_captures: bool,
    ) {
        for region in state.regions.iter() {
            if !matches!(
                region.header.kind,
                RegionKind::OldGen | RegionKind::Survivor | RegionKind::LargeObject
            ) {
                continue;
            }
            let base = region.base as usize;
            let top = region.header.alloc_top as usize;
            let mut cursor = base;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *mut u8;
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                if body_size == 0 && type_id == 0 {
                    break;
                }
                let total = align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                if !unsafe { header_is_forwarded(header_ptr) } {
                    // SAFETY: a valid live object header at `header_ptr`.
                    let body = unsafe { header_ptr.add(body_offset(header_ptr)) };
                    if skip_remembered_captures
                        && type_id == crate::object::type_id::FUNCTION_INTERPRETED
                    {
                        unsafe {
                            trace_function_heap_words(body, body_size as usize, |slot| {
                                relocate_slot(slot, heap_base, heap_end);
                            });
                        }
                    } else {
                        unsafe {
                            trace_object(body, type_id, body_size as usize, |slot| {
                                relocate_slot(slot, heap_base, heap_end);
                            });
                        }
                    }
                }
                cursor += total;
            }
        }
    }

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
        needed: usize,
    ) -> Option<usize> {
        #[cfg(test)]
        evacuation_target_tests::TARGET_SEARCHES.with(|count| count.set(count.get() + 1));
        // First, look for an existing region of the right kind with space for
        // the OBJECT BEING PLACED. The old fixed `> header + alignment` test
        // could return a region with a sliver of free space smaller than the
        // object; copy_object would then fail, the retry would pick the same
        // region again, and the live object was silently dropped even with
        // Free regions available (bliss-wc4t).
        for (idx, region) in state.regions.iter().enumerate() {
            if region.header.kind == kind {
                // Never place ordinary (reclaimable) data in a pinned-host
                // region: the pins make the region unreclaimable, so anything
                // co-located with them becomes permanent garbage (bliss-wc4t).
                if state.pinned_hosts.contains(&idx) {
                    continue;
                }
                let top = region.header.alloc_top as usize;
                let limit = region.header.alloc_limit as usize;
                if limit.saturating_sub(top) >= needed {
                    return Some(idx);
                }
            }
        }
        Self::allocate_target_region(state, kind, gen_age)
    }

    /// Whether a minor collection can place every marked, unpinned nursery
    /// object. It replays the evacuation loop's placement — the current
    /// destination, then the first region of the kind with room, then a Free
    /// region — over a copy of the region rooms, so it agrees with the copy
    /// that follows. When enough regions are Free that even an all-live nursery
    /// fits, the replay is skipped.
    fn evacuation_fits(
        state: &HeapState,
        nursery_indices: &[usize],
        pinned_indices: &[usize],
        nursery_index: &NurseryObjectMap,
        survivor_idx: Option<usize>,
        promotion_threshold: u8,
    ) -> bool {
        let region_size = state.config.region_size;
        let largest_object = state.config.tlab_size;
        // Free regions evacuation may take; the emergency reserve is not one.
        let free = state
            .regions
            .iter()
            .filter(|r| r.header.kind == RegionKind::Free)
            .count()
            .saturating_sub(emergency_reserve_regions(state));
        let evacuated = nursery_indices
            .iter()
            .filter(|idx| !pinned_indices.contains(idx))
            .count();
        // Every destination region but the two open ones is filled to within
        // one object of its end, so an all-live nursery needs at most this many.
        if largest_object < region_size {
            let worst_case = (evacuated * region_size).div_ceil(region_size - largest_object) + 2;
            if free >= worst_case {
                return true;
            }
        }

        let mut kinds: Vec<RegionKind> = state.regions.iter().map(|r| r.header.kind).collect();
        let mut room: Vec<usize> = state
            .regions
            .iter()
            .map(|r| (r.header.alloc_limit as usize).saturating_sub(r.header.alloc_top as usize))
            .collect();
        // [survivor, old-generation] destinations, as in the evacuation loop.
        let mut current = [survivor_idx, None];
        let mut fresh = 0usize;
        for &nursery_idx in nursery_indices {
            if pinned_indices.contains(&nursery_idx) {
                continue;
            }
            let (kind, slot) = if state.regions[nursery_idx].header.gen_age >= promotion_threshold {
                (RegionKind::OldGen, 1)
            } else {
                (RegionKind::Survivor, 0)
            };
            for body_addr in nursery_index.marked_bodies(nursery_idx) {
                let header_ptr = (body_addr - OBJECT_HEADER_SIZE) as *const u8;
                let (_, body_size) = unsafe { read_object_header(header_ptr) };
                let size = align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                if let Some(idx) = current[slot]
                    && room[idx] >= size
                {
                    room[idx] -= size;
                    continue;
                }
                let existing = (0..kinds.len()).find(|&idx| {
                    kinds[idx] == kind && !state.pinned_hosts.contains(&idx) && room[idx] >= size
                });
                let target = existing.or_else(|| {
                    if fresh == free {
                        return None;
                    }
                    fresh += 1;
                    let idx = kinds.iter().position(|&k| k == RegionKind::Free)?;
                    kinds[idx] = kind;
                    let region = &state.regions[idx];
                    room[idx] =
                        (region.header.alloc_limit as usize).saturating_sub(region.base as usize);
                    Some(idx)
                });
                match target {
                    Some(idx) if room[idx] >= size => {
                        room[idx] -= size;
                        current[slot] = Some(idx);
                    }
                    _ => return false,
                }
            }
        }
        true
    }

    /// Start a fresh evacuation destination. Major collections must not copy
    /// into another source region that may itself be evacuated and freed.
    fn allocate_target_region(
        state: &mut HeapState,
        kind: RegionKind,
        gen_age: u8,
    ) -> Option<usize> {
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
    /// Also installs a forwarding pointer at the old location: sets the
    /// FORWARDED gc-bit (preserving type_id and size) and writes the new body
    /// pointer into the object's first payload word.
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

        // Install forwarding at the old location: set the FORWARDED gc-bit and
        // store the new body pointer in the first payload word. type_id and size
        // stay intact so the heap walker can stride over the stale copy. The
        // minimum object is 16 bytes (header + one aligned word), so there is
        // always room for the pointer.
        unsafe {
            header_set_forwarded(source_header, new_body);
        }

        Some(new_body)
    }
}

impl Default for HeapCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl HeapCollector {
    /// Stop-the-world minor (nursery) collection.
    /// Cheney-style scavenge: copies live nursery objects into survivor space
    /// or promotes to old-gen based on gen_age vs promotion_threshold.
    fn minor_gc_stw_body(&mut self) -> Result<(), EgclError> {
        let start = std::time::Instant::now();
        let mut phases = MinorPhaseLog::new(start);

        let mut guard = heap_state().lock().unwrap();
        let state = guard
            .as_mut()
            .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;

        let promotion_threshold = self.promotion_threshold;
        // Before any root scan: both external-root passes and the old scan
        // below must agree on who visits old functions' captures.
        let (_captures_scope, captures_remembered) = CapturesRememberedScope::enter();

        // Phase 1: Ensure we have a survivor region to copy into.
        let mut survivor_idx = Self::find_or_create_target_region(
            state,
            RegionKind::Survivor,
            1,
            OBJECT_HEADER_SIZE + OBJECT_ALIGNMENT,
        );
        let mut old_gen_idx = None;

        // Phase 2: Build the precise young-generation live set.  A nursery
        // collection must not copy every allocated object: doing so merely
        // turns short-lived garbage into old-generation garbage and makes an
        // allocation loop consume the whole heap.  Index nursery objects, mark
        // them from the runtime roots and older generations, then follow young
        // object fields transitively.
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

        // Record exact starts while repairing TLAB gaps and finding retained
        // (pinned) regions. Headers remain intact until evacuation; one walk
        // per region supplies all three facts. Only regions that can hold a
        // pinned object are walked up front; any other region is walked when
        // marking first reaches it, so dead regions are never read
        // (bliss-xyt4l). Every pin of a nursery object records its region in
        // `nursery_pin_candidates` (see `note_nursery_pin`).
        let mut nursery_index = MINOR_OBJECT_MAP.with(|s| std::mem::take(&mut *s.borrow_mut()));
        let nursery_tops: Vec<usize> = nursery_indices
            .iter()
            .map(|&idx| state.regions[idx].header.alloc_top as usize)
            .collect();
        // SAFETY: mutators are stopped and these are allocated nursery regions
        // through their tops; marking (which indexes them) ends before
        // evacuation, and prepare_region repairs their zero-filled TLAB gaps.
        unsafe {
            nursery_index.reset_lazy(
                state.heap_base as usize,
                state.config.region_size,
                state.regions.len(),
                &nursery_indices,
                &nursery_tops,
            );
        }
        let pin_candidates = std::mem::take(&mut state.nursery_pin_candidates);
        // EGCL_GC_VERIFY walks every region, as before, and checks that the
        // side list named each region a pin was found in.
        let index_all = gc_verify_enabled();
        let mut pinned_indices = Vec::new();
        for &idx in &nursery_indices {
            let candidate = pin_candidates.contains(&idx);
            if (candidate || index_all) && nursery_index.ensure_indexed(idx) {
                assert!(
                    candidate,
                    "gc-verify(pin-candidates): nursery region {idx} holds a pinned object \
                     its pin never recorded (bliss-xyt4l)"
                );
                pinned_indices.push(idx);
            }
        }

        // Reused across collections with retained capacity (bliss-5i3f).
        let mut mark_worklist = MINOR_WORKLIST.with(|s| std::mem::take(&mut *s.borrow_mut()));
        mark_worklist.clear();
        let mark_ref =
            |v: EgclVal, nursery_index: &mut NurseryObjectMap, worklist: &mut Vec<usize>| {
                if is_heap_ref(v) {
                    // Resolve PERSISTENT forwarding (a CHANGE-CLASS stub) before
                    // the index lookup: the stub's header is forwarded so the
                    // index excludes it, and without resolution the migrated
                    // TARGET was never marked — relocate would then rewrite
                    // stub-holding slots to a dead nursery address (bliss-334;
                    // caught by gc-verify's holder trace).
                    let v = resolve_forwarded(v);
                    let target = ref_body_addr(v);
                    if nursery_index.mark(target) {
                        worklist.push(target);
                    }
                }
            };

        phases.mark(MinorPhase::SetupIndex);

        // Direct roots shared with the major collector.
        mark_ref(
            get_entry_continuation(),
            &mut nursery_index,
            &mut mark_worklist,
        );
        let root_clock = root_phase::start();
        Self::scan_cl_stack_roots(|v| mark_ref(v, &mut nursery_index, &mut mark_worklist));
        root_phase::finish(
            root_clock,
            b"minor_mark_cl_stack",
            state.stats.minor_gc_count + 1,
            0,
        );
        let tracing = gc_verify_enabled();
        if tracing {
            VERIFY_TRACE.with(|t| {
                let mut t = t.borrow_mut();
                t.mark.clear();
                t.reloc.clear();
            });
        }
        let root_clock = root_phase::start();
        crate::symbols::for_each_root_slot(|slot| {
            let v = unsafe { *slot };
            if tracing {
                VERIFY_TRACE.with(|t| t.borrow_mut().mark.insert(slot as usize, v.0));
            }
            mark_ref(v, &mut nursery_index, &mut mark_worklist);
        });
        root_phase::finish(
            root_clock,
            b"minor_mark_symbols",
            state.stats.minor_gc_count + 1,
            0,
        );
        scan_external_mark_roots(ExternalRootScanKind::MinorMark, |slot| {
            let v = unsafe { *slot };
            if tracing {
                VERIFY_TRACE.with(|t| t.borrow_mut().mark.insert(slot as usize, v.0));
            }
            mark_ref(v, &mut nursery_index, &mut mark_worklist);
        });

        // Barrier-recorded old-to-young slots are roots even when their holder
        // has an opaque/untyped layout that `trace_object` cannot inspect.
        let heap_base_addr = state.heap_base as usize;
        let heap_end = heap_base_addr + state.config.heap_size;
        for &slot_addr in &state.remembered {
            // A slot in a nursery holder is no root (see `write_barrier`); a
            // live holder is traced, a dead one must not keep its referent.
            if slot_addr >= heap_base_addr
                && slot_addr + 8 <= heap_end
                && !slot_in_nursery(state, slot_addr)
            {
                mark_ref(
                    unsafe { *(slot_addr as *const EgclVal) },
                    &mut nursery_index,
                    &mut mark_worklist,
                );
            }
        }

        phases.mark(MinorPhase::RootsMark);

        // Scan non-nursery objects for young references.  The remembered set
        // is the fast-path record of old-to-young stores, but not every store
        // runs the barrier, and survivor-to-nursery edges need the same cover.
        // With soft-dirty tracking armed, only pages written since the previous
        // minor collection are scanned (bliss-3uwzg); otherwise every older
        // object is traced, as before.
        let mut old_scan = OldGenScan::take();
        let dirty_scan = old_scan.begin(state);
        #[cfg(test)]
        if dirty_scan {
            old_scan_tests::DIRTY_SCANS.with(|count| count.set(count.get() + 1));
        }
        if dirty_scan {
            Self::scan_dirty_old_generation(
                state,
                &mut old_scan,
                captures_remembered,
                |slot, scan| {
                    // SAFETY: the scan hands out live, readable reference slots.
                    let v = unsafe { *slot };
                    if !is_heap_ref(v) {
                        return;
                    }
                    let target = ref_body_addr(resolve_forwarded(v));
                    if nursery_index.position(target).is_some() {
                        scan.slots.push(slot as usize);
                        if nursery_index.mark(target) {
                            mark_worklist.push(target);
                        }
                    }
                },
            );
        }
        for (region_idx, region) in state.regions.iter().enumerate() {
            if dirty_scan {
                break;
            }
            if !matches!(
                region.header.kind,
                RegionKind::OldGen | RegionKind::Survivor | RegionKind::LargeObject
            ) {
                old_scan.forget_region(region_idx);
                continue;
            }
            old_scan.start_region(region_idx);
            let mut cursor = region.base as usize;
            let top = region.header.alloc_top as usize;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *const u8;
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                if body_size == 0 && type_id == 0 {
                    break;
                }
                // Stride and payload must honour the large-object 16-byte header
                // (bliss-tjru): a large SIMPLE_VECTOR's payload is at cursor+16,
                // and its footprint is the stored u64 total, not
                // align_up(8+body). Reading fields at cursor+8 would treat the
                // size-extension word as the element count and scan far past the
                // object.
                let total_size = unsafe { header_total_bytes(header_ptr) };
                if !unsafe { header_is_forwarded(header_ptr) } {
                    old_scan.note_object(region_idx, cursor, type_id);
                    let body_off = unsafe { body_offset(header_ptr) };
                    let body = (cursor + body_off) as *mut u8;
                    if captures_remembered
                        && type_id == crate::object::type_id::FUNCTION_INTERPRETED
                    {
                        // The remembered-capture scanner marks every capture
                        // written this epoch; an older capture refers to no
                        // nursery object. Tracing every old function's
                        // captures here made each minor collection walk the
                        // frames and bodies of every live function
                        // (bliss-gg3i1.7.12), which the dirty-page scan
                        // already avoids.
                        unsafe {
                            trace_function_heap_words(body, body_size as usize, |slot| {
                                mark_ref(*slot, &mut nursery_index, &mut mark_worklist)
                            });
                        }
                    } else {
                        unsafe {
                            trace_object(body, type_id, body_size as usize, |slot| {
                                mark_ref(*slot, &mut nursery_index, &mut mark_worklist)
                            });
                        }
                    }
                }
                cursor += total_size;
            }
            old_scan.finish_region(region_idx, region.header.kind, top);
        }

        if gc_region_log_enabled() {
            crate::syscall::dbg_write(if dirty_scan {
                b"[gc-regions] minor old scan: dirty pages, walked "
            } else {
                b"[gc-regions] minor old scan: whole old generation, walked "
            });
            write_decimal(old_scan.walked_regions);
            crate::syscall::dbg_write(b" regions, traced ");
            write_decimal(old_scan.traced_objects);
            crate::syscall::dbg_write(b" objects, ");
            write_decimal(old_scan.slots.len() as u64);
            crate::syscall::dbg_write(b" young refs\n");
        }

        phases.mark(MinorPhase::OldScan);

        // A pinned nursery region stays in place as a unit.  Treat every object
        // in it as live so its outbound references are retained and relocated;
        // otherwise an unmarked neighbour could keep a stale pointer after the
        // region is promoted wholesale.
        for &nursery_idx in &pinned_indices {
            let mut cursor = state.regions[nursery_idx].base as usize;
            let top = state.regions[nursery_idx].header.alloc_top as usize;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let (type_id, body_size) = unsafe { read_object_header(cursor as *const u8) };
                if type_id == 0 && body_size == 0 {
                    break;
                }
                let body = cursor + OBJECT_HEADER_SIZE;
                if nursery_index.mark(body) {
                    mark_worklist.push(body);
                }
                cursor += align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
            }
        }

        // Young-to-young transitive closure.
        while let Some(body_addr) = mark_worklist.pop() {
            // Worklist entries are indexed nursery bodies, and no evacuation
            // has begun yet: their headers still contain the original layout.
            unsafe {
                let (type_id, body_len) =
                    read_object_header((body_addr - OBJECT_HEADER_SIZE) as *const u8);
                trace_object(body_addr as *mut u8, type_id, body_len as usize, |slot| {
                    mark_ref(*slot, &mut nursery_index, &mut mark_worklist);
                });
            }
        }

        phases.mark(MinorPhase::YoungMark);

        // Nothing has moved yet. If the survivors cannot all be placed, stop
        // here with the heap intact and report exhaustion, rather than panic
        // halfway through evacuation (GitHub #471 part 3).
        if !Self::evacuation_fits(
            state,
            &nursery_indices,
            &pinned_indices,
            &nursery_index,
            survivor_idx,
            promotion_threshold,
        ) {
            // An empty survivor destination opened above goes back to Free.
            if let Some(idx) = survivor_idx {
                let region = &mut state.regions[idx];
                if region.header.alloc_top == region.base {
                    region.header.kind = RegionKind::Free;
                    region.header.gen_age = 0;
                    region.header.live_bytes = 0;
                    state.stats.regions_free += 1;
                }
            }
            MINOR_OBJECT_MAP.with(|s| *s.borrow_mut() = std::mem::take(&mut nursery_index));
            MINOR_WORKLIST.with(|s| *s.borrow_mut() = std::mem::take(&mut mark_worklist));
            // The nursery stays as it was, pins included.
            state.nursery_pin_candidates = pin_candidates;
            // Nothing changed the older generation, so its scan lists and
            // armed dirty pages stay valid for the retry.
            old_scan.put_back();
            return Err(EgclError::Oom);
        }

        // Everything above these tops after evacuation is a copy made below.
        old_scan.snapshot_tops(state);
        for &nursery_idx in &nursery_indices {
            if pinned_indices.contains(&nursery_idx) {
                continue; // Retained in place (pinned) — do not evacuate.
            }
            let base = state.regions[nursery_idx].base as usize;
            let top = state.regions[nursery_idx].header.alloc_top as usize;
            let used = top.saturating_sub(base) as u64;
            _nursery_used += used;

            if top <= base {
                continue; // Empty nursery region.
            }

            // The mark bitmap already identifies every evacuation candidate.
            // Do not reread dead objects just to advance past their payloads.
            for body_addr in nursery_index.marked_bodies(nursery_idx) {
                let header_ptr = (body_addr - OBJECT_HEADER_SIZE) as *mut u8;
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                debug_assert!(!unsafe { header_is_forwarded(header_ptr) });
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

                // Keep copying into the current destination until it fills.
                // Searching all regions per object makes evacuation quadratic
                // in live heap size; only refill needs the region search.
                let target_idx = if target_kind == RegionKind::Survivor {
                    &mut survivor_idx
                } else {
                    &mut old_gen_idx
                };

                // Try to copy the object.
                let mut copied = false;
                if let Some(tidx) = *target_idx {
                    if Self::copy_object(state, header_ptr, body_size, tidx).is_some() {
                        copied = true;
                    }
                }

                // If copy failed (target full), get a target that FITS and retry.
                if !copied {
                    *target_idx = Self::find_or_create_target_region(
                        state,
                        target_kind,
                        target_gen_age,
                        total_size,
                    );
                    if let Some(tidx) = *target_idx {
                        if Self::copy_object(state, header_ptr, body_size, tidx).is_some() {
                            copied = true;
                        }
                    }
                }
                if !copied {
                    // A live object the collector cannot place. Silently dropping
                    // it — what this branch used to do, with a comment reading
                    // "the object is lost (OOM during GC)" — is deferred heap
                    // corruption: every root keeps the stale pointer, the
                    // nursery is reset underneath it, and the mutator later
                    // reads zeros through it (bliss-wc4t manifested exactly
                    // that as `unbound variable: CONS`). With the
                    // old-occupancy major-GC trigger this should be
                    // unreachable; if it fires, dying loudly with a heap
                    // picture beats corrupting silently.
                    let free_regions = state
                        .regions
                        .iter()
                        .filter(|r| r.header.kind == RegionKind::Free)
                        .count();
                    panic!(
                        "GC minor evacuation exhausted regions: live object at {body_addr:#x} \
                         (type {type_id}, {total_size} bytes, target {target_kind:?}) cannot be \
                         placed; {free_regions} free of {} regions [{}]. Report a bead: \
                         evacuation_fits approved this collection (GitHub #471, bliss-wc4t).",
                        state.regions.len(),
                        Self::region_census(state),
                    );
                }

                bytes_promoted += total_size as u64;
            }

            // Finalizer/weak-pointer side tables are NOT touched here: firing a
            // finalizer per nursery object would fire it for *promoted survivors*
            // too (Phase 2 evacuates every non-pinned nursery object), closing
            // live resources at a stale from-space address. Instead, survivors'
            // side-table keys are forwarded after the relocation passes below
            // (relocate_side_tables), and finalizers for genuinely dead objects
            // fire once the freed nursery ranges are known (fire_finalizers_in_ranges).
            // This is egcl's analogue of SBCL's post-mark scan_finalizers
            // discipline (egcl-jtc.7f).
        }

        old_scan.note_copies(state);

        // Promote retained (pinned) nursery regions to old-gen in place, before
        // the relocation passes, so their objects' fields are traced and any
        // references they hold to just-evacuated objects are rewritten. The
        // pinned objects themselves did not move (bliss-jtc.18).
        for &idx in &pinned_indices {
            let region = &mut state.regions[idx];
            // Old-generation space above `alloc_top` is zero-filled, as pinned
            // allocation into a pinned host relies on; this region may still
            // hold bytes from an earlier nursery cycle there.
            region.zero_stale_tail();
            let live = (region.header.alloc_top as usize).saturating_sub(region.base as usize);
            region.header.kind = RegionKind::OldGen;
            region.header.gen_age = 0;
            region.header.live_bytes = live as u32;
            // A retained region holds a pinned object: record it as a pinned
            // host so ordinary evacuation never adds reclaimable data to it.
            state.pinned_hosts.insert(idx);
            if std::env::var_os("EGCL_GC_CENSUS").is_some() {
                // Name WHAT pinned this region: type_id histogram of its
                // pinned objects (diagnostic for bliss-wc4t).
                let base = region.base as usize;
                let top = region.header.alloc_top as usize;
                let mut kinds: Vec<u8> = Vec::new();
                let mut cursor = base;
                while cursor + OBJECT_HEADER_SIZE <= top {
                    let ptr = cursor as *const u8;
                    let (type_id, body_size) = unsafe { read_object_header(ptr) };
                    if body_size == 0 && type_id == 0 {
                        break;
                    }
                    if unsafe { !header_is_forwarded(ptr) && header_is_pinned(ptr) } {
                        kinds.push(type_id);
                    }
                    cursor += align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                }
                eprintln!("[GC-RETAIN] region {idx} retained; pinned type_ids {kinds:?}");
            }
        }

        phases.mark(MinorPhase::Evacuate);

        // Relocate old→young references recorded by the write barrier (jtc.21).
        // Each remembered slot may hold a pointer to a nursery object that was
        // just evacuated; chase its forwarding pointer and rewrite the slot to
        // the survivor/old-gen location so the reference stays valid after the
        // nursery is reset. This runs before any nursery region is zeroed, while
        // the forwarding pointers are still intact. Every slot and referent is
        // bounds-checked before it is dereferenced. Remembered slots are cleared:
        // their referents now live in regions a minor GC does not move.
        let remembered: Vec<usize> = state.remembered.drain().collect();
        for slot_addr in remembered {
            // Nursery holders were not roots; a live one's copy is relocated
            // with the evacuated objects below.
            if slot_addr < heap_base_addr
                || slot_addr + 8 > heap_end
                || slot_in_nursery(state, slot_addr)
            {
                continue;
            }
            // SAFETY: slot_addr lies within the managed heap.
            unsafe { relocate_slot(slot_addr as *mut EgclVal, heap_base_addr, heap_end) };
        }

        // Relocate object reference fields to the moved young objects (jtc.17):
        // young→young links (e.g. the cdr chain of a freshly-read list, which the
        // reader builds without going through the write barrier) live in the
        // survivor copies and must be rewritten to their evacuated targets. This
        // also covers old→young fields of typed objects; the remembered set above
        // additionally covers barrier-recorded slots in untyped objects.
        if dirty_scan {
            // Only the slots the scan found, this collection's copies, and the
            // retained regions can refer into the nursery (bliss-3uwzg).
            Self::relocate_minor_targets(
                state,
                &old_scan,
                &pinned_indices,
                heap_base_addr,
                heap_end,
                captures_remembered,
            );
        } else {
            Self::relocate_object_fields(state, heap_base_addr, heap_end, captures_remembered);
        }

        phases.mark(MinorPhase::HeapRelocate);

        // Complete the young-object root set (bliss-jtc.17): a nursery object may
        // also be reachable only from a CL-stack frame or the entry continuation.
        // Relocate those roots to the moved locations too, while forwarding is
        // still intact and before the nursery is reset.
        let root_clock = root_phase::start();
        Self::relocate_cl_stack_refs(heap_base_addr, state.config.heap_size);
        root_phase::finish(
            root_clock,
            b"minor_relocate_cl_stack",
            state.stats.minor_gc_count + 1,
            0,
        );
        relocate_entry_continuation(heap_base_addr, heap_end);
        // Symbol-table roots (bliss-jtc.6 Stage C): a nursery object may be
        // reachable only from a global symbol's cell; rewrite those cells to the
        // evacuated location while forwarding is intact.
        let root_clock = root_phase::start();
        crate::symbols::for_each_root_slot(|slot| unsafe {
            let before = (*slot).0;
            relocate_slot(slot, heap_base_addr, heap_end);
            if tracing {
                let after = (*slot).0;
                VERIFY_TRACE.with(|t| t.borrow_mut().reloc.insert(slot as usize, (before, after)));
            }
        });
        root_phase::finish(
            root_clock,
            b"minor_relocate_symbols",
            state.stats.minor_gc_count + 1,
            0,
        );
        scan_external_roots_with_kind(ExternalRootScanKind::MinorRelocate, |slot| unsafe {
            let before = (*slot).0;
            relocate_slot(slot, heap_base_addr, heap_end);
            if tracing {
                let after = (*slot).0;
                VERIFY_TRACE.with(|t| t.borrow_mut().reloc.insert(slot as usize, (before, after)));
            }
        });

        phases.mark(MinorPhase::RootsRelocate);

        // Forward finalizer-registry keys and weak-pointer referents for every
        // object promoted out of the nursery this cycle, while the forwarding
        // pointers are still intact (egcl-jtc.7f). A promoted survivor's key is
        // moved to its new address so its finalizer is NOT fired and its weak
        // pointers stay valid; only genuinely dead objects remain for the
        // range-based finalizer/weak-pointer passes below.
        relocate_side_tables(heap_base_addr, heap_end);

        let nursery_ranges: Vec<(usize, usize)> = nursery_indices
            .iter()
            .filter(|idx| !pinned_indices.contains(idx))
            .map(|&idx| {
                let base = state.regions[idx].base as usize;
                let limit = state.regions[idx].header.alloc_limit as usize;
                (base, limit)
            })
            .collect();

        // Clear weak pointers first, preserving the collector's established
        // death ordering, then run finalizers while the dead object's header
        // and payload are still intact (STREAM finalization reads its off-heap
        // state pointer). Survivor side-table keys were forwarded above; every
        // key still in these ranges is genuinely dead.
        // Weak containers, before the weak pointers and finalizers and for the
        // same reason: the forwarding pointers are still intact, so a surviving
        // referent can be relocated and anything still inside an evacuated range
        // is genuinely dead.
        process_weak_containers(&|slot: *mut EgclVal| {
            // SAFETY: the container owns the slot and is quiescent under the
            // heap lock the collector holds.
            let value = unsafe { *slot };
            if !is_heap_ref(value) {
                return true;
            }
            unsafe { relocate_slot(slot, heap_base_addr, heap_end) };
            let addr = ref_body_addr(unsafe { *slot });
            !nursery_ranges
                .iter()
                .any(|&(base, limit)| addr >= base && addr < limit)
        });
        break_dead_weak_pointers(&|val: EgclVal| {
            let addr = val.to_raw() as usize;
            nursery_ranges
                .iter()
                .any(|&(base, limit)| addr >= base && addr < limit)
        });
        fire_finalizers_in_ranges(&nursery_ranges);

        // EGCL_GC_VERIFY: every reference must have been relocated out of the
        // evacuated ranges by the passes above; a survivor pointer into them is
        // an untraced/unrelocated slot — panic now, naming the holder, instead
        // of letting the reset turn it into delayed corruption (bliss-4bp).
        if gc_verify_enabled() {
            let stale = |v: EgclVal| -> bool {
                if !is_heap_ref(v) {
                    return false;
                }
                let a = ref_body_addr(v);
                nursery_ranges.iter().any(|&(b, l)| a >= b && a < l)
            };
            let check = |v: EgclVal, holder: &str, slot_addr: usize| {
                if stale(v) {
                    // Diagnose the asymmetry directly (bliss-wc4t): mark_ref
                    // requires the EXACT body address to be a nursery_index key,
                    // while this range check does not — so an interior/mistagged
                    // pointer or an index-excluded object is mark-missed,
                    // relocate-no-op'd, and flagged only here.
                    let body = ref_body_addr(v);
                    let indexed = nursery_index.contains(body);
                    let header_ptr = (body - OBJECT_HEADER_SIZE) as *const u8;
                    let header_word = unsafe { *(header_ptr as *const u64) };
                    let forwarded = unsafe { header_is_forwarded(header_ptr) };
                    panic!(
                        "gc-verify: reference {:#x} into evacuated nursery survives minor GC \
                         in {holder} slot {slot_addr:#x} [{}] \
                         [target body {body:#x}: in nursery_index={indexed}, \
                         header={header_word:#x}, forwarded={forwarded}]",
                        v.to_raw(),
                        verify_trace_report(slot_addr)
                    );
                }
            };
            let describe_target = |v: EgclVal| -> String {
                let body = ref_body_addr(v);
                let indexed = nursery_index.contains(body);
                let header_ptr = (body - OBJECT_HEADER_SIZE) as *const u8;
                let header_word = unsafe { *(header_ptr as *const u64) };
                let forwarded = unsafe { header_is_forwarded(header_ptr) };
                format!(
                    "target body {body:#x}: in nursery_index={indexed}, \
                     header={header_word:#x}, forwarded={forwarded}"
                )
            };
            verify_rooted_lists_naming(&stale, &describe_target);
            check(get_entry_continuation(), "entry-continuation", 0);
            crate::symbols::for_each_root_slot(|slot| {
                check(unsafe { *slot }, "symbol-root", slot as usize);
            });
            scan_external_roots(|slot| {
                check(unsafe { *slot }, "external-root", slot as usize);
            });
            {
                let mut chase = |slot: &mut EgclVal| {
                    check(*slot, "cl-stack-frame", slot as *mut EgclVal as usize);
                };
                let mut verify_from = |fp: *const crate::stack::Frame| {
                    // SAFETY: `fp` is a valid published frame chain (STW).
                    unsafe { crate::stack::visit_stack_refs(fp, &mut chase) };
                };
                let cur = crate::thread::current_thread_id();
                let current_fiber = crate::thread::current_fiber_id();
                verify_from(crate::thread::current_stack().fp());
                for id in crate::thread::all_thread_ids() {
                    if id == cur {
                        continue;
                    }
                    if let Some(fp) = crate::thread::thread_published_fp(id) {
                        verify_from(fp);
                    }
                }
                for id in crate::thread::all_fiber_ids() {
                    if Some(id) == current_fiber {
                        continue;
                    }
                    if let Some(fp) = crate::thread::fiber_published_fp(id) {
                        verify_from(fp);
                    }
                }
            }
            for region in state.regions.iter() {
                if !matches!(
                    region.header.kind,
                    RegionKind::OldGen | RegionKind::Survivor | RegionKind::LargeObject
                ) {
                    continue;
                }
                let mut cursor = region.base as usize;
                let top = region.header.alloc_top as usize;
                while cursor + OBJECT_HEADER_SIZE <= top {
                    let header_ptr = cursor as *const u8;
                    let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                    if body_size == 0 && type_id == 0 {
                        break;
                    }
                    let total_size =
                        align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
                    if !unsafe { header_is_forwarded(header_ptr) } {
                        let body_addr = cursor + OBJECT_HEADER_SIZE;
                        unsafe {
                            trace_object(
                                body_addr as *mut u8,
                                type_id,
                                body_size as usize,
                                |slot| {
                                    let v = *slot;
                                    if stale(v) {
                                        panic!(
                                            "gc-verify: reference {:#x} into evacuated nursery \
                                             survives minor GC in live object body {body_addr:#x} \
                                             (type {type_id}) slot {:#x}",
                                            v.to_raw(),
                                            slot as usize
                                        );
                                    }
                                },
                            );
                        }
                    }
                    cursor += total_size;
                }
            }
        }

        // Phase 4: Reset all nursery regions for reuse (after relocation, above,
        // read their forwarding pointers). Retained pinned regions were promoted
        // to old-gen in place and must NOT be zeroed (bliss-jtc.18).
        let mut regions_retained = 0u64;
        let mut regions_evacuated = 0u64;
        for &nursery_idx in &nursery_indices {
            if pinned_indices.contains(&nursery_idx) {
                regions_retained += 1;
                continue;
            }
            regions_evacuated += 1;
            let region = &mut state.regions[nursery_idx];
            // The region is not zeroed here: that cost the pause a memset of
            // every byte the nursery allocated. Walkers stop at `alloc_top`,
            // which is reset below, and each TLAB carved from the region is
            // zeroed as it is handed out (`HeapRegion::zero_for_reuse`).
            // `dirty_end` records how far the stale bytes reach (bliss-xyt4l).
            let region_used =
                (region.header.alloc_top as usize).saturating_sub(region.base as usize);
            region.dirty_end = region.dirty_end.max(region.header.alloc_top as usize);
            if region_used > 0 && gc_poison_enabled() {
                unsafe {
                    // bliss-6b2 #2 diagnostic: EGCL_GC_POISON fills reclaimed
                    // nursery with 0xFA instead of 0. As a EgclVal, 0xFAFA…FA
                    // is TAG_HEAP_OBJECT (low bits 010) with a non-canonical
                    // pointer, so an unrooted value the tree-walker held across
                    // this collection — now pointing here — FAULTS on its next
                    // dereference (SIGSEGV) instead of silently reading NIL from
                    // a zeroed slot. Run under gdb to get the backtrace at the
                    // offending deref, which names the missing GC root.
                    std::ptr::write_bytes(region.base, 0xFA, region_used);
                }
            }
            region.header.alloc_top = region.base;
            region.header.live_bytes = 0;
            region.header.gen_age = 0;
        }

        // Return regions an exhausted heap lent the nursery from the emergency
        // reserve (GitHub #471), now that its survivors have found room.
        let nursery_region_limit = state.config.nursery_size.div_ceil(state.config.region_size);
        let mut nursery_regions = state
            .regions
            .iter()
            .filter(|region| region.header.kind == RegionKind::Nursery)
            .count();
        for &nursery_idx in nursery_indices.iter().rev() {
            if nursery_regions <= nursery_region_limit {
                break;
            }
            if pinned_indices.contains(&nursery_idx) {
                continue;
            }
            // Free regions are zero-filled; this one holds stale nursery bytes.
            state.regions[nursery_idx].zero_stale_tail();
            state.regions[nursery_idx].header.kind = RegionKind::Free;
            state.stats.regions_free += 1;
            nursery_regions -= 1;
        }

        // The nursery is empty, so no older object refers into it: start
        // recording writes for the next minor collection's scan (bliss-3uwzg).
        old_scan.put_back();
        crate::gc_soft_dirty::rearm(
            heap_base_addr,
            state.config.heap_size,
            old_generation_bytes(state),
            state.config.nursery_size,
            &older_tracking_regions(state),
        );

        // Update stats.
        state.stats.nursery_regions_retained += regions_retained;
        state.stats.nursery_regions_evacuated += regions_evacuated;
        if gc_region_log_enabled() {
            crate::syscall::dbg_write(b"[gc-regions] minor: retained ");
            write_decimal(regions_retained);
            crate::syscall::dbg_write(b" evacuated ");
            write_decimal(regions_evacuated);
            crate::syscall::dbg_write(b" indexed ");
            write_decimal(nursery_index.lazily_indexed);
            crate::syscall::dbg_write(b"\n");
        }
        state.stats.minor_gc_count += 1;
        state.stats.bytes_promoted += bytes_promoted;
        state.stats.nursery_used = 0; // nursery was just collected
        let elapsed_us = start.elapsed().as_micros() as u64;
        state.stats.total_minor_pause_us += elapsed_us;

        // JFR-style event stream (bliss-u3h0): a minor collection finished. This
        // runs at collection *completion* — all scanning/moving is done — and
        // record() allocates no heap value, so it cannot re-enter the GC.
        crate::events::record(
            crate::events::EventKind::GcMinor,
            crate::events::NO_SYM,
            elapsed_us,
            bytes_promoted,
        );

        // Update local stats copy.
        self.gc_stats = state.stats.clone();
        GC_MOVE_EPOCH.fetch_add(1, Ordering::Release);
        // The nursery is empty: everything written before this point now
        // refers to older objects only (bliss-gg3i1.7.9).
        MINOR_EPOCH.fetch_add(1, Ordering::Release);

        // Return the scratch to the thread-local with its capacity retained
        // (bliss-5i3f). No early exit runs between the takes above and here.
        let phase_indexed = nursery_index.lazily_indexed;
        MINOR_OBJECT_MAP.with(|s| *s.borrow_mut() = std::mem::take(&mut nursery_index));
        MINOR_WORKLIST.with(|s| *s.borrow_mut() = std::mem::take(&mut mark_worklist));
        phases.mark(MinorPhase::ReclaimRearm);
        phases.finish(
            state.stats.minor_gc_count,
            dirty_scan,
            phase_indexed,
            elapsed_us,
        );

        Ok(())
    }

    fn minor_gc_stop_the_world(&mut self) -> Result<(), EgclError> {
        let profiling = minor_phase::enabled();
        let start = profiling.then(std::time::Instant::now);
        let mut entered = None;
        let mut finished = None;
        let result = with_stopped_world(|| {
            if profiling {
                entered = Some(std::time::Instant::now());
            }
            let result = self.minor_gc_stw_body();
            if profiling {
                finished = Some(std::time::Instant::now());
            }
            result
        });
        if let Some(start) = start {
            minor_phase::log_stw(
                self.gc_stats.minor_gc_count,
                start,
                entered,
                finished,
                result.is_ok(),
            );
        }
        result
    }
}

impl HeapCollector {
    /// One-line region census for diagnostics: counts per kind plus how many
    /// regions hold a pinned object (and so are unreclaimable).
    fn region_census(state: &HeapState) -> String {
        let mut free = 0usize;
        let mut nursery = 0usize;
        let mut survivor = 0usize;
        let mut old = 0usize;
        let mut large = 0usize;
        let mut pinned = 0usize;
        for r in state.regions.iter() {
            match r.header.kind {
                RegionKind::Free => free += 1,
                RegionKind::Nursery => nursery += 1,
                RegionKind::Survivor => survivor += 1,
                RegionKind::OldGen => old += 1,
                RegionKind::LargeObject => large += 1,
            }
            let base = r.base as usize;
            let top = r.header.alloc_top as usize;
            if top > base && unsafe { region_has_pinned(base, top) } {
                pinned += 1;
            }
        }
        format!(
            "free={free} nursery={nursery} survivor={survivor} oldgen={old} large={large} \
             with-pinned={pinned} total={}",
            state.regions.len()
        )
    }

    /// Fraction of heap regions not currently Free, against
    /// `config.old_occupancy_trigger`. Consulted after each minor collection to
    /// decide whether a major collection should reclaim old-gen garbage.
    fn occupancy_exceeds_trigger() -> bool {
        let guard = heap_state().lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = guard.as_ref() else {
            return false;
        };
        let total = state.regions.len();
        if total == 0 {
            return false;
        }
        let non_free = state
            .regions
            .iter()
            .filter(|r| r.header.kind != RegionKind::Free)
            .count();
        (non_free as f64 / total as f64) > state.config.old_occupancy_trigger
    }
}

impl Collector for HeapCollector {
    /// Stop-the-world minor (nursery) collection.
    /// Cheney-style scavenge: copies live nursery objects into survivor space
    /// or promotes to old-gen based on gen_age vs promotion_threshold.
    fn minor_gc(&mut self) -> Result<(), EgclError> {
        let needs_major = {
            let _gc_admission = acquire_gc_admission();
            match self.minor_gc_stop_the_world() {
                Ok(()) => {}
                // The survivors do not fit, and nothing moved. Old-generation
                // garbage may be what fills the heap, as after a program
                // handles STORAGE-CONDITION and drops its data, so reclaim it
                // before reporting exhaustion (GitHub #471). A normal major
                // collection would first drain the nursery, which is exactly
                // what failed: this one marks through the nursery in place,
                // frees wholly dead older regions, and the minor collection is
                // retried. Oom only when it still cannot place the survivors.
                Err(EgclError::Oom) => {
                    with_stopped_world(|| self.major_gc_stw_body(MajorMode::KeepNursery))?;
                    self.minor_gc_stop_the_world()?;
                }
                Err(error) => return Err(error),
            }
            // `old_occupancy_trigger` was a config knob wired to NOTHING: no
            // automatic major collection existed, so old-gen garbage accumulated
            // until evacuation ran out of regions mid-minor-GC and silently
            // dropped live objects — the heap corruption behind bliss-wc4t
            // ("unbound variable: CONS" after enough stress-mode collections).
            // Honor the knob: when the non-Free region fraction crosses it,
            // run a major collection to sweep old-gen garbage back to Free.
            let requested = MAJOR_GC_REQUESTED.swap(false, Ordering::AcqRel);
            requested || Self::occupancy_exceeds_trigger()
        };
        // Outside the admission gate: major_gc acquires it itself.
        // No retrigger loop: major_gc drains the nursery via
        // minor_gc_stop_the_world directly, not through this wrapper.
        if needs_major {
            let census = std::env::var_os("EGCL_GC_CENSUS").is_some();
            if census {
                let guard = heap_state().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = guard.as_ref() {
                    eprintln!("[GC-CENSUS] before-major: {}", Self::region_census(state));
                }
            }
            self.major_gc()?;
            if census {
                let guard = heap_state().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = guard.as_ref() {
                    eprintln!("[GC-CENSUS] after-major:  {}", Self::region_census(state));
                }
            }
        }
        Ok(())
    }

    /// Concurrent old-gen marking + evacuation cycle (A3.02).
    ///
    /// Simplified sequential implementation that runs under the heap lock:
    /// 1. Mark phase: walk all old-gen/survivor/large-object regions, scan objects
    ///    from base to alloc_top, and compute accurate live_bytes per region.
    /// 2. Region selection: identify regions with high garbage ratio
    ///    (live_bytes / region_capacity < 0.5), sparsest first.
    /// 3. Evacuation: copy live objects from selected regions to fresh old-gen
    ///    regions, install forwarding pointers, and free evacuated regions.
    fn major_gc(&mut self) -> Result<(), EgclError> {
        let _gc_admission = acquire_gc_admission();
        with_stopped_world(|| self.major_gc_stw_body(MajorMode::DrainNursery))
    }

    /// Full GC: runs minor + major collections. Used before image save.
    fn full_gc(&mut self) -> Result<(), EgclError> {
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

impl HeapCollector {
    /// The current major collector is sequential: its root reads and pointer
    /// rewrites require one uninterrupted pause, including the nursery drain.
    /// A heap mutex alone cannot exclude mutators using their TLABs or roots.
    fn major_gc_stw_body(&mut self, mode: MajorMode) -> Result<(), EgclError> {
        let start = std::time::Instant::now();
        let keep_nursery = mode == MajorMode::KeepNursery;

        // A standalone major collection must first drain nursery state so
        // nursery deaths trigger finalizers and weak-reference clearing too.
        // KeepNursery instead traces through the nursery where it stands and
        // leaves its deaths to the minor collection that follows.
        if !keep_nursery {
            // This collection runs the function-capture sweep, which is what
            // an outstanding request asked for.
            MAJOR_GC_REQUESTED.store(false, Ordering::Release);
            self.minor_gc_stw_body()?;
        }
        // Until this collection completes (and re-arms the tracker below), the
        // next minor collection must scan the whole old generation.
        crate::gc_soft_dirty::disarm();

        // Set marking flag (§3.6.2: SATB barrier only fires when marking active).
        set_gc_marking_in_progress(true);

        let mut guard = heap_state().lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            set_gc_marking_in_progress(false);
            EgclError::Internal("heap not initialized".into())
        })?;

        // Phase 1: Mark phase — precise tracing (bliss-jtc.20).
        //
        // Marking starts from precise roots (the entry continuation and every
        // green thread's CL-stack references) and follows only genuine reference
        // fields via `trace_object`, keyed on each object's type_id. Numeric and
        // byte payloads are never scanned for pointer-shaped words, so a byte
        // vector or bignum that happens to contain an object-shaped value cannot
        // falsely retain that object. Mark state lives in a side table (external
        // to the object header), per §1.3 / R3.17.
        //   1. Index every object body in old-gen/survivor/LO regions with its
        //      footprint, type_id, and body length.
        //   2. Mark precise roots into a worklist.
        //   3. Trace transitively, visiting only reference fields.
        //   4. Compute live_bytes from the mark set.
        let region_count = state.regions.len();
        let heap_base_addr = state.heap_base as usize;
        let heap_size = state.config.heap_size;
        let region_size = state.config.region_size;
        // Record TAMS (Top-At-Mark-Start) per region.
        let mut tams: Vec<usize> = Vec::with_capacity(region_count);
        for region in state.regions.iter() {
            tams.push(region.header.alloc_top as usize);
        }

        // Index each live object START into a granule bitmap, and keep the mark
        // set in a second bitmap beside it (side tables, not header bits —
        // R3.17). This is the same `NurseryObjectMap` the minor collector uses,
        // reset over the old-gen/survivor/large-object regions instead of the
        // nursery: one `starts` bit and one `marked` bit per 16-byte granule.
        //
        // It used to be a `HashMap<body_addr, (total_size, region_idx, type_id,
        // body_len)>` plus a `HashSet` mark set — roughly 40 bytes of transient
        // metadata per live object, built before marking even began, and a hash
        // probe on every traced edge. A full collection over 1.6M live conses
        // took 3.7 s against SBCL's 0.053 s (71x) and grew SUPER-linearly where
        // SBCL grows linearly (bliss-ub8a). None of the tuple ever needed
        // storing: `type_id`/`body_len` come from the object's own header, which
        // the tracing loop already reads to find the payload offset.
        let mut object_map = MAJOR_OBJECT_MAP.with(|s| std::mem::take(&mut *s.borrow_mut()));
        let mut marked_regions: Vec<usize> = Vec::new();
        for (idx, region) in state.regions.iter().enumerate() {
            if matches!(
                region.header.kind,
                RegionKind::OldGen | RegionKind::Survivor | RegionKind::LargeObject
            ) && tams[idx] > region.base as usize
            {
                marked_regions.push(idx);
            }
        }
        // Nursery objects are indexed only so marking can pass through them;
        // their liveness is accounted for by the minor collection.
        let nursery_regions: Vec<usize> = if keep_nursery {
            state
                .regions
                .iter()
                .enumerate()
                .filter(|(idx, region)| {
                    region.header.kind == RegionKind::Nursery && tams[*idx] > region.base as usize
                })
                .map(|(idx, _)| idx)
                .collect()
        } else {
            Vec::new()
        };
        let indexed: Vec<usize> = marked_regions
            .iter()
            .chain(&nursery_regions)
            .copied()
            .collect();
        object_map.reset(heap_base_addr, region_size, region_count, &indexed);
        for &idx in &nursery_regions {
            // SAFETY: the world is stopped and this is an allocated nursery
            // region; prepare_region fills its TLAB gaps as a minor GC does.
            unsafe { object_map.prepare_region(idx, tams[idx]) };
        }
        for &idx in &marked_regions {
            let region = &state.regions[idx];
            let base = region.base as usize;
            let top = tams[idx];
            let mut cursor = base;
            while cursor + OBJECT_HEADER_SIZE <= top {
                let header_ptr = cursor as *const u8;
                let (type_id, body_size) = unsafe { read_object_header(header_ptr) };
                if body_size == 0 && type_id == 0 {
                    break;
                }
                // Footprint from the header so large objects (16-byte header +
                // u64 size extension) stride correctly (bliss-tjru). The indexed
                // identity stays cursor+8, matching `ref_body_addr` for every
                // heap reference — which is also what `position` expects.
                let total_size = unsafe { header_total_bytes(header_ptr) };
                if !unsafe { header_is_forwarded(header_ptr) } {
                    object_map.insert(cursor + OBJECT_HEADER_SIZE);
                }
                cursor += total_size;
            }
        }

        let mut scan_worklist = MAJOR_WORKLIST.with(|s| std::mem::take(&mut *s.borrow_mut()));
        scan_worklist.clear();

        // Helper: mark a candidate reference if it targets an indexed object.
        // `mark` returns true only when the target is a known object start that
        // was not already marked, which is exactly the old
        // `contains_key(..) && marked.insert(..)` pair — in two bit operations
        // instead of two hash lookups.
        // Root scanners hold locks ordered after the symbol registry. Resolve
        // immediate identities through a stopped-world snapshot, not a nested lock.
        let symbol_objects = crate::symbols::uninterned_objects();
        let mark_ref = |v: EgclVal, map: &mut NurseryObjectMap, worklist: &mut Vec<usize>| {
            let v = v
                .symbol_index()
                .and_then(|index| symbol_objects.get(&index).copied())
                .unwrap_or(v);
            if is_heap_ref(v) {
                // Resolve persistent (CHANGE-CLASS) forwarding first, as in
                // the minor collector's mark_ref (bliss-334).
                let v = resolve_forwarded(v);
                let target = ref_body_addr(v);
                if map.mark(target) {
                    worklist.push(target);
                }
            }
        };

        // Precise root: the saved entry continuation (§7.2.3).
        mark_ref(
            get_entry_continuation(),
            &mut object_map,
            &mut scan_worklist,
        );

        // Precise CL-stack roots (nmq.3): walk every green thread's EgclStack
        // frames and mark exactly the heap references their slots hold —
        // identified by EgclVal tag, so no non-reference CL data is pinned.
        // Interpreter (T0) and compiled (T1) frames share the §2.4.2 layout, so
        // this one walk covers mixed-tier stacks.
        Self::scan_cl_stack_roots(|v| mark_ref(v, &mut object_map, &mut scan_worklist));

        // Interned symbols own their cells permanently. Uninterned symbols
        // enter the marking graph only through reachable symbol identities.
        crate::symbols::visit_interned_roots(|value| {
            mark_ref(value, &mut object_map, &mut scan_worklist);
        });
        // External roots (bliss-jtc.8): EgclVals owned outside the GC heap, e.g.
        // hash-table entries in a Rust Vec.
        scan_external_mark_roots(ExternalRootScanKind::Full, |slot| {
            let v = unsafe { *slot };
            mark_ref(v, &mut object_map, &mut scan_worklist);
        });

        // Transitive closure: trace only the reference fields of each marked
        // object, following its type_id-specific layout.
        while let Some(obj_addr) = scan_worklist.pop() {
            // `mark` only succeeds for an indexed, non-forwarded object start, so
            // a worklist entry is always one, and its own header carries the
            // type_id and body length the old index tuple duplicated.
            //
            // The indexed identity is the header+8 address; the real payload is
            // header + body_offset (8, or 16 for a large object). Trace the real
            // payload so a large object's fields are read at the right offset
            // rather than 8 bytes into its size-extension word (bliss-tjru).
            let header_ptr = (obj_addr - OBJECT_HEADER_SIZE) as *const u8;
            let (type_id, body_len) = unsafe { read_object_header(header_ptr) };
            let payload = (obj_addr - OBJECT_HEADER_SIZE) + unsafe { body_offset(header_ptr) };
            // SAFETY: payload is a live object body of body_len bytes.
            unsafe {
                trace_object(payload as *mut u8, type_id, body_len as usize, |slot| {
                    mark_ref(*slot, &mut object_map, &mut scan_worklist);
                });
            }
        }

        let dead_symbols =
            crate::symbols::sweep_uninterned(|object| object_map.is_marked(ref_body_addr(object)));
        let dead_symbol_objects: std::collections::HashSet<_> = dead_symbols
            .iter()
            .map(|(_, object)| (object.to_raw() & !crate::value::TAG_MASK) as usize)
            .collect();

        // Compute live_bytes per region from mark results.
        // Also run finalizers for dead objects and break their weak pointers.
        let mut dead_object_vals: Vec<EgclVal> = Vec::new();
        let mut dead_function_slots = Vec::new();
        let mut dead_closure_states = Vec::new();
        let mut dead_functions = std::collections::HashSet::new();
        let mut live_functions = std::collections::HashSet::new();
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
                        // Stride by the true footprint so large objects are
                        // walked correctly (bliss-tjru); the marked-set key is
                        // the cursor+8 identity, matching object_index.
                        let total_size = unsafe { header_total_bytes(header_ptr) };
                        let _ = body_size;
                        if !unsafe { header_is_forwarded(header_ptr) } {
                            let body_addr = cursor + OBJECT_HEADER_SIZE;
                            // Large objects (single- AND multi-region) are
                            // marked by reachability like any other object
                            // (bliss-jg6g / bliss-hy5v): the start region's
                            // live_bytes records the whole footprint, and the
                            // unit-based large-object free pass below frees or
                            // retains a multi-region object's continuation
                            // regions together with their start — never
                            // per-region (a continuation carries no header at
                            // its base, so per-region liveness is meaningless
                            // there).
                            if type_id == crate::object::type_id::FUNCTION_INTERPRETED
                                && body_size >= 32
                            {
                                let payload = unsafe { header_ptr.add(body_offset(header_ptr)) };
                                let name = unsafe { *(payload as *const EgclVal).add(3) };
                                let definition = if body_size >= 56 {
                                    let offset = core::mem::offset_of!(
                                        crate::object::FunctionData,
                                        definition_index
                                    ) - core::mem::size_of::<ObjectHeader>();
                                    let index = unsafe { *payload.add(offset).cast::<u32>() };
                                    (index != 0).then(|| EgclVal::from_symbol_index(index))
                                } else {
                                    None
                                };
                                if object_map.is_marked(body_addr) {
                                    live_functions.insert(name);
                                    live_functions.extend(definition);
                                } else {
                                    dead_functions.insert(name);
                                    dead_functions.extend(definition);
                                    if body_size as usize >= FUNCTION_BODY_BYTES {
                                        // Take the dead object's closure state
                                        // before its storage is erased below.
                                        let state = unsafe {
                                            &*payload
                                                .add(FUNCTION_CLOSURE_STATE_OFFSET)
                                                .cast::<core::sync::atomic::AtomicUsize>()
                                        }
                                        .swap(0, Ordering::AcqRel);
                                        if state != 0 {
                                            dead_closure_states.push(state);
                                        }
                                    }
                                    if unsafe { header_is_pinned(header_ptr) } {
                                        dead_function_slots.push((cursor, unsafe {
                                            header_exact_body_len(header_ptr)
                                        }));
                                    }
                                }
                            }
                            if dead_symbol_objects.contains(&cursor) {
                                dead_function_slots
                                    .push((cursor, unsafe { header_exact_body_len(header_ptr) }));
                            }
                            if object_map.is_marked(body_addr) {
                                live += total_size as u32;
                            } else {
                                // Object is dead — queue for finalization.
                                let obj_val = EgclVal::from_raw(body_addr as u64);
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

        // Function names are counted live only from older regions, so with a
        // nursery that may hold the live definition the sweep waits for the
        // next ordinary major collection.
        // Storage of the dead functions is reused from here on. Advance the
        // epoch before the sweep hook, which revalidates surviving caches.
        if !dead_function_slots.is_empty() {
            PINNED_FUNCTION_EPOCH.fetch_add(1, Ordering::AcqRel);
        }
        if !keep_nursery && let Some(sweep) = FUNCTION_CAPTURE_SWEEP_FN.get() {
            dead_functions.retain(|name| !live_functions.contains(name));
            sweep(&dead_functions.into_iter().collect::<Vec<_>>());
        }
        // A dead object's own state dies with it in either mode: its storage
        // is erased below. Release runs host destructors only.
        if !dead_closure_states.is_empty() {
            let release = CLOSURE_STATE_RELEASE_FN
                .get()
                .expect("closure state stored without a release hook");
            for state in dead_closure_states {
                release(state);
            }
        }

        // Run finalizers for dead objects (R3.12).
        for obj_val in &dead_object_vals {
            run_finalizers_for(*obj_val);
        }

        // Break weak pointers to dead objects (R3.13).
        {
            let mut dead_set: std::collections::HashSet<u64> =
                dead_object_vals.iter().map(|v| v.to_raw()).collect();
            dead_set.extend(
                dead_symbols
                    .iter()
                    .map(|(index, _)| EgclVal::from_symbol_index(*index).to_raw()),
            );
            break_dead_weak_pointers(&|val: EgclVal| dead_set.contains(&val.to_raw()));
            // Weak containers drop entries for the same dead objects. Nothing has
            // moved yet at this point in a major cycle, so this pass only decides
            // liveness; the post-evacuation pass below does the relocating.
            process_weak_containers(&|slot: *mut EgclVal| {
                // SAFETY: as for the minor pass above.
                let value = unsafe { *slot };
                // `dead_set` holds untagged body addresses for heap objects
                // (and tagged values for symbols), so normalize a tagged heap
                // reference before asking. Comparing the tagged word never
                // matched, so no weak entry died in a major collection
                // (bliss-4due).
                let key = if is_heap_ref(value) {
                    ref_body_addr(value) as u64
                } else {
                    value.to_raw()
                };
                !dead_set.contains(&key)
            });
        }

        // Captures, finalizers and weak references have now observed the old
        // identity. Erase dead pinned object fields before reusing their storage;
        // type-zero fillers preserve heap walking without retaining references.
        for &(address, size) in &dead_function_slots {
            unsafe {
                std::ptr::write_bytes(address as *mut u8, 0, object_footprint(size).0);
                write_object_header(address as *mut u8, 0, size as u32);
            }
        }
        state.free_pinned_slots.extend(dead_function_slots);

        // Phase 2: Region selection — find old-gen regions with high garbage ratio.
        // Measure density against capacity, not the allocation high-water mark:
        // a tiny fully-live survivor cohort otherwise permanently occupies one
        // old region per major collection (bliss-copxf).
        let mut evacuation_set: Vec<usize> = Vec::new();
        // Relocation below rewrites references from older regions and roots,
        // not from the nursery, so KeepNursery moves nothing: it only frees
        // regions with no live object.
        for (idx, region) in state.regions.iter().enumerate() {
            if keep_nursery {
                break;
            }
            // Survivor regions are candidates too: minor collections do not
            // re-collect them (nursery_indices is Nursery-kind only), and the
            // sweep below used to skip them as well, so survivor garbage was
            // PERMANENT — under allocation churn survivor regions accumulated
            // until evacuation ran out of regions and dropped live objects
            // (bliss-wc4t). A survivor object still marked live at a major has
            // survived long enough that copying it to OldGen is its promotion.
            if !matches!(
                region.header.kind,
                RegionKind::OldGen | RegionKind::Survivor
            ) {
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

            // Never evacuate a region that holds a pinned object — its objects
            // must keep their addresses (bliss-jtc.18).
            if unsafe { region_has_pinned(base, top) } {
                continue;
            }
            if (region.header.live_bytes as usize) < state.config.region_size / 2 {
                evacuation_set.push(idx);
            }
        }
        evacuation_set.sort_by_key(|&idx| state.regions[idx].header.live_bytes);

        // Phase 3: Evacuation — copy live objects from selected regions to fresh
        // ones. A region may only be freed afterwards if EVERY live object in it
        // was successfully copied; the old code freed the region unconditionally
        // even when a copy silently failed (`let _ = copy_object(...)`), which
        // zeroed a still-referenced live object — the same corruption class as
        // the minor-evacuation loss (bliss-wc4t), and newly reachable now that
        // major collections run automatically.
        let mut fully_evacuated: Vec<usize> = Vec::with_capacity(evacuation_set.len());
        // Destinations belong only to this collection. Reusing arbitrary old
        // regions could choose a source, then free its newly copied objects
        // when that source's turn in the collection set arrives.
        let mut evacuation_target: Option<usize> = None;
        for &evac_idx in &evacuation_set {
            let base = state.regions[evac_idx].base as usize;
            let top = state.regions[evac_idx].header.alloc_top as usize;
            let mut all_copied = true;

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
                if !unsafe { header_is_forwarded(header_ptr) } && object_map.is_marked(body_addr) {
                    let mut copied = false;
                    if !evacuation_target.is_some_and(|idx| {
                        let region = &state.regions[idx];
                        (region.header.alloc_limit as usize)
                            .saturating_sub(region.header.alloc_top as usize)
                            >= total_size
                    }) {
                        // Compaction is optional; it must not spend the
                        // emergency reserve (GitHub #471). An object left
                        // uncopied keeps its region, as when no region is Free.
                        evacuation_target =
                            if free_region_count(state) > emergency_reserve_regions(state) {
                                Self::allocate_target_region(state, RegionKind::OldGen, 0)
                            } else {
                                None
                            };
                    }
                    if let Some(tidx) = evacuation_target {
                        copied = Self::copy_object(state, header_ptr, body_size, tidx).is_some();
                    }
                    if !copied {
                        all_copied = false;
                    }
                }

                cursor += total_size;
            }
            if all_copied {
                fully_evacuated.push(evac_idx);
            }
        }

        // Rewrite EVERY live reference to its forwarded location before any
        // evacuated region is reclaimed (bliss-jtc.17): object reference fields
        // (traced precisely by type_id), CL-stack frame slots of every green
        // thread (nmq.3), and the entry-continuation root. After this pass no
        // reachable slot points at a stale, about-to-be-freed location.
        let heap_end = heap_base_addr + heap_size;
        Self::relocate_object_fields(state, heap_base_addr, heap_end, false);
        Self::relocate_cl_stack_refs(heap_base_addr, heap_size);
        relocate_entry_continuation(heap_base_addr, heap_end);
        // Symbol-table roots (bliss-jtc.6 Stage C): rewrite any symbol cell that
        // referenced an evacuated object. Idempotent w.r.t. the object-field pass
        // above (a cell already relocated points at a non-forwarded target).
        crate::symbols::for_each_root_slot(|slot| unsafe {
            relocate_slot(slot, heap_base_addr, heap_end)
        });
        scan_external_roots(|slot| unsafe { relocate_slot(slot, heap_base_addr, heap_end) });
        // A weak container's own scanner skips its weak slots, so relocate those
        // here. Liveness was settled by the mark-phase pass above; every referent
        // still held is live, hence the unconditional `true`.
        process_weak_containers(&|slot: *mut EgclVal| {
            unsafe { relocate_slot(slot, heap_base_addr, heap_end) };
            true
        });

        // Forward finalizer-registry keys and weak-pointer referents for old-gen
        // objects that were evacuated in Phase 3, while the forwarding pointers
        // are still intact (egcl-jtc.7f). Dead objects already had their
        // finalizers fired / weak pointers broken above (before evacuation, at
        // their stable pre-evacuation addresses); this pass only moves the keys
        // of live survivors that relocated.
        relocate_side_tables(heap_base_addr, heap_end);

        // Now free the evacuated regions — their forwarding pointers are no
        // longer needed. Only regions whose every live object copied out; a
        // region with a stranded live object stays OldGen (its forwarded husks
        // are skipped by every walk, and the stranded object remains valid in
        // place).
        for &evac_idx in &fully_evacuated {
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

        // Large objects are freed as UNITS (bliss-hy5v): a multi-region large
        // object's continuation regions carry no object header — their
        // alloc_top stays at base — so per-region liveness is meaningless
        // there, and the old per-region arm freed a zero-live continuation out
        // from under a still-live start region (handing its tail to the next
        // allocation). Walk regions by index: a LargeObject region with
        // alloc_top > base is a START whose footprint spans
        // ceil(footprint/region_size) CONSECUTIVE regions (alloc_large takes
        // them contiguously); everything in that span lives or dies with the
        // start's reachability-derived live_bytes.
        let n_regions = state.regions.len();
        let mut large_spans: Vec<(usize, usize, usize, bool)> = Vec::new();
        {
            let mut i = 0;
            while i < n_regions {
                let region = &state.regions[i];
                if region.header.kind != RegionKind::LargeObject {
                    i += 1;
                    continue;
                }
                let base = region.base as usize;
                let footprint = (region.header.alloc_top as usize).saturating_sub(base);
                if footprint == 0 {
                    // A continuation whose start was not seen (cannot happen for
                    // a well-formed heap, since starts precede continuations);
                    // leave it alone rather than freeing blind.
                    i += 1;
                    continue;
                }
                let span = footprint.div_ceil(region_size).max(1);
                let pinned = unsafe { header_is_pinned(base as *const u8) };
                let dead = region.header.live_bytes == 0 && !pinned;
                large_spans.push((i, span, footprint, dead));
                i += span;
            }
        }
        for &(start, span, footprint, dead) in &large_spans {
            if !dead {
                continue;
            }
            state.stats.large_object_bytes = state
                .stats
                .large_object_bytes
                .saturating_sub(footprint as u64);
            // The span's regions are consecutive slices of one contiguous
            // memory range starting at the start region's base; zero the whole
            // footprint once.
            unsafe {
                std::ptr::write_bytes(state.regions[start].base, 0, footprint);
            }
            for j in start..start + span {
                let region = &mut state.regions[j];
                region.header.kind = RegionKind::Free;
                region.header.alloc_top = region.base;
                region.header.gen_age = 0;
                region.header.live_bytes = 0;
                regions_freed += 1;
            }
        }

        for region in state.regions.iter_mut() {
            match region.header.kind {
                RegionKind::OldGen => {
                    let base = region.base as usize;
                    let top = region.header.alloc_top as usize;
                    let used = top.saturating_sub(base);

                    // A region holding a pinned object is never reclaimed, even if
                    // it is otherwise all garbage — the pin keeps its address live
                    // (bliss-jtc.18).
                    let pinned = used > 0 && unsafe { region_has_pinned(base, top) };
                    if region.header.live_bytes == 0 && !pinned {
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
                    // Handled by the unit-based large-object pass above
                    // (bliss-hy5v), which frees a start region together with
                    // its continuation regions and honours the pinned bit
                    // (bliss-7puh) on the start header.
                }
                RegionKind::Survivor => {
                    let base = region.base as usize;
                    let top = region.header.alloc_top as usize;
                    let used = top.saturating_sub(base);
                    let pinned = used > 0 && unsafe { region_has_pinned(base, top) };
                    if region.header.live_bytes == 0 && !pinned {
                        // Entirely garbage: reclaim, exactly as for OldGen.
                        // Survivor regions were previously never freed at all
                        // (bliss-wc4t).
                        unsafe {
                            std::ptr::write_bytes(region.base, 0, used);
                        }
                        region.header.kind = RegionKind::Free;
                        region.header.alloc_top = region.base;
                        region.header.gen_age = 0;
                        regions_freed += 1;
                    } else {
                        // Promote survivors that have aged past threshold to OldGen.
                        if region.header.gen_age >= self.promotion_threshold {
                            region.header.kind = RegionKind::OldGen;
                            region.header.gen_age = 0;
                        }
                        if top > base {
                            old_gen_used += region.header.live_bytes as u64;
                        }
                    }
                }
                _ => {}
            }
        }

        // A region made entirely of garbage may have been released above.
        // Never let a recycled slot refer to a freed or evacuated region.
        state.pinned_hosts.retain(|&index| {
            let region = &state.regions[index];
            region.header.kind == RegionKind::OldGen
                && unsafe {
                    region_has_pinned(region.base as usize, region.header.alloc_top as usize)
                }
        });
        state.free_pinned_slots.retain(|&(address, _)| {
            let index = (address - heap_base_addr) / region_size;
            state.pinned_hosts.contains(&index)
                && address < state.regions[index].header.alloc_top as usize
        });

        state.stats.major_gc_count += 1;
        state.stats.old_gen_used = old_gen_used;
        state.stats.regions_free += regions_freed;
        let elapsed_us = start.elapsed().as_micros() as u64;
        state.stats.total_major_pause_us += elapsed_us;

        // JFR-style event stream (bliss-u3h0): a major collection finished (see
        // the minor-GC note above for why recording here is GC-safe).
        crate::events::record(
            crate::events::EventKind::GcMajor,
            crate::events::NO_SYM,
            elapsed_us,
            regions_freed as u64,
        );

        self.gc_stats = state.stats.clone();

        // Return the mark scratch so the next major collection reuses its bitmap
        // capacity instead of reallocating over the whole old generation (the
        // minor collector does the same with MINOR_OBJECT_MAP). An early `?`
        // above merely loses that capacity: the take leaves a default map, and
        // the next cycle resets it before use.
        MAJOR_OBJECT_MAP.with(|s| *s.borrow_mut() = std::mem::take(&mut object_map));
        MAJOR_WORKLIST.with(|s| *s.borrow_mut() = std::mem::take(&mut scan_worklist));

        // Objects moved or regions were freed: rebuild every region's
        // off-heap-tracing list (bliss-3uwzg).
        OldGenScan::rebuild_regions(state);
        // After a drained nursery no older object refers into it, so writes can
        // be recorded afresh. KeepNursery left the nursery populated: older
        // pages written since the last re-arm may hold its only references, so
        // the tracker stays disarmed and the retried minor collection scans the
        // whole older generation.
        if !keep_nursery {
            crate::gc_soft_dirty::rearm(
                heap_base_addr,
                heap_size,
                old_generation_bytes(state),
                state.config.nursery_size,
                &older_tracking_regions(state),
            );
        }

        // Clear marking flag.
        drop(guard);
        set_gc_marking_in_progress(false);

        Ok(())
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
    satb_buffer: OrderedMutex<Vec<EgclVal>>,
    /// Card table — one byte per 512-byte card. A non-zero byte means the card is dirty.
    /// In a full implementation this would be a fixed-size array mapped over the heap;
    /// here we use a Vec sized to cover the configured heap.
    card_table: OrderedMutex<Vec<u8>>,
    /// Card size in bytes (default 512).
    card_shift: u32,
    /// Heap base address, used to compute card index from a slot address.
    heap_base: usize,
}

impl SatbCardBarrier {
    /// Create a new SATB+card barrier for the initialized heap.
    pub fn new() -> Result<Self, EgclError> {
        let guard = heap_state().lock().unwrap();
        let state = guard
            .as_ref()
            .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;
        let card_shift = 9; // 512-byte cards → shift by 9
        let card_count = (state.config.heap_size >> card_shift) + 1;
        let heap_base = state.heap_base as usize;
        Ok(SatbCardBarrier {
            satb_buffer: OrderedMutex::new(
                LockLevel::GcWorld,
                20,
                "SATB fallback buffer",
                Vec::with_capacity(state.config.satb_buffer_size),
            ),
            card_table: OrderedMutex::new(
                LockLevel::GcWorld,
                21,
                "GC card table",
                vec![0u8; card_count],
            ),
            card_shift,
            heap_base,
        })
    }

    /// Drain the SATB buffer, returning all logged old values.
    /// Called by the concurrent marker during marking termination.
    pub fn drain_satb_buffer(&self) -> Vec<EgclVal> {
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
    fn write_barrier(&self, slot_addr: *mut EgclVal, old_val: EgclVal, new_val: EgclVal) {
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

// ── Authoritative write barrier (bliss-jtc.21) ────────────────────
//
// Every reference store into a heap object routes through `write_barrier`
// (directly, via `store_ref`, or a compiler-emitted equivalent) so the
// generational invariants stay authoritative: old→young references are recorded
// in the remembered set before a minor GC can move the young referent, and the
// pre-write value is logged for concurrent old-gen marking (SATB). Immediate
// stores (fixnums, chars, NIL) record nothing.

/// Run the write barrier for a store of `new_val` into the slot at `slot_addr`,
/// whose current value is `old_val`. Records the SATB pre-write value (only
/// while marking) and adds the slot to the remembered set when a heap reference
/// is stored; the minor GC filters remembered slots to genuine old→young
/// pointers when it processes the set.
pub fn write_barrier(slot_addr: *mut EgclVal, old_val: EgclVal, new_val: EgclVal) {
    let mut guard = heap_state().lock().unwrap();
    if let Some(state) = guard.as_mut() {
        if gc_marking_in_progress() {
            state.satb_log.push(old_val);
        }
        // A slot inside a nursery object is not an old→young root: if its
        // holder is live, tracing the holder reaches the referent. Recording it
        // made every list built by tail mutation (`(setf (cdr tail) ..)`,
        // LOOP COLLECT) a root at the next minor collection, so dead lists
        // survived it and the whole nursery was copied every cycle.
        if is_heap_ref(new_val) && !slot_in_nursery(state, slot_addr as usize) {
            state.remembered.insert(slot_addr as usize);
        }
    }
}

/// Whether `slot_addr` lies in a nursery region of `state`'s heap.
fn slot_in_nursery(state: &HeapState, slot_addr: usize) -> bool {
    let base = state.heap_base as usize;
    if slot_addr < base {
        return false;
    }
    state
        .regions
        .get((slot_addr - base) / state.config.region_size)
        .is_some_and(|region| region.header.kind == RegionKind::Nursery)
}

/// Store `new` into the reference slot at `slot`, running the write barrier
/// first. This is the shared helper that mutators — the interpreter, stdlib,
/// and compiled stores — route reference writes through so the remembered set
/// and SATB log stay authoritative (bliss-jtc.21).
///
/// # Safety
/// `slot` must point at a valid, aligned `EgclVal` reference slot.
pub unsafe fn store_ref(slot: *mut EgclVal, new: EgclVal) {
    let old = unsafe { *slot };
    write_barrier(slot, old, new);
    unsafe { *slot = new };
}

/// Number of slots currently in the remembered set (test/diagnostic hook).
pub fn remembered_set_len() -> usize {
    let guard = heap_state().lock().unwrap();
    guard.as_ref().map(|s| s.remembered.len()).unwrap_or(0)
}

/// Drain the SATB pre-write log — consumed by the concurrent old-gen marker at
/// marking termination (§3.7.1).
pub fn drain_satb_log() -> Vec<EgclVal> {
    let mut guard = heap_state().lock().unwrap();
    guard
        .as_mut()
        .map(|s| std::mem::take(&mut s.satb_log))
        .unwrap_or_default()
}

// ── Precise pinning (bliss-jtc.18) ────────────────────────────────
//
// A pinned object must never be moved by the collector (needed across FFI /
// native calls, raw-pointer exposure, and identity hashing). Pinning is coarse
// at region granularity: a region that holds any live pinned object is retained
// in place — minor GC promotes such a nursery region without copying, and major
// GC never selects it for evacuation — so pinned objects keep their address.
// Large-object regions are non-moving unconditionally.

/// True if the region `[base, top)` holds a live (non-forwarded) pinned object.
/// Safety: `[base, top)` must be a walkable region of objects.
unsafe fn region_has_pinned(base: usize, top: usize) -> bool {
    let mut cursor = base;
    while cursor + OBJECT_HEADER_SIZE <= top {
        let ptr = cursor as *const u8;
        let (type_id, body_size) = unsafe { read_object_header(ptr) };
        if body_size == 0 && type_id == 0 {
            break;
        }
        if unsafe { !header_is_forwarded(ptr) && header_is_pinned(ptr) } {
            return true;
        }
        cursor += align_up(OBJECT_HEADER_SIZE + body_size as usize, OBJECT_ALIGNMENT);
    }
    false
}

fn set_pin_bit(v: EgclVal, pinned: bool) {
    if !is_heap_ref(v) {
        return;
    }
    let body = ref_body_addr(v);
    let mut guard = heap_state().lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    let base = state.heap_base as usize;
    if body < base + OBJECT_HEADER_SIZE || body >= base + state.config.heap_size {
        return;
    }
    // Large-object regions never move, so pinning within one is a no-op (and its
    // header sits at a different offset — never touch it here).
    let header_region = (body - OBJECT_HEADER_SIZE - base) / state.config.region_size;
    if let Some(region) = state.regions.get(header_region) {
        match region.header.kind {
            RegionKind::LargeObject => return,
            // The next minor collection must find this region to retain it.
            RegionKind::Nursery if pinned => {
                state.nursery_pin_candidates.insert(header_region);
            }
            _ => {}
        }
    }
    // SAFETY: `body` is a normal object body within the heap; its header
    // precedes it by OBJECT_HEADER_SIZE.
    let header = (body - OBJECT_HEADER_SIZE) as *mut ObjectHeader;
    unsafe {
        if pinned {
            (*header).set_pinned();
        } else {
            (*header).clear_pinned();
        }
    }
}

/// Pin the heap object referenced by `v` so the GC never moves it (bliss-jtc.18).
/// A no-op for immediates and for objects already in non-moving (large-object)
/// regions. Pin scope is the caller's responsibility: unpin once the raw pointer
/// / FFI exposure ends so the object can be compacted again.
pub fn pin(v: EgclVal) {
    set_pin_bit(v, true);
}

/// Release a pin taken with [`pin`], allowing the object to be moved again.
pub fn unpin(v: EgclVal) {
    set_pin_bit(v, false);
}

// ── External root scanners (bliss-jtc.8) ─────────────────────────────────────
//
// Some Lisp-visible heap objects own `EgclVal` storage *outside* the GC heap —
// e.g. a hash table's entries live in a Rust `Vec`. Their keys/values are still
// live references the collector must mark and relocate. Such a module registers
// a root scanner here; the collector calls every scanner during both the mark
// and the relocate passes with a visitor that receives each root reference slot.
//
// The scanner runs while the collector holds the heap lock, so it must not
// allocate on the GC heap or block on it. Registration is expected once, at
// startup (idempotent by function pointer).

/// A scanner that yields each external root reference slot to `visit`.
pub type RootScanner = fn(&mut dyn FnMut(*mut EgclVal));

fn root_scanners() -> &'static OrderedMutex<Vec<RootScanner>> {
    static S: OnceLock<OrderedMutex<Vec<RootScanner>>> = OnceLock::new();
    S.get_or_init(|| OrderedMutex::new(LockLevel::GcWorld, 4, "GC root scanners", Vec::new()))
}

/// Register an external root scanner (idempotent by function pointer).
pub fn register_root_scanner(f: RootScanner) {
    let mut v = root_scanners().lock().unwrap();
    if !v.iter().any(|&g| g as usize == f as usize) {
        v.push(f);
    }
}

/// Host-cache preparation performed before any external roots are enumerated.
/// Runs under the same stopped-world/heap-snapshot exclusion as root scanners.
/// It may discard host-owned cache roots, but must not allocate Lisp objects,
/// evaluate, poll, or recursively enumerate roots. Active references must retain
/// their own precise roots independently of an evictable cache.
pub type RootPreparer = fn();

fn root_preparers() -> &'static OrderedMutex<Vec<RootPreparer>> {
    static PREPARERS: OnceLock<OrderedMutex<Vec<RootPreparer>>> = OnceLock::new();
    PREPARERS
        .get_or_init(|| OrderedMutex::new(LockLevel::GcWorld, 4, "GC root preparers", Vec::new()))
}

/// Register host-cache preparation, idempotently by function pointer.
/// Registration is expected once at startup. Preparers always run before all
/// external scanners, regardless of the two registration orders. No preparer
/// or scanner registry lock is held while a preparer runs, so discarding a
/// host root may safely unregister it from its own root registry.
pub fn register_root_preparer(prepare: RootPreparer) {
    let mut preparers = root_preparers().lock().unwrap();
    if !preparers
        .iter()
        .any(|&other| other as usize == prepare as usize)
    {
        preparers.push(prepare);
    }
}

fn prepare_external_roots() {
    let preparers = root_preparers().lock().unwrap().clone();
    for prepare in preparers {
        prepare();
    }
}

/// Monotone id of the current external-root scan pass. Bumped once each time
/// the collector begins a full pass over the registered scanners, so host-side
/// visitors can share one visited set per pass — deduplicating structures (env
/// frame chains) reachable from many roots — and know exactly when to reset it
/// (bliss-s56e: without this, every rooted Env re-walked shared frame chains on
/// every minor GC, dominating allocation-heavy loads).
static SCAN_PASS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current root-scan pass id (see [`SCAN_PASS`]). Only meaningful while a
/// collection is scanning; host visitors compare it against their cached id.
pub fn root_scan_pass() -> u64 {
    SCAN_PASS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Advance the root-scan pass id by one and return the new value. The collector
/// bumps [`SCAN_PASS`] itself at the start of each real scan; this is for code
/// that drives the host root visitors OUTSIDE a collection (e.g. a test that
/// invokes `visit_gc_roots` directly and needs a *fresh* per-pass visited-set so
/// a second manual walk is not suppressed by the first walk's dedup state).
#[doc(hidden)]
pub fn advance_root_scan_pass() -> u64 {
    SCAN_PASS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

// Diagnostics must compare duplicate root homes before any relocation. This
// marker is debug-only; production root scans keep their existing cost.
#[cfg(debug_assertions)]
thread_local! {
    static MARKING_EXTERNAL_ROOTS: Cell<bool> = const { Cell::new(false) };
}

/// True only while this collector thread enumerates external roots for marking,
/// before it rewrites any root. For read-only debug verification by scanners.
#[cfg(debug_assertions)]
pub fn marking_external_roots() -> bool {
    MARKING_EXTERNAL_ROOTS.with(Cell::get)
}

fn scan_external_mark_roots(kind: ExternalRootScanKind, visit: impl FnMut(*mut EgclVal)) {
    #[cfg(debug_assertions)]
    struct Marking(bool);
    #[cfg(debug_assertions)]
    impl Drop for Marking {
        fn drop(&mut self) {
            MARKING_EXTERNAL_ROOTS.with(|slot| slot.set(self.0));
        }
    }
    #[cfg(debug_assertions)]
    let _marking = Marking(MARKING_EXTERNAL_ROOTS.with(|slot| slot.replace(true)));
    scan_external_roots_with_kind(kind, visit);
}

/// Invoke every registered external root scanner with `visit`.
fn scan_external_roots(visit: impl FnMut(*mut EgclVal)) {
    scan_external_roots_with_kind(ExternalRootScanKind::Full, visit);
}

fn scan_external_roots_with_kind(kind: ExternalRootScanKind, mut visit: impl FnMut(*mut EgclVal)) {
    let _kind = root_kind::Scope::enter(kind);
    SCAN_PASS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    prepare_external_roots();
    let scanners = root_scanners().lock().unwrap().clone();
    for s in scanners {
        let root_clock = root_phase::start();
        s(&mut visit);
        root_phase::finish(root_clock, b"external_scanner", 0, s as *const () as usize);
    }
}

/// Inspect host roots for application shake, replacing registry scanners
/// with the caller's semantic dependency graph.
///
/// # Safety
/// Call only inside `with_heap_snapshot`, without allocating Lisp objects.
/// `excluded` must be covered by the shake analyzer (including captures).
/// The callback must not mutate slots or let unrooted values escape the snapshot.
pub unsafe fn visit_shake_host_roots(
    excluded: &[RootScanner],
    visit: &mut dyn FnMut(*mut EgclVal),
) {
    let _kind = root_kind::Scope::enter(ExternalRootScanKind::Full);
    advance_root_scan_pass();
    prepare_external_roots();
    let scanners = root_scanners().lock().unwrap().clone();
    for scanner in scanners {
        if !excluded
            .iter()
            .any(|&skip| scanner as usize == skip as usize)
        {
            scanner(visit);
        }
    }
}

/// Inspect a live object's reference fields using the same layouts as the GC.
/// Symbol/package identity does not imply retention of all their definitions;
/// shake handles symbol cells separately.
///
/// # Safety
/// `value` must be live, and the call must be inside `with_heap_snapshot`.
/// The callback must neither allocate Lisp objects nor mutate reference slots.
pub unsafe fn visit_shake_references(value: EgclVal, visit: &mut dyn FnMut(EgclVal)) {
    if !is_heap_ref(value) {
        return;
    }
    let value = resolve_forwarded(value);
    let address = ref_body_addr(value);
    let base = HEAP_RANGE_BASE.load(Ordering::Relaxed);
    let end = HEAP_RANGE_END.load(Ordering::Relaxed);
    // Native entry points and pinned objects outside the managed heap also
    // carry pointer tags. The GC does not interpret their bytes as headers.
    if base == 0 || address < base + OBJECT_HEADER_SIZE || address >= end {
        return;
    }
    let header = (address - OBJECT_HEADER_SIZE) as *const u8;
    // SAFETY: the caller provides a live object under a stopped-world snapshot.
    let (kind, size) = unsafe { read_object_header(header) };
    if matches!(
        kind,
        crate::object::type_id::SYMBOL | crate::object::type_id::PACKAGE
    ) {
        return;
    }
    let body = unsafe { header.add(body_offset(header)) };
    unsafe { trace_object(body as *mut u8, kind, size as usize, |slot| visit(*slot)) };
}

// ── Host-container / in-place stack roots (bliss-6b2 #2) ───────────────────
//
// `StackRoot` registers the *address of an existing Rust local* `EgclVal` slot
// so a moving collection rewrites that local in place — no `.get()` indirection,
// so existing evaluator code that reads the local directly keeps working. This
// is the ergonomic retrofit primitive for the pervasive dispatch-level locals
// (`eval_list` car/cdr, operand splits) that a copying GC would otherwise leave
// dangling. `HostRoot<T>` is the owning counterpart for a Rust container (e.g. a
// `Vec<EgclVal>`) that must stay rooted while its owner calls allocating Lisp
// code. Both are thread-affine and unregister on drop; the caller must keep the
// referenced slot immobile (not moved/returned by value) for the guard's extent.

pub trait TraceHostRoots {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal));
}

// Scalar operation results contain no Lisp pointers, but can be part of a
// rooted Result/tuple that also carries a Lisp value or error datum.
impl TraceHostRoots for () {
    fn trace_host_roots(&mut self, _visit: &mut dyn FnMut(*mut EgclVal)) {}
}
impl TraceHostRoots for bool {
    fn trace_host_roots(&mut self, _visit: &mut dyn FnMut(*mut EgclVal)) {}
}
impl TraceHostRoots for u64 {
    fn trace_host_roots(&mut self, _visit: &mut dyn FnMut(*mut EgclVal)) {}
}

impl TraceHostRoots for EgclVal {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(self as *mut EgclVal);
    }
}

impl<T: TraceHostRoots> TraceHostRoots for Vec<T> {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for value in self {
            value.trace_host_roots(visit);
        }
    }
}

/// Roots a fixed-size array in place, so a caller can root a small, stack-
/// resident argument buffer instead of heap-allocating a `Vec` to root
/// (bliss-lxpg.1). Every element is visited, so unused trailing slots must hold
/// a traceable value — `NIL` is the natural filler.
impl<T: TraceHostRoots, const N: usize> TraceHostRoots for [T; N] {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for value in self {
            value.trace_host_roots(visit);
        }
    }
}

impl<T: TraceHostRoots> TraceHostRoots for Option<T> {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let Some(value) = self {
            value.trace_host_roots(visit);
        }
    }
}

impl<A: TraceHostRoots, B: TraceHostRoots> TraceHostRoots for (A, B) {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.0.trace_host_roots(visit);
        self.1.trace_host_roots(visit);
    }
}

impl<T: TraceHostRoots, E: TraceHostRoots> TraceHostRoots for Result<T, E> {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        match self {
            Ok(value) => value.trace_host_roots(visit),
            Err(error) => error.trace_host_roots(visit),
        }
    }
}

impl TraceHostRoots for EgclError {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        match self {
            EgclError::TypeError { datum, .. }
            | EgclError::IoTimeout { stream: datum, .. }
            | EgclError::UnboundVariable(datum)
            | EgclError::UndefinedFunction(datum) => datum.trace_host_roots(visit),
            // The already-signalled condition is a live EgclVal and must be
            // relocated with the moving GC (bliss-9kc).
            EgclError::Signalled(details) => {
                details.condition.trace_host_roots(visit);
                details.backtrace.trace_host_roots(visit);
            }
            EgclError::Traced(details) => {
                details.error.trace_host_roots(visit);
                details.backtrace.trace_host_roots(visit);
            }
            // A Python raise carries strings and a PyObject pointer. The pointer
            // names an object in CPython's heap, which this collector neither
            // moves nor traces, so there is nothing here to visit.
            #[cfg(feature = "python")]
            EgclError::PythonRaised(_) => {}
            EgclError::Oom
            | EgclError::StackOverflow(_)
            | EgclError::InvalidImage(_)
            | EgclError::FfiError(_)
            | EgclError::SignalError(_)
            | EgclError::Interrupt
            | EgclError::Timeout
            | EgclError::Shutdown
            | EgclError::Internal(_)
            | EgclError::ProgramError(_)
            | EgclError::ArithmeticError(_)
            | EgclError::PackageError(_)
            | EgclError::StreamError(_)
            | EgclError::FileError(_)
            | EgclError::SocketError { .. }
            | EgclError::SandboxViolation(_)
            | EgclError::ControlError(_) => {}
        }
    }
}

// Cross-thread roots use a reader/writer gate in addition to their per-value
// mutex. A moving collection owns the write side for its whole mark/relocate
// cycle; a background reader owns the read side while inspecting the rooted
// value. Merely locking during each external-root scan would leave a race in
// the interval between marking the old address and rewriting the slot.
fn cross_thread_root_gc_gate() -> &'static RwLock<()> {
    static GATE: OnceLock<RwLock<()>> = OnceLock::new();
    GATE.get_or_init(|| RwLock::new(()))
}

/// The exclusive root-reader gate also serializes collectors. A competing
/// collector is still a mutator until admitted: blocking in RwLock::write
/// would prevent it from acknowledging the current collector's safepoint.
/// Poll only while holding no gate, then retry admission after the pause.
fn acquire_gc_admission() -> std::sync::RwLockWriteGuard<'static, ()> {
    loop {
        match cross_thread_root_gc_gate().try_write() {
            Ok(guard) => return guard,
            Err(std::sync::TryLockError::Poisoned(error)) => return error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                #[cfg(all(test, target_arch = "x86_64", any(unix, windows)))]
                allocator_admission_tests::before_poll();
                crate::safepoint::poll_safepoint();
                std::thread::yield_now();
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod native_tlab_tests {
    use super::*;

    /// What generated code does: bump the published window, or decline.
    fn native_bump(bytes: usize) -> Option<usize> {
        NATIVE_TLAB.with(|window| {
            let cursor = window.cursor.get();
            (window.epoch.get() == GC_MOVE_EPOCH.load(Ordering::Acquire)
                && cursor + bytes <= window.limit.get())
            .then(|| {
                window.cursor.set(cursor + bytes);
                cursor
            })
        })
    }

    fn native_cons(layout: NativeAllocLayout) -> Option<usize> {
        let header = native_bump(layout.cons_footprint as usize)?;
        // SAFETY: the window handed out these bytes of a zeroed TLAB.
        unsafe {
            *(header as *mut u64) = layout.cons_header;
            *((header + 8) as *mut EgclVal) = EgclVal::from_fixnum(7);
            *((header + 16) as *mut EgclVal) = crate::value::NIL;
        }
        Some(header)
    }

    /// Run `body` until no collection (here or on another test thread)
    /// changes the epoch while it runs; the window is invalid across one.
    fn without_collection<R>(mut body: impl FnMut() -> Option<R>) -> R {
        for _ in 0..1000 {
            let epoch = GC_MOVE_EPOCH.load(Ordering::Acquire);
            if let Some(result) = body()
                && epoch == GC_MOVE_EPOCH.load(Ordering::Acquire)
            {
                return result;
            }
        }
        panic!("collections never paused long enough to observe the window");
    }

    #[test]
    fn layout_describes_a_cons_and_a_double() {
        let layout = native_alloc_layout().expect("x86-64 Linux supports inline allocation");
        assert_eq!(layout.cons_footprint as usize, object_footprint(16).0);
        assert_eq!(layout.double_footprint as usize, object_footprint(8).0);
        let cons = ObjectHeader(layout.cons_header);
        assert_eq!(cons.type_id(), crate::object::type_id::CONS);
        let double = ObjectHeader(layout.double_header);
        assert_eq!(double.type_id(), crate::object::type_id::DOUBLE_FLOAT);
        assert_eq!(layout.epoch_address, GC_MOVE_EPOCH.as_ptr() as u64);
    }

    #[test]
    fn every_thread_finds_its_window_at_the_same_offset() {
        let expected = native_alloc_offset().map(i64::from);
        assert!(expected.is_some());
        let offsets: Vec<_> = (0..4)
            .map(|_| std::thread::spawn(native_tlab_thread_offset))
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(
            offsets.iter().all(|offset| *offset == expected),
            "{offsets:?} vs {expected:?}"
        );
    }

    #[test]
    fn runtime_allocation_continues_past_inline_objects() {
        let layout = native_alloc_layout().unwrap();
        let (first, inline, next) = without_collection(|| {
            let first = alloc_typed(16, crate::object::type_id::CONS)? as usize;
            let inline = native_cons(layout)?;
            let next = alloc_typed(16, crate::object::type_id::CONS)? as usize;
            // A TLAB refill between the two runtime allocations proves
            // nothing; try again.
            let refilled = T0_ALLOCATOR.with(|cell| {
                cell.borrow().as_ref().is_some_and(|(_, allocator)| {
                    !(allocator.tlab.cursor as usize > first
                        && (allocator.tlab.cursor as usize) - first < allocator.tlab_size)
                })
            });
            (!refilled).then_some((first, inline, next))
        });
        // Bodies follow their one-word headers, each cons 32 bytes apart.
        assert_eq!(
            inline,
            first - OBJECT_HEADER_SIZE + layout.cons_footprint as usize
        );
        assert_eq!(
            next,
            inline + layout.cons_footprint as usize + OBJECT_HEADER_SIZE
        );
        // SAFETY: the inline cons is still allocated and was not overwritten.
        assert_eq!(unsafe { *(inline as *const u64) }, layout.cons_header);
    }

    #[test]
    fn absorbing_the_window_counts_inline_bytes() {
        let layout = native_alloc_layout().unwrap();
        let counted = without_collection(|| {
            alloc_typed(16, crate::object::type_id::CONS)?;
            let before =
                T0_ALLOCATOR.with(|cell| cell.borrow().as_ref().map(|(_, a)| a.pending_bytes))?;
            native_cons(layout)?;
            native_cons(layout)?;
            T0_ALLOCATOR.with(|cell| {
                let mut guard = cell.borrow_mut();
                let (epoch, allocator) = guard.as_mut()?;
                absorb_native_tlab(allocator, *epoch);
                Some(allocator.pending_bytes.checked_sub(before))
            })?
        });
        // A flush in between (none expected) would only lower the count.
        assert_eq!(counted, 2 * layout.cons_footprint as usize);
    }

    #[test]
    fn safepoint_tail_filler_starts_after_inline_objects() {
        let layout = native_alloc_layout().unwrap();
        let (inline, cursor) = without_collection(|| {
            alloc_typed(16, crate::object::type_id::CONS)?;
            let inline = native_cons(layout)?;
            publish_current_t0_tlab_for_safepoint();
            let cursor = T0_ALLOCATOR.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map(|(_, allocator)| allocator.tlab.cursor as usize)
            })?;
            Some((inline, cursor))
        });
        assert_eq!(cursor, inline + layout.cons_footprint as usize);
        // SAFETY: both words lie in this thread's live TLAB.
        unsafe {
            assert_eq!(*(inline as *const u64), layout.cons_header);
            assert_eq!(
                ObjectHeader(*(cursor as *const u64)).type_id(),
                0,
                "tail filler"
            );
        }
    }

    #[test]
    fn retiring_for_a_safepoint_disables_the_window() {
        let layout = native_alloc_layout().unwrap();
        without_collection(|| {
            alloc_typed(16, crate::object::type_id::CONS)?;
            native_cons(layout)?;
            Some(())
        });
        retire_current_t0_tlab_for_safepoint();
        assert!(native_bump(layout.cons_footprint as usize).is_none());
    }
}

#[cfg(all(test, target_arch = "x86_64", any(unix, windows)))]
mod allocator_admission_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    static ARMED: AtomicBool = AtomicBool::new(false);
    static REACHED: AtomicBool = AtomicBool::new(false);
    static OBSERVER_QUEUED: AtomicBool = AtomicBool::new(false);

    pub(super) fn before_poll() {
        if ARMED.load(Ordering::Acquire)
            && T0_ALLOCATOR.with(|cell| cell.try_borrow_mut().is_err())
            && ARMED.swap(false, Ordering::AcqRel)
        {
            crate::thread::current_fiber().unwrap().request_yield();
            REACHED.store(true, Ordering::Release);
            while !OBSERVER_QUEUED.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }
    }

    fn allocating() -> EgclVal {
        while !REACHED.load(Ordering::Acquire) {
            if alloc_typed(1024, crate::object::type_id::DOUBLE_FLOAT).is_none() {
                return EgclVal::from_fixnum(2);
            }
        }
        EgclVal::from_fixnum(!crate::thread::current_fiber().unwrap().can_yield() as i64)
    }

    fn observing() -> EgclVal {
        EgclVal::from_fixnum(T0_ALLOCATOR.with(|cell| cell.try_borrow_mut().is_err()) as i64)
    }

    #[test]
    fn collector_admission_does_not_suspend_a_borrowed_carrier_allocator() {
        const CHILD: &str = "EGCL_ALLOCATOR_ADMISSION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "gc::allocator_admission_tests::collector_admission_does_not_suspend_a_borrowed_carrier_allocator", "--nocapture"])
                .env(CHILD, "1")
                .env_remove("EGCL_GC_STRESS")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        init_heap(&GcConfig {
            heap_size: 4 * 1024 * 1024,
            heap_max: 16 * 1024 * 1024,
            nursery_size: 64 * 1024,
            tlab_size: 4096,
            region_size: 4096,
            promotion_threshold: 3,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 32,
            old_occupancy_trigger: 0.9,
        })
        .unwrap();
        let gate = cross_thread_root_gc_gate().read().unwrap();
        let group =
            crate::SchedulerGroup::init(&crate::SchedulerConfig { num_workers: 1 }).unwrap();
        let function = unsafe { EgclVal::from_function_ptr(allocating as *const () as *mut u8) };
        group
            .submit(crate::thread::make_fiber(function).unwrap())
            .unwrap();
        ARMED.store(true, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !REACHED.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "allocator never waited for admission"
            );
            std::thread::yield_now();
        }
        let observer = unsafe { EgclVal::from_function_ptr(observing as *const () as *mut u8) };
        group
            .submit(crate::thread::make_fiber(observer).unwrap())
            .unwrap();
        OBSERVER_QUEUED.store(true, Ordering::Release);
        // A pinned allocator must still acknowledge another collector while
        // waiting for admission; otherwise pinning trades the borrow bug for
        // a stop-the-world deadlock.
        let paused = crate::safepoint::wait_for_all_threads();
        if paused.is_ok() {
            crate::safepoint::resume_all_threads().unwrap();
        }
        // The test driver is now a registered mutator. Stay quiescent while
        // the worker collects and we wait for its immediate-valued results.
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        drop(gate);
        assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(0); 2]);
        assert!(
            paused.is_ok(),
            "pinned allocator missed the GC pause: {paused:?}"
        );
    }
}

#[derive(Clone, Copy)]
struct CrossThreadRootEntry {
    address: usize,
    trace: unsafe fn(usize, &mut dyn FnMut(*mut EgclVal)),
}

fn cross_thread_roots() -> &'static OrderedMutex<HashMap<usize, CrossThreadRootEntry>> {
    static ROOTS: OnceLock<OrderedMutex<HashMap<usize, CrossThreadRootEntry>>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            6,
            "GC cross-thread roots",
            HashMap::new(),
        )
    })
}

struct CrossThreadRootInner<T: TraceHostRoots + Send + 'static> {
    value: Mutex<T>,
}

unsafe fn trace_cross_thread_root<T: TraceHostRoots + Send + 'static>(
    address: usize,
    visit: &mut dyn FnMut(*mut EgclVal),
) {
    // SAFETY: the Arc allocation is stable, and CrossThreadRootInner::drop
    // removes the entry under the registry lock before the allocation is freed.
    let inner = unsafe { &*(address as *const CrossThreadRootInner<T>) };
    inner
        .value
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .trace_host_roots(visit);
}

fn scan_cross_thread_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let roots = cross_thread_roots()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for entry in roots.values() {
        // SAFETY: the registry lock excludes the last Arc's unregister/drop.
        unsafe { (entry.trace)(entry.address, visit) };
    }
}

fn install_cross_thread_root_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| register_root_scanner(scan_cross_thread_roots));
}

impl<T: TraceHostRoots + Send + 'static> Drop for CrossThreadRootInner<T> {
    fn drop(&mut self) {
        let address = self as *mut Self as usize;
        let removed = cross_thread_roots()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&address);
        debug_assert!(removed.is_some(), "cross-thread root missing from registry");
    }
}

/// A cloneable precise root for data handed to a non-mutator worker thread.
///
/// Moving GC rewrites every [`EgclVal`] reached through `T` in place. Use
/// [`CrossThreadRoot::with_gc_stable`] to inspect the value: its closure excludes
/// moving collections, so the worker cannot observe an address halfway through
/// a mark/relocate cycle. The closure must not allocate on the Lisp heap or
/// otherwise trigger GC, because collection waits for all such readers to exit;
/// its return value must not retain a `EgclVal` copied out of the rooted data.
#[derive(Clone)]
pub struct CrossThreadRoot<T: TraceHostRoots + Send + 'static> {
    inner: Arc<CrossThreadRootInner<T>>,
}

impl<T: TraceHostRoots + Send + 'static> CrossThreadRoot<T> {
    pub fn new(value: T) -> Self {
        install_cross_thread_root_scanner();
        let inner = Arc::new(CrossThreadRootInner {
            value: Mutex::new(value),
        });
        let address = Arc::as_ptr(&inner) as usize;
        let previous = cross_thread_roots()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                address,
                CrossThreadRootEntry {
                    address,
                    trace: trace_cross_thread_root::<T>,
                },
            );
        debug_assert!(previous.is_none(), "duplicate cross-thread root address");
        Self { inner }
    }

    pub fn with_gc_stable<R>(&self, inspect: impl FnOnce(&T) -> R) -> R {
        let _gc_stable = cross_thread_root_gc_gate()
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let value = self
            .inner
            .value
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        inspect(&value)
    }

    /// Inspect from a registered Running mutator. Unlike the non-mutator
    /// reader, this must acknowledge a collector while waiting for its gate:
    /// the collector owns that gate before asking Running threads to park.
    /// The same no-allocation/no-unrooted-value-escape contract applies.
    pub fn with_gc_stable_mutator<R>(&self, inspect: impl FnOnce(&T) -> R) -> R {
        let _gc_stable = loop {
            match cross_thread_root_gc_gate().try_read() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    crate::safepoint::poll_safepoint();
                    std::thread::yield_now();
                }
            }
        };
        let value = self
            .inner
            .value
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        inspect(&value)
    }
}

#[cfg(test)]
mod callback_root_admission_tests {
    use super::*;

    #[test]
    fn a_running_root_reader_acknowledges_gc_while_waiting_for_the_gate() {
        const CHILD: &str = "EGCL_CALLBACK_ROOT_GATE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "gc::callback_root_admission_tests::a_running_root_reader_acknowledges_gc_while_waiting_for_the_gate", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        ensure_heap_initialized();
        crate::thread::current_thread_id();
        let root = CrossThreadRoot::new(EgclVal::from_fixnum(42));
        let gate = cross_thread_root_gc_gate().write().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            crate::thread::current_thread_id();
            ready_tx.send(()).unwrap();
            root.with_gc_stable_mutator(|value| value.as_fixnum())
        });
        ready_rx.recv().unwrap();
        let paused = crate::safepoint::wait_for_all_threads();
        if paused.is_ok() {
            crate::safepoint::resume_all_threads().unwrap();
        }
        drop(gate);
        assert_eq!(reader.join().unwrap(), 42);
        assert!(
            paused.is_ok(),
            "a participating reader must poll instead of blocking its collector: {paused:?}"
        );
    }
}

#[derive(Clone, Copy)]
struct HostRootEntry {
    address: usize,
    trace: unsafe fn(usize, &mut dyn FnMut(*mut EgclVal)),
}

fn host_roots() -> &'static OrderedMutex<HashMap<ThreadId, Vec<HostRootEntry>>> {
    static ROOTS: OnceLock<OrderedMutex<HashMap<ThreadId, Vec<HostRootEntry>>>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            6,
            "GC host-container roots",
            HashMap::new(),
        )
    })
}

unsafe fn trace_host_root<T: TraceHostRoots>(address: usize, visit: &mut dyn FnMut(*mut EgclVal)) {
    // SAFETY: HostRoot registers the address of its boxed T and unregisters it
    // before dropping the box. Moving HostRoot does not move the box allocation.
    unsafe { (&mut *(address as *mut T)).trace_host_roots(visit) };
}

fn scan_host_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut roots = host_roots().lock().unwrap_or_else(|e| e.into_inner());
    for entries in roots.values_mut() {
        for entry in entries {
            // SAFETY: every live registry entry belongs to a live HostRoot/StackRoot.
            unsafe { (entry.trace)(entry.address, visit) };
        }
    }
}

fn install_host_root_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| register_root_scanner(scan_host_roots));
}

/// A stable, precisely-scanned owner for Rust containers that hold Lisp values.
///
/// Unlike [`ShadowRootScope`], this is suitable for a `Vec<EgclVal>` that must
/// itself be returned from a helper and remain rooted while its caller invokes
/// allocating Lisp code. The value is boxed so moving this handle cannot
/// invalidate the registered address.
pub struct HostRoot<T: TraceHostRoots> {
    thread: ThreadId,
    fiber: Option<crate::thread::FiberId>,
    value: Box<T>,
    address: usize,
    _not_send: PhantomData<Rc<()>>,
}

impl<T: TraceHostRoots> HostRoot<T> {
    pub fn new(value: T) -> Self {
        install_host_root_scanner();
        let thread = std::thread::current().id();
        let mut value = Box::new(value);
        let address = (&mut *value as *mut T) as usize;
        host_roots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(thread)
            .or_default()
            .push(HostRootEntry {
                address,
                trace: trace_host_root::<T>,
            });
        Self {
            thread,
            fiber: crate::thread::current_fiber_id(),
            value,
            address,
            _not_send: PhantomData,
        }
    }
}

/// A precise GC root for an existing Rust stack/local `EgclVal` slot.
///
/// This is for evaluator code that already keeps values in mutable locals and
/// must have those locals rewritten in place by a moving collection. The caller
/// must ensure the referenced slot outlives the guard and is not moved while the
/// guard is live.
pub struct StackRoot {
    thread: ThreadId,
    address: usize,
    _not_send: PhantomData<Rc<()>>,
}

impl StackRoot {
    pub fn new(slot: &mut EgclVal) -> Self {
        install_host_root_scanner();
        let thread = std::thread::current().id();
        let address = slot as *mut EgclVal as usize;
        host_roots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(thread)
            .or_default()
            .push(HostRootEntry {
                address,
                trace: trace_host_root::<EgclVal>,
            });
        Self {
            thread,
            address,
            _not_send: PhantomData,
        }
    }
}

impl Drop for StackRoot {
    fn drop(&mut self) {
        assert_eq!(
            self.thread,
            std::thread::current().id(),
            "stack roots are thread-affine"
        );
        let mut roots = host_roots().lock().unwrap_or_else(|e| e.into_inner());
        let remove_thread = if let Some(entries) = roots.get_mut(&self.thread) {
            let index = entries
                .iter()
                .rposition(|entry| entry.address == self.address)
                .expect("stack root missing from registry");
            entries.remove(index);
            entries.is_empty()
        } else {
            false
        };
        if remove_thread {
            roots.remove(&self.thread);
        }
    }
}

impl<T: TraceHostRoots> Deref for HostRoot<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: TraceHostRoots> DerefMut for HostRoot<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<T: TraceHostRoots> Drop for HostRoot<T> {
    fn drop(&mut self) {
        if let Some(fiber) = self.fiber {
            assert_eq!(
                Some(fiber),
                crate::thread::current_fiber_id(),
                "host root belongs to another fiber"
            );
        } else {
            assert_eq!(
                self.thread,
                std::thread::current().id(),
                "native host roots are thread-affine"
            );
        }
        let mut roots = host_roots().lock().unwrap_or_else(|e| e.into_inner());
        let remove_thread = if let Some(entries) = roots.get_mut(&self.thread) {
            let index = entries
                .iter()
                .rposition(|entry| entry.address == self.address)
                .expect("host root missing from registry");
            entries.remove(index);
            entries.is_empty()
        } else {
            false
        };
        if remove_thread {
            roots.remove(&self.thread);
        }
    }
}

// ── Scoped shadow roots for host-language temporaries (bliss-6b2.1) ────────
//
// The tree-walker keeps Lisp values in Rust locals rather than on EgclStack.
// A copying collection cannot conservatively find or rewrite those values. A
// ShadowRootScope gives such code stable, precisely scanned handle slots. The
// slots live in a process registry (partitioned by mutator thread), so the GC
// can visit every thread's active roots rather than only its own TLS.

fn shadow_roots() -> &'static OrderedMutex<HashMap<ThreadId, Vec<EgclVal>>> {
    static ROOTS: OnceLock<OrderedMutex<HashMap<ThreadId, Vec<EgclVal>>>> = OnceLock::new();
    ROOTS
        .get_or_init(|| OrderedMutex::new(LockLevel::GcWorld, 5, "GC shadow roots", HashMap::new()))
}

fn scan_shadow_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut roots = shadow_roots().lock().unwrap_or_else(|e| e.into_inner());
    for stack in roots.values_mut() {
        for value in stack {
            visit(value as *mut EgclVal);
        }
    }
}

fn install_shadow_root_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| register_root_scanner(scan_shadow_roots));
}

/// A lexical extent containing precise GC handles for Rust-side `EgclVal`
/// temporaries. Dropping the scope removes every handle created through it,
/// including during error unwinding.
///
/// Scopes are thread-affine and must nest in normal stack order. A rooted value
/// is read back through [`ShadowRoot::get`] after any operation that may collect;
/// callers must not retain and later use the original unrooted `EgclVal` copy.
pub struct ShadowRootScope {
    thread: ThreadId,
    base: usize,
    _not_send: PhantomData<Rc<()>>,
}

impl ShadowRootScope {
    /// Start a new shadow-root extent on the current mutator thread.
    pub fn new() -> Self {
        install_shadow_root_scanner();
        let thread = std::thread::current().id();
        let base = shadow_roots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(thread)
            .or_default()
            .len();
        Self {
            thread,
            base,
            _not_send: PhantomData,
        }
    }

    /// Copy `value` into a precise mutable root slot tied to this scope.
    pub fn root(&self, value: EgclVal) -> ShadowRoot<'_> {
        assert_eq!(
            self.thread,
            std::thread::current().id(),
            "shadow roots are thread-affine"
        );
        let mut roots = shadow_roots().lock().unwrap_or_else(|e| e.into_inner());
        let stack = roots.entry(self.thread).or_default();
        let index = stack.len();
        stack.push(value);
        ShadowRoot {
            thread: self.thread,
            index,
            _scope: PhantomData,
        }
    }

    /// Root every value in iteration order, returning one handle per value.
    pub fn root_values(&self, values: impl IntoIterator<Item = EgclVal>) -> Vec<ShadowRoot<'_>> {
        assert_eq!(
            self.thread,
            std::thread::current().id(),
            "shadow roots are thread-affine"
        );
        let values: Vec<_> = values.into_iter().collect();
        let mut roots = shadow_roots().lock().unwrap_or_else(|e| e.into_inner());
        let stack = roots.entry(self.thread).or_default();
        let start = stack.len();
        stack.extend_from_slice(&values);
        (start..start + values.len())
            .map(|index| ShadowRoot {
                thread: self.thread,
                index,
                _scope: PhantomData,
            })
            .collect()
    }
}

impl Default for ShadowRootScope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ShadowRootScope {
    fn drop(&mut self) {
        assert_eq!(
            self.thread,
            std::thread::current().id(),
            "shadow-root scope dropped on a different thread"
        );
        let mut roots = shadow_roots().lock().unwrap_or_else(|e| e.into_inner());
        let remove = if let Some(stack) = roots.get_mut(&self.thread) {
            assert!(
                stack.len() >= self.base,
                "shadow-root scopes must be dropped in stack order"
            );
            stack.truncate(self.base);
            stack.is_empty()
        } else {
            false
        };
        if remove {
            roots.remove(&self.thread);
        }
    }
}

/// A precise, relocatable handle to one `EgclVal` in a [`ShadowRootScope`].
pub struct ShadowRoot<'scope> {
    thread: ThreadId,
    index: usize,
    _scope: PhantomData<&'scope ShadowRootScope>,
}

impl ShadowRoot<'_> {
    /// Read the current value, including any relocation performed by the GC.
    pub fn get(&self) -> EgclVal {
        shadow_roots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.thread)
            .and_then(|stack| stack.get(self.index))
            .copied()
            .expect("shadow root used outside its scope")
    }

    /// Replace the value held by this root slot.
    pub fn set(&self, value: EgclVal) {
        let mut roots = shadow_roots().lock().unwrap_or_else(|e| e.into_inner());
        let slot = roots
            .get_mut(&self.thread)
            .and_then(|stack| stack.get_mut(self.index))
            .expect("shadow root used outside its scope");
        *slot = value;
    }
}

#[cfg(test)]
mod shadow_root_scope_tests {
    use super::*;

    fn current_depth() -> usize {
        shadow_roots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&std::thread::current().id())
            .map_or(0, Vec::len)
    }

    #[test]
    fn scopes_nest_and_unwind_without_leaking_slots() {
        assert_eq!(current_depth(), 0);
        let outer = ShadowRootScope::new();
        let first = outer.root(EgclVal::from_fixnum(1));
        assert_eq!(current_depth(), 1);
        {
            let inner = ShadowRootScope::new();
            let second = inner.root(EgclVal::from_fixnum(2));
            assert_eq!(current_depth(), 2);
            assert_eq!(second.get(), EgclVal::from_fixnum(2));
        }
        assert_eq!(current_depth(), 1);
        assert_eq!(first.get(), EgclVal::from_fixnum(1));
        drop(outer);
        assert_eq!(current_depth(), 0);

        let result = std::panic::catch_unwind(|| {
            let scope = ShadowRootScope::new();
            let _root = scope.root(EgclVal::from_fixnum(3));
            panic!("exercise shadow-root unwind");
        });
        assert!(result.is_err());
        assert_eq!(current_depth(), 0);

        fn return_error() -> Result<(), EgclError> {
            let scope = ShadowRootScope::new();
            let _root = scope.root(EgclVal::from_fixnum(4));
            Err(EgclError::Internal("exercise Result unwind".into()))
        }
        assert!(return_error().is_err());
        assert_eq!(current_depth(), 0);
    }
}

// ── Intrusive, lock-free precise roots (bliss-a03) ─────────────────────────
//
// `Rooted<T>` unifies StackRoot / HostRoot / ShadowRoot into one primitive: an
// intrusive node on a thread-local singly-linked list (SpiderMonkey's
// `Rooted<T>` model). Construction links, destruction unlinks — O(1) with no
// lock on the mutator hot path (the mutex-guarded registries above take a
// global lock per root construction/drop, and StackRoot's drop is O(n), so
// deeply-recursive rooted code paid O(n²) in lock-guarded work).
//
// The value lives INLINE in the node: reads/writes are direct (Deref/DerefMut),
// and the collector rewrites the slot in place via the same TraceHostRoots
// machinery as HostRoot. The one global structure is a registry of each
// thread's head-cell address, touched once per thread — the scanner walks each
// list during STW, when mutators are parked and the lists are quiescent (the
// same quiescence the existing registries rely on).
//
// Cross-thread publication: the registered head-cell address IS this thread's
// published root list — a stable location the collector reads while the thread
// is parked. When the native-thread STW protocol lands (bliss-h6z.5), its
// safepoint parking must guarantee quiescence of these lists exactly as it
// must for the mutex-guarded registries above; no separate per-safepoint
// republication is needed because the cell's address never changes.
//
// Each fiber owns a separately registered head. Mounting selects that head in
// carrier TLS; unmounting restores the carrier's head. Suspended lists remain
// registered, and guards resume on the same list after carrier migration.
//
// A linked node must not move (the list holds its address), which plain Rust
// cannot express for a by-value local — so the ONLY blessed constructor is the
// `rooted!` macro: it creates the unlinked node as a hidden local, then shadows
// the name with a `RootedGuard` that links on creation and unlinks on drop.
// While the guard borrows the node, the borrow checker makes the node immobile.
//
//     egcl_rt::rooted!(form = some_val);          // form: RootedGuard<EgclVal>
//     let v = *form;                               // direct read (post-GC value)
//     *form = other_val;                           // direct write
//     egcl_rt::rooted!(items = Vec::<EgclVal>::new());
//     items.push(v);                               // Deref to Vec
//
// Out-of-order drops (a guard dropped before a later-created one, e.g. via
// explicit `drop`) are tolerated: unlink walks from the head when the node is
// not the head. Normal lexical scoping is LIFO and hits the O(1) fast path.

/// Type-erased intrusive list node embedded in every [`Rooted<T>`].
#[repr(C)]
pub struct RootLink {
    next: *mut RootLink,
    trace: unsafe fn(*mut RootLink, &mut dyn FnMut(*mut EgclVal)),
}

thread_local! {
    static ACTIVE_ROOTED_HEAD: std::cell::Cell<*const std::cell::Cell<*mut RootLink>> =
        const { std::cell::Cell::new(std::ptr::null()) };
    /// Head of this thread's intrusive root list. The cell's ADDRESS is stable
    /// for the thread's lifetime and is what the global registry records.
    static ROOTED_HEAD: std::cell::Cell<*mut RootLink> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };

    /// Registers this thread's head cell on first use; its destructor removes
    /// the registry entry when the thread exits.
    static ROOTED_HEAD_REGISTRATION: RootedHeadRegistration =
        RootedHeadRegistration::install();
}

fn rooted_heads() -> &'static OrderedMutex<Vec<usize>> {
    static HEADS: OnceLock<OrderedMutex<Vec<usize>>> = OnceLock::new();
    HEADS.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            20,
            "GC intrusive rooted-list heads",
            Vec::new(),
        )
    })
}

struct RootedHeadRegistration {
    head_address: usize,
}

impl RootedHeadRegistration {
    fn install() -> Self {
        let head_address = ROOTED_HEAD.with(|cell| cell as *const _ as usize);
        Self::register(head_address)
    }

    fn register(head_address: usize) -> Self {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| register_root_scanner(scan_rooted_lists));
        rooted_heads()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(head_address);
        Self { head_address }
    }
}

/// Stable, registered host roots for a fiber, including while it is suspended.
/// The scheduler's exclusive mount and GC safepoint protocol protect the cell.
pub(crate) struct FiberRoots {
    // Unregister before freeing the cell (fields drop in declaration order).
    _registration: RootedHeadRegistration,
    head: Box<std::cell::Cell<*mut RootLink>>,
}

impl FiberRoots {
    pub(crate) fn new() -> Self {
        let head = Box::new(std::cell::Cell::new(std::ptr::null_mut()));
        let registration = RootedHeadRegistration::register(&*head as *const _ as usize);
        Self {
            _registration: registration,
            head,
        }
    }

    /// Select only while exclusively mounting this fiber. The returned scope
    /// must stay on the carrier stack, outliving the switch into the fiber.
    pub(crate) unsafe fn mount(&self) -> MountedFiberRoots<'_> {
        let previous = ACTIVE_ROOTED_HEAD.with(|active| active.replace(&*self.head));
        MountedFiberRoots {
            previous,
            _roots: self,
        }
    }
}

pub(crate) struct MountedFiberRoots<'a> {
    previous: *const std::cell::Cell<*mut RootLink>,
    _roots: &'a FiberRoots,
}

impl Drop for MountedFiberRoots<'_> {
    fn drop(&mut self) {
        ACTIVE_ROOTED_HEAD.with(|active| active.set(self.previous));
    }
}

// A fiber may resume on another OS thread. Keep the TLS lookup in a separate
// call so LLVM cannot cache a carrier's TLS address across a suspension in the
// caller. Optimized musl code reused the address across an allocating call and
// linked a RootedRef into the old carrier's list; its later drop then failed.
#[inline(never)]
fn with_rooted_head<R>(f: impl FnOnce(&std::cell::Cell<*mut RootLink>) -> R) -> R {
    let active = ACTIVE_ROOTED_HEAD.with(|head| head.get());
    if active.is_null() {
        ROOTED_HEAD_REGISTRATION.with(|_| {});
        ROOTED_HEAD.with(f)
    } else {
        // The carrier's mount scope borrows the owning FiberRoots and the
        // scheduler prevents another carrier from using it concurrently.
        f(unsafe { &*active })
    }
}

impl Drop for RootedHeadRegistration {
    fn drop(&mut self) {
        let mut heads = rooted_heads().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = heads.iter().rposition(|&a| a == self.head_address) {
            heads.remove(index);
        }
    }
}

/// EGCL_GC_VERIFY only (bliss-wc4t): walk the intrusive rooted lists like
/// [`scan_rooted_lists`], but per NODE, so a stale slot can be attributed to
/// its holder. Each node's `trace` fn is monomorphized per rooted type
/// (`trace_rooted::<T>` / `trace_rooted_ref::<T>`), so its address names `T`:
/// resolve it in the binary with nm, using the printed address of
/// `scan_rooted_lists` itself as the anchor to compute the load slide.
fn verify_rooted_lists_naming(
    stale: &dyn Fn(EgclVal) -> bool,
    describe_target: &dyn Fn(EgclVal) -> String,
) {
    let heads = rooted_heads().lock().unwrap_or_else(|e| e.into_inner());
    for &head_address in heads.iter() {
        let mut node = unsafe { (*(head_address as *const std::cell::Cell<*mut RootLink>)).get() };
        let mut node_index = 0usize;
        while !node.is_null() {
            let trace_fn = unsafe { (*node).trace } as usize;
            let mut hit: Option<(usize, u64)> = None;
            unsafe {
                ((*node).trace)(node, &mut |slot: *mut EgclVal| {
                    let v = *slot;
                    if hit.is_none() && stale(v) {
                        hit = Some((slot as usize, v.0));
                    }
                });
            }
            if let Some((slot_addr, value)) = hit {
                panic!(
                    "gc-verify(holder): stale {value:#x} in slot {slot_addr:#x} held by rooted \
                     node {:#x} (index {node_index} from head {head_address:#x}), trace fn \
                     {trace_fn:#x}; anchor scan_rooted_lists={:#x} \
                     (slide = runtime_anchor - nm_anchor; holder type = nm symbol at \
                     trace_fn - slide) [{}] [{}]",
                    node as usize,
                    scan_rooted_lists as *const () as usize,
                    verify_trace_report(slot_addr),
                    describe_target(EgclVal(value)),
                );
            }
            node = unsafe { (*node).next };
            node_index += 1;
        }
    }
}

fn scan_rooted_lists(visit: &mut dyn FnMut(*mut EgclVal)) {
    let heads = rooted_heads().lock().unwrap_or_else(|e| e.into_inner());
    for &head_address in heads.iter() {
        // SAFETY: the registry holds addresses of live threads' ROOTED_HEAD
        // cells (removed by the TLS destructor on thread exit), and the lists
        // are quiescent while the collector runs (mutators parked at
        // safepoints — the same contract as every registry above).
        let mut node = unsafe { (*(head_address as *const std::cell::Cell<*mut RootLink>)).get() };
        while !node.is_null() {
            unsafe {
                ((*node).trace)(node, visit);
                node = (*node).next;
            }
        }
    }
}

/// The intrusive root node. Create ONLY via the [`rooted!`] macro (see the
/// module comment): a `Rooted` must not move between [`RootedGuard::new`] and
/// the guard's drop, which the macro's shadowing guarantees.
#[repr(C)]
pub struct Rooted<T: TraceHostRoots> {
    link: RootLink,
    value: T,
    _not_send: PhantomData<Rc<()>>,
}

unsafe fn trace_rooted<T: TraceHostRoots>(
    link: *mut RootLink,
    visit: &mut dyn FnMut(*mut EgclVal),
) {
    // SAFETY: `link` is the first field of a live, immobile `Rooted<T>`
    // (repr(C)), so the container pointer is the link pointer.
    unsafe { (*(link as *mut Rooted<T>)).value.trace_host_roots(visit) };
}

impl<T: TraceHostRoots> Rooted<T> {
    /// An UNLINKED node. Not scanned until a [`RootedGuard`] links it; use the
    /// [`rooted!`] macro rather than calling this directly.
    pub fn new_unlinked(value: T) -> Self {
        Rooted {
            link: RootLink {
                next: std::ptr::null_mut(),
                trace: trace_rooted::<T>,
            },
            value,
            _not_send: PhantomData,
        }
    }
}

/// Links a [`Rooted`] onto the thread's root list for the guard's lifetime and
/// unlinks it on drop. Derefs to the rooted value. The borrow it holds keeps
/// the node immobile for exactly the linked extent.
pub struct RootedGuard<'r, T: TraceHostRoots> {
    node: &'r mut Rooted<T>,
}

impl<'r, T: TraceHostRoots> RootedGuard<'r, T> {
    pub fn new(node: &'r mut Rooted<T>) -> Self {
        with_rooted_head(|head| {
            node.link.next = head.get();
            head.set(&mut node.link as *mut RootLink);
        });
        RootedGuard { node }
    }
}

impl<T: TraceHostRoots> Deref for RootedGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.node.value
    }
}

impl<T: TraceHostRoots> DerefMut for RootedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.node.value
    }
}

impl<T: TraceHostRoots> Drop for RootedGuard<'_, T> {
    fn drop(&mut self) {
        let target = &mut self.node.link as *mut RootLink;
        with_rooted_head(|head| {
            let first = head.get();
            if first == target {
                // LIFO fast path: normal lexical scoping.
                head.set(self.node.link.next);
                return;
            }
            // Out-of-order drop: unlink from the middle.
            let mut cursor = first;
            while !cursor.is_null() {
                // SAFETY: list nodes are live linked Rooteds on this thread.
                unsafe {
                    if (*cursor).next == target {
                        (*cursor).next = (*target).next;
                        return;
                    }
                    cursor = (*cursor).next;
                }
            }
            unreachable!("RootedGuard dropped but its node is not on the root list");
        });
    }
}

/// Root a value for the current lexical scope: `rooted!(name = expr)` binds
/// `name` to a [`RootedGuard`] over `expr`. Reads/writes go through Deref
/// (`*name`), always seeing the post-GC (relocated) value.
#[macro_export]
macro_rules! rooted {
    ($name:ident = $value:expr) => {
        let mut $name = $crate::gc::Rooted::new_unlinked($value);
        #[allow(unused_mut)]
        let mut $name = $crate::gc::RootedGuard::new(&mut $name);
    };
}

/// The borrowed-target counterpart of [`Rooted`]: an intrusive node that roots
/// EXISTING data in place (a Rust local, a `Vec`, an `Env`/`Lowerer` struct)
/// instead of owning a value. This is `StackRoot`/`VecRootGuard`/
/// `LiveEnvRootGuard` semantics — the target keeps being read and written
/// directly by the surrounding code and the GC rewrites it in place — but on
/// the lock-free thread-local list instead of the mutex-guarded registries.
///
/// Like those guards, the borrow on the target is released immediately (a raw
/// pointer is stored), so the CALLER must keep the target immobile and live for
/// the guard's extent — same contract as `StackRoot` today. The node itself is
/// pinned by the [`rooted_ref!`] macro's guard-shadowing, like [`rooted!`].
#[repr(C)]
pub struct RootedRef<T: TraceHostRoots> {
    link: RootLink,
    target: *mut T,
    _not_send: PhantomData<Rc<()>>,
}

unsafe fn trace_rooted_ref<T: TraceHostRoots>(
    link: *mut RootLink,
    visit: &mut dyn FnMut(*mut EgclVal),
) {
    // SAFETY: `link` is the first field of a live, immobile `RootedRef<T>`
    // (repr(C)), and its target outlives the guard per the caller contract.
    unsafe { (*(*(link as *mut RootedRef<T>)).target).trace_host_roots(visit) };
}

impl<T: TraceHostRoots> RootedRef<T> {
    /// An UNLINKED node over `target`. Not scanned until a [`RootedRefGuard`]
    /// links it; use the [`rooted_ref!`] macro rather than calling this
    /// directly.
    pub fn new_unlinked(target: &mut T) -> Self {
        RootedRef {
            link: RootLink {
                next: std::ptr::null_mut(),
                trace: trace_rooted_ref::<T>,
            },
            target: target as *mut T,
            _not_send: PhantomData,
        }
    }
}

/// Links a [`RootedRef`] for its lifetime and unlinks on drop (LIFO fast path,
/// mid-list tolerated), exactly like [`RootedGuard`].
pub struct RootedRefGuard<'r, T: TraceHostRoots> {
    node: &'r mut RootedRef<T>,
}

impl<'r, T: TraceHostRoots> RootedRefGuard<'r, T> {
    pub fn new(node: &'r mut RootedRef<T>) -> Self {
        with_rooted_head(|head| {
            node.link.next = head.get();
            head.set(&mut node.link as *mut RootLink);
        });
        RootedRefGuard { node }
    }
}

impl<T: TraceHostRoots> Drop for RootedRefGuard<'_, T> {
    fn drop(&mut self) {
        let target = &mut self.node.link as *mut RootLink;
        with_rooted_head(|head| {
            let first = head.get();
            if first == target {
                head.set(self.node.link.next);
                return;
            }
            let mut cursor = first;
            while !cursor.is_null() {
                // SAFETY: list nodes are live linked roots on this thread.
                unsafe {
                    if (*cursor).next == target {
                        (*cursor).next = (*target).next;
                        return;
                    }
                    cursor = (*cursor).next;
                }
            }
            unreachable!("RootedRefGuard dropped but its node is not on the root list");
        });
    }
}

/// Root EXISTING data in place for the current lexical scope:
/// `rooted_ref!(_g = &mut local)` keeps `local` scanned and rewritten by the GC
/// while `_g` is live — the drop-in replacement for
/// `let _g = StackRoot::new(&mut local)` (and `VecRootGuard` /
/// `LiveEnvRootGuard`-style struct rooting, given a `TraceHostRoots` impl),
/// with O(1) lock-free link/unlink.
#[macro_export]
macro_rules! rooted_ref {
    ($name:ident = $target:expr) => {
        let mut $name = $crate::gc::RootedRef::new_unlinked($target);
        let $name = $crate::gc::RootedRefGuard::new(&mut $name);
    };
}

#[cfg(test)]
mod rooted_tests {
    use super::*;

    fn list_len() -> usize {
        let mut n = 0;
        with_rooted_head(|head| {
            let mut node = head.get();
            while !node.is_null() {
                n += 1;
                node = unsafe { (*node).next };
            }
        });
        n
    }

    /// Trace only THIS thread's list. `scan_rooted_lists` is process-wide and
    /// relies on STW quiescence, which parallel test threads don't provide.
    fn scan_local(visit: &mut dyn FnMut(*mut EgclVal)) {
        with_rooted_head(|head| {
            let mut node = head.get();
            while !node.is_null() {
                unsafe {
                    ((*node).trace)(node, visit);
                    node = (*node).next;
                }
            }
        });
    }

    #[test]
    fn rooted_links_unlinks_and_scans() {
        assert_eq!(list_len(), 0);
        {
            rooted!(a = EgclVal::from_fixnum(1));
            rooted!(b = vec![EgclVal::from_fixnum(2), EgclVal::from_fixnum(3)]);
            assert_eq!(list_len(), 2);
            assert_eq!(*a, EgclVal::from_fixnum(1));
            assert_eq!(b.len(), 2);
            // The local scan visits every slot on this thread's list once.
            let mut seen = Vec::new();
            scan_local(&mut |slot| seen.push(unsafe { *slot }));
            assert_eq!(seen.len(), 3);
            for expected in [1, 2, 3] {
                assert!(seen.contains(&EgclVal::from_fixnum(expected)));
            }
            // Writes through the guard are visible to the scanner.
            *a = EgclVal::from_fixnum(987654);
            let mut seen = Vec::new();
            scan_local(&mut |slot| seen.push(unsafe { *slot }));
            assert!(seen.contains(&EgclVal::from_fixnum(987654)));
        }
        assert_eq!(list_len(), 0);
    }

    #[test]
    fn out_of_order_drop_unlinks_correctly() {
        let mut a = Rooted::new_unlinked(EgclVal::from_fixnum(1));
        let a = RootedGuard::new(&mut a);
        let mut b = Rooted::new_unlinked(EgclVal::from_fixnum(2));
        let b = RootedGuard::new(&mut b);
        let mut c = Rooted::new_unlinked(EgclVal::from_fixnum(3));
        let c = RootedGuard::new(&mut c);
        assert_eq!(list_len(), 3);
        drop(b); // middle of the list
        assert_eq!(list_len(), 2);
        let mut seen = Vec::new();
        scan_local(&mut |slot| seen.push(unsafe { *slot }));
        assert!(seen.contains(&EgclVal::from_fixnum(1)));
        assert!(seen.contains(&EgclVal::from_fixnum(3)));
        drop(a); // now the tail
        drop(c); // head
        assert_eq!(list_len(), 0);
    }

    #[test]
    fn unwind_unlinks() {
        let result = std::panic::catch_unwind(|| {
            rooted!(_x = EgclVal::from_fixnum(7));
            panic!("exercise rooted unwind");
        });
        assert!(result.is_err());
        assert_eq!(list_len(), 0);
    }

    #[test]
    fn rooted_ref_scans_target_in_place() {
        let mut local = EgclVal::from_fixnum(41);
        let mut vec = vec![EgclVal::from_fixnum(42), EgclVal::from_fixnum(43)];
        {
            rooted_ref!(_a = &mut local);
            rooted_ref!(_b = &mut vec);
            assert_eq!(list_len(), 2);
            let mut seen = Vec::new();
            scan_local(&mut |slot| seen.push(unsafe { *slot }));
            assert_eq!(seen.len(), 3);
            for expected in [41, 42, 43] {
                assert!(seen.contains(&EgclVal::from_fixnum(expected)));
            }
            // The scanner writes through to the ORIGINAL storage (in-place
            // rewrite), which is the whole point of the borrowed variant.
            scan_local(&mut |slot| unsafe {
                if *slot == EgclVal::from_fixnum(41) {
                    *slot = EgclVal::from_fixnum(410);
                }
            });
        }
        assert_eq!(list_len(), 0);
        assert_eq!(local, EgclVal::from_fixnum(410));
        assert_eq!(vec[0], EgclVal::from_fixnum(42));
    }
}

// ── T0 evaluator allocation on the shared GC heap (bliss-jtc.1) ───
//
// The tree-walking interpreter allocates its Lisp objects (conses, strings, …)
// through `alloc_typed`, so they use the same object layouts and allocation path
// as compiled code and are visible to heap stats/walking. The heap is
// initialized lazily on first use. Nursery exhaustion is a T0 safepoint: the
// collector traces registered evaluator/stdlib roots, evacuates survivors, and
// reuses the nursery for short-lived allocation.

/// Heap size requested on the command line (`--heap-size`), in bytes; 0 when
/// none was given. Set by the CLI before the heap is first initialized; the CLI
/// rejects a requested size of 0.
static HEAP_SIZE_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

/// Whether a major collection drains the nursery first, or marks through it in
/// place because a minor collection just failed to find room (GitHub #471).
#[derive(Clone, Copy, PartialEq, Eq)]
enum MajorMode {
    DrainNursery,
    KeepNursery,
}

/// Default T0 heap size when nothing selects one.
const T0_DEFAULT_HEAP_SIZE: usize = 512 * 1024 * 1024;
/// Smallest T0 heap accepted; smaller requests are raised to it.
const T0_MIN_HEAP_SIZE: usize = 16 * 1024 * 1024;
/// Largest nursery a T0 heap uses; smaller heaps get a quarter of the heap.
const T0_MAX_NURSERY_SIZE: usize = 64 * 1024 * 1024;
const T0_REGION_SIZE: usize = 1024 * 1024;

/// Free regions held back from nursery growth and minor-collection
/// evacuation: about 1/64 of the heap, at least two regions. When a minor
/// collection cannot place its survivors, the failed allocation continues into
/// this reserve so the program can reach a poll and signal STORAGE-CONDITION
/// rather than fail at an allocation site that cannot report it (GitHub #471).
fn emergency_reserve_regions(state: &HeapState) -> usize {
    (state.regions.len() / 64).max(2)
}

thread_local! {
    /// Set when this thread has been sent STORAGE-CONDITION for an exhausted
    /// heap, until an allocation finds room again after a collection.
    static HEAP_EXHAUSTION_SIGNALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn free_region_count(state: &HeapState) -> usize {
    state
        .regions
        .iter()
        .filter(|region| region.header.kind == RegionKind::Free)
        .count()
}

/// Request a heap of `bytes` for the lazily initialized T0 heap (`--heap-size`,
/// GitHub #471). It takes precedence over `EGCL_HEAP_SIZE` and `EGCL_HEAP_MB`.
/// Returns false when the heap was already initialized, so the request came too
/// late to have any effect.
pub fn set_default_heap_size(bytes: usize) -> bool {
    HEAP_SIZE_OVERRIDE.store(bytes, Ordering::Release);
    heap_state()
        .lock()
        .map(|guard| guard.is_none())
        .unwrap_or(false)
}

/// Parse a heap size (`--heap-size`, `EGCL_HEAP_SIZE`): a size as
/// [`parse_size`](crate::runtime::parse_size) accepts, other than 0.
pub fn parse_heap_size(value: &str, context: &str) -> Result<usize, EgclError> {
    match crate::runtime::parse_size(value, context)? {
        0 => Err(EgclError::Internal(format!(
            "{context}: heap size must be greater than 0"
        ))),
        bytes => Ok(bytes),
    }
}

/// The T0 heap size, by precedence: `--heap-size` (see
/// [`set_default_heap_size`]), then `EGCL_HEAP_SIZE` (a size such as `2g`),
/// then the older `EGCL_HEAP_MB` (MiB), then 512 MiB. Before GitHub #471 only
/// `EGCL_HEAP_MB` was read here, so the documented knobs were silently ignored.
fn t0_requested_heap_size() -> usize {
    let override_bytes = HEAP_SIZE_OVERRIDE.load(Ordering::Acquire);
    if override_bytes != 0 {
        return override_bytes;
    }
    // The CLI rejects a malformed EGCL_HEAP_SIZE at startup; an embedder that
    // never parsed it falls through to the next source.
    if let Some(bytes) = std::env::var("EGCL_HEAP_SIZE")
        .ok()
        .and_then(|s| parse_heap_size(&s, "EGCL_HEAP_SIZE").ok())
    {
        return bytes;
    }
    std::env::var("EGCL_HEAP_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|mb| mb.checked_mul(1024 * 1024))
        .filter(|&bytes| bytes != 0)
        .unwrap_or(T0_DEFAULT_HEAP_SIZE)
}

/// Default heap for a standalone T0 evaluator, with survivor/old-generation
/// reserve left outside the nursery so moving collections always have space.
fn t0_default_config() -> GcConfig {
    t0_config_for_heap_size(t0_requested_heap_size())
}

/// T0 heap configuration for a requested total size: rounded up to whole
/// regions, at least [`T0_MIN_HEAP_SIZE`], with a nursery of a quarter of the
/// heap capped at 64 MiB so a small heap still leaves room to evacuate into.
fn t0_config_for_heap_size(requested: usize) -> GcConfig {
    let heap_size = requested
        .max(T0_MIN_HEAP_SIZE)
        .checked_next_multiple_of(T0_REGION_SIZE)
        .unwrap_or(usize::MAX / T0_REGION_SIZE * T0_REGION_SIZE);
    // `EGCL_GC_DISABLE` makes the whole heap a single nursery so a minor GC is
    // never triggered until the heap is exhausted — a diagnostic for isolating
    // GC/heap-corruption bugs (bliss-6b2): if a workload that intermittently
    // corrupts memory runs cleanly with the collector effectively off, the fault
    // is in the collector/write-barrier, not the mutator. Pair it with a large
    // heap (`--heap-size`, `EGCL_HEAP_SIZE` or `EGCL_HEAP_MB`) to run to
    // completion without ever collecting.
    let gc_disabled = std::env::var_os("EGCL_GC_DISABLE").is_some();
    let nursery_size = if gc_disabled {
        heap_size
    } else {
        (heap_size / 4 / T0_REGION_SIZE * T0_REGION_SIZE).min(T0_MAX_NURSERY_SIZE)
    };
    GcConfig {
        heap_size,
        heap_max: heap_size,
        nursery_size,
        tlab_size: 256 * 1024,
        region_size: T0_REGION_SIZE,
        promotion_threshold: 3,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 1024,
        old_occupancy_trigger: 0.45,
    }
}

/// Ensure the shared GC heap is initialized (idempotent) so the interpreter can
/// allocate without an explicit runtime boot.
pub fn ensure_heap_initialized() {
    // Serialize auto-boot: the check-and-init must be atomic. Two threads that
    // both observe an uninitialized heap would both call init_heap, which
    // unconditionally re-mmaps the heap and drops (munmaps) the previous
    // HeapState — leaving a concurrent allocator writing into a freed mapping,
    // i.e. a SIGSEGV (bliss-52k). A dedicated lock (NOT the heap lock, which
    // init_heap re-acquires internally) closes the TOCTOU; unlike a `Once` it
    // still permits a legitimate re-boot after an explicit shutdown.
    static INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serialize = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if heap_state().lock().unwrap().is_none() {
        let config = t0_default_config();
        if let Err(error) = init_heap(&config) {
            // Every later allocation would fail far from the cause, as a panic
            // naming whatever allocated first (GitHub #471). The usual cause is
            // a requested heap beyond what the kernel will reserve.
            eprintln!(
                "egcl: cannot reserve {} bytes for the heap ({error}); \
                 choose a smaller --heap-size or EGCL_HEAP_SIZE",
                config.heap_size
            );
            std::process::exit(1);
        }
    }
}

thread_local! {
    /// Per-thread bump allocator over the shared GC heap for T0 allocation. The
    /// fast path (TLAB) is lock-free; only a TLAB refill touches the heap lock.
    static T0_ALLOCATOR: RefCell<Option<(u64, HeapAllocator)>> = const { RefCell::new(None) };
}

/// Incremented whenever nursery addresses can become invalid. T0 allocators
/// compare their cached epoch before every allocation and lazily replace a stale
/// TLAB. Keeping the old allocator installed throughout collection preserves
/// the region's walkable reservation frontier.
// Changes only when all heap identities are replaced, not on collections.
static HEAP_IDENTITY_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Detect stale process-lifetime reserves after heap reset or image restore.
pub fn heap_identity_epoch() -> u64 {
    HEAP_IDENTITY_EPOCH.load(Ordering::Acquire)
}

static GC_MOVE_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Monotone counter bumped on every collection that may relocate objects
/// (minor and major). Host data structures that hash by object address — e.g.
/// `egcl-stdlib` hash tables — read this cheaply (a plain atomic load, no lock)
/// to detect when their address-based bucket placement has gone stale and needs
/// rehashing (bliss-jtc.22 / bliss-cpje). Unlike [`gc_generation`], it counts
/// minor GCs too (which relocate nursery keys) and takes no lock.
pub fn gc_move_epoch() -> u64 {
    GC_MOVE_EPOCH.load(Ordering::Acquire)
}

/// Overwrite the type_id of a freshly-allocated object (its size/hash are
/// already set by the allocator's placeholder header).
///
/// # Safety
/// `body` must be a body pointer returned by the allocator for a `body_size`-byte
/// object.
unsafe fn set_object_type_id(body: *mut u8, body_size: usize, type_id: u8) {
    let (_total, large) = object_footprint(body_size);
    let off = if large {
        LARGE_OBJECT_PAYLOAD_OFFSET
    } else {
        OBJECT_HEADER_SIZE
    };
    // SAFETY: the header precedes the body by the payload offset.
    unsafe {
        let header = body.sub(off) as *mut ObjectHeader;
        (*header).0 = ((*header).0 & 0x00FF_FFFF_FFFF_FFFF) | ((type_id as u64) << 56);
    }
}

/// Allocate a `body_size`-byte object of `type_id` on the shared GC heap and
/// return a pointer to its body (past the header), or `None` if the heap cannot
/// be initialized or is exhausted. Nursery exhaustion automatically runs a
/// moving minor collection; evaluator and stdlib owners must therefore expose
/// every live value through precise root scanners.
/// Configured stride for `EGCL_GC_STRESS` (0 = disabled). When set to N>0,
/// [`alloc_typed`] runs a minor collection every N allocations. This is a
/// diagnostic for rooting bugs (bliss-6b2 root cause #2): the default 64 MiB
/// nursery makes real minor GCs rare, so a value the tree-walker holds across
/// an allocation without a `ShadowRootScope` root only rarely moves under a
/// collection — an intermittent corruption. Forcing frequent minor GCs turns
/// that into a deterministic, near-immediate failure at the offending site.
/// Counterpart to `EGCL_GC_DISABLE` (which does the opposite).
/// Whether `EGCL_GC_POISON` is set (cached). See the fill site in `minor_gc`.
/// `EGCL_GC_REGION_LOG=1`: report each minor collection's retained/evacuated
/// nursery-region split on stderr.
///
/// Off by default and deliberately allocation-free: this prints from inside a
/// collection, where allocating on the Lisp heap is not allowed. Pair it with
/// `EGCL_GC_STRESS_AT=N` for a single line rather than one per allocation.
fn gc_region_log_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_GC_REGION_LOG").is_some())
}

/// Write a decimal number to stderr without allocating.
fn write_decimal(mut value: u64) {
    let mut digits = [0u8; 20];
    let mut n = 0;
    if value == 0 {
        crate::syscall::dbg_write(b"0");
        return;
    }
    while value > 0 {
        digits[n] = b'0' + (value % 10) as u8;
        value /= 10;
        n += 1;
    }
    let mut out = [0u8; 20];
    for i in 0..n {
        out[i] = digits[n - 1 - i];
    }
    crate::syscall::dbg_write(&out[..n]);
}

fn gc_poison_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_GC_POISON").is_some())
}

/// Whether `EGCL_GC_VERIFY` is set (cached). After the relocation passes of a
/// minor GC (before the evacuated nursery is reset), walk every root slot and
/// every live non-nursery object field and panic on any reference still
/// pointing into the about-to-be-freed ranges. Such a reference is a slot the
/// collector failed to trace or update — the direct cause of the "stale value
/// read from freed nursery" corruption class (bliss-4bp) — and this names the
/// holder at the exact collection that orphaned it.
/// EGCL_GC_VERIFY diagnostics (bliss-wc4t): per-cycle record of every root
/// slot the mark pass and the relocate pass visited, so a verify failure can
/// say WHICH pass lost the slot. Cleared at the start of each verified minor
/// collection; only touched under gc_verify_enabled().
struct VerifyTrace {
    /// slot address -> value seen when the MARK pass visited it.
    mark: std::collections::HashMap<usize, u64>,
    /// slot address -> (value before, value after) at the RELOCATE pass.
    reloc: std::collections::HashMap<usize, (u64, u64)>,
}
thread_local! {
    static VERIFY_TRACE: std::cell::RefCell<VerifyTrace> =
        std::cell::RefCell::new(VerifyTrace {
            mark: std::collections::HashMap::new(),
            reloc: std::collections::HashMap::new(),
        });
}

fn verify_trace_report(slot_addr: usize) -> String {
    VERIFY_TRACE.with(|t| {
        let t = t.borrow();
        let mark = match t.mark.get(&slot_addr) {
            Some(v) => format!("mark saw {v:#x}"),
            None => "NOT VISITED by mark pass".to_string(),
        };
        let reloc = match t.reloc.get(&slot_addr) {
            Some((b, a)) if b == a => {
                format!("relocate saw {b:#x}, left unchanged (no forwarding)")
            }
            Some((b, a)) => format!("relocate rewrote {b:#x} -> {a:#x}"),
            None => "NOT VISITED by relocate pass".to_string(),
        };
        format!(
            "{mark}; {reloc}; mark visited {} slots, relocate visited {} slots",
            t.mark.len(),
            t.reloc.len()
        )
    })
}

fn gc_verify_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_GC_VERIFY").is_some())
}

fn gc_stress_stride() -> u64 {
    static STRIDE: OnceLock<u64> = OnceLock::new();
    *STRIDE.get_or_init(|| {
        std::env::var("EGCL_GC_STRESS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    })
}

/// `EGCL_GC_STRESS_SKIP=N` (0 = disabled): with `EGCL_GC_STRESS` active,
/// suppress the forced collections until the per-thread allocation counter has
/// passed N. Lets a bisection stress only a *suffix* of a run — pair a `SKIP`
/// lower bound against runs at different N to bracket the allocation whose
/// collection corrupts state (bliss-1uzt).
fn gc_stress_skip() -> u64 {
    static SKIP: OnceLock<u64> = OnceLock::new();
    *SKIP.get_or_init(|| {
        std::env::var("EGCL_GC_STRESS_SKIP")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    })
}

/// `EGCL_GC_STRESS_AT=N` (0 = disabled): force a minor collection at *exactly*
/// allocation index N and nowhere else, dumping the Rust allocation backtrace at
/// that point. Binary-searching N pins the single allocation across which a live
/// value is left unrooted: the run crashes iff the collection at N moves an
/// object still reachable only from an unrooted Rust local. Independent of
/// `EGCL_GC_STRESS`; combine with `EGCL_GC_POISON` so the later stale deref
/// faults immediately (bliss-1uzt).
fn gc_stress_at() -> u64 {
    static AT: OnceLock<u64> = OnceLock::new();
    *AT.get_or_init(|| {
        std::env::var("EGCL_GC_STRESS_AT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    })
}

thread_local! {
    static GC_STRESS_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether strided stressing is live. `EGCL_GC_STRESS_AFTER_INIT` starts it
/// disarmed; the host arms it with [`stress_arm_after_init`] once the image is
/// up. Process-wide rather than per thread: the host's call and the
/// allocations it means to cover need not share a thread. The allocation
/// counter advances regardless, so `EGCL_GC_STRESS_AT` indices are unchanged.
fn gc_stress_armed() -> &'static std::sync::atomic::AtomicBool {
    static ARMED: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    ARMED.get_or_init(|| {
        std::sync::atomic::AtomicBool::new(std::env::var_os("EGCL_GC_STRESS_AFTER_INIT").is_none())
    })
}

/// Start the strided `EGCL_GC_STRESS` collections here.
///
/// Booting the Lisp image allocates about 100,000 objects before any user code
/// runs, and under `EGCL_GC_STRESS=1` each of those costs a minor collection at
/// 1.5–2 ms: a stress test that boots pays minutes, on every target, before it
/// tests anything (bliss-v2b6o measured 281 s for `(print 1)` on a z17 debug
/// build, and 611 s with poison and verification on; the deopt cleanup tests
/// overran a 900 s cap). The bootstrap is not what such a test is probing. With
/// `EGCL_GC_STRESS_AFTER_INIT` set, stressing stays off until the CLI calls
/// this before its first user form. A numeric `EGCL_GC_STRESS_SKIP` cannot
/// express the same thing, because the bootstrap's allocation count moves with
/// every image change.
pub fn stress_arm_after_init() {
    gc_stress_armed().store(true, std::sync::atomic::Ordering::SeqCst);
    // Say where the stress starts, so a clean run is known to have stressed
    // something (a skip past the program's end proves nothing: bliss-sqpi).
    if gc_stress_stride() != 0 {
        let so_far = GC_STRESS_COUNTER.with(std::cell::Cell::get);
        eprintln!("[gc-stress] EGCL_GC_STRESS_AFTER_INIT: stressing from allocation #{so_far}");
    }
}

/// Run a minor collection every `EGCL_GC_STRESS` allocations, at a GC-safe
/// point (before this allocation, mirroring the real TLAB-refill trigger — the
/// caller is not yet holding a half-built object from this call).
///
/// The per-thread allocation counter advances whenever any stress mode is armed,
/// so an allocation index is stable across runs of a deterministic program — the
/// property the `EGCL_GC_STRESS_AT` / `EGCL_GC_STRESS_SKIP` bisector relies on.
#[inline]
fn maybe_gc_stress() {
    let stride = gc_stress_stride();
    let at = gc_stress_at();
    if stride == 0 && at == 0 {
        return;
    }
    let n = GC_STRESS_COUNTER.with(|c| {
        let n = c.get().wrapping_add(1);
        c.set(n);
        n
    });
    // Single forced collection at a pinpointed allocation index, with a
    // backtrace naming the allocation site.
    if at != 0 && n == at {
        eprintln!(
            "[gc-stress] EGCL_GC_STRESS_AT: forcing minor GC at allocation #{n}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        let _ = collect_t0_minor();
        return;
    }
    // Strided stressing, optionally skipping the first `SKIP` allocations, and
    // only once armed (immediately, unless EGCL_GC_STRESS_AFTER_INIT defers it).
    if stride != 0
        && n > gc_stress_skip()
        && n % stride == 0
        && gc_stress_armed().load(std::sync::atomic::Ordering::Relaxed)
    {
        let _ = collect_t0_minor();
    }
}

thread_local! {
    /// Why this thread's last `alloc_typed` failed, for the callers that can
    /// only panic. `alloc_typed` answers `None`, which dropped the collector's
    /// own error and left "GC heap unavailable" with no cause (bliss-wyt5n).
    static LAST_ALLOCATION_FAILURE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn note_allocation_failure(error: &EgclError) {
    LAST_ALLOCATION_FAILURE.with(|cell| *cell.borrow_mut() = Some(error.to_string()));
}

/// The message for an allocation of `what` that `alloc_typed` refused, naming
/// the collector's error when it reported one.
pub fn allocation_failure_message(what: &str) -> String {
    match LAST_ALLOCATION_FAILURE.with(|cell| cell.borrow_mut().take()) {
        Some(cause) => format!("GC heap unavailable for {what}: {cause}"),
        None => format!("GC heap unavailable for {what}"),
    }
}

/// Return `None` for an empty body or a footprint that exceeds the runtime's
/// u32 size metadata, without initializing the heap or attempting collection.
pub fn alloc_typed(body_size: usize, type_id: u8) -> Option<*mut u8> {
    // Reject before safepoints, stress collections, or heap initialization.
    allocation_body_size(body_size)?;
    let _ = crate::thread::current_thread_id();
    crate::safepoint::poll_safepoint();
    maybe_gc_stress();
    T0_ALLOCATOR.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            ensure_heap_initialized();
        }
        // Take back whatever generated code bumped from this TLAB first: its
        // objects lie below the window's cursor, and the slow path below may
        // retire the TLAB or collect (bliss-8yo5p).
        if let Some((seen, allocator)) = guard.as_mut() {
            absorb_native_tlab(allocator, *seen);
        }
        // Republished below on success; an early failure leaves it disabled.
        disable_native_tlab();
        let epoch = GC_MOVE_EPOCH.load(Ordering::Acquire);
        if guard.as_ref().is_none_or(|(seen, _)| *seen != epoch) {
            if let Some((_, stale)) = guard.as_mut() {
                stale.discard_pending_after_collection();
            }
            *guard = HeapAllocator::without_tlab()
                .map_err(|error| note_allocation_failure(&error))
                .ok()
                .map(|allocator| (epoch, allocator));
        }
        let body = {
            let alloc = &mut guard.as_mut()?.1;
            alloc.alloc_fast(body_size).or_else(|| {
                alloc
                    .alloc_slow(body_size)
                    .map_err(|error| note_allocation_failure(&error))
                    .ok()
            })?
        };
        // The slow path may have collected and then refilled this allocator.
        // Publish its new epoch so the next allocation reuses that fresh TLAB.
        let epoch = GC_MOVE_EPOCH.load(Ordering::Acquire);
        let (seen, allocator) = guard.as_mut()?;
        *seen = epoch;
        // SAFETY: `body` is a freshly allocated object body of `body_size` bytes.
        unsafe { set_object_type_id(body, body_size, type_id) };
        publish_native_tlab(allocator, epoch);
        Some(body)
    })
}

// ── Inline allocation from generated code (bliss-8yo5p) ────────────
//
// Generated code bump-allocates small objects from this thread's T0 TLAB
// without calling the runtime. It reaches the TLAB through a per-thread
// window at a fixed offset from the thread pointer (`%fs` on x86-64 Linux):
// a carrier-local slot is exactly right here, because the TLAB itself is
// per carrier and the fast path has no safepoint, so a fiber cannot migrate
// in the middle of one.
//
// The window is a copy of the T0 allocator's cursor and limit plus the
// GC_MOVE_EPOCH it was published under. While it is valid the window's cursor
// is authoritative: every runtime site that reads or retires the T0 TLAB
// first absorbs the window (advancing the allocator's cursor past the
// objects generated code wrote and counting their bytes as pending), and
// `alloc_typed` republishes it after allocating. Generated code checks
// `cursor + size <= limit` and that the published epoch is still current; a
// collection changes the epoch, so a window into a reset nursery is never
// used. On failure generated code calls the ordinary runtime allocation,
// which refills, collects or stress-collects as before.

/// Byte offsets within [`NativeTlab`] that generated code uses.
pub mod native_tlab_offsets {
    /// Next free byte of the TLAB.
    pub const CURSOR: i32 = 0;
    /// End of the usable window; zero when generated code must not allocate.
    pub const LIMIT: i32 = 8;
    /// `GC_MOVE_EPOCH` under which the window was published.
    pub const EPOCH: i32 = 16;
}

/// This thread's window onto its T0 TLAB for generated code.
#[repr(C)]
struct NativeTlab {
    cursor: std::cell::Cell<usize>,
    limit: std::cell::Cell<usize>,
    epoch: std::cell::Cell<u64>,
}

thread_local! {
    // Constant-initialized and without a destructor, so it lives in the
    // executable's static TLS block at the same thread-pointer offset in
    // every thread (checked when the window is published).
    static NATIVE_TLAB: NativeTlab = const {
        NativeTlab {
            cursor: std::cell::Cell::new(0),
            limit: std::cell::Cell::new(0),
            epoch: std::cell::Cell::new(u64::MAX),
        }
    };
}

/// What generated code needs to bump-allocate a cons inline.
#[derive(Clone, Copy, Debug)]
pub struct NativeAllocLayout {
    /// Offset of this thread's [`NativeTlab`] from the thread pointer.
    pub tls_offset: i32,
    /// Address of the `GC_MOVE_EPOCH` word.
    pub epoch_address: u64,
    /// Complete header word of a cons.
    pub cons_header: u64,
    /// Bytes one cons occupies, header included.
    pub cons_footprint: u32,
    /// Complete header word of a boxed DOUBLE-FLOAT.
    pub double_header: u64,
    /// Bytes one boxed DOUBLE-FLOAT occupies, header included.
    pub double_footprint: u32,
}

/// Small packed-U8 header/body contract for native templates. This describes
/// encoding only, not an allocation guarantee or a prezeroed-payload promise.
#[derive(Clone, Copy, Debug)]
pub struct NativePackedU8Layout {
    pub header_base: u64,
    pub size_units_one: u64,
    pub size_units_mask: u64,
    pub body_size_mask: u64,
    pub body_size_shift: u8,
    pub alignment: u8,
    pub header_bytes: u8,
    pub length_offset: u8,
    pub data_offset: u8,
    pub element_tag: u8,
}

pub fn native_packed_u8_layout() -> NativePackedU8Layout {
    let mut body_bit = ObjectHeader::new(0, 0);
    body_bit.set_hash(1);
    let mut body_mask = ObjectHeader::new(0, 0);
    body_mask.set_hash(u32::MAX);
    NativePackedU8Layout {
        header_base: ObjectHeader::new(crate::object::type_id::SIMPLE_ARRAY, 0).0,
        size_units_one: ObjectHeader::new(0, 1).0,
        size_units_mask: ObjectHeader::new(0, u16::MAX).0,
        body_size_mask: body_mask.0,
        body_size_shift: body_bit.0.trailing_zeros() as u8,
        alignment: OBJECT_ALIGNMENT as u8,
        header_bytes: OBJECT_HEADER_SIZE as u8,
        length_offset: crate::packed_u8::LENGTH_OFFSET as u8,
        data_offset: crate::packed_u8::DATA_OFFSET as u8,
        element_tag: crate::object::ElementTypeTag::U8 as u8,
    }
}

#[cfg(test)]
#[test]
fn native_packed_u8_small_header_contract_matches_allocator() {
    let layout = native_packed_u8_layout();
    for length in [0usize, 1, 7, 8, 9, 136, 256] {
        let body = crate::packed_u8::DATA_OFFSET + length;
        let mut expected = 0u64;
        unsafe {
            write_object_header(
                (&raw mut expected).cast(),
                crate::object::type_id::SIMPLE_ARRAY,
                body as u32,
            );
        }
        let footprint = object_footprint(body).0;
        let actual = layout.header_base
            | ((body as u64) << layout.body_size_shift)
            | ((footprint as u64 / 8) * layout.size_units_one);
        assert_eq!(actual, expected, "length={length}");
    }
}

/// The window budget while `EGCL_GC_STRESS` is active: a few objects, so
/// generated code still allocates inline but reaches the stressing runtime
/// allocation (and its forced collection) every few conses.
const STRESS_WINDOW_BYTES: usize = 128;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn native_tlab_thread_offset() -> Option<i64> {
    let window = NATIVE_TLAB
        .try_with(|window| window as *const NativeTlab as usize)
        .ok()?;
    let thread_pointer: usize;
    // SAFETY: on x86-64 Linux (glibc and musl) the word at %fs:0 is the
    // thread pointer itself; reading it has no side effect.
    unsafe {
        std::arch::asm!("mov {}, qword ptr fs:[0]", out(reg) thread_pointer,
            options(nostack, readonly, preserves_flags));
    }
    Some(window.wrapping_sub(thread_pointer) as i64)
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
fn native_tlab_thread_offset() -> Option<i64> {
    None
}

fn native_alloc_offset() -> Option<i32> {
    static OFFSET: OnceLock<Option<i32>> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        if std::env::var("EGCL_INLINE_ALLOC").is_ok_and(|value| value == "0") {
            return None;
        }
        native_tlab_thread_offset().and_then(|offset| i32::try_from(offset).ok())
    })
}

/// The layout for inline cons allocation, or `None` when generated code must
/// call the runtime for every allocation: off x86-64 Linux, with
/// `EGCL_INLINE_ALLOC=0`, or when the window is not at a fixed thread-pointer
/// offset.
pub fn native_alloc_layout() -> Option<NativeAllocLayout> {
    let tls_offset = native_alloc_offset()?;
    let header = |type_id, body_size| {
        let mut word = 0u64;
        // SAFETY: a small object's header is one word, written into a local.
        unsafe { write_object_header((&raw mut word).cast(), type_id, body_size) };
        word
    };
    Some(NativeAllocLayout {
        tls_offset,
        epoch_address: GC_MOVE_EPOCH.as_ptr() as u64,
        cons_header: header(crate::object::type_id::CONS, 16),
        cons_footprint: object_footprint(16).0 as u32,
        double_header: header(crate::object::type_id::DOUBLE_FLOAT, 8),
        double_footprint: object_footprint(8).0 as u32,
    })
}

/// Advance `allocator` past the objects generated code bump-allocated from its
/// TLAB, counting their bytes. A window from another epoch, or one that does
/// not lie within this TLAB, has nothing to give back.
fn absorb_native_tlab(allocator: &mut HeapAllocator, epoch: u64) {
    let _ = NATIVE_TLAB.try_with(|window| {
        let cursor = window.cursor.get();
        let start = allocator.tlab.cursor as usize;
        if window.epoch.get() == epoch
            && window.limit.get() != 0
            && cursor > start
            && cursor <= allocator.tlab.limit as usize
        {
            allocator.update_nursery_live_bytes(cursor - start);
            allocator.tlab.cursor = cursor as *mut u8;
        }
    });
}

/// Hand this thread's TLAB to generated code. Disabled windows (limit zero)
/// make every inline allocation take the runtime path.
fn publish_native_tlab(allocator: &HeapAllocator, epoch: u64) {
    let Some(expected) = native_alloc_offset() else {
        return;
    };
    let _ = NATIVE_TLAB.try_with(|window| {
        let cursor = allocator.tlab.cursor as usize;
        let mut limit = allocator.tlab.limit as usize;
        if cursor == 0 || limit < cursor {
            limit = 0;
        } else if gc_stress_stride() != 0 || gc_stress_at() != 0 {
            limit = limit.min(cursor + STRESS_WINDOW_BYTES);
        }
        // Checked at this thread's first publication (its epoch is still the
        // initial sentinel); the offset is fixed for the thread's lifetime.
        if window.epoch.get() == u64::MAX
            && native_tlab_thread_offset() != Some(i64::from(expected))
        {
            // Generated code baked `expected`; a thread whose window lies
            // elsewhere would bump through unrelated memory.
            eprintln!("egcl: inline allocation window is not at a fixed thread offset");
            std::process::abort();
        }
        window.cursor.set(cursor);
        window.limit.set(limit);
        window.epoch.set(epoch);
    });
}

/// Stop generated code from allocating from this thread's TLAB until the
/// runtime republishes it.
fn disable_native_tlab() {
    let _ = NATIVE_TLAB.try_with(|window| window.limit.set(0));
}

/// Allocate a heap DOUBLE-FLOAT boxing `value` and return it as a tagged
/// heap-object `EgclVal`. The body is a single raw `f64` word (a
/// reference-free leaf the GC never scans for pointers, §1.8.4); the tagged
/// value points at the header, with the payload one word past it.
///
/// GC-safety: takes an `f64` (no `EgclVal` inputs), so there is nothing to
/// root across the allocation. Callers that hold live `EgclVal`s across this
/// call must root those as usual.
/// The header→payload offset (`OBJECT_HEADER_SIZE`, or `LARGE_OBJECT_PAYLOAD_OFFSET`
/// for large objects) that [`alloc_typed`] used for a body of `body_size` bytes.
/// A builder that constructs an object with `alloc_typed` and then forms the
/// tagged heap value MUST subtract THIS from the returned body pointer, not a
/// hardcoded 8 — otherwise a large object's value points 8 bytes past its real
/// header (into the size-extension word) and is misread as the wrong type
/// (bliss-tjru).
pub fn body_header_offset(body_size: usize) -> usize {
    let (_total, large) = object_footprint(body_size);
    if large {
        LARGE_OBJECT_PAYLOAD_OFFSET
    } else {
        OBJECT_HEADER_SIZE
    }
}

pub fn alloc_double_float(value: f64) -> EgclVal {
    let body = alloc_typed(8, crate::object::type_id::DOUBLE_FLOAT)
        .unwrap_or_else(|| panic!("{}", allocation_failure_message("double-float")));
    // SAFETY: `body` is a freshly allocated 8-byte DOUBLE_FLOAT body; the header
    // precedes it by OBJECT_HEADER_SIZE, so the tagged value is `body - 8`.
    unsafe {
        *(body as *mut f64) = value;
        EgclVal::from_heap_ptr(body.sub(8))
    }
}

/// Allocate a fresh, mutable 32-bit `SIMPLE_CHARACTER_STRING` ON the GC heap.
///
/// Body layout (matching the reader's `alloc_string` and the
/// `write_character_string` read choke point): after the object header,
/// `[char_len:u64 | u32 code points…]`. Allocating on the GC heap (rather than
/// the historical off-heap `std::alloc` used by `make_lisp_string_fresh`) means
/// the string RIDES the core-image heap snapshot and relocates like any other
/// object — off-heap fresh strings were invisible to the snapshot AND to the
/// intern-table image carry, so every reference to one dangled after restore
/// (bliss-tmbg; the make_lisp_string_fresh gap noted for bliss-jtc.2). The
/// returned object moves under GC exactly like a reader-produced string, so
/// callers must root it across later allocations — the same contract they
/// already honour for the vector/list results of SUBSEQ/REVERSE/COPY-SEQ.
pub fn alloc_character_string(s: &str) -> EgclVal {
    let char_len = s.chars().count();
    // Total padded size includes the 8-byte header; the GC body is everything
    // after it: the char_len word + the (padded) u32 code points.
    let padded = crate::object::character_string_alloc_size(s);
    let body_size = padded - OBJECT_HEADER_SIZE;
    let body = alloc_typed(body_size, crate::object::type_id::SIMPLE_CHARACTER_STRING)
        .unwrap_or_else(|| panic!("{}", allocation_failure_message("character-string")));
    unsafe {
        // payload+0 = char_len, payload+8 = u32 data (mirrors reader alloc_string).
        *(body as *mut u64) = char_len as u64;
        let data = body.add(8) as *mut u32;
        for (i, c) in s.chars().enumerate() {
            *data.add(i) = c as u32;
        }
        let off = body_header_offset(body_size);
        EgclVal::from_heap_ptr(body.sub(off))
    }
}

/// Allocate an 8-bit `SIMPLE_BASE_STRING` holding one character per octet of
/// `bytes`, on the GC heap like [`alloc_character_string`]. The image-embedded
/// file registry stores file contents this way: a base string costs one byte
/// per octet where a `(SIMPLE-ARRAY (UNSIGNED-BYTE 8))` would cost a fixnum
/// word each, and it rides the core-image snapshot unchanged. Read it back
/// with `EgclVal::as_string`, whose characters are the octets.
pub fn alloc_base_string_bytes(bytes: &[u8]) -> EgclVal {
    let padded = crate::object::simple_string_size(bytes.len(), 1);
    let body_size = padded - OBJECT_HEADER_SIZE;
    let body = alloc_typed(body_size, crate::object::type_id::SIMPLE_BASE_STRING)
        .unwrap_or_else(|| panic!("{}", allocation_failure_message("base-string")));
    unsafe {
        *(body as *mut u64) = bytes.len() as u64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), body.add(8), bytes.len());
        let off = body_header_offset(body_size);
        EgclVal::from_heap_ptr(body.sub(off))
    }
}

/// Allocate a pinned object directly in old-gen.
///
/// This is for process-lifetime objects whose raw addresses are cached outside
/// the moving heap, such as interned symbols and their names. Interpreted
/// functions and uninterned symbols also use stable addresses, but major GC can reclaim
/// their slots once unreachable. Unlike
/// [`alloc_typed`], this path intentionally does not run `EGCL_GC_STRESS`
/// before allocation: stress collections are meant to shake out ordinary
/// nursery relocation bugs, while allocating a pinned object in the nursery
/// would promote a whole TLAB/region for each object.
/// Empty bodies and footprints exceeding the runtime's u32 size metadata are
/// rejected before heap initialization.
pub fn alloc_pinned_typed(body_size: usize, type_id: u8) -> Option<*mut u8> {
    let header_body_size = allocation_body_size(body_size)?;
    let _ = crate::thread::current_thread_id();
    crate::safepoint::poll_safepoint();
    ensure_heap_initialized();

    let (total_size, large) = object_footprint(body_size);
    let mut guard = heap_state().lock().unwrap();
    let state = guard.as_mut()?;

    if large {
        let regions_needed = total_size.div_ceil(state.config.region_size);
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
            let body_off = unsafe { write_object_header(ptr, type_id, header_body_size) };
            let header = ptr as *mut ObjectHeader;
            unsafe {
                (*header).set_pinned();
            }
            return Some(unsafe { ptr.add(body_off) });
        }
        return None;
    }

    if matches!(
        type_id,
        crate::object::type_id::FUNCTION_INTERPRETED | crate::object::type_id::SYMBOL
    ) {
        if let Some(index) = state
            .free_pinned_slots
            .iter()
            .rposition(|&(_, size)| size == body_size)
        {
            let (address, _) = state.free_pinned_slots.swap_remove(index);
            let ptr = address as *mut u8;
            let body_off = unsafe { write_object_header(ptr, type_id, header_body_size) };
            unsafe { (*(ptr as *mut ObjectHeader)).set_pinned() };
            let region_index = (address - state.heap_base as usize) / state.config.region_size;
            state.regions[region_index].header.live_bytes += total_size as u32;
            state.stats.bytes_allocated += total_size as u64;
            state.stats.old_gen_used += total_size as u64;
            return Some(unsafe { ptr.add(body_off) });
        }
    }

    // Pack pinned allocations into DEDICATED host regions (bliss-wc4t). A
    // region hosting even one pinned object can never be evacuated or freed,
    // so first-fitting pins into arbitrary OldGen regions poisoned each such
    // region forever: under macroexpansion churn every gensym (a pinned
    // symbol + a pinned name) landed in whatever region had space, until
    // hundreds of regions were unreclaimable and evacuation ran out of space.
    // Packing bounds the poisoned area to ceil(total pinned bytes / region).
    let fits = |state: &HeapState, idx: usize| {
        let region = &state.regions[idx];
        region.header.kind == RegionKind::OldGen
            && state.pinned_hosts.contains(&idx)
            && (region.header.alloc_limit as usize).saturating_sub(region.header.alloc_top as usize)
                >= total_size
    };
    let mut target_idx = state
        .pinned_alloc_hint
        .filter(|&idx| idx < state.regions.len() && fits(state, idx));
    for &idx in state.pinned_hosts.iter() {
        if target_idx.is_some() {
            break;
        }
        let region = &state.regions[idx];
        if region.header.kind != RegionKind::OldGen {
            continue;
        }
        let top = region.header.alloc_top as usize;
        let limit = region.header.alloc_limit as usize;
        if limit.saturating_sub(top) >= total_size {
            target_idx = Some(idx);
            break;
        }
    }

    if target_idx.is_none() {
        // Function allocation can collect and retry. Leave enough free space
        // for the minor phase of that collection to evacuate the nursery.
        // Other pinned allocation callers retain their non-collecting contract.
        let evacuation_reserve = state
            .regions
            .iter()
            .filter(|region| region.header.kind == RegionKind::Nursery)
            .count()
            + 1;
        if type_id == crate::object::type_id::FUNCTION_INTERPRETED
            && std::env::var_os("EGCL_GC_DISABLE").is_none()
            && state.stats.regions_free as usize <= evacuation_reserve
        {
            return None;
        }
        for (idx, region) in state.regions.iter_mut().enumerate() {
            if region.header.kind != RegionKind::Free {
                continue;
            }
            region.header.kind = RegionKind::OldGen;
            region.header.gen_age = 0;
            region.header.alloc_top = region.base;
            region.header.live_bytes = 0;
            state.stats.regions_free = state.stats.regions_free.saturating_sub(1);
            state.pinned_hosts.insert(idx);
            target_idx = Some(idx);
            break;
        }
        // A workload whose allocations are all pinned functions (closure
        // instances) never fills the nursery, so the occupancy check after a
        // minor collection never runs. Ask for the major collection that
        // reclaims dead functions when a new function region crosses the
        // trigger; closure creation honors the request at its next safe point.
        if type_id == crate::object::type_id::FUNCTION_INTERPRETED && target_idx.is_some() {
            let total = state.regions.len();
            let non_free = total.saturating_sub(state.stats.regions_free as usize);
            if total != 0 && (non_free as f64 / total as f64) > state.config.old_occupancy_trigger {
                MAJOR_GC_REQUESTED.store(true, Ordering::Release);
            }
        }
    }

    let idx = target_idx?;
    state.pinned_alloc_hint = Some(idx);
    let region = &mut state.regions[idx];
    let ptr = region.header.alloc_top;
    let body_off = unsafe { write_object_header(ptr, type_id, header_body_size) };
    let header = ptr as *mut ObjectHeader;
    unsafe {
        (*header).set_pinned();
    }
    region.header.alloc_top = unsafe { region.header.alloc_top.add(total_size) };
    region.header.live_bytes = region.header.live_bytes.saturating_add(total_size as u32);
    state.stats.bytes_allocated += total_size as u64;
    state.stats.old_gen_used += total_size as u64;
    Some(unsafe { ptr.add(body_off) })
}

/// Retire the current thread's T0 TLAB before it reports safepoint arrival.
///
/// A TLAB reservation advances its nursery region's `alloc_top`, while heap
/// walkers stop at a zero header. Turning the unused tail into filler makes the
/// whole reserved slice walkable before a moving collector scans that region.
pub(crate) fn retire_current_t0_tlab_for_safepoint() {
    // Native-thread retirement can run during TLS teardown, after this slot
    // has already been destroyed. Ordinary safepoints still visit the live slot.
    let _ = T0_ALLOCATOR.try_with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            // The current thread is collecting from alloc_slow with its T0
            // allocator already borrowed; alloc_slow retired that TLAB before
            // triggering GC, so there is nothing more to publish here.
            return;
        };
        if let Some((epoch, allocator)) = guard.as_mut() {
            absorb_native_tlab(allocator, *epoch);
            disable_native_tlab();
            if *epoch == GC_MOVE_EPOCH.load(Ordering::Acquire) {
                allocator.retire_tlab();
            }
        }
    });
}

/// Make the reserved TLAB tail walkable while native code is blocked, without
/// discarding it. If no collection occurs, the next allocation overwrites the
/// filler. If one does occur, `alloc_typed` sees the changed GC_MOVE_EPOCH and
/// replaces the allocator before touching the old nursery reservation.
pub(crate) fn publish_current_t0_tlab_for_safepoint() {
    let _ = T0_ALLOCATOR.try_with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some((epoch, allocator)) = guard.as_mut() {
                // The filler must start past what generated code allocated.
                // The window stays valid: like the next runtime allocation,
                // the next inline one simply overwrites the filler.
                absorb_native_tlab(allocator, *epoch);
                // A previous blocked interval may have collected, with no
                // intervening Lisp allocation to refresh this allocator yet.
                if *epoch == GC_MOVE_EPOCH.load(Ordering::Acquire) {
                    allocator.publish_tlab_tail();
                }
            }
        }
    });
}

/// Run an explicit minor collection for the T0 evaluator.
///
/// The evaluator's current TLAB points into nursery space, so it must be
/// discarded before a moving collection and reacquired lazily on the next
/// [`alloc_typed`] call. All live evaluator values must already be registered
/// with a precise root scanner (for Rust temporaries, use [`ShadowRootScope`]).
/// This does not enable automatic collection in `alloc_typed`.
pub fn collect_t0_minor() -> Result<(), EgclError> {
    HeapCollector::new().minor_gc()
}

// ── Weak references ────────────────────────────────────────────────

/// A weak pointer that is cleared when its referent is collected.
pub struct WeakPointer {
    referent: EgclVal,
    broken: bool,
}

impl WeakPointer {
    /// Create a new weak pointer to `referent`.
    pub fn new(referent: EgclVal) -> Self {
        WeakPointer {
            referent,
            broken: false,
        }
    }

    /// Get the referent value. Returns `(value, broken)`.
    /// If the weak pointer has been broken by GC, returns `(NIL, true)`.
    pub fn value(&self) -> (EgclVal, bool) {
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

    /// Update the referent to its post-evacuation address. Called by the GC when
    /// the referent survived a collection but moved, so the weak pointer keeps
    /// referring to the live object rather than a stale from-space address
    /// (egcl-jtc.7f).
    pub fn forward_ref(&mut self, new_referent: EgclVal) {
        if !self.broken {
            self.referent = new_referent;
        }
    }

    /// Returns whether this weak pointer has been broken.
    pub fn is_broken(&self) -> bool {
        self.broken
    }
}

// ── Weak pointer registry ─────────────────────────────────────────

// ── Weak containers (weak hash tables, R3.13) ──────────────────────────────
//
// A weak container holds referents the collector must NOT keep alive. Its own
// root scanner therefore skips those slots, which leaves two jobs for the
// collector to hand back: relocate a referent that SURVIVED and moved, and tell
// the container which referents DIED so it can drop those entries.
//
// Both are one question per slot, so the collector passes a single resolver:
// it relocates the slot in place and answers whether the object is still live.
// This runs at exactly the points where the answer is knowable — while
// forwarding pointers are intact (minor), at the collector's established death
// ordering (major mark), and after evacuation (major relocation) — alongside the
// weak-pointer and finalizer passes.

/// A weak container's collector callback: apply `resolve` to each weak referent
/// slot and drop every entry whose resolve answered `false`.
pub type WeakContainerHook = fn(&dyn Fn(*mut EgclVal) -> bool);

fn weak_container_hooks() -> &'static OrderedMutex<Vec<WeakContainerHook>> {
    static S: std::sync::OnceLock<OrderedMutex<Vec<WeakContainerHook>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| OrderedMutex::new(LockLevel::GcWorld, 5, "weak container hooks", Vec::new()))
}

/// Register a weak container kind. Idempotent per function pointer.
pub fn register_weak_container_hook(hook: WeakContainerHook) {
    let mut hooks = weak_container_hooks().lock().unwrap();
    if !hooks.iter().any(|h| std::ptr::fn_addr_eq(*h, hook)) {
        hooks.push(hook);
    }
}

/// Invoke every weak container with `resolve`. The hook list is cloned and the
/// registry lock released first, so a container may take its own locks.
fn process_weak_containers(resolve: &dyn Fn(*mut EgclVal) -> bool) {
    let hooks = weak_container_hooks().lock().unwrap().clone();
    for hook in hooks {
        hook(resolve);
    }
}

/// Wrapper around raw pointer to WeakPointer for Send/Sync.
/// Safety: access is always guarded by the registry mutex.
struct WeakPtrHandle(*mut WeakPointer);
unsafe impl Send for WeakPtrHandle {}
unsafe impl Sync for WeakPtrHandle {}

/// Global registry of weak pointers so the GC can break them during collection.
fn weak_pointer_registry() -> &'static OrderedMutex<Vec<WeakPtrHandle>> {
    static REGISTRY: OnceLock<OrderedMutex<Vec<WeakPtrHandle>>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| OrderedMutex::new(LockLevel::GcWorld, 6, "GC weak pointers", Vec::new()))
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
/// `is_dead` returns true if the given EgclVal's referent is unreachable.
fn break_dead_weak_pointers<F>(is_dead: &F)
where
    F: Fn(EgclVal) -> bool,
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

// ── GC side-table forwarding (egcl-jtc.7f) ────────────────────────
//
// The finalizer registry and the weak-pointer registry key objects by their
// untagged body address (`EgclVal::from_raw(body)`), NOT by a tagged heap
// reference. Like SBCL's finalizer store — whose keys are untagged fixnums,
// "purposely opaque to GC" (src/code/final.lisp) — these keys are invisible to
// the ordinary scavenger: they neither keep the object alive nor get rewritten
// by `relocate_slot`. A moving collection would therefore leave them dangling.
//
// The fix mirrors SBCL's post-mark `scan_finalizers` / `smash_weak_pointers`
// (src/runtime/gc-common.c): after the strong scavenge has installed forwarding
// pointers, walk the side tables and, for each key whose object was evacuated,
// follow the forwarding pointer to the new address. Survivors are re-keyed (their
// finalizers must NOT fire); genuinely dead objects are handled by the dead-object
// passes (`fire_finalizers_in_ranges`, `break_dead_weak_pointers`).

/// Given a side-table key `from_raw(body)`, return `from_raw(new_body)` if the
/// object's header carries a forwarding pointer (it was evacuated this cycle),
/// else `None`. Bounds-checked against `[heap_base, heap_end)`, and uses the
/// same `header = body - OBJECT_HEADER_SIZE` normalization as `relocate_slot`.
fn forwarded_side_key(key: EgclVal, heap_base: usize, heap_end: usize) -> Option<EgclVal> {
    let body = key.to_raw() as usize;
    if body < heap_base + OBJECT_HEADER_SIZE || body >= heap_end {
        return None;
    }
    let header = (body - OBJECT_HEADER_SIZE) as *const u8;
    // SAFETY: `body` lies within the managed heap; its header precedes it.
    if unsafe { header_is_forwarded(header) } {
        let new_body = unsafe { header_forwarding_addr(header) } as u64;
        Some(EgclVal::from_raw(new_body))
    } else {
        None
    }
}

/// Forward finalizer-registry keys and weak-pointer referents for objects that
/// were evacuated this collection, to each object's new address. Objects that
/// were not evacuated are left untouched. MUST run while forwarding pointers are
/// still intact (before the from-space regions are zeroed). This is the egcl
/// analogue of SBCL's `scan_finalizers` + weak-pointer forwarding (jtc.7f).
fn relocate_side_tables(heap_base: usize, heap_end: usize) {
    {
        let mut reg = finalizer_registry().lock().unwrap();
        for e in &mut reg.entries {
            if let Some(new_key) = forwarded_side_key(e.object, heap_base, heap_end) {
                e.object = new_key;
            }
        }
    }
    {
        let reg = weak_pointer_registry().lock().unwrap();
        for handle in reg.iter() {
            // SAFETY: the weak pointer was registered by its owner and access is
            // guarded by the registry mutex.
            let wp = unsafe { &mut *handle.0 };
            if !wp.is_broken() {
                if let Some(new_key) = forwarded_side_key(wp.referent, heap_base, heap_end) {
                    wp.forward_ref(new_key);
                }
            }
        }
    }
}

/// Fire and remove finalizers whose object died in one of the given freed
/// address ranges. Symmetric with `break_dead_weak_pointers`: survivors have
/// already had their keys forwarded out of these ranges by
/// `relocate_side_tables`, so any finalizer key still inside a freed range
/// belongs to a genuinely dead object (egcl-jtc.7f).
fn fire_finalizers_in_ranges(ranges: &[(usize, usize)]) {
    // Snapshot dead keys first; `run_finalizers_for` re-locks the registry.
    let dead: Vec<EgclVal> = {
        let reg = finalizer_registry().lock().unwrap();
        reg.entries
            .iter()
            .filter(|e| {
                let addr = e.object.to_raw() as usize;
                ranges
                    .iter()
                    .any(|&(base, limit)| addr >= base && addr < limit)
            })
            .map(|e| e.object)
            .collect()
    };
    for key in dead {
        run_finalizers_for(key);
    }
}

// ── Finalization ───────────────────────────────────────────────────

/// Entry in the finalizer registry: maps an object to its finalizer callback.
struct FinalizerEntry {
    object: EgclVal,
    finalizer: EgclVal,
    /// Lisp callbacks must run after the collector has released the heap lock;
    /// native resource destructors must run immediately while their dead
    /// object's payload is still intact.
    deferred: bool,
    /// Rust-level callback that performs the actual invocation of the finalizer.
    /// This is set by `set_finalizer_dispatch` and called with (finalizer, object).
    callback: Option<fn(EgclVal, EgclVal)>,
}

#[derive(Default)]
struct FinalizerState {
    entries: Vec<FinalizerEntry>,
    /// Lisp functions whose targets died during a collection.  These remain
    /// precise external roots until the evaluator takes and invokes them at a
    /// safe boundary outside the collector.
    deferred: Vec<EgclVal>,
}

/// Global finalizer registry.
fn finalizer_registry() -> &'static OrderedMutex<FinalizerState> {
    static REGISTRY: OnceLock<OrderedMutex<FinalizerState>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            7,
            "GC finalizers",
            FinalizerState::default(),
        )
    })
}

/// Trace callback functions retained by the finalizer registry and deferred
/// queue.  Target objects are deliberately NOT visited: doing so would turn
/// finalization into a strong reference and the target could never die.
fn scan_finalizer_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut state = finalizer_registry().lock().unwrap();
    for entry in &mut state.entries {
        visit(&mut entry.finalizer);
    }
    for finalizer in &mut state.deferred {
        visit(finalizer);
    }
}

fn install_finalizer_root_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| register_root_scanner(scan_finalizer_roots));
}

/// Global finalizer dispatch function. Set by the runtime during startup
/// to wire finalizer invocation through the evaluator/function-call mechanism.
/// Signature: fn(finalizer: EgclVal, object: EgclVal)
static FINALIZER_DISPATCH: OnceLock<fn(EgclVal, EgclVal)> = OnceLock::new();

/// Set the global finalizer dispatch function. Called once during runtime
/// initialization to register how finalizer EgclVal functions are invoked.
pub fn set_finalizer_dispatch(dispatch: fn(EgclVal, EgclVal)) {
    let _ = FINALIZER_DISPATCH.set(dispatch);
}

/// Trace off-heap state owned by a live interpreted function object: its
/// captured lexical bindings and its bytecode. Receives the function object.
type FunctionCaptureTraceFn = fn(EgclVal, &mut dyn FnMut(*mut EgclVal));
static FUNCTION_CAPTURE_TRACE_FN: OnceLock<FunctionCaptureTraceFn> = OnceLock::new();

/// Release one nonzero closure-state word left in a dead function object.
type ClosureStateReleaseFn = fn(usize);
static CLOSURE_STATE_RELEASE_FN: OnceLock<ClosureStateReleaseFn> = OnceLock::new();

/// Install the evaluator's release for the raw closure-state words it stores
/// in function objects (`function::set_closure_state`). Called during a major
/// collection, with the world stopped, once per dead object holding a state;
/// it must not allocate Lisp objects or reach the collector.
pub fn set_closure_state_release_fn(release: ClosureStateReleaseFn) {
    let _ = CLOSURE_STATE_RELEASE_FN.set(release);
}

/// Advanced whenever a collection frees pinned function storage. An address
/// recorded with an earlier value may since have been reused, so callers that
/// cache a function object's address outside the heap revalidate it then.
static PINNED_FUNCTION_EPOCH: AtomicU64 = AtomicU64::new(1);

/// The current pinned-function reclamation epoch (see [`PINNED_FUNCTION_EPOCH`]).
#[inline]
pub fn pinned_function_epoch() -> u64 {
    PINNED_FUNCTION_EPOCH.load(Ordering::Acquire)
}

/// Install the evaluator's function-owned reference tracer. Called for marking,
/// relocation, and heap snapshots; it must not allocate Lisp objects.
pub fn set_function_capture_trace_fn(trace: FunctionCaptureTraceFn) {
    let _ = FUNCTION_CAPTURE_TRACE_FN.set(trace);
}

type FunctionCaptureSweepFn = fn(&[EgclVal]);
static FUNCTION_CAPTURE_SWEEP_FN: OnceLock<FunctionCaptureSweepFn> = OnceLock::new();

/// Install cleanup for function identities with no remaining live heap owner.
/// Runs during major GC with all executions stopped, before dead storage is freed.
/// Function objects are pinned, so nursery collection cannot end their lifetime.
pub fn set_function_capture_sweep_fn(sweep: FunctionCaptureSweepFn) {
    let _ = FUNCTION_CAPTURE_SWEEP_FN.set(sweep);
}

// ── Remembered function captures (bliss-gg3i1.7.9) ────────────────
//
// A minor collection leaves nothing in the nursery: every survivor is copied
// out or its region is promoted in place. So off-heap storage that has not been
// written since the last completed minor collection cannot refer into the
// nursery, and re-tracing it is wasted work. Tracing every old interpreted
// function's captures (its bytecode body and captured frames) on every minor
// collection made each one cost time proportional to the number of live
// functions. The evaluator instead records which captured frames and bodies
// were written or published in each epoch, scans exactly those as minor
// roots, and tells this collector so through the hook below; the collector
// then skips the captures of old FUNCTION_INTERPRETED objects on clean pages.
// Major collections and every non-minor walk always trace captures in full.

/// Count of completed minor collections; the "epoch" writers stamp.
static MINOR_EPOCH: AtomicU64 = AtomicU64::new(0);

/// The number of minor collections that have completed. Off-heap storage
/// written while this reads `e` may refer into the nursery until minor
/// collection `e + 1` completes; storage last written at an earlier epoch was
/// visited by a completed minor collection and refers to no nursery object.
#[inline]
pub fn minor_epoch() -> u64 {
    MINOR_EPOCH.load(Ordering::Acquire)
}

type FunctionCapturesRememberedFn = fn() -> bool;
static FUNCTION_CAPTURES_REMEMBERED_FN: OnceLock<FunctionCapturesRememberedFn> = OnceLock::new();
/// The decision for the minor collection in progress (false outside one).
static FUNCTION_CAPTURES_REMEMBERED: AtomicBool = AtomicBool::new(false);

/// Install the evaluator's answer to "does a registered root scanner visit, as
/// minor roots, every captured frame and bytecode body written since the last
/// completed minor collection?". Asked once at the start of each minor
/// collection, with the world stopped; the answer holds for that whole
/// collection.
pub fn set_function_captures_remembered_fn(remembered: FunctionCapturesRememberedFn) {
    let _ = FUNCTION_CAPTURES_REMEMBERED_FN.set(remembered);
}

/// True during a minor collection that skips the off-heap captures of old
/// interpreted functions, because the evaluator's remembered-capture scanner
/// visits every capture written since the last completed minor collection.
/// That scanner must visit them exactly when this is true.
pub fn function_captures_remembered() -> bool {
    FUNCTION_CAPTURES_REMEMBERED.load(Ordering::Acquire)
}

/// Publishes the remembered-capture decision for one minor collection and
/// withdraws it on every exit path.
struct CapturesRememberedScope;

impl CapturesRememberedScope {
    fn enter() -> (Self, bool) {
        let remembered = FUNCTION_CAPTURES_REMEMBERED_FN
            .get()
            .is_some_and(|remembered| remembered());
        FUNCTION_CAPTURES_REMEMBERED.store(remembered, Ordering::Release);
        (Self, remembered)
    }
}

impl Drop for CapturesRememberedScope {
    fn drop(&mut self) {
        FUNCTION_CAPTURES_REMEMBERED.store(false, Ordering::Release);
    }
}

/// Set by [`request_major_gc`]; consumed by the next minor collection.
static MAJOR_GC_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ask for a major collection at the next minor collection's safepoint.
///
/// For owners of host-side state that only a major collection can release.
/// Private closure identities are the case in point: their function objects
/// are pinned, so minor collections promote them, and their side-table
/// entries (bytecode body, captured frame, tiering state) are swept only when
/// a major collection finds the function dead. That host memory is invisible
/// to the region-occupancy trigger, so a loop that creates short-lived FLET
/// closures grew without bound until an explicit full GC (bliss-5ndk). This
/// only sets a flag: it never collects, allocates or blocks, so it is safe to
/// call from any non-allocating context.
pub fn request_major_gc() {
    MAJOR_GC_REQUESTED.store(true, Ordering::Release);
}

/// Run the major collection asked for by [`request_major_gc`], if any.
///
/// The request is otherwise honored at the next minor collection, but a
/// workload whose only allocations are pinned (closure function objects and
/// their uninterned names) never fills the nursery, so no minor collection
/// comes. Callers invoke this at a point where an allocation could collect:
/// every live reference they hold must be rooted. A minor collection consumes
/// the request and escalates to the major one.
pub fn collect_if_major_requested() -> Result<(), EgclError> {
    if !MAJOR_GC_REQUESTED.load(Ordering::Acquire) || std::env::var_os("EGCL_GC_DISABLE").is_some()
    {
        return Ok(());
    }
    if heap_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_none()
    {
        return Ok(());
    }
    HeapCollector::new().minor_gc()
}

/// Callback that traces the heap references reachable from a STREAM handle.
/// The stdlib owns the stream layout (a GC handle pointing at an off-heap block
/// holding component references), so it registers this hook; the GC tracer calls
/// it for every STREAM object during marking and relocation, passing the handle
/// body pointer and the `visit` closure that marks/forwards each reference slot
/// (egcl-jtc.7a).
type StreamTraceFn = fn(*mut u8, &mut dyn FnMut(*mut EgclVal));
static STREAM_TRACE_FN: OnceLock<StreamTraceFn> = OnceLock::new();

/// Register the STREAM tracing hook (see [`STREAM_TRACE_FN`]).
pub fn set_stream_trace_fn(f: fn(*mut u8, &mut dyn FnMut(*mut EgclVal))) {
    let _ = STREAM_TRACE_FN.set(f);
}

/// Register a finalizer for a heap object.
/// The finalizer function will be called when the object is about to be collected.
pub fn register_finalizer(object: EgclVal, finalizer: EgclVal) -> Result<(), EgclError> {
    install_finalizer_root_scanner();
    let dispatch = FINALIZER_DISPATCH.get().copied();
    let mut state = finalizer_registry().lock().unwrap();
    // Replace the subsystem destructor for the same object, but never replace
    // user callbacks registered through `register_deferred_finalizer`: both may
    // legitimately coexist on a Lisp-visible stream wrapper.
    if let Some(entry) = state
        .entries
        .iter_mut()
        .find(|e| e.object == object && !e.deferred)
    {
        entry.finalizer = finalizer;
        entry.callback = dispatch;
    } else {
        state.entries.push(FinalizerEntry {
            object,
            finalizer,
            deferred: false,
            callback: dispatch,
        });
    }
    Ok(())
}

/// Convert a Lisp heap value into the opaque, untagged body key used by the
/// finalizer side table.  Keeping this representation out of ordinary root
/// scanning is what makes the association weak.  The helper also handles the
/// extended header used by large heap objects and symbols' indirect identities.
pub fn finalizer_key(object: EgclVal) -> Result<EgclVal, EgclError> {
    let object = object
        .symbol_index()
        .and_then(crate::symbols::symbol_object_ptr)
        // Registry entries point at pinned SymbolData headers.
        .map(|address| unsafe { EgclVal::from_heap_ptr(address as *mut u8) })
        .unwrap_or(object);
    if object.is_cons() {
        // A cons-tagged value points directly at its two-word body.
        return Ok(EgclVal::from_raw(unsafe { object.as_ptr() } as u64));
    }
    if object.is_heap_object() {
        // General heap values point at their header; finalizer keys point at
        // the payload, matching alloc_typed and the collector's side-table code.
        let header = unsafe { object.as_ptr() };
        let offset = unsafe { body_offset(header) };
        return Ok(EgclVal::from_raw(unsafe { header.add(offset) } as u64));
    }
    Err(EgclError::TypeError {
        datum: object,
        expected: "heap object".into(),
    })
}

/// Register a Lisp callback to run after `object` becomes unreachable.
///
/// Unlike [`register_finalizer`], registrations stack: Trivial-Garbage permits
/// multiple finalizers on one object.  The callback is a strong external root,
/// while the opaque object key is intentionally weak.  Collection only queues
/// the callback; the evaluator invokes it later, outside collector locks.
pub fn register_deferred_finalizer(object: EgclVal, finalizer: EgclVal) -> Result<(), EgclError> {
    install_finalizer_root_scanner();
    finalizer_registry()
        .lock()
        .unwrap()
        .entries
        .push(FinalizerEntry {
            object,
            finalizer,
            deferred: true,
            callback: None,
        });
    Ok(())
}

/// Cancel all deferred Lisp callbacks registered for `object`.
/// Native resource destructors are intentionally unaffected.
pub fn cancel_deferred_finalizers(object: EgclVal) {
    finalizer_registry()
        .lock()
        .unwrap()
        .entries
        .retain(|entry| !entry.deferred || entry.object != object);
}

/// Take the Lisp callbacks queued by completed collections.  The returned
/// values must be rooted by the caller before invoking any allocating code.
pub fn take_deferred_finalizers() -> Vec<EgclVal> {
    std::mem::take(&mut finalizer_registry().lock().unwrap().deferred)
}

/// Run all registered finalizers for the given object (called during collection
/// for unreachable objects). Actually invokes each finalizer callback on the object.
/// Returns the list of finalizer EgclVals that were invoked (for testing/debugging).
///
/// Per R3.16, finalizer errors must not corrupt GC state — any panic or error
/// from a finalizer invocation is caught and silently discarded.
pub fn run_finalizers_for(object: EgclVal) -> Vec<EgclVal> {
    let mut state = finalizer_registry().lock().unwrap();
    let mut matching = Vec::new();
    let mut retained = Vec::with_capacity(state.entries.len());
    for entry in state.entries.drain(..) {
        if entry.object == object {
            matching.push(entry);
        } else {
            retained.push(entry);
        }
    }
    state.entries = retained;

    // Queue arbitrary Lisp before releasing the registry lock.  This performs
    // no Lisp allocation and the queue itself is traced as an external root.
    for entry in &matching {
        if entry.deferred {
            state.deferred.push(entry.finalizer);
        }
    }
    drop(state);

    // Finalizers are arbitrary subsystem callbacks. Never invoke one while the
    // GC registry is locked: stream finalization, for example, must acquire the
    // lower-ranked per-stream lock to close its file descriptor.
    let mut invoked = Vec::with_capacity(matching.len());
    for entry in matching {
        if !entry.deferred {
            let dispatch = entry.callback.or_else(|| FINALIZER_DISPATCH.get().copied());
            if let Some(dispatch) = dispatch {
                // R3.16: native destructor panics must not corrupt GC state.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(entry.finalizer, object);
                }));
            }
        }
        invoked.push(entry.finalizer);
    }
    invoked
}

/// Drain and invoke every finalizer still registered with the runtime.
///
/// Shutdown uses this to honor the lifecycle contract even when no GC cycle
/// happens to collect the associated objects first.
pub fn run_pending_finalizers() -> Vec<(EgclVal, EgclVal)> {
    let mut state = finalizer_registry().lock().unwrap();
    let entries: Vec<_> = state.entries.drain(..).collect();
    for entry in &entries {
        if entry.deferred {
            state.deferred.push(entry.finalizer);
        }
    }
    drop(state);

    let mut invoked = Vec::with_capacity(entries.len());
    for entry in entries {
        if !entry.deferred
            && let Some(dispatch) = entry.callback.or_else(|| FINALIZER_DISPATCH.get().copied())
        {
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
    /// Nursery regions a minor collection RETAINED IN PLACE, cumulative. A region
    /// is retained whole when anything in it is pinned: it is promoted to old-gen
    /// without being relocated or poisoned, so nothing in it moves.
    ///
    /// Counted because it decides whether a GC-stress run can detect a rooting bug
    /// at all — an unrooted pointer into a retained region stays valid, so the bug
    /// is invisible no matter how many collections are forced. Without this the
    /// split was unobservable from outside, and a clean stress run could not be
    /// distinguished from a stress run that moved nothing (bliss-ahnzt, measured in
    /// bliss-c0diw).
    pub nursery_regions_retained: u64,
    /// Nursery regions a minor collection evacuated and reclaimed, cumulative.
    /// Objects in these DID move, so an unrooted reference to one is detectable.
    pub nursery_regions_evacuated: u64,
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
    /// End of the bytes a minor collection left behind in this nursery region
    /// without zeroing them (bliss-xyt4l). Memory at or above `alloc_top` is
    /// zero from here on; below it, it is zeroed as TLABs are carved.
    /// `base` when nothing stale remains.
    dirty_end: usize,
}

// Safety: HeapRegion is only accessed under the HeapState mutex.
unsafe impl Send for HeapRegion {}

impl HeapRegion {
    /// Zero whatever stale bytes lie in `[start, end)`, where `start` is the
    /// region's `alloc_top` and the span is about to be handed out.
    ///
    /// # Safety
    /// `[start, end)` must lie in this region and hold no live object.
    #[inline]
    unsafe fn zero_for_reuse(&mut self, start: usize, end: usize) {
        if start >= self.dirty_end {
            return;
        }
        let stale_end = end.min(self.dirty_end);
        // SAFETY: the caller hands out this unused span of the region.
        unsafe { std::ptr::write_bytes(start as *mut u8, 0, stale_end - start) };
        if end >= self.dirty_end {
            self.dirty_end = self.base as usize;
        }
    }

    /// Zero the stale bytes above `alloc_top`, so the region again satisfies
    /// the zero-above-top invariant of the non-nursery kinds.
    fn zero_stale_tail(&mut self) {
        let top = self.header.alloc_top as usize;
        let end = self.dirty_end;
        // SAFETY: nothing above `alloc_top` is an object.
        unsafe { self.zero_for_reuse(top, end.max(top)) };
        self.dirty_end = self.base as usize;
    }
}

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
    /// Remembered set (bliss-jtc.21): addresses of reference slots recorded by
    /// the write barrier. A minor GC scans these to find and relocate old→young
    /// pointers without walking the whole old generation, so young referents of
    /// older objects survive and their slots are fixed up after the nursery moves.
    remembered: std::collections::HashSet<usize>,
    /// SATB pre-write log: old reference values captured by the write barrier
    /// while concurrent old-gen marking is active, so the marker traces the
    /// snapshot-at-the-beginning graph (§3.6.2).
    satb_log: Vec<EgclVal>,
    /// Region indices hosting PINNED allocations (bliss-wc4t). A region with a
    /// pinned object can never be evacuated or freed, so pinned allocations are
    /// PACKED into dedicated host regions instead of first-fitting into any
    /// OldGen region: co-locating ordinary (reclaimable) data with immortal
    /// pins was turning every OldGen region unreclaimable one gensym at a time
    /// until the heap ran out of regions.
    pinned_hosts: std::collections::HashSet<usize>,
    /// The pinned host region the last pinned allocation used. Checked before
    /// scanning `pinned_hosts`, whose full regions a pinned-allocation-heavy
    /// workload (closure instances) would otherwise rescan on every object.
    pinned_alloc_hint: Option<usize>,
    /// Dead pinned function/symbol slots, recorded after major-GC reachability.
    /// Each slot remains a walkable filler until reused; live objects never move.
    free_pinned_slots: Vec<(usize, usize)>,
    /// Nursery regions an object has been pinned in since the last minor
    /// collection (bliss-xyt4l). That collection walks only these regions to
    /// find the pinned ones it must retain in place, instead of every nursery
    /// object. A region stays listed after an unpin; the walk decides.
    nursery_pin_candidates: std::collections::HashSet<usize>,
}

// Safety: HeapState is only accessed under the global mutex.
unsafe impl Send for HeapState {}

impl Drop for HeapState {
    fn drop(&mut self) {
        if !self.heap_base.is_null() {
            #[cfg(any(unix, windows))]
            // SAFETY: heap_base/size came from the anonymous mmap in init_heap.
            unsafe {
                let _ = crate::syscall::munmap(self.heap_base, self.heap_layout.size());
            }
            // Safety: heap_base was allocated with heap_layout in init_heap.
            #[cfg(not(any(unix, windows)))]
            unsafe {
                std::alloc::dealloc(self.heap_base, self.heap_layout);
            }
            self.heap_base = std::ptr::null_mut();
        }
    }
}

/// Global heap state, initialized by `init_heap`.
fn heap_state() -> &'static OrderedMutex<Option<HeapState>> {
    static STATE: OnceLock<OrderedMutex<Option<HeapState>>> = OnceLock::new();
    STATE.get_or_init(|| OrderedMutex::new(LockLevel::GcWorld, 2, "GC heap state", None))
}

/// Initialize the GC heap. Called once during runtime startup.
/// Validates configuration and sets up the region-based heap structure.
pub fn init_heap(config: &GcConfig) -> Result<(), EgclError> {
    let state = allocate_heap_state(config)?;
    install_heap_state(state);
    Ok(())
}

fn allocate_heap_state(config: &GcConfig) -> Result<HeapState, EgclError> {
    if config.heap_size == 0 {
        return Err(EgclError::Internal("heap_size must be non-zero".into()));
    }
    if config.heap_size > config.heap_max {
        return Err(EgclError::Internal("heap_size exceeds heap_max".into()));
    }
    if config.nursery_size > config.heap_size {
        return Err(EgclError::Internal("nursery_size exceeds heap_size".into()));
    }
    if config.region_size == 0 {
        return Err(EgclError::Internal("region_size must be non-zero".into()));
    }
    // tlab_size must be a non-zero power of two. The power-of-two constraint
    // also implicitly rejects region_size+1 (never a power of two when region_size
    // is a power of two), preventing TLAB sizes that can't fit in a single region.
    if config.tlab_size == 0 || (config.tlab_size & (config.tlab_size - 1)) != 0 {
        return Err(EgclError::Internal(
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
        .map_err(|e| EgclError::Internal(format!("invalid heap layout: {}", e)))?;

    // Reserve the heap with an anonymous mmap so its pages are committed (and
    // zeroed) lazily on first touch. A large heap then costs almost nothing until
    // objects are actually allocated — important for the T0 evaluator, which
    // sizes a big no-collection heap but usually touches only a few MB. mmap
    // returns page-aligned memory, satisfying `align`.
    #[cfg(any(unix, windows))]
    let heap_base = {
        // SAFETY: standard anonymous mapping; a mapping failure returns Err.
        unsafe {
            crate::syscall::mmap(
                std::ptr::null_mut(),
                config.heap_size,
                crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
                crate::syscall::MAP_PRIVATE | crate::syscall::MAP_ANONYMOUS,
                -1,
                0,
            )
            .unwrap_or(std::ptr::null_mut())
        }
    };
    // Safety: layout is valid (non-zero size, power-of-two alignment).
    #[cfg(not(any(unix, windows)))]
    let heap_base = unsafe { std::alloc::alloc_zeroed(heap_layout) };
    if heap_base.is_null() {
        return Err(EgclError::Oom);
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
            dirty_end: region_base as usize,
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
        pinned_hosts: std::collections::HashSet::new(),
        pinned_alloc_hint: None,
        free_pinned_slots: Vec::new(),
        nursery_pin_candidates: std::collections::HashSet::new(),
        config: config.clone(),
        stats,
        regions,
        heap_base,
        heap_layout,
        remembered: std::collections::HashSet::new(),
        satb_log: Vec::new(),
    };
    Ok(state)
}

fn install_heap_state(state: HeapState) {
    HEAP_RANGE_BASE.store(
        state.heap_base as usize,
        std::sync::atomic::Ordering::Relaxed,
    );
    HEAP_RANGE_END.store(
        state.heap_base as usize + state.config.heap_size,
        std::sync::atomic::Ordering::Relaxed,
    );
    // A new heap has no record of its writes yet (bliss-3uwzg).
    crate::gc_soft_dirty::disarm();
    *heap_state().lock().unwrap() = Some(state);
    HEAP_IDENTITY_EPOCH.fetch_add(1, Ordering::Release);
    GC_MOVE_EPOCH.fetch_add(1, Ordering::Release);
}

/// Query the current heap stats. Returns default (zeroed) stats if the
/// heap has not been initialized yet.
pub fn heap_stats() -> GcStats {
    // This thread's allocator counts its TLAB's bytes privately; publish them
    // so the stats include its own allocations. Other threads' counts join
    // when their TLABs retire, at most one TLAB each. A borrowed allocator is
    // collecting from alloc_slow, which already retired (and flushed) it.
    let _ = T0_ALLOCATOR.try_with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut()
            && let Some((epoch, allocator)) = guard.as_mut()
            && *epoch == GC_MOVE_EPOCH.load(Ordering::Acquire)
        {
            absorb_native_tlab(allocator, *epoch);
            allocator.flush_pending();
        }
    });
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
pub fn walk_heap<F>(mut callback: F) -> Result<(), EgclError>
where
    F: FnMut(*const u8, u8, usize) -> bool,
{
    walk_heap_records(|ptr, kind, size, _| callback(ptr, kind, size))
}

// Also expose header+8 identity internally: large objects have a 16-byte
// payload offset, while tagged references still identify them by header+8.
fn walk_heap_records(
    mut callback: impl FnMut(*const u8, u8, usize, usize) -> bool,
) -> Result<(), EgclError> {
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
            if unsafe { header_is_forwarded(header_ptr) } {
                cursor += unsafe { header_total_bytes(header_ptr) };
                continue;
            }

            // Report the exact logical body length (byte-exact for the image
            // serializer), but stride by the true footprint.
            let _ = body_size;
            let exact = unsafe { header_exact_body_len(header_ptr) };
            let obj_ptr = unsafe { header_ptr.add(body_offset(header_ptr)) };
            let should_continue = callback(obj_ptr, type_id, exact, cursor + OBJECT_HEADER_SIZE);
            if !should_continue {
                return Ok(());
            }
            cursor += unsafe { header_total_bytes(header_ptr) };
        }
    }

    Ok(())
}

/// Inspect heap liveness and update host metadata in one mutator-free extent.
/// The callback may call `walk_heap`, but must not allocate Lisp objects,
/// invoke Lisp, or request collection. Roots and object headers stay stable
/// until it returns, including against other collectors.
pub fn with_heap_snapshot<R>(inspect: impl FnOnce() -> R) -> Result<R, EgclError> {
    let _admission = acquire_gc_admission();
    with_stopped_world(|| Ok(inspect()))
}

/// Release pause coordination on errors and unwinds alike. A collector panic
/// still propagates: releasing the world does not repair an interrupted heap,
/// but must not strand native-thread teardown waiting for a vanished requester.
fn with_stopped_world<R>(body: impl FnOnce() -> Result<R, EgclError>) -> Result<R, EgclError> {
    crate::safepoint::wait_for_all_threads()?;
    struct ResumeOnUnwind;
    impl Drop for ResumeOnUnwind {
        fn drop(&mut self) {
            let _ = crate::safepoint::resume_all_threads();
        }
    }
    let resume_on_unwind = ResumeOnUnwind;
    retire_current_t0_tlab_for_safepoint();
    let result = body();
    let resumed = crate::safepoint::resume_all_threads();
    std::mem::forget(resume_on_unwind);
    let value = result?;
    resumed?;
    Ok(value)
}

// ── Image / persistence helpers ───────────────────────────────────

fn entry_continuation_cell() -> &'static OrderedMutex<EgclVal> {
    static ENTRY: OnceLock<OrderedMutex<EgclVal>> = OnceLock::new();
    ENTRY.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            3,
            "GC entry continuation",
            crate::value::NIL,
        )
    })
}

fn byte_store(name: &'static str) -> &'static OrderedMutex<Vec<u8>> {
    static SYMBOLS: OnceLock<OrderedMutex<Vec<u8>>> = OnceLock::new();
    static PACKAGES: OnceLock<OrderedMutex<Vec<u8>>> = OnceLock::new();
    static CODE: OnceLock<OrderedMutex<Vec<u8>>> = OnceLock::new();
    match name {
        "symbols" => SYMBOLS.get_or_init(|| {
            OrderedMutex::new(LockLevel::ImageSave, 1, "image symbol bytes", Vec::new())
        }),
        "packages" => PACKAGES.get_or_init(|| {
            OrderedMutex::new(LockLevel::ImageSave, 2, "image package bytes", Vec::new())
        }),
        "code" => CODE.get_or_init(|| {
            OrderedMutex::new(LockLevel::ImageSave, 3, "image code bytes", Vec::new())
        }),
        _ => unreachable!(),
    }
}

fn clear_heap_objects(state: &mut HeapState) {
    state.free_pinned_slots.clear();
    state.pinned_hosts.clear();
    state.nursery_pin_candidates.clear();
    for region in &mut state.regions {
        let used = (region.header.alloc_top as usize).saturating_sub(region.base as usize);
        region.zero_stale_tail();
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

/// Append one restored object and return its new BODY address (header +
/// header_size), so the image loader can build an old→new relocation map.
fn append_serialized_object(
    state: &mut HeapState,
    type_id: u8,
    body: &[u8],
    targets: &mut [Option<usize>; 2],
) -> Result<usize, EgclError> {
    let (total_size, _) = object_footprint(body.len());
    let region_limit = state.config.region_size / 2;
    let desired_kind = if total_size > region_limit {
        RegionKind::LargeObject
    } else {
        RegionKind::Nursery
    };

    // The cache belongs to this restore, while the heap lock is held: no GC
    // can recycle its regions. Keep large objects separate from nursery objects.
    let target_slot = usize::from(desired_kind == RegionKind::LargeObject);
    let fits = |region: &HeapRegion| {
        let used = (region.header.alloc_top as usize).saturating_sub(region.base as usize);
        region.header.kind == desired_kind && used + total_size <= region.size
    };
    let mut target_idx = targets[target_slot].filter(|&idx| fits(&state.regions[idx]));
    if target_idx.is_none() {
        #[cfg(test)]
        restore_target_tests::TARGET_SEARCHES.with(|count| count.set(count.get() + 1));
        target_idx = state.regions.iter().position(fits);
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

    let idx = target_idx.ok_or(EgclError::Oom)?;
    targets[target_slot] = Some(idx);
    if desired_kind == RegionKind::Nursery {
        // restore_heap pins every restored object (its Pass 2).
        state.nursery_pin_candidates.insert(idx);
    }
    let region = &mut state.regions[idx];
    let header_ptr = region.header.alloc_top;
    {
        let top = header_ptr as usize;
        // SAFETY: the object is written into this unused span next.
        unsafe { region.zero_for_reuse(top, top + total_size) };
    }
    let body_addr = unsafe {
        let body_off = write_object_header(header_ptr, type_id, body.len() as u32);
        std::ptr::copy_nonoverlapping(body.as_ptr(), header_ptr.add(body_off), body.len());
        if total_size > body_off + body.len() {
            std::ptr::write_bytes(
                header_ptr.add(body_off + body.len()),
                0,
                total_size - body_off - body.len(),
            );
        }
        header_ptr.add(body_off) as usize
    };
    region.header.alloc_top = unsafe { region.header.alloc_top.add(total_size) };
    region.header.live_bytes = region.header.live_bytes.saturating_add(total_size as u32);

    match desired_kind {
        RegionKind::Nursery => state.stats.nursery_used += total_size as u64,
        RegionKind::LargeObject => state.stats.large_object_bytes += total_size as u64,
        _ => {}
    }
    state.stats.bytes_allocated += total_size as u64;
    Ok(body_addr)
}

pub fn record_object(type_id: u8, data: Vec<u8>) {
    let mut guard = heap_state().lock().unwrap();
    if let Some(state) = guard.as_mut() {
        let _ = append_serialized_object(state, type_id, &data, &mut [None; 2]);
    }
}

pub fn set_entry_continuation(val: EgclVal) {
    *entry_continuation_cell().lock().unwrap() = val;
}

pub fn get_entry_continuation() -> EgclVal {
    *entry_continuation_cell().lock().unwrap()
}

pub fn full_gc() -> Result<(), EgclError> {
    let guard = heap_state().lock().unwrap();
    if guard.is_none() {
        return Ok(());
    }
    drop(guard);
    let mut collector = HeapCollector::new();
    collector.full_gc()
}

/// Snapshot occupied heap spans without changing the live heap.
///
/// # Safety
/// The caller must hold a stopped-world heap snapshot for the entire call.
pub unsafe fn snapshot_heap_regions(
    reachable_only: bool,
) -> Result<crate::image_heap::HeapSnapshot, EgclError> {
    use crate::image_heap::{HeapSnapshot, IMAGE_PAGE_SIZE, SnapshotRegion};
    let retained = if reachable_only {
        Some(unsafe { reachable_heap_objects()? })
    } else {
        None
    };
    let (region_size, heap_base, kinds) = {
        let guard = heap_state().lock().unwrap();
        let state = guard
            .as_ref()
            .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;
        (
            state.config.region_size,
            state.heap_base as usize,
            state
                .regions
                .iter()
                .map(|region| region.header.kind)
                .collect::<Vec<_>>(),
        )
    };
    if region_size % IMAGE_PAGE_SIZE != 0 {
        return Err(EgclError::InvalidImage(
            "heap region size is not page aligned".into(),
        ));
    }
    let mut objects = std::collections::BTreeMap::<usize, Vec<_>>::new();
    walk_heap_records(|body, kind, len, identity| {
        if kind != 0
            && retained
                .as_ref()
                .is_none_or(|set| set.contains(&(body as usize)))
        {
            let header = identity - OBJECT_HEADER_SIZE;
            let region = (header - heap_base) / region_size;
            let footprint = unsafe { header_total_bytes(header as *const u8) };
            objects
                .entry(region)
                .or_default()
                .push((header, body, kind, len, footprint));
        }
        true
    })?;
    let mut regions = Vec::with_capacity(objects.len());
    for (index, objects) in objects {
        let saved_start = heap_base + index * region_size;
        let &(header, _, _, _, footprint) = objects.last().unwrap();
        let used = header + footprint - saved_start;
        let mut region = SnapshotRegion {
            saved_start,
            kind: kinds[index],
            used,
            bytes: vec![0; align_up(used, IMAGE_PAGE_SIZE)],
            fixups: Vec::new(),
        };
        let mut previous_end = 0;
        for (header, body, kind, len, footprint) in objects {
            let offset = header - saved_start;
            let gap = offset - previous_end;
            if gap != 0 {
                // Use a type-zero filler instead of a zero header: all GC walkers
                // must be able to reach later live objects across filtered gaps.
                let (_, extended) = object_footprint(gap - OBJECT_HEADER_SIZE);
                let header_len = if extended {
                    LARGE_OBJECT_PAYLOAD_OFFSET
                } else {
                    OBJECT_HEADER_SIZE
                };
                let mut filler = [0u64; 2];
                unsafe {
                    write_object_header(filler.as_mut_ptr().cast(), 0, (gap - header_len) as u32);
                }
                let bytes =
                    unsafe { std::slice::from_raw_parts(filler.as_ptr().cast::<u8>(), header_len) };
                region.bytes[previous_end..previous_end + header_len].copy_from_slice(bytes);
            }
            let initialized = body as usize - header + len;
            let original = unsafe { std::slice::from_raw_parts(header as *const u8, initialized) };
            region.bytes[offset..offset + initialized].copy_from_slice(original);
            let mut copied_header = ObjectHeader(u64::from_ne_bytes(
                region.bytes[offset..offset + 8].try_into().unwrap(),
            ));
            copied_header.set_pinned();
            region.bytes[offset..offset + 8].copy_from_slice(&copied_header.0.to_ne_bytes());
            let body_offset = body as usize - saved_start;
            use crate::object::type_id as tid;
            // A FOREIGN-LIBRARY keeps its registry token: the image records
            // the library and restore reopens it into the same slot (#440).
            if matches!(
                kind,
                tid::FOREIGN_POINTER | tid::FOREIGN_CALLBACK | tid::PYTHON_OBJECT
            ) {
                region.bytes[body_offset..body_offset + len].fill(0);
            } else if matches!(kind, tid::STREAM | tid::MUTEX | tid::CONDITION_VARIABLE) {
                region.bytes[body_offset..body_offset + len.min(8)].fill(0);
            } else if kind == tid::FUNCTION_INTERPRETED && len >= FUNCTION_BODY_BYTES {
                // A closure state is process-local host memory; the image
                // host registry re-attaches it on restore.
                let state = body_offset + FUNCTION_CLOSURE_STATE_OFFSET;
                region.bytes[state..state + 8].fill(0);
            }
            unsafe {
                trace_object(body as *mut u8, kind, len, |slot| {
                    let address = slot as usize;
                    // Some trace hooks also expose references in off-heap host
                    // state, which belongs to its own serializer, not this span.
                    if address < body as usize || address + 8 > body as usize + len {
                        return;
                    }
                    let value = *slot;
                    if is_heap_ref(value) {
                        let value = resolve_forwarded(value);
                        let field = address - saved_start;
                        region.bytes[field..field + 8].copy_from_slice(&value.0.to_ne_bytes());
                        region.fixups.push(field);
                    }
                });
            }
            previous_end = offset + footprint;
        }
        region.fixups.sort_unstable();
        region.fixups.dedup();
        regions.push(region);
    }
    Ok(HeapSnapshot {
        region_size,
        regions,
    })
}

/// Per-object record: `[old_body u64][type_id u8][size u32][body bytes]`. The old
/// body address lets the loader build an old→new map so pointers relocate
/// per-object (not by a single uniform delta, which only worked when the restored
/// heap reproduced the saved layout exactly — bliss-x0f2 M2.0).
pub fn serialize_heap_objects() -> Vec<u8> {
    serialize_heap_objects_matching(|_| true)
}

// Call only while the heap snapshot is stopped; the returned addresses are
// host-side metadata, not roots that could survive a moving collection.
unsafe fn reachable_heap_objects() -> Result<std::collections::HashSet<usize>, EgclError> {
    let mut objects = HashMap::new();
    let mut pending = Vec::new();
    walk_heap_records(|ptr, kind, size, identity| {
        objects.insert(identity, (ptr, kind, size));
        if matches!(
            kind,
            crate::object::type_id::SYMBOL | crate::object::type_id::PACKAGE
        ) {
            pending.push(identity);
        }
        true
    })?;
    let mut root = |value| {
        if is_heap_ref(value) {
            pending.push(ref_body_addr(resolve_forwarded(value)));
        }
    };
    root(get_entry_continuation());
    HeapCollector::scan_cl_stack_roots(&mut root);
    crate::symbols::for_each_root_slot(|slot| root(unsafe { *slot }));
    // Includes all off-heap hash table entries, even weak entries below: their
    // independent image section must never refer to an omitted heap object.
    scan_external_roots(|slot| root(unsafe { *slot }));
    let weak_roots = RefCell::new(Vec::new());
    process_weak_containers(&|slot| {
        // Weak referents are not ordinary roots, but preserving them in the
        // shaken snapshot is conservative and keeps serialized slots valid.
        let value = unsafe { *slot };
        if is_heap_ref(value) {
            weak_roots
                .borrow_mut()
                .push(ref_body_addr(resolve_forwarded(value)));
        }
        true
    });
    pending.extend(weak_roots.into_inner());
    let mut retained = std::collections::HashSet::new();
    while let Some(identity) = pending.pop() {
        let Some(&(ptr, kind, size)) = objects.get(&identity) else {
            continue;
        };
        if !retained.insert(ptr as usize) {
            continue;
        }
        unsafe {
            trace_object(ptr as *mut u8, kind, size, |slot| {
                let value = *slot;
                if is_heap_ref(value) {
                    pending.push(ref_body_addr(resolve_forwarded(value)));
                }
            });
        }
    }
    Ok(retained)
}

fn serialize_heap_objects_matching(mut retain: impl FnMut(*const u8) -> bool) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = walk_heap(|ptr, type_id, size| {
        if !retain(ptr) {
            return true;
        }
        out.extend_from_slice(&(ptr as u64).to_le_bytes());
        out.push(type_id);
        out.extend_from_slice(&(size as u32).to_le_bytes());
        let data = unsafe { std::slice::from_raw_parts(ptr, size) };
        // A STREAM's body word 0 is a raw pointer to a process-local Rust
        // block (file handle + buffers) that dies with the saving process, so
        // serialize the HANDLE with that word NULLED: the restored object is a
        // finalized/closed stream (get_stream_alloc errors, STREAM_TRACE_FN
        // and the finalizer no-op on null), while every heap reference to it
        // remaps like any other object. The handle used to be SKIPPED
        // entirely, which left each such reference dangling at its pre-save
        // address — the first dereference in the restored process (e.g. the
        // ansi-test *mini-universe* stream element under a type probe)
        // segfaulted (bliss-agmi). Streams are re-opened on load; the loader
        // re-creates the standard ones.
        // Mutexes also own process-local native state. Preserve the Lisp handle
        // and metadata, but never a dangling native pointer. The stdlib lazily
        // recreates a fresh, unlocked mutex when the restored handle is used.
        if matches!(
            type_id,
            crate::object::type_id::FOREIGN_POINTER
                | crate::object::type_id::FOREIGN_CALLBACK
                // A PyObject address names an object in an interpreter that does
                // not outlive this process, so it must be nulled rather than
                // carried: a restored proxy reports itself dead, which every
                // accessor already handles, instead of dereferencing an address
                // that now means something else entirely.
                | crate::object::type_id::PYTHON_OBJECT
        ) {
            // Neither native addresses nor allocation identities survive an
            // image restart. Restore pointers as null. A FOREIGN-LIBRARY token
            // is kept: the image records its library, and restore reopens it
            // into the same slot, so it never names a different library.
            out.resize(out.len() + size, 0);
        } else if matches!(
            type_id,
            crate::object::type_id::STREAM
                | crate::object::type_id::MUTEX
                | crate::object::type_id::CONDITION_VARIABLE
        ) {
            let zeros = [0u8; 8];
            out.extend_from_slice(&zeros[..size.min(8)]);
            if size > 8 {
                out.extend_from_slice(&data[8..]);
            }
        } else if type_id == crate::object::type_id::FUNCTION_INTERPRETED
            && size >= FUNCTION_BODY_BYTES
        {
            // Never serialize a process-local closure-state word.
            let start = out.len();
            out.extend_from_slice(data);
            let state = start + FUNCTION_CLOSURE_STATE_OFFSET;
            out[state..state + 8].fill(0);
        } else {
            out.extend_from_slice(data);
        }
        true
    });
    out
}

/// The old→new object-body map from the most recent [`restore_heap`], consulted
/// by registry restores (symbols/packages) to remap their saved object addresses.
static RELOC_MAP: std::sync::Mutex<Option<ImageRelocations>> = std::sync::Mutex::new(None);

/// Remap a saved tagged Lisp value through the last restore's map.
/// Fixnums and other immediate values pass through, even if their bits match
/// an address in a saved region. Registry restores carry `EgclVal`s.
pub fn remap_saved_pointer(raw: u64) -> u64 {
    let guard = RELOC_MAP.lock().unwrap();
    match guard.as_ref() {
        Some(map) => map.remap_value(raw),
        None => raw,
    }
}

/// Translate an explicitly untagged native address. Use `remap_saved_pointer`
/// for Lisp values so pointer-shaped fixnums remain unchanged.
pub fn remap_saved_address(address: usize) -> usize {
    RELOC_MAP
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|map| map.translate_address(address))
        .unwrap_or(address)
}

// ── Off-heap-body object serialization (bliss-x0f2 M3) ─────────────────
//
// Some Lisp VALUES (hash-tables) keep their real body in a Box'd Rust struct OFF
// the GC heap; the heap only sees a header-tagged pointer to that Box, and
// `walk_heap` never visits it. Such a body cannot be snapshotted with the heap.
// The host (the `egcl`/`egcl-stdlib` layer) registers hooks to serialize these
// bodies to their own image section and re-materialize them on load. The RESTORE
// hook runs INSIDE `restore_heap`, BETWEEN Pass 1 (objects materialized, old→new
// heap map built) and Pass 2 (pointer remap): it re-creates each off-heap object
// — remapping the heap references it holds through the Pass-1 map — and returns
// each object's (old_body_addr, new_body_addr) so `restore_heap` folds them into
// the map. Pass 2 (and the later symbol/package remaps) then relocate every
// reference TO an off-heap object, exactly like an on-heap one.
// Restore is TWO-PHASE to break a circular dependency: the objects' new
// addresses must be folded into the heap map BEFORE Pass 2 (so references TO
// them relocate), but their contents can only be populated AFTER Pass 2 (a
// hash-table hashes each key, which for EQUAL/EQUALP recurses into the key's
// structure — valid only once Pass 2 has remapped the key object's internal
// pointers). So `allocate` creates the empty bodies + returns old→new; `populate`
// fills them once the final map is in place.
type OffHeapSerializeHook = fn() -> Vec<u8>;
type OffHeapAllocateHook = fn(&[u8]) -> Vec<(usize, usize)>;
type OffHeapPopulateHook = fn(&dyn Fn(u64) -> u64);
static OFFHEAP_SERIALIZE: std::sync::Mutex<Option<OffHeapSerializeHook>> =
    std::sync::Mutex::new(None);
static OFFHEAP_ALLOCATE: std::sync::Mutex<Option<OffHeapAllocateHook>> =
    std::sync::Mutex::new(None);
static OFFHEAP_POPULATE: std::sync::Mutex<Option<OffHeapPopulateHook>> =
    std::sync::Mutex::new(None);
/// Off-heap section bytes, stashed by `load_image` before it calls `restore_heap`
/// so the restore hooks (invoked mid-restore) can read them.
static PENDING_OFFHEAP: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// Register the off-heap-body (de)serialization hooks (called once at startup).
pub fn set_offheap_hooks(
    serialize: OffHeapSerializeHook,
    allocate: OffHeapAllocateHook,
    populate: OffHeapPopulateHook,
) {
    *OFFHEAP_SERIALIZE.lock().unwrap() = Some(serialize);
    *OFFHEAP_ALLOCATE.lock().unwrap() = Some(allocate);
    *OFFHEAP_POPULATE.lock().unwrap() = Some(populate);
}

/// Serialize the off-heap object bodies for the image (empty if no hook).
pub fn serialize_offheap_objects() -> Vec<u8> {
    match *OFFHEAP_SERIALIZE.lock().unwrap() {
        Some(hook) => hook(),
        None => Vec::new(),
    }
}

/// Stash (or clear) the off-heap section bytes for the next `restore_heap`.
pub fn set_pending_offheap(data: Option<Vec<u8>>) {
    *PENDING_OFFHEAP.lock().unwrap() = data;
}

/// Restore a region snapshot into a separately owned heap allocation.
///
/// # Safety
/// The caller must stop all mutators and supply a trusted native heap snapshot
/// for this runtime. If backing is supplied, its bytes at the given section
/// offset must match data and remain unchanged while the restored heap lives.
pub unsafe fn restore_heap_regions(
    data: &[u8],
    backing: Option<(&std::fs::File, u64)>,
) -> Result<(), EgclError> {
    use crate::image_relocation::RegionRelocation;
    let image = crate::image_heap::HeapImageView::parse(data)?;
    image.validate_objects()?;
    let invalid = || EgclError::InvalidImage("invalid restored heap extent".into());
    let mut config = heap_state()
        .lock()
        .unwrap()
        .as_ref()
        .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?
        .config
        .clone();
    config.region_size = image.region_size;
    if config.tlab_size > config.region_size {
        return Err(EgclError::InvalidImage(
            "image regions are smaller than the configured TLAB".into(),
        ));
    }
    let mut occupied = 0usize;
    for region in &image.regions {
        let span = region
            .used
            .checked_add(config.region_size - 1)
            .ok_or_else(invalid)?
            / config.region_size
            * config.region_size;
        occupied = occupied.checked_add(span).ok_or_else(invalid)?;
    }
    // The restored data, a nursery, as much again for the first minor
    // collection's survivors, and the emergency reserve. A configured heap
    // (`--heap-size`) smaller than that grows rather than refusing an image the
    // process can hold (GitHub #471): the image, not the flag, fixes how much
    // is live at start. It grows to three times the restored data: room to run
    // and not only to start, with the data under the default 45% occupancy
    // trigger so that not every minor collection is followed by a major one.
    let required = config
        .nursery_size
        .checked_mul(2)
        .and_then(|reserve| occupied.checked_add(reserve))
        .and_then(|bytes| bytes.checked_next_multiple_of(config.region_size))
        .and_then(|bytes| {
            let emergency = (bytes / config.region_size / 63 + 1).max(2);
            bytes.checked_add(emergency * config.region_size)
        })
        .ok_or_else(invalid)?;
    if config.heap_size < required {
        config.heap_size = occupied
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_next_multiple_of(config.region_size))
            .ok_or_else(invalid)?
            .max(required);
    }
    config.heap_max = config.heap_max.max(config.heap_size);
    if let Some((file, offset)) = backing {
        let end = offset.checked_add(data.len() as u64).ok_or_else(invalid)?;
        let file_len = file
            .metadata()
            .map_err(|e| EgclError::InvalidImage(e.to_string()))?
            .len();
        if end > file_len || end > i64::MAX as u64 {
            return Err(invalid());
        }
    }
    // Own the entire destination before using MAP_FIXED on any of its pages.
    // Errors drop only this new allocation; the current heap remains alive.
    let mut state = allocate_heap_state(&config)?;
    let mut ranges = Vec::with_capacity(image.regions.len());
    let mut index = 0usize;
    for region in &image.regions {
        let count = region.used.div_ceil(config.region_size);
        let destination = state.regions[index].base;
        #[cfg(target_os = "linux")]
        let mut mapped = false;
        #[cfg(not(target_os = "linux"))]
        let mapped = false;
        #[cfg(target_os = "linux")]
        if let Some((file, section_offset)) = backing {
            use std::os::fd::AsRawFd;
            let offset = section_offset
                .checked_add(region.data_offset as u64)
                .ok_or_else(invalid)?;
            let page = crate::syscall::page_size();
            if offset % page as u64 == 0
                && destination as usize % page == 0
                && region.bytes.len() % page == 0
            {
                // SAFETY: this complete extent belongs to the unpublished state;
                // replacing it cannot clobber the old heap or another mapping.
                let result = unsafe {
                    crate::syscall::mmap(
                        destination,
                        region.bytes.len(),
                        crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
                        crate::syscall::MAP_PRIVATE | crate::syscall::MAP_FIXED,
                        file.as_raw_fd(),
                        offset as i64,
                    )
                };
                result.map_err(|errno| {
                    EgclError::InvalidImage(format!("heap region mmap failed: errno {errno}"))
                })?;
                mapped = true;
            }
        }
        if !mapped {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    region.bytes.as_ptr(),
                    destination,
                    region.bytes.len(),
                );
            }
        }
        let kind = if region.kind == RegionKind::LargeObject {
            RegionKind::LargeObject
        } else {
            RegionKind::OldGen
        };
        for part in 0..count {
            let target = &mut state.regions[index + part];
            target.header.kind = kind;
            target.header.gen_age = config.promotion_threshold;
            // Large-object continuations have no independent objects to walk.
            target.header.alloc_top = if part == 0 {
                unsafe { destination.add(region.used) }
            } else {
                target.base
            };
            target.header.live_bytes = region
                .used
                .saturating_sub(part * config.region_size)
                .min(config.region_size) as u32;
            state.pinned_hosts.insert(index + part);
        }
        state.stats.bytes_allocated += region.used as u64;
        if kind == RegionKind::LargeObject {
            state.stats.large_object_bytes += region.used as u64;
        } else {
            state.stats.old_gen_used += region.used as u64;
        }
        ranges.push(RegionRelocation {
            saved_start: region.saved_start,
            restored_start: destination as usize,
            len: region.used,
        });
        index += count;
    }
    state.stats.regions_free -= index as u32;
    let mut map = ImageRelocations::new(ranges)?;
    if let Some(allocate) = *OFFHEAP_ALLOCATE.lock().unwrap() {
        let pending = PENDING_OFFHEAP.lock().unwrap();
        if let Some(bytes) = pending.as_ref().filter(|bytes| !bytes.is_empty()) {
            for (old_header, new_header) in allocate(bytes) {
                map.insert_object(
                    old_header
                        .checked_add(OBJECT_HEADER_SIZE)
                        .ok_or_else(invalid)?,
                    new_header
                        .checked_add(OBJECT_HEADER_SIZE)
                        .ok_or_else(invalid)?,
                );
            }
        }
    }
    for region in &image.regions {
        let base = map
            .translate_address(region.saved_start)
            .ok_or_else(invalid)?;
        for offset in region.fixups() {
            let slot = (base + offset) as *mut u64;
            let old = unsafe { std::ptr::read_unaligned(slot) };
            let new = map.remap_value(old);
            if new != old {
                unsafe {
                    std::ptr::write_unaligned(slot, new);
                }
            }
        }
    }
    install_heap_state(state);
    *RELOC_MAP.lock().unwrap() = Some(map);
    Ok(())
}

pub fn restore_heap(data: &[u8]) -> Result<(), EgclError> {
    let mut guard = heap_state().lock().unwrap();
    let state = guard
        .as_mut()
        .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;
    HEAP_IDENTITY_EPOCH.fetch_add(1, Ordering::Release);
    crate::gc_soft_dirty::disarm();
    clear_heap_objects(state);
    // The pre-restore region set is gone: every cached per-thread T0 allocator
    // still holds a TLAB carved from it, and the reset region alloc_tops will
    // hand that same space out again — so a post-restore allocation from a stale
    // TLAB (e.g. the re-opened stdio streams) is silently overwritten by a later
    // one (bliss-64r1). Bump the move epoch so each thread lazily discards its
    // allocator and refills from the restored region state.
    GC_MOVE_EPOCH.fetch_add(1, Ordering::Release);

    // Pass 1: materialize every object, recording payload and tagged identity
    // translations separately when an extended header makes them differ. Pre-size
    // to a generous estimate of the object count (each record is a >=13-byte
    // header plus payload) so the per-object inserts never trigger a rehash
    // (bliss-pohq: the rehash storm dominated large restores).
    let mut map = ImageRelocations::with_object_capacity(data.len() / 24 + 16);
    let mut restored = Vec::new();
    let mut offset = 0usize;
    let mut targets = [None; 2];
    while offset < data.len() {
        if data.len() - offset < 13 {
            return Err(EgclError::InvalidImage(
                "truncated heap object record".into(),
            ));
        }
        let old_body = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let type_id = data[offset];
        offset += 1;
        let size = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if data.len() - offset < size {
            return Err(EgclError::InvalidImage(
                "truncated heap object payload".into(),
            ));
        }
        let header_offset = body_header_offset(size);
        let old_identity = old_body
            .checked_sub(header_offset)
            .and_then(|header| header.checked_add(OBJECT_HEADER_SIZE))
            .ok_or_else(|| EgclError::InvalidImage("invalid saved object address".into()))?;
        let new_body =
            append_serialized_object(state, type_id, &data[offset..offset + size], &mut targets)?;
        map.insert_object(old_body, new_body);
        if header_offset != OBJECT_HEADER_SIZE {
            map.insert_object(old_identity, new_body - header_offset + OBJECT_HEADER_SIZE);
        }
        restored.push((new_body, size, header_offset));
        offset += size;
    }

    // Interlude (phase 1 — ALLOCATE): create the empty OFF-HEAP object bodies
    // (hash-tables) now and fold each (old_body, new_body) into the map BEFORE
    // Pass 2, so Pass 2 and the later symbol/package remaps relocate every
    // reference TO an off-heap object like an on-heap one. Contents are filled
    // AFTER Pass 2 (phase 2 — POPULATE), once key objects' internals are remapped
    // and therefore hashable. The hook allocates only off the GC heap (Box), so
    // holding the heap lock across it is safe (no GC, no heap_state re-entry).
    // `map` holds only GC-heap objects (Pass 1). `full_map` additionally holds the
    // off-heap object addresses; it is used for POINTER LOOKUPS (remap), while the
    // Pass-2 pin/field-walk below uses the actual payload records, not map
    // targets: extended objects contribute two aliases for one allocation.
    let mut full_map = map.clone();
    let have_offheap = {
        let pend = PENDING_OFFHEAP.lock().unwrap();
        pend.as_ref().is_some_and(|b| !b.is_empty())
    };
    if have_offheap {
        if let Some(alloc_hook) = *OFFHEAP_ALLOCATE.lock().unwrap() {
            let bytes = PENDING_OFFHEAP.lock().unwrap().clone().unwrap_or_default();
            let pairs = alloc_hook(&bytes);
            for (old_body, new_body) in pairs {
                // Off-heap objects follow the header-based convention: a
                // reference's masked address is the header, body = header +
                // OBJECT_HEADER_SIZE — the key ImageRelocations::remap looks up for a
                // TAG_HEAP_OBJECT reference.
                full_map
                    .insert_object(old_body + OBJECT_HEADER_SIZE, new_body + OBJECT_HEADER_SIZE);
            }
        }
    }

    // Pass 2: remap every pointer field of every restored object via the map,
    // and PIN each object. A loaded core is the immortal base world: the rebuilt
    // symbol/package/macro registries and the RELOC_MAP hold its post-restore
    // addresses, and a minor GC firing after load (e.g. the first mutator
    // allocation) must never relocate it out from under them. Pinning marks each
    // restored object's header so `region_has_pinned` retains its nursery region
    // in place; fresh mutator garbage still collects normally in other regions.
    for (new_body, body_size, header_offset) in restored {
        unsafe {
            let hdr = (new_body - header_offset) as *mut ObjectHeader;
            (*hdr).set_pinned();
        }
        let mut fo = 0usize;
        while fo + 8 <= body_size {
            let field = unsafe { (new_body as *mut u8).add(fo) as *mut u64 };
            let raw = unsafe { std::ptr::read_unaligned(field) };
            let new_raw = full_map.remap(raw);
            if new_raw != raw {
                unsafe { std::ptr::write_unaligned(field, new_raw) };
            }
            fo += 8;
        }
    }

    *RELOC_MAP.lock().unwrap() = Some(full_map);
    Ok(())
}

/// Phase 2 of off-heap restore (POPULATE): fill the off-heap objects allocated by
/// `restore_heap`, remapping their stored references through the now-final map.
/// MUST run AFTER `restore_heap` returns — it releases the heap-state lock, and
/// populate calls back into the collector (hashing keys, `is_in_heap`,
/// `gc_move_epoch`) which re-acquires that lock. By now Pass 2 has remapped every
/// heap object's internals, so key objects are structurally hashable.
pub fn run_offheap_populate() {
    let have = {
        let pend = PENDING_OFFHEAP.lock().unwrap();
        pend.as_ref().is_some_and(|b| !b.is_empty())
    };
    if !have {
        return;
    }
    if let Some(pop_hook) = *OFFHEAP_POPULATE.lock().unwrap() {
        pop_hook(&remap_saved_pointer);
    }
}

// A whole-heap image (§7) already snapshots the SymbolData/PackageData objects
// Cross-process image restore rebuilds the Rust-side symbol registry to index the
// RESTORED symbol objects, remapping each saved object address through the
// restore_heap old→new map (bliss-x0f2 M2). The objects (with their cells) ride
// in the heap section; this only re-establishes the index→object mapping and
// re-pins the immortal symbol/name regions in the fresh process.

/// Mark the region containing `addr` as a pinned host so restored immortal
/// objects (symbols and their name strings) are never reclaimed or relocated in
/// the fresh process. No-op outside the heap.
pub fn pin_region_containing(addr: usize) {
    let mut guard = heap_state().lock().unwrap();
    if let Some(state) = guard.as_mut() {
        let base = state.heap_base as usize;
        if addr < base || addr >= base + state.config.heap_size {
            return;
        }
        let idx = (addr - base) / state.config.region_size;
        if idx < state.regions.len() {
            state.pinned_hosts.insert(idx);
        }
    }
}

pub fn serialize_symbols() -> Vec<u8> {
    crate::symbols::serialize_objects()
}

pub fn restore_symbols(data: &[u8]) -> Result<(), EgclError> {
    crate::symbols::restore_objects(data)
}

pub fn serialize_packages() -> Vec<u8> {
    crate::packages::serialize_objects()
}

pub fn restore_packages(data: &[u8]) -> Result<(), EgclError> {
    crate::packages::restore_objects(data)
}

pub fn serialize_code_cache() -> Vec<u8> {
    byte_store("code").lock().unwrap().clone()
}

pub fn restore_code_cache(data: &[u8]) -> Result<(), EgclError> {
    *byte_store("code").lock().unwrap() = data.to_vec();
    Ok(())
}

pub fn heap_base_address() -> u64 {
    let guard = heap_state().lock().unwrap();
    guard
        .as_ref()
        .map_or(0, |state| state.heap_base as usize as u64)
}

/// Whether `addr` lies within the current managed heap span. Registry restores
/// use this to skip dangling addresses (e.g. a symbol left in the global registry
/// by an earlier heap that this fresh heap does not contain) before dereferencing
/// them — dereferencing an address outside the live mmap would fault.
pub fn is_in_heap(addr: usize) -> bool {
    let guard = heap_state().lock().unwrap();
    match guard.as_ref() {
        Some(state) => {
            let base = state.heap_base as usize;
            addr >= base && addr < base + state.config.heap_size
        }
        None => false,
    }
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

    // A field needs relocation if it holds a reference to another heap object —
    // whether a raw (untagged) body pointer OR a TAGGED Lisp value (cons=|001,
    // heap-object=|010, function=|110). The earlier scan only matched raw body
    // pointers (`raw % 16 == 8`), silently missing every tagged pointer, so a
    // real Lisp graph (cons cars/cdrs, symbol value/function cells) did NOT
    // relocate across a base change (bliss-x0f2). Normalise each field to the
    // object BODY address it would denote and record it if that is a known
    // object. `apply_relocations` adds the (page-aligned) delta to the whole
    // word, preserving the low tag bits, so no per-entry tag is needed.
    use crate::value::{TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT, TAG_MASK};
    let mut relocs = Vec::new();
    let mut object_offset = 0usize;
    let _ = walk_heap(|ptr, _type_id, size| {
        let mut field_offset = 0usize;
        while field_offset + 8 <= size {
            let field_ptr = unsafe { ptr.add(field_offset) };
            let raw = unsafe { std::ptr::read_unaligned(field_ptr as *const u64) };
            let untagged = (raw & !TAG_MASK) as usize;
            let candidate_body = match raw & TAG_MASK {
                // Cons references point directly at the body.
                TAG_CONS => untagged,
                // Other heap references and functions point at the header; the
                // body (what walk_heap records) is one header further on.
                TAG_HEAP_OBJECT | TAG_FUNCTION => untagged + OBJECT_HEADER_SIZE,
                // A bare (untagged) body pointer, e.g. an internal raw slot.
                0 => raw as usize,
                // Any other tag (fixnum/char/symbol/immediate) is not a pointer.
                _ => {
                    field_offset += 8;
                    continue;
                }
            };
            if candidate_body >= heap_base
                && candidate_body % OBJECT_ALIGNMENT == OBJECT_HEADER_SIZE
                && object_addresses.contains(&candidate_body)
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
) -> Result<(), EgclError> {
    if reloc_data.is_empty() {
        return Ok(());
    }
    if reloc_data.len() < 8 {
        return Err(EgclError::InvalidImage("relocation table too small".into()));
    }

    let count = u64::from_le_bytes(reloc_data[..8].try_into().unwrap()) as usize;
    let expected_len = 8 + count * 8;
    if reloc_data.len() != expected_len {
        return Err(EgclError::InvalidImage(
            "relocation table length mismatch".into(),
        ));
    }

    for i in 0..count {
        let start = 8 + i * 8;
        let offset = u64::from_le_bytes(reloc_data[start..start + 8].try_into().unwrap()) as usize;
        if offset + 8 > object_data.len() {
            return Err(EgclError::InvalidImage(
                "relocation entry out of range".into(),
            ));
        }
        let raw = u64::from_le_bytes(object_data[offset..offset + 8].try_into().unwrap());
        let relocated = if delta >= 0 {
            raw.checked_add(delta as u64)
        } else {
            raw.checked_sub((-delta) as u64)
        }
        .ok_or_else(|| EgclError::InvalidImage("relocation overflow".into()))?;
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

pub fn restore_gc_metadata(data: &[u8]) -> Result<(), EgclError> {
    const FIELD_COUNT: usize = 14;
    if data.is_empty() {
        return Ok(());
    }
    if data.len() != FIELD_COUNT * 8 {
        return Err(EgclError::InvalidImage(
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
        .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;
    state.stats.minor_gc_count = read(0);
    state.stats.major_gc_count = read(1);
    state.stats.total_minor_pause_us = read(2);
    state.stats.total_major_pause_us = read(3);
    state.stats.bytes_allocated = read(4);
    state.stats.bytes_promoted = read(5);
    // Occupancy and capacities describe the newly installed region layout,
    // not the pre-save nursery/old-generation split.
    Ok(())
}

// ── Object-header helper tests (bliss-jtc.19) ─────────────────────

#[cfg(test)]
mod allocation_size_tests {
    use super::*;

    #[test]
    fn maximum_body_leaves_room_for_header_and_alignment() {
        for size in [1, MAX_OBJECT_BODY_SIZE - 1, MAX_OBJECT_BODY_SIZE] {
            assert_eq!(allocation_body_size(size), Some(size as u32));
            let (total, _) = object_footprint(size);
            assert!(total <= u32::MAX as usize);
        }
        let (total, large) = object_footprint(MAX_OBJECT_BODY_SIZE);
        assert!(large);
        assert_eq!(total, u32::MAX as usize & !(OBJECT_ALIGNMENT - 1));
        assert_eq!(total - LARGE_OBJECT_PAYLOAD_OFFSET, MAX_OBJECT_BODY_SIZE);
        for size in [0, MAX_OBJECT_BODY_SIZE + 1, u32::MAX as usize, usize::MAX] {
            assert_eq!(allocation_body_size(size), None);
        }
    }

    #[test]
    fn typed_entry_points_reject_sizes_before_touching_thread_allocator() {
        // Keeping the TLS allocator borrowed catches a guard placed after
        // entering the allocating path. No heap allocation or GC is needed.
        T0_ALLOCATOR.with(|cell| {
            let _borrow = cell.borrow_mut();
            let stress_count = GC_STRESS_COUNTER.with(std::cell::Cell::get);
            for size in [0, MAX_OBJECT_BODY_SIZE + 1, u32::MAX as usize, usize::MAX] {
                assert!(alloc_typed(size, crate::object::type_id::SIMPLE_ARRAY).is_none());
                assert!(alloc_pinned_typed(size, crate::object::type_id::SIMPLE_ARRAY).is_none());
            }
            assert_eq!(GC_STRESS_COUNTER.with(std::cell::Cell::get), stress_count);
        });
    }

    #[test]
    fn direct_allocator_rejects_oversized_bodies_without_refill_or_collection() {
        let mut allocator = HeapAllocator {
            tlab: Tlab {
                cursor: std::ptr::null_mut(),
                limit: std::ptr::null(),
                region_idx: 0,
            },
            region_size: 64,
            tlab_size: 32,
            pending_bytes: 0,
        };
        for size in [MAX_OBJECT_BODY_SIZE + 1, u32::MAX as usize, usize::MAX] {
            assert!(allocator.alloc_fast(size).is_none());
            assert!(matches!(allocator.alloc_slow(size), Err(EgclError::Oom)));
            assert!(matches!(allocator.alloc_large(size), Err(EgclError::Oom)));
            assert!(allocator.tlab.cursor.is_null());
            assert!(allocator.tlab.limit.is_null());
            assert_eq!(allocator.tlab.region_idx, 0);
        }
        assert!(allocator.alloc_fast(0).is_none());
        assert!(matches!(
            allocator.alloc_slow(0),
            Err(EgclError::Internal(_))
        ));
        assert!(matches!(
            allocator.alloc_large(0),
            Err(EgclError::Internal(_))
        ));
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;

    /// An 8-byte-aligned scratch buffer big enough for a header + a few words.
    fn scratch() -> Vec<u64> {
        vec![0u64; 8]
    }

    #[test]
    fn write_read_roundtrip_small_object() {
        let mut buf = scratch();
        let ptr = buf.as_mut_ptr() as *mut u8;
        // Request a 13-byte body → footprint align_up(8+13,16) = 32, units = 4.
        let body_off = unsafe { write_object_header(ptr, crate::object::type_id::RATIO, 13) };
        assert_eq!(
            body_off, OBJECT_HEADER_SIZE,
            "small object body at offset 8"
        );
        assert!(!unsafe { header_is_free(ptr) });
        assert!(!unsafe { header_is_forwarded(ptr) });
        assert_eq!(unsafe { header_total_bytes(ptr) }, 32);
        let (tid, body) = unsafe { read_object_header(ptr) };
        assert_eq!(tid, crate::object::type_id::RATIO);
        // Body is the aligned payload footprint (total 32 − header 8).
        assert_eq!(body, 24);
        // The exact logical body length is preserved for byte-exact serialization.
        assert_eq!(unsafe { header_exact_body_len(ptr) }, 13);
    }

    #[test]
    fn zeroed_slot_reads_as_free() {
        let mut buf = scratch();
        let ptr = buf.as_mut_ptr() as *mut u8;
        assert!(unsafe { header_is_free(ptr) });
        assert_eq!(unsafe { read_object_header(ptr) }, (0, 0));
    }

    #[test]
    fn forwarding_preserves_type_size_and_records_address() {
        let mut buf = scratch();
        let ptr = buf.as_mut_ptr() as *mut u8;
        unsafe { write_object_header(ptr, crate::object::type_id::STANDARD_OBJECT, 16) };
        let total_before = unsafe { header_total_bytes(ptr) };
        let new_body = 0xCAFE_0000usize as *mut u8;
        unsafe { header_set_forwarded(ptr, new_body) };
        assert!(unsafe { header_is_forwarded(ptr) });
        assert_eq!(unsafe { header_forwarding_addr(ptr) }, new_body);
        // type_id and size survive so the walker can still stride the stale copy.
        let (tid, _) = unsafe { read_object_header(ptr) };
        assert_eq!(tid, crate::object::type_id::STANDARD_OBJECT);
        assert_eq!(unsafe { header_total_bytes(ptr) }, total_before);
    }

    #[test]
    fn object_footprint_boundary() {
        // Small: fits the inline size field.
        let (total, large) = object_footprint(64);
        assert!(!large);
        assert_eq!(total, align_up(OBJECT_HEADER_SIZE + 64, OBJECT_ALIGNMENT));
        // Large: footprint needs ≥ 0xFFFF 8-byte units → extension.
        let big = (LARGE_SIZE_SENTINEL as usize) * 8; // 524_280 bytes body
        let (total_l, large_l) = object_footprint(big);
        assert!(large_l);
        assert_eq!(
            total_l,
            align_up(LARGE_OBJECT_PAYLOAD_OFFSET + big, OBJECT_ALIGNMENT)
        );
    }

    #[test]
    fn large_object_header_uses_extension() {
        let mut buf = scratch();
        let ptr = buf.as_mut_ptr() as *mut u8;
        let big = (LARGE_SIZE_SENTINEL as usize) * 8;
        let body_off =
            unsafe { write_object_header(ptr, crate::object::type_id::SIMPLE_ARRAY, big as u32) };
        assert_eq!(
            body_off, LARGE_OBJECT_PAYLOAD_OFFSET,
            "large object body at offset 16"
        );
        assert_eq!(unsafe { header_at(ptr) }.size_units(), LARGE_SIZE_SENTINEL);
        // The true byte size is stored just after the header and read back.
        let (expected_total, _) = object_footprint(big);
        assert_eq!(unsafe { header_total_bytes(ptr) }, expected_total);
        let (tid, _) = unsafe { read_object_header(ptr) };
        assert_eq!(tid, crate::object::type_id::SIMPLE_ARRAY);
    }
}

// ── Precise object-tracing tests (bliss-jtc.20) ───────────────────

#[cfg(test)]
mod trace_tests {
    use super::*;
    use crate::object::type_id as tid;

    /// A pointer-shaped, heap-tagged word for address `addr`.
    fn heapish(addr: u64) -> u64 {
        (addr & !crate::value::TAG_MASK) | crate::value::TAG_HEAP_OBJECT
    }

    /// Trace `body` as an object of `type_id`, collecting the raw field values
    /// handed to `visit` (before any is_heap_ref filtering).
    fn traced(type_id: u8, body: &[u64]) -> Vec<u64> {
        let mut out = Vec::new();
        let bytes = body.len() * 8;
        let mut body = body.to_vec();
        // SAFETY: `body` is a live slice of at least `bytes` bytes.
        unsafe {
            trace_object(body.as_mut_ptr() as *mut u8, type_id, bytes, |slot| {
                out.push((*slot).0)
            });
        }
        out
    }

    #[test]
    fn byte_and_numeric_payloads_are_never_traced() {
        // Bodies full of pointer-shaped words must yield NO references — the
        // core jtc.20 guarantee: raw payloads are not scanned for pointers.
        let ptrs = [heapish(0x4000), heapish(0x5000), heapish(0x6000)];
        for &t in &[
            tid::SIMPLE_BASE_STRING,
            tid::SIMPLE_CHARACTER_STRING,
            tid::BIGNUM,
            tid::DOUBLE_FLOAT,
        ] {
            assert!(
                traced(t, &ptrs).is_empty(),
                "type {t:#x} must not trace payload bytes as references"
            );
        }
    }

    #[test]
    fn ratio_and_complex_trace_both_reference_fields() {
        let body = [heapish(0x4000), heapish(0x5000)];
        assert_eq!(
            traced(tid::RATIO, &body),
            vec![heapish(0x4000), heapish(0x5000)]
        );
        assert_eq!(
            traced(tid::COMPLEX, &body),
            vec![heapish(0x4000), heapish(0x5000)]
        );
    }

    #[test]
    fn simple_vector_traces_elements_not_the_length_word() {
        // [length=2, elem0, elem1, padding]. The length is a raw count, not a ref.
        let body = [2, heapish(0x4000), heapish(0x5000), 0];
        assert_eq!(
            traced(tid::SIMPLE_VECTOR, &body),
            vec![heapish(0x4000), heapish(0x5000)]
        );
    }

    #[test]
    fn symbol_traces_five_reference_fields_not_flags() {
        // name/value/function/plist/package are refs; the flags/tls word is raw.
        let body = [
            heapish(0x1000),
            heapish(0x2000),
            heapish(0x3000),
            heapish(0x4000),
            heapish(0x5000),
            0xDEAD_BEEF,
        ];
        let got = traced(tid::SYMBOL, &body);
        assert_eq!(got.len(), 5);
        assert!(!got.contains(&0xDEAD_BEEF), "flags word must not be traced");
    }

    #[test]
    fn standard_object_skips_wrapper_pointer_traces_slots() {
        // [wrapper(raw ptr), slot0, slot1]. The wrapper is not a GC reference.
        let body = [0xAABB_CCDD, heapish(0x4000), heapish(0x5000)];
        assert_eq!(
            traced(tid::STANDARD_OBJECT, &body),
            vec![heapish(0x4000), heapish(0x5000)]
        );
    }

    #[test]
    fn compiled_function_skips_entry_and_code_size() {
        // [entry_ptr, code_size, name, lambda_list, min/max/tier, constants].
        let body = [
            0x1111_2222,     // entry_point (raw)
            700,             // code_size (raw)
            heapish(0x4000), // name
            heapish(0x5000), // lambda_list
            0,               // min/max/tier/pad (raw)
            heapish(0x6000), // constants
        ];
        assert_eq!(
            traced(tid::COMPILED_FUNCTION, &body),
            vec![heapish(0x4000), heapish(0x5000), heapish(0x6000)]
        );
    }

    #[test]
    fn is_heap_ref_recognizes_only_reference_tags() {
        assert!(is_heap_ref(EgclVal(heapish(0x4000))));
        assert!(is_heap_ref(EgclVal(0x1000 | crate::value::TAG_CONS)));
        assert!(is_heap_ref(EgclVal(0x1000 | crate::value::TAG_FUNCTION)));
        assert!(!is_heap_ref(EgclVal::from_fixnum(0x4000)));
        assert!(!is_heap_ref(crate::value::NIL));
        assert!(!is_heap_ref(crate::value::T));
    }
}

#[cfg(test)]
mod nursery_scan_tests {
    use super::*;
    use crate::value::NIL;

    thread_local! {
        pub(super) static HEADER_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn fresh_thread_collects_when_initial_nursery_is_full() {
        const CHILD: &str = "EGCL_TEST_FRESH_ALLOCATOR_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new("timeout")
                .args(["--kill-after=5", "30"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gc::nursery_scan_tests::fresh_thread_collects_when_initial_nursery_is_full",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_STRESS_AT")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        init_heap(&GcConfig {
            heap_size: 64 * 1024,
            heap_max: 64 * 1024,
            nursery_size: 4096,
            tlab_size: 4096,
            region_size: 4096,
            promotion_threshold: 3,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 32,
            old_occupancy_trigger: 0.9,
        })
        .unwrap();
        // The parent reserves the only nursery TLAB. It holds a live object
        // whose relocation proves that the worker actually collects and retries.
        let body = alloc_typed(8, crate::object::type_id::DOUBLE_FLOAT).unwrap();
        unsafe { *(body as *mut f64) = 42.0 };
        crate::rooted!(parent_value = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
        let original = parent_value.to_raw();
        let allocated = {
            // No Lisp/root accesses while the parent waits for the worker.
            let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
            std::thread::spawn(|| {
                let body = alloc_typed(8, crate::object::type_id::DOUBLE_FLOAT);
                if let Some(body) = body {
                    unsafe { *(body as *mut f64) = 17.0 };
                }
                body.is_some()
            })
            .join()
            .unwrap()
        };
        assert!(
            allocated,
            "first allocation must collect a full nursery and retry"
        );
        assert!(heap_stats().minor_gc_count > 0);
        assert_ne!(parent_value.to_raw(), original);
        assert_eq!(
            unsafe { *(parent_value.as_ptr().add(8) as *const f64) },
            42.0
        );
    }

    #[test]
    fn simultaneous_collectors_complete_safepoint_handshakes() {
        const CHILD: &str = "EGCL_TEST_SIMULTANEOUS_COLLECTORS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gc::nursery_scan_tests::simultaneous_collectors_complete_safepoint_handshakes",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        ensure_heap_initialized();
        crate::thread::current_thread_id();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_start = std::sync::Arc::clone(&start);
        let (finish, finished) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            crate::thread::current_thread_id();
            // Both participants are Running before either requests collection.
            // After this barrier each must either collect or acknowledge its
            // peer's pause while waiting for the collector admission lock.
            worker_start.wait();
            let result = HeapCollector::new().minor_gc();
            // Stay registered and explicitly Blocked until the peer finishes;
            // thread-exit coordination is not what this fixture is testing.
            let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
            finished
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            result
        });
        start.wait();
        let parent_result = HeapCollector::new().minor_gc();
        finish.send(()).unwrap();
        let worker_result = {
            let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
            worker.join().unwrap()
        };
        assert!(parent_result.is_ok(), "parent collector: {parent_result:?}");
        assert!(worker_result.is_ok(), "worker collector: {worker_result:?}");
    }

    #[test]
    fn minor_gc_visits_dead_nursery_objects_once() {
        // Isolate the collector from unrelated unit-test roots and heap resets.
        const CHILD: &str = "EGCL_TEST_NURSERY_SCAN_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gc::nursery_scan_tests::minor_gc_visits_dead_nursery_objects_once",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove("EGCL_GC_VERIFY")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let config = GcConfig {
            heap_size: 4 * 1024 * 1024,
            heap_max: 8 * 1024 * 1024,
            nursery_size: 4 * 1024 * 1024,
            tlab_size: 4096,
            region_size: 8192,
            promotion_threshold: 15,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 64,
            old_occupancy_trigger: 0.5,
        };
        init_heap(&config).unwrap();
        let mut alloc = HeapAllocator::new().unwrap();
        crate::rooted!(last = NIL);
        const OBJECTS: usize = 2000;
        for i in 0..OBJECTS {
            let body = alloc
                .alloc_fast(16)
                .or_else(|| alloc.alloc_slow(16).ok())
                .unwrap();
            unsafe {
                write_object_header(
                    body.sub(OBJECT_HEADER_SIZE),
                    crate::object::type_id::CONS,
                    16,
                );
                *(body as *mut EgclVal) = EgclVal::from_fixnum(i as i64);
                *((body as *mut EgclVal).add(1)) = NIL;
            }
            // All but the final cons are dead. Their headers are needed for
            // preparation, not again to locate the single evacuation candidate.
            *last = EgclVal(body as u64 | crate::value::TAG_CONS);
        }
        let before = *last;
        HEADER_READS.with(|count| count.set(0));
        HeapCollector::new().minor_gc().unwrap();
        let reads = HEADER_READS.with(|count| count.get());
        eprintln!("{OBJECTS} nursery objects: {reads} header reads");
        assert_ne!(*last, before, "the live object must actually evacuate");
        let body = (last.0 & !crate::value::TAG_MASK) as *const EgclVal;
        unsafe {
            assert_eq!(*body, EgclVal::from_fixnum((OBJECTS - 1) as i64));
            assert_eq!(*body.add(1), NIL);
        }
        // Allow the final unused TLAB tail and the live object's tracing and
        // relocation; a second full dead-object walk cannot fit this budget.
        assert!(
            reads < OBJECTS + 256,
            "{OBJECTS} nursery objects required {reads} header reads"
        );
    }
}

#[cfg(test)]
mod old_scan_tests {
    use super::*;
    use crate::value::NIL;

    thread_local! {
        pub(super) static DIRTY_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn alloc_object(alloc: &mut HeapAllocator, type_id: u8, body_len: usize) -> *mut u8 {
        let body = alloc
            .alloc_fast(body_len)
            .or_else(|| alloc.alloc_slow(body_len).ok())
            .unwrap();
        unsafe { write_object_header(body.sub(OBJECT_HEADER_SIZE), type_id, body_len as u32) };
        body
    }

    fn cons(alloc: &mut HeapAllocator, car: EgclVal, cdr: EgclVal) -> EgclVal {
        let body = alloc_object(alloc, crate::object::type_id::CONS, 16);
        unsafe {
            *(body as *mut EgclVal) = car;
            *(body as *mut EgclVal).add(1) = cdr;
        }
        EgclVal(body as u64 | crate::value::TAG_CONS)
    }

    fn vector(alloc: &mut HeapAllocator, len: usize, fill: EgclVal) -> EgclVal {
        let body = alloc_object(alloc, crate::object::type_id::SIMPLE_VECTOR, 8 * (len + 1));
        unsafe {
            *(body as *mut u64) = len as u64;
            for i in 0..len {
                *(body as *mut EgclVal).add(1 + i) = fill;
            }
            EgclVal::from_heap_ptr(body.sub(OBJECT_HEADER_SIZE))
        }
    }

    fn slot(v: EgclVal, index: usize) -> *mut EgclVal {
        (ref_body_addr(v) as *mut EgclVal).wrapping_add(index)
    }

    /// GitHub #471 / bliss-3uwzg: a minor collection used to walk and trace
    /// every older object twice, so its pause grew with the old generation.
    /// With soft-dirty tracking it reads only the headers of written pages,
    /// and old-to-young stores that bypass the write barrier — as compiled
    /// code and most of the runtime's stores do — are still found.
    #[test]
    fn minor_gc_scans_only_written_old_pages() {
        const CHILD: &str = "EGCL_TEST_DIRTY_OLD_SCAN_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gc::old_scan_tests::minor_gc_scans_only_written_old_pages",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove("EGCL_GC_VERIFY")
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_STRESS_AT")
                .env("EGCL_GC_SOFT_DIRTY", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        init_heap(&GcConfig {
            heap_size: 16 * 1024 * 1024,
            heap_max: 16 * 1024 * 1024,
            nursery_size: 2 * 1024 * 1024,
            tlab_size: 4096,
            region_size: 64 * 1024,
            promotion_threshold: 0,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 64,
            old_occupancy_trigger: 0.9,
        })
        .unwrap();
        let mut alloc = HeapAllocator::new().unwrap();

        // An old generation of OLD conses (promotion_threshold 0 promotes on
        // the first minor collection), a vector spanning several pages, and a
        // stub-to-be followed by a holder on a later page.
        const OLD: usize = 20_000;
        crate::rooted!(list = NIL);
        for i in 0..OLD {
            let next = cons(&mut alloc, EgclVal::from_fixnum(i as i64), *list);
            *list = next;
        }
        crate::rooted!(vec = vector(&mut alloc, 2048, NIL));
        let stub = vector(&mut alloc, 600, NIL);
        crate::rooted!(holder = cons(&mut alloc, stub, NIL));
        HeapCollector::new().minor_gc().unwrap();
        let old_first = (list.0 & !crate::value::TAG_MASK) as usize;
        assert!(
            heap_kind_of(old_first) == Some(RegionKind::OldGen),
            "the list must have been promoted"
        );

        // Nothing older was written: the scan must not walk the old generation.
        DIRTY_SCANS.with(|count| count.set(0));
        nursery_scan_tests::HEADER_READS.with(|count| count.set(0));
        HeapCollector::new().minor_gc().unwrap();
        let reads = nursery_scan_tests::HEADER_READS.with(|count| count.get());
        if DIRTY_SCANS.with(|count| count.get()) == 0 {
            eprintln!("soft-dirty tracking unavailable on this kernel; skipping");
            return;
        }
        eprintln!("{OLD} old conses, nothing written: {reads} header reads");
        assert!(
            reads < OLD / 10,
            "an unwritten old generation of {OLD} conses cost {reads} header reads"
        );

        // Old-to-young stores with no write barrier: into an old cons, into a
        // vector element several pages past its header, and a CHANGE-CLASS
        // style forwarding stub whose holder sits on an unwritten page.
        let mut alloc = HeapAllocator::new().unwrap();
        let young = cons(&mut alloc, EgclVal::from_fixnum(777), NIL);
        let mut target_cons = *list;
        for _ in 0..OLD / 2 {
            target_cons = unsafe { *slot(target_cons, 1) };
        }
        unsafe {
            *slot(target_cons, 0) = young;
            *slot(*vec, 1 + 1500) = young;
        }
        let replacement = vector(&mut alloc, 1, EgclVal::from_fixnum(4242));
        unsafe { forward_object_to(stub, replacement) };

        DIRTY_SCANS.with(|count| count.set(0));
        HeapCollector::new().minor_gc().unwrap();
        assert_eq!(DIRTY_SCANS.with(|count| count.get()), 1);

        let moved = unsafe { *slot(target_cons, 0) };
        assert_ne!(moved, young, "the young cons must have been evacuated");
        assert_eq!(unsafe { *slot(moved, 0) }, EgclVal::from_fixnum(777));
        assert_eq!(unsafe { *slot(*vec, 1 + 1500) }, moved);
        assert_eq!(unsafe { *slot(*vec, 1 + 1499) }, NIL);

        let held = unsafe { *slot(*holder, 0) };
        // The holder's page was not written, so it is normally left pointing
        // at the stub. A kernel may still report the page soft-dirty (a fresh
        // or merged VMA reads as all dirty), and then the slot is collapsed to
        // the stub's new target instead; either is correct.
        assert!(
            held == stub || held == resolve_forwarded(stub),
            "the holder must keep the stub or its current target"
        );
        let resolved = resolve_forwarded(held);
        assert_ne!(resolved, replacement, "the stub's target must have moved");
        assert_eq!(
            unsafe { *slot(resolved, 0) },
            EgclVal(1),
            "raw vector length"
        );
        assert_eq!(unsafe { *slot(resolved, 1) }, EgclVal::from_fixnum(4242));

        // The list itself is intact.
        let mut cursor = *list;
        for i in (0..OLD).rev() {
            let car = unsafe { *slot(cursor, 0) };
            if cursor == target_cons {
                assert_eq!(car, moved);
            } else {
                assert_eq!(car, EgclVal::from_fixnum(i as i64));
            }
            cursor = unsafe { *slot(cursor, 1) };
        }
        assert_eq!(cursor, NIL);
    }

    fn heap_kind_of(addr: usize) -> Option<RegionKind> {
        let guard = heap_state().lock().unwrap();
        let state = guard.as_ref()?;
        state
            .regions
            .iter()
            .find(|r| {
                let base = r.base as usize;
                addr >= base && addr < base + state.config.region_size
            })
            .map(|r| r.header.kind)
    }
}

#[cfg(test)]
mod evacuation_target_tests {
    use super::*;
    use crate::value::NIL;

    thread_local! {
        pub(super) static TARGET_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn minor_evacuation_searches_per_region_not_per_object() {
        // A collecting test must not scan the roots of concurrently running
        // unit tests. Run this test alone in a child with its own global heap.
        const CHILD: &str = "EGCL_TEST_EVACUATION_TARGET_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "gc::evacuation_target_tests::minor_evacuation_searches_per_region_not_per_object", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        for promotion_threshold in [0, 15] {
            let config = GcConfig {
                heap_size: 4 * 1024 * 1024,
                heap_max: 8 * 1024 * 1024,
                nursery_size: 4 * 1024 * 1024,
                tlab_size: 4096,
                region_size: 8192,
                promotion_threshold,
                pause_target_ms: 10,
                gc_workers: 1,
                satb_buffer_size: 64,
                old_occupancy_trigger: 0.5,
            };
            init_heap(&config).unwrap();
            let mut alloc = HeapAllocator::new().unwrap();
            crate::rooted!(list = NIL);
            for i in 0..2000 {
                let body = alloc
                    .alloc_fast(16)
                    .or_else(|| alloc.alloc_slow(16).ok())
                    .unwrap();
                unsafe {
                    write_object_header(
                        body.sub(OBJECT_HEADER_SIZE),
                        crate::object::type_id::CONS,
                        16,
                    );
                    *(body as *mut EgclVal) = EgclVal::from_fixnum(i);
                    *((body as *mut EgclVal).add(1)) = *list;
                }
                *list = EgclVal(body as u64 | crate::value::TAG_CONS);
            }
            TARGET_SEARCHES.with(|count| count.set(0));
            HeapCollector::new().minor_gc().unwrap();
            let searches = TARGET_SEARCHES.with(|count| count.get());
            let mut cursor = *list;
            for i in (0..2000).rev() {
                let body = (cursor.0 & !crate::value::TAG_MASK) as *const EgclVal;
                unsafe {
                    assert_eq!(*body, EgclVal::from_fixnum(i));
                    cursor = *body.add(1);
                }
            }
            assert_eq!(cursor, NIL);
            let live_bytes = 2000 * align_up(OBJECT_HEADER_SIZE + 16, OBJECT_ALIGNMENT);
            let target_regions = live_bytes.div_ceil(config.region_size);
            assert!(
                searches <= target_regions + 1,
                "{live_bytes} bytes of live conses need {target_regions} regions, not {searches} searches (promotion threshold {promotion_threshold})"
            );
        }
    }
}

// ── Heap exhaustion during a minor collection (GitHub #471) ─────────

#[cfg(test)]
mod evacuation_exhaustion_tests {
    use super::*;
    use crate::value::NIL;

    /// Cons `car` onto the list in the rooted `cdr` slot, which is read only
    /// after the allocation: a collection there may move the list.
    fn cons_onto(
        alloc: &mut HeapAllocator,
        car: i64,
        cdr: *const EgclVal,
    ) -> Result<EgclVal, EgclError> {
        let body = match alloc.alloc_fast(16) {
            Some(body) => body,
            None => alloc.alloc_slow(16)?,
        };
        unsafe {
            write_object_header(
                body.sub(OBJECT_HEADER_SIZE),
                crate::object::type_id::CONS,
                16,
            );
            *(body as *mut EgclVal) = EgclVal::from_fixnum(car);
            *((body as *mut EgclVal).add(1)) = *cdr;
        }
        Ok(EgclVal(body as u64 | crate::value::TAG_CONS))
    }

    fn list_length_descending(mut cursor: EgclVal) -> i64 {
        let mut count = 0;
        while cursor != NIL {
            let body = (cursor.0 & !crate::value::TAG_MASK) as *const EgclVal;
            unsafe {
                count += 1;
                cursor = *body.add(1);
                // Built by pushing 0, 1, 2, ...: the cell below holds one less.
                let next = if cursor == NIL { None } else { Some(cursor) };
                if let Some(next) = next {
                    let here = *body;
                    let below = *((next.0 & !crate::value::TAG_MASK) as *const EgclVal);
                    assert_eq!(below.as_fixnum() + 1, here.as_fixnum());
                }
            }
        }
        count
    }

    /// A heap whose live data outgrows it used to panic mid-evacuation ("GC
    /// minor evacuation exhausted regions"). The collection must instead leave
    /// every live object where it was, let the failed allocation proceed into
    /// the emergency reserve, post a HeapExhausted signal, and recover once the
    /// data is dropped.
    #[test]
    fn minor_collection_reports_exhaustion_instead_of_panicking() {
        const CHILD: &str = "EGCL_TEST_EVACUATION_EXHAUSTION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "gc::evacuation_exhaustion_tests::minor_collection_reports_exhaustion_instead_of_panicking", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let region_size = 8192;
        let config = GcConfig {
            heap_size: 64 * region_size,
            heap_max: 64 * region_size,
            nursery_size: 16 * region_size,
            tlab_size: 4096,
            region_size,
            promotion_threshold: 15,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 64,
            old_occupancy_trigger: 0.99,
        };
        init_heap(&config).unwrap();
        let mut alloc = HeapAllocator::new().unwrap();
        crate::rooted!(list = NIL);
        // 64 regions of 8 KiB hold at most 16,384 conses of 32 bytes.
        let mut pushed = 0i64;
        let mut exhausted = false;
        while pushed < 100_000 {
            match cons_onto(&mut alloc, pushed, &*list) {
                Ok(cell) => {
                    *list = cell;
                    pushed += 1;
                }
                Err(EgclError::Oom) => {
                    exhausted = true;
                    break;
                }
                Err(error) => panic!("unexpected allocation error: {error:?}"),
            }
        }
        assert!(
            exhausted,
            "{pushed} live conses never exhausted a 512 KiB heap"
        );
        assert_eq!(
            crate::thread::take_current_pending_signal(),
            Some(crate::thread::PendingSignal::HeapExhausted)
        );
        // Nothing live was dropped or moved out from under the root.
        assert_eq!(list_length_descending(*list), pushed);

        // Once the data is garbage, collections succeed again and the regions
        // lent to the nursery go back to the reserve.
        *list = NIL;
        for i in 0..50_000 {
            *list = cons_onto(&mut alloc, i % 1000, &NIL).expect("allocation after recovery");
        }
        let guard = heap_state().lock().unwrap();
        let state = guard.as_ref().unwrap();
        let nursery = state
            .regions
            .iter()
            .filter(|region| region.header.kind == RegionKind::Nursery)
            .count();
        assert!(nursery <= 16, "{nursery} nursery regions after recovery");
    }

    #[test]
    fn t0_heap_configuration_scales_the_nursery_with_the_heap() {
        let large = t0_config_for_heap_size(8 << 30);
        assert_eq!((large.heap_size, large.heap_max), (8 << 30, 8 << 30));
        assert_eq!(large.nursery_size, 64 << 20);
        let small = t0_config_for_heap_size(32 << 20);
        assert_eq!((small.heap_size, small.nursery_size), (32 << 20, 8 << 20));
        // Raised to the minimum, and rounded up to whole regions.
        assert_eq!(t0_config_for_heap_size(1).heap_size, T0_MIN_HEAP_SIZE);
        assert_eq!(
            t0_config_for_heap_size((100 << 20) + 1).heap_size,
            101 << 20
        );
    }
}

// ── Reference relocation tests (bliss-jtc.17) ─────────────────────

#[cfg(test)]
mod relocation_tests {
    use super::*;
    use crate::object::type_id as tid;

    const TAG_HEAP: u64 = 0b010;

    /// An evacuated object's reference field is rewritten to the forwarded
    /// location, and non-reference fields are left untouched. This is the inner
    /// loop of `relocate_object_fields` (trace_object → relocate_slot).
    #[test]
    fn object_field_relocation_rewrites_forwarded_field() {
        // `target` is forwarded to `moved`; `holder` (a RATIO) references target.
        let mut target = vec![0u64; 2]; // header@0, body@8
        let mut moved = vec![0u64; 2];
        unsafe {
            write_object_header(target.as_mut_ptr() as *mut u8, tid::DOUBLE_FLOAT, 8);
            write_object_header(moved.as_mut_ptr() as *mut u8, tid::DOUBLE_FLOAT, 8);
        }
        let target_body = unsafe { (target.as_mut_ptr() as *mut u8).add(8) } as usize;
        let moved_body = unsafe { (moved.as_mut_ptr() as *mut u8).add(8) } as usize;
        moved[1] = 0xFEED_1234; // marker at moved body
        unsafe {
            header_set_forwarded(target.as_mut_ptr() as *mut u8, moved_body as *mut u8);
        }

        let mut holder = vec![0u64; 3]; // header@0, numerator@8, denominator@16
        unsafe { write_object_header(holder.as_mut_ptr() as *mut u8, tid::RATIO, 16) };
        // Heap objects reference the object header (target_body − 8), not the body.
        holder[1] = ((target_body as u64) - 8) | TAG_HEAP; // numerator → target
        holder[2] = EgclVal::from_fixnum(7).0; // denominator (non-reference)
        let holder_body = unsafe { (holder.as_mut_ptr() as *mut u8).add(8) };

        // Heap bounds covering every buffer.
        let starts = [
            target.as_ptr() as usize,
            moved.as_ptr() as usize,
            holder.as_ptr() as usize,
        ];
        let lo = *starts.iter().min().unwrap();
        let hi = *starts.iter().max().unwrap() + 64;

        // SAFETY: holder_body is a live RATIO body of 16 bytes.
        unsafe {
            trace_object(holder_body, tid::RATIO, 16, |slot| {
                relocate_slot(slot, lo, hi);
            });
        }

        assert_eq!(
            holder[1] & !0b111,
            (moved_body as u64) - 8,
            "field relocated to moved header"
        );
        assert_eq!(holder[1] & 0b111, TAG_HEAP, "tag preserved");
        assert_eq!(
            unsafe { *(moved_body as *const u64) },
            0xFEED_1234,
            "contents intact"
        );
        assert_eq!(
            holder[2],
            EgclVal::from_fixnum(7).0,
            "non-reference field untouched"
        );
    }

    /// relocate_slot leaves non-references and references to un-forwarded objects
    /// unchanged.
    #[test]
    fn relocate_slot_leaves_non_forwarded_and_immediates_alone() {
        let mut obj = vec![0u64; 2];
        unsafe { write_object_header(obj.as_mut_ptr() as *mut u8, tid::CONS, 8) };
        let body = unsafe { (obj.as_mut_ptr() as *mut u8).add(8) } as usize;
        let lo = obj.as_ptr() as usize;
        let hi = lo + 64;

        // Reference to a live (non-forwarded) object: unchanged.
        let mut refslot = EgclVal((body as u64) | TAG_HEAP);
        unsafe { relocate_slot(&mut refslot as *mut EgclVal, lo, hi) };
        assert_eq!(refslot.0, (body as u64) | TAG_HEAP);

        // Immediate (fixnum): unchanged.
        let mut fix = EgclVal::from_fixnum(42);
        unsafe { relocate_slot(&mut fix as *mut EgclVal, lo, hi) };
        assert_eq!(fix, EgclVal::from_fixnum(42));
    }
}

#[cfg(test)]
mod restore_target_tests {
    use super::*;
    use crate::value::NIL;

    thread_local! {
        pub(super) static TARGET_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn extended_objects_restore_real_headers_and_tagged_links() {
        const CHILD: &str = "EGCL_TEST_EXTENDED_RESTORE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            for stress in [false, true] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", "gc::restore_target_tests::extended_objects_restore_real_headers_and_tagged_links", "--nocapture"])
                    .env(CHILD, "1").env_remove("EGCL_GC_STRESS").env_remove("EGCL_GC_STRESS_SKIP")
                    .env_remove("EGCL_GC_STRESS_AFTER_INIT").env_remove("EGCL_GC_DISABLE")
                    .env_remove("EGCL_GC_STRESS_AT").env_remove("EGCL_GC_POISON");
                if stress {
                    command
                        .env("EGCL_GC_STRESS", "1")
                        .env("EGCL_GC_POISON", "1");
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "stress={stress}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }
        let config = GcConfig {
            heap_size: 8 << 20,
            heap_max: 8 << 20,
            nursery_size: 1 << 20,
            region_size: 1 << 20,
            tlab_size: 32 << 10,
            promotion_threshold: 2,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 64,
            old_occupancy_trigger: 0.5,
        };
        init_heap(&config).unwrap();
        let small_body = alloc_typed(16, crate::object::type_id::SIMPLE_VECTOR).unwrap();
        unsafe {
            *(small_body as *mut u64) = 1;
            *(small_body.add(8) as *mut EgclVal) = EgclVal::from_fixnum(42);
        }
        crate::rooted!(
            small = unsafe { EgclVal::from_heap_ptr(small_body.sub(OBJECT_HEADER_SIZE)) }
        );
        let small_before = *small;
        const SIZE: usize = 600 << 10;
        let body = alloc_typed(SIZE, crate::object::type_id::SIMPLE_VECTOR).unwrap();
        unsafe {
            std::ptr::write_bytes(body, 0, SIZE);
            *(body as *mut u64) = ((SIZE - 8) / 8) as u64;
            *(body.add(8) as *mut EgclVal) = *small;
            *(body.add(SIZE - 8) as *mut EgclVal) = EgclVal::from_fixnum(73);
        }
        crate::rooted!(
            large = unsafe { EgclVal::from_heap_ptr(body.sub(LARGE_OBJECT_PAYLOAD_OFFSET)) }
        );
        if gc_stress_stride() != 0 {
            assert_ne!(
                *small, small_before,
                "stress must actually relocate the nursery child"
            );
        }
        for _ in 0..2 {
            let old_payload = unsafe { large.as_ptr().add(LARGE_OBJECT_PAYLOAD_OFFSET) } as usize;
            let before_restore = *large;
            // Reorder actual serialized records so restore cannot accidentally
            // validate a missing relocation by reusing the original addresses.
            let saved = serialize_heap_objects();
            let mut records = Vec::new();
            let mut offset = 0;
            while offset < saved.len() {
                let size =
                    u32::from_le_bytes(saved[offset + 9..offset + 13].try_into().unwrap()) as usize;
                let end = offset + 13 + size;
                records.push(&saved[offset..end]);
                offset = end;
            }
            let bytes: Vec<u8> = records.into_iter().rev().flatten().copied().collect();
            restore_heap(&bytes).unwrap();
            *large = EgclVal(remap_saved_pointer(large.0));
            *small = EgclVal(remap_saved_pointer(small.0));
            assert_ne!(
                *large, before_restore,
                "large identity must actually relocate"
            );
            let header = unsafe { large.as_ptr() };
            assert_eq!(
                unsafe { header_at(header) }.type_id(),
                crate::object::type_id::SIMPLE_VECTOR
            );
            assert_eq!(
                unsafe { header_at(header) }.size_units(),
                LARGE_SIZE_SENTINEL
            );
            assert_eq!(
                unsafe { header_total_bytes(header) },
                object_footprint(SIZE).0
            );
            assert_eq!(unsafe { header_exact_body_len(header) }, SIZE);
            assert!(unsafe { header_is_pinned(header) });
            let payload = unsafe { header.add(LARGE_OBJECT_PAYLOAD_OFFSET) };
            assert_eq!(
                remap_saved_address(old_payload),
                payload as usize,
                "raw payload alias"
            );
            assert_eq!(unsafe { *(payload as *const u64) }, ((SIZE - 8) / 8) as u64);
            assert_eq!(unsafe { *(payload.add(8) as *const EgclVal) }, *small);
            assert_eq!(
                unsafe { *(payload.add(SIZE - 8) as *const EgclVal) },
                EgclVal::from_fixnum(73)
            );
            assert_eq!(
                unsafe { header_at(small.as_ptr()) }.type_id(),
                crate::object::type_id::SIMPLE_VECTOR
            );
            assert!(unsafe { header_is_pinned(small.as_ptr()) });
            assert_eq!(
                unsafe { *(small.as_ptr().add(16) as *const EgclVal) },
                EgclVal::from_fixnum(42)
            );
            let before = *large;
            collect_t0_minor().unwrap();
            assert_eq!(*large, before, "restored image address must stay pinned");
            assert_eq!(
                unsafe { header_total_bytes(large.as_ptr()) },
                object_footprint(SIZE).0
            );
        }
    }

    #[test]
    fn restored_objects_search_per_region_and_preserve_links() {
        const CHILD: &str = "EGCL_TEST_RESTORE_TARGET_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "gc::restore_target_tests::restored_objects_search_per_region_and_preserve_links", "--nocapture"])
                .env(CHILD, "1")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let config = GcConfig {
            heap_size: 4 * 1024 * 1024,
            heap_max: 8 * 1024 * 1024,
            nursery_size: 4 * 1024 * 1024,
            tlab_size: 4096,
            region_size: 8192,
            promotion_threshold: 2,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 64,
            old_occupancy_trigger: 0.5,
        };
        init_heap(&config).unwrap();
        // Synthetic old addresses exercise relocation independently of the new heap base.
        let mut bytes = Vec::new();
        let mut tail = NIL.0;
        for i in 0..2000 {
            let old_body = 0x10000000u64 + i * 32;
            bytes.extend_from_slice(&old_body.to_le_bytes());
            bytes.push(crate::object::type_id::CONS);
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&EgclVal::from_fixnum(i as i64).0.to_ne_bytes());
            bytes.extend_from_slice(&tail.to_ne_bytes());
            tail = old_body | crate::value::TAG_CONS;
        }
        // A second restore must not retain a cursor into the previous heap.
        for _ in 0..2 {
            TARGET_SEARCHES.with(|count| count.set(0));
            restore_heap(&bytes).unwrap();
            let searches = TARGET_SEARCHES.with(|count| count.get());
            let mut cursor = remap_saved_pointer(tail);
            for i in (0..2000).rev() {
                let body = (cursor & !crate::value::TAG_MASK) as *const EgclVal;
                unsafe {
                    assert_eq!(*body, EgclVal::from_fixnum(i));
                    cursor = (*body.add(1)).0;
                }
            }
            assert_eq!(cursor, NIL.0);
            let regions = (2000 * object_footprint(16).0).div_ceil(config.region_size);
            assert!(
                searches <= regions + 1,
                "restoring {regions} regions required {searches} region searches"
            );
        }
        // Interleave large and small objects, then force a search back into an
        // older nursery region's gap. Verify every payload remains disjoint.
        let mut guard = heap_state().lock().unwrap();
        let state = guard.as_mut().unwrap();
        clear_heap_objects(state);
        let mut targets = [None; 2];
        let mut objects = Vec::new();
        for size in [4000, 5000, 4000, 1000, 6000, 4000, 3032, 128] {
            let mut body = vec![b'x'; size];
            body[..8].copy_from_slice(&((size - 8) as u64).to_ne_bytes());
            let addr = append_serialized_object(
                state,
                crate::object::type_id::SIMPLE_BASE_STRING,
                &body,
                &mut targets,
            )
            .unwrap();
            let slot = usize::from(object_footprint(size).0 > config.region_size / 2);
            assert_eq!(
                state.regions[targets[slot].unwrap()].header.kind,
                if slot == 0 {
                    RegionKind::Nursery
                } else {
                    RegionKind::LargeObject
                }
            );
            objects.push((addr, body));
        }
        let first_region = (objects[0].0 - state.heap_base as usize) / config.region_size;
        let last_region =
            (objects.last().unwrap().0 - state.heap_base as usize) / config.region_size;
        assert_eq!(
            first_region, last_region,
            "small object should reuse the earlier gap"
        );
        for (addr, body) in objects {
            assert_eq!(
                unsafe { std::slice::from_raw_parts(addr as *const u8, body.len()) },
                body
            );
        }
    }
}

#[cfg(test)]
mod region_snapshot_tests {
    use super::*;
    use crate::object::type_id as tid;
    use crate::value::{NIL, TAG_CONS};

    #[test]
    fn snapshot_preserves_layout_and_marks_only_pointer_slots() {
        const CHILD: &str = "EGCL_TEST_REGION_SNAPSHOT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "gc::region_snapshot_tests::snapshot_preserves_layout_and_marks_only_pointer_slots", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let config = GcConfig {
            heap_size: 4 * 1024 * 1024,
            heap_max: 8 * 1024 * 1024,
            nursery_size: 4 * 1024 * 1024,
            tlab_size: 4096,
            region_size: 8192,
            promotion_threshold: 2,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 64,
            old_occupancy_trigger: 0.5,
        };
        init_heap(&config).unwrap();
        let mut cons_bytes = Vec::new();
        cons_bytes.extend_from_slice(&EgclVal::from_fixnum(7).0.to_ne_bytes());
        cons_bytes.extend_from_slice(&NIL.0.to_ne_bytes());
        record_object(tid::CONS, cons_bytes.clone());
        record_object(tid::CONS, cons_bytes.clone()); // unreachable hole
        record_object(tid::CONS, cons_bytes);
        let mut bodies = Vec::new();
        walk_heap(|ptr, _, _| {
            bodies.push(ptr as usize);
            true
        })
        .unwrap();
        let first = bodies[0];
        let last = bodies[2];
        unsafe {
            *(first as *mut u64) = first as u64; // legitimate fixnum with pointer-shaped bits
            *((first as *mut u64).add(1)) = last as u64 | TAG_CONS;
            *((last as *mut u64).add(1)) = first as u64 | TAG_CONS;
        }
        // The same pointer-shaped bits are data in a string and a fixnum slot.
        let mut string = 8u64.to_ne_bytes().to_vec();
        string.extend_from_slice(&(first as u64 | TAG_CONS).to_ne_bytes());
        record_object(tid::SIMPLE_BASE_STRING, string.clone());
        record_object(tid::FOREIGN_POINTER, vec![0x55; 16]);
        let snapshot = unsafe { snapshot_heap_regions(false) }.unwrap();
        assert_eq!(snapshot.region_size, config.region_size);
        assert_eq!(snapshot.regions.len(), 1);
        let region = &snapshot.regions[0];
        let base = region.saved_start;
        assert_eq!(region.bytes.len() % crate::image_heap::IMAGE_PAGE_SIZE, 0);
        assert_eq!(region.fixups, vec![first + 8 - base, last + 8 - base]);
        assert_eq!(
            u64::from_ne_bytes(
                region.bytes[first - base..first + 8 - base]
                    .try_into()
                    .unwrap()
            ),
            first as u64
        );
        assert_eq!(
            u64::from_ne_bytes(
                region.bytes[first + 8 - base..first + 16 - base]
                    .try_into()
                    .unwrap()
            ),
            last as u64 | TAG_CONS
        );
        let mut native_unchanged = false;
        walk_heap(|ptr, kind, size| {
            let offset = ptr as usize - base;
            if kind == tid::SIMPLE_BASE_STRING {
                assert_eq!(&region.bytes[offset..offset + size], string.as_slice());
            }
            if kind == tid::FOREIGN_POINTER {
                assert!(region.bytes[offset..offset + size].iter().all(|&b| b == 0));
                native_unchanged = unsafe { std::slice::from_raw_parts(ptr, size) }
                    .iter()
                    .all(|&b| b == 0x55);
            }
            true
        })
        .unwrap();
        assert!(native_unchanged);
        crate::rooted!(root = EgclVal(first as u64 | TAG_CONS));
        let reachable = unsafe { snapshot_heap_regions(true) }.unwrap();
        assert_eq!(reachable.regions.len(), 1);
        let region = &reachable.regions[0];
        assert_eq!(
            region.used,
            last - OBJECT_HEADER_SIZE + object_footprint(16).0 - base
        );
        let hole = bodies[1] - OBJECT_HEADER_SIZE - base;
        let header = ObjectHeader(u64::from_ne_bytes(
            region.bytes[hole..hole + 8].try_into().unwrap(),
        ));
        assert_eq!(
            header.type_id(),
            0,
            "filtered objects become walkable fillers"
        );
        assert_eq!(region.fixups.len(), 2);
        assert_eq!(root.0, first as u64 | TAG_CONS);
        // Large objects retain their extended header and span multiple regions.
        let mut allocator = HeapAllocator::new().unwrap();
        let large_len = 600_000;
        let large_body = allocator.alloc_large(large_len).unwrap();
        unsafe {
            write_object_header(
                large_body.sub(LARGE_OBJECT_PAYLOAD_OFFSET),
                tid::SIMPLE_ARRAY,
                large_len as u32,
            );
            std::ptr::write_bytes(large_body, 0x5a, large_len);
            std::ptr::write_unaligned(large_body.cast::<u64>(), first as u64 | TAG_CONS);
        }
        let snapshot = unsafe { snapshot_heap_regions(false) }.unwrap();
        let large = snapshot
            .regions
            .iter()
            .find(|region| region.kind == RegionKind::LargeObject)
            .unwrap();
        assert_eq!(large.used, object_footprint(large_len).0);
        assert!(large.used > 2 * config.region_size);
        let encoded = snapshot.encode().unwrap();
        let decoded = crate::image_heap::HeapImageView::parse(&encoded).unwrap();
        assert_eq!(decoded.regions.len(), snapshot.regions.len());
        for (saved, loaded) in snapshot.regions.iter().zip(&decoded.regions) {
            assert_eq!(loaded.saved_start, saved.saved_start);
            assert_eq!(loaded.used, saved.used);
            assert_eq!(loaded.bytes, saved.bytes);
            assert_eq!(loaded.fixups().collect::<Vec<_>>(), saved.fixups);
        }
        assert_eq!(
            u64::from_ne_bytes(large.bytes[8..16].try_into().unwrap()),
            object_footprint(large_len).0 as u64
        );
        assert!(
            large.fixups.is_empty(),
            "packed array data must never be relocated"
        );
        assert_eq!(
            &large.bytes[LARGE_OBJECT_PAYLOAD_OFFSET..LARGE_OBJECT_PAYLOAD_OFFSET + large_len],
            unsafe { std::slice::from_raw_parts(large_body, large_len) }
        );
        // Exercise both the buffer path and a real file-backed restore, then
        // mutate, collect, and save the restored heap again.
        let path = std::env::temp_dir().join(format!("egcl-region-{}.image", std::process::id()));
        let mut next_image = encoded;
        for file_backed in [false, true] {
            let old_root = root.0;
            let before_epoch = gc_move_epoch();
            let mut file_bytes = vec![0; crate::image_heap::IMAGE_PAGE_SIZE];
            file_bytes.extend_from_slice(&next_image);
            std::fs::write(&path, &file_bytes).unwrap();
            let file = std::fs::File::open(&path).unwrap();
            #[cfg(target_os = "linux")]
            if file_backed && crate::syscall::page_size() == 4096 {
                let write_only = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                let old_base = heap_base_address();
                assert!(
                    unsafe { restore_heap_regions(&next_image, Some((&write_only, 4096))) }
                        .is_err()
                );
                assert_eq!(
                    heap_base_address(),
                    old_base,
                    "failed mapping replaced live heap"
                );
                assert_eq!(gc_move_epoch(), before_epoch);
            }
            unsafe {
                restore_heap_regions(&next_image, file_backed.then_some((&file, 4096))).unwrap();
            }
            *root = EgclVal(remap_saved_pointer(old_root));
            assert_ne!(root.0, old_root, "fresh reservation must force relocation");
            assert!(gc_move_epoch() > before_epoch);
            let restored = (root.0 & !crate::value::TAG_MASK) as *mut u64;
            #[cfg(target_os = "linux")]
            if file_backed && crate::syscall::page_size() == 4096 {
                let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
                assert!(
                    maps.lines().any(|line| {
                        let range = line.split_whitespace().next().unwrap();
                        let (start, end) = range.split_once('-').unwrap();
                        let start = usize::from_str_radix(start, 16).unwrap();
                        let end = usize::from_str_radix(end, 16).unwrap();
                        start <= restored as usize
                            && (restored as usize) < end
                            && line.contains(path.to_str().unwrap())
                            && line.contains("rw-p")
                    }),
                    "restored object is not in a private file mapping"
                );
            }
            unsafe {
                assert_eq!(*restored, first as u64, "pointer-shaped fixnum changed");
                let tail = (*restored.add(1) & !crate::value::TAG_MASK) as *const u64;
                assert_eq!(*tail.add(1), root.0, "cycle lost");
                *restored = EgclVal::from_fixnum(91).0;
                *restored = first as u64;
            }
            assert_eq!(
                std::fs::read(&path).unwrap(),
                file_bytes,
                "private writes changed file"
            );
            let mut young_allocator = HeapAllocator::new().unwrap();
            let young = young_allocator.alloc_fast(16).unwrap();
            unsafe {
                write_object_header(young.sub(OBJECT_HEADER_SIZE), tid::CONS, 16);
                *(young as *mut u64) = EgclVal::from_fixnum(123).0;
                *(young as *mut u64).add(1) = NIL.0;
                store_ref(restored.cast::<EgclVal>(), EgclVal(young as u64 | TAG_CONS));
            }
            let mut fresh = HeapCollector::new();
            fresh.minor_gc().unwrap();
            unsafe {
                let moved = (*restored & !crate::value::TAG_MASK) as *const u64;
                assert_ne!(
                    moved,
                    young.cast(),
                    "probe must actually move the young object"
                );
                assert_eq!(*moved, EgclVal::from_fixnum(123).0);
                store_ref(restored.cast::<EgclVal>(), EgclVal(first as u64));
            }
            let saved_again = unsafe { snapshot_heap_regions(false) }.unwrap();
            assert!(saved_again.regions.iter().any(|r| r.used == large.used));
            next_image = saved_again.encode().unwrap();
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod allocation_failure_message_tests {
    use super::*;

    /// A refused allocation names the collector's error once, then the next
    /// message without a recorded cause stays generic (bliss-wyt5n).
    #[test]
    fn names_the_recorded_cause_once() {
        note_allocation_failure(&EgclError::Internal("safepoint handshake failed".into()));
        let message = allocation_failure_message("double-float");
        assert!(
            message.starts_with("GC heap unavailable for double-float: "),
            "{message}"
        );
        assert!(message.contains("safepoint handshake failed"), "{message}");
        assert_eq!(
            allocation_failure_message("double-float"),
            "GC heap unavailable for double-float"
        );
    }
}

#[cfg(test)]
#[path = "gc_root_kind_tests.rs"]
mod root_kind_tests;

#[cfg(test)]
#[path = "gc_root_preparer_tests.rs"]
mod root_preparer_tests;
