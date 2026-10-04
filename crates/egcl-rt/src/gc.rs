// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Garbage collector — generational, region-based, concurrent old-gen marking.
//!
//! Inspired by HotSpot G1 and ZGC. See §3 of the spec.

use crate::error::EgclError;
use crate::image_relocation::ImageRelocations;
use crate::value::EgclVal;

use std::alloc::Layout;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
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
        })
    }

    /// Refill the TLAB from a nursery region. If the configured nursery budget
    /// has room, lazily converts a Free region to Nursery. Returns OOM once the
    /// budget is full; the collecting slow path uses that result as its trigger.
    fn refill_tlab(&mut self) -> Result<(), EgclError> {
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

        // No nursery region with space. Commit another region only while doing
        // so stays within the configured nursery budget; the rest of the heap
        // is survivor/old-generation reserve, not an extension of the nursery.
        let nursery_regions = state
            .regions
            .iter()
            .filter(|region| region.header.kind == RegionKind::Nursery)
            .count();
        let nursery_region_limit = state.config.nursery_size.div_ceil(self.region_size);
        if nursery_regions < nursery_region_limit {
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
        self.publish_tlab_tail();
        self.tlab.cursor = self.tlab.limit as *mut u8;
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
            let body_off = unsafe { write_object_header(header_ptr, 0, size as u32) };
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
            HeapCollector::new().minor_gc()?;
            self.refill_tlab()?;
        }

        // Retry fast-path allocation after refill.
        self.alloc_fast(size).ok_or(EgclError::Oom)
    }

    fn alloc_large(&mut self, size: usize) -> Result<*mut u8, EgclError> {
        if size == 0 {
            return Err(EgclError::Internal("zero-size large alloc".into()));
        }
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
                    let body_off = unsafe { write_object_header(ptr, 0, size as u32) };
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
                let body_off = unsafe { write_object_header(ptr, 0, size as u32) };
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
#[derive(Default)]
struct NurseryObjectMap {
    heap_base: usize,
    region_size: usize,
    region_offsets: Vec<usize>,
    starts: Vec<u64>,
    marked: Vec<u64>,
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

    #[inline]
    fn contains(&self, body: usize) -> bool {
        self.position(body)
            .is_some_and(|(word, bit)| self.starts[word] & bit != 0)
    }

    #[inline]
    fn mark(&mut self, body: usize) -> bool {
        let Some((word, bit)) = self.position(body) else {
            return false;
        };
        if self.starts[word] & bit == 0 || self.marked[word] & bit != 0 {
            return false;
        }
        self.marked[word] |= bit;
        true
    }

    #[inline]
    fn is_marked(&self, body: usize) -> bool {
        self.position(body)
            .is_some_and(|(word, bit)| self.marked[word] & bit != 0)
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
        let words = (self.region_size / OBJECT_ALIGNMENT).div_ceil(64);
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
        // ── Reference-free leaves: numbers and byte/character payloads. Their
        //    bodies are raw bits and must never be scanned for pointers. ──
        tid::BIGNUM
        | tid::DOUBLE_FLOAT
        | tid::FOREIGN_POINTER
        | tid::FOREIGN_LIBRARY
        | tid::FOREIGN_CALLBACK
        | tid::PYTHON_OBJECT
        | tid::SIMPLE_BASE_STRING
        | tid::SIMPLE_CHARACTER_STRING => {}

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
        //    trailing flags/tls_index words are raw (§1.7). ──
        tid::SYMBOL => {
            for i in 0..5 {
                visit_word(i);
            }
        }

        // ── Interpreted function: lambda_list/body/env/name (§1.11.1). ──
        tid::FUNCTION_INTERPRETED => {
            for i in 0..4 {
                visit_word(i);
            }
            if words >= 4 {
                if let Some(trace) = FUNCTION_CAPTURE_TRACE_FN.get() {
                    trace(word(3), &mut visit);
                }
            }
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

    /// Rewrite every reference field of every surviving object to its referent's
    /// forwarded location (bliss-jtc.17). Walks all old-gen/survivor/large-object
    /// regions — which after evacuation hold the live survivors *and* the fresh
    /// copies — and, for each non-forwarded object, traces its reference fields
    /// (precisely, by type_id) and relocates any that point at an evacuated
    /// object. Forwarded (stale) originals are skipped; their storage is about to
    /// be reclaimed. Must run after all copying installs forwarding and before
    /// any region is freed, so no live field is left pointing at a stale location.
    fn relocate_object_fields(state: &HeapState, heap_base: usize, heap_end: usize) {
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
                    unsafe {
                        trace_object(body, type_id, body_size as usize, |slot| {
                            relocate_slot(slot, heap_base, heap_end);
                        });
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

        let mut guard = heap_state().lock().unwrap();
        let state = guard
            .as_mut()
            .ok_or_else(|| EgclError::Internal("heap not initialized".into()))?;

        let promotion_threshold = self.promotion_threshold;

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
        // supplies all three facts without rereading every dead nursery object.
        let mut nursery_index = MINOR_OBJECT_MAP.with(|s| std::mem::take(&mut *s.borrow_mut()));
        nursery_index.reset(
            state.heap_base as usize,
            state.config.region_size,
            state.regions.len(),
            &nursery_indices,
        );
        let mut pinned_indices = Vec::new();
        for &idx in &nursery_indices {
            let top = state.regions[idx].header.alloc_top as usize;
            // SAFETY: mutators are stopped and these are allocated nursery
            // regions; prepare_region repairs their zero-filled TLAB gaps.
            if unsafe { nursery_index.prepare_region(idx, top) } {
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

        // Direct roots shared with the major collector.
        mark_ref(
            get_entry_continuation(),
            &mut nursery_index,
            &mut mark_worklist,
        );
        Self::scan_cl_stack_roots(|v| mark_ref(v, &mut nursery_index, &mut mark_worklist));
        let tracing = gc_verify_enabled();
        if tracing {
            VERIFY_TRACE.with(|t| {
                let mut t = t.borrow_mut();
                t.mark.clear();
                t.reloc.clear();
            });
        }
        crate::symbols::for_each_root_slot(|slot| {
            let v = unsafe { *slot };
            if tracing {
                VERIFY_TRACE.with(|t| t.borrow_mut().mark.insert(slot as usize, v.0));
            }
            mark_ref(v, &mut nursery_index, &mut mark_worklist);
        });
        scan_external_roots(|slot| {
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
            if slot_addr >= heap_base_addr && slot_addr + 8 <= heap_end {
                mark_ref(
                    unsafe { *(slot_addr as *const EgclVal) },
                    &mut nursery_index,
                    &mut mark_worklist,
                );
            }
        }

        // Scan every non-nursery object for young references.  The remembered
        // set is the fast-path record of old-to-young stores, but a complete
        // collection must also cover objects constructed before a barrier was
        // available and survivor-to-nursery edges.
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
                // Stride and payload must honour the large-object 16-byte header
                // (bliss-tjru): a large SIMPLE_VECTOR's payload is at cursor+16,
                // and its footprint is the stored u64 total, not
                // align_up(8+body). Reading fields at cursor+8 would treat the
                // size-extension word as the element count and scan far past the
                // object.
                let total_size = unsafe { header_total_bytes(header_ptr) };
                if !unsafe { header_is_forwarded(header_ptr) } {
                    let body_off = unsafe { body_offset(header_ptr) };
                    unsafe {
                        trace_object(
                            (cursor + body_off) as *mut u8,
                            type_id,
                            body_size as usize,
                            |slot| mark_ref(*slot, &mut nursery_index, &mut mark_worklist),
                        );
                    }
                }
                cursor += total_size;
            }
        }

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
                         placed; {free_regions} free of {} regions [{}]. Increase EGCL_HEAP_MB, \
                         or report a bead: the old-occupancy major-GC trigger should have \
                         prevented this (bliss-wc4t).",
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

        // Promote retained (pinned) nursery regions to old-gen in place, before
        // the relocation passes, so their objects' fields are traced and any
        // references they hold to just-evacuated objects are rewritten. The
        // pinned objects themselves did not move (bliss-jtc.18).
        for &idx in &pinned_indices {
            let region = &mut state.regions[idx];
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
            if slot_addr < heap_base_addr || slot_addr + 8 > heap_end {
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
        Self::relocate_object_fields(state, heap_base_addr, heap_end);

        // Complete the young-object root set (bliss-jtc.17): a nursery object may
        // also be reachable only from a CL-stack frame or the entry continuation.
        // Relocate those roots to the moved locations too, while forwarding is
        // still intact and before the nursery is reset.
        Self::relocate_cl_stack_refs(heap_base_addr, state.config.heap_size);
        relocate_entry_continuation(heap_base_addr, heap_end);
        // Symbol-table roots (bliss-jtc.6 Stage C): a nursery object may be
        // reachable only from a global symbol's cell; rewrite those cells to the
        // evacuated location while forwarding is intact.
        crate::symbols::for_each_root_slot(|slot| unsafe {
            let before = (*slot).0;
            relocate_slot(slot, heap_base_addr, heap_end);
            if tracing {
                let after = (*slot).0;
                VERIFY_TRACE.with(|t| t.borrow_mut().reloc.insert(slot as usize, (before, after)));
            }
        });
        scan_external_roots(|slot| unsafe {
            let before = (*slot).0;
            relocate_slot(slot, heap_base_addr, heap_end);
            if tracing {
                let after = (*slot).0;
                VERIFY_TRACE.with(|t| t.borrow_mut().reloc.insert(slot as usize, (before, after)));
            }
        });

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
            // Zero the region memory so walk_heap doesn't see stale forwarding pointers.
            let region_used =
                (region.header.alloc_top as usize).saturating_sub(region.base as usize);
            if region_used > 0 {
                unsafe {
                    // bliss-6b2 #2 diagnostic: EGCL_GC_POISON fills reclaimed
                    // nursery with 0xFA instead of 0. As a EgclVal, 0xFAFA…FA
                    // is TAG_HEAP_OBJECT (low bits 010) with a non-canonical
                    // pointer, so an unrooted value the tree-walker held across
                    // this collection — now pointing here — FAULTS on its next
                    // dereference (SIGSEGV) instead of silently reading NIL from
                    // a zeroed slot. Run under gdb to get the backtrace at the
                    // offending deref, which names the missing GC root.
                    let fill = if gc_poison_enabled() { 0xFA } else { 0x00 };
                    std::ptr::write_bytes(region.base, fill, region_used);
                }
            }
            region.header.alloc_top = region.base;
            region.header.live_bytes = 0;
            region.header.gen_age = 0;
        }

        // Update stats.
        state.stats.nursery_regions_retained += regions_retained;
        state.stats.nursery_regions_evacuated += regions_evacuated;
        if gc_region_log_enabled() {
            crate::syscall::dbg_write(b"[gc-regions] minor: retained ");
            write_decimal(regions_retained);
            crate::syscall::dbg_write(b" evacuated ");
            write_decimal(regions_evacuated);
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

        // Return the scratch to the thread-local with its capacity retained
        // (bliss-5i3f). No early exit runs between the takes above and here.
        MINOR_OBJECT_MAP.with(|s| *s.borrow_mut() = std::mem::take(&mut nursery_index));
        MINOR_WORKLIST.with(|s| *s.borrow_mut() = std::mem::take(&mut mark_worklist));

        Ok(())
    }

    fn minor_gc_stop_the_world(&mut self) -> Result<(), EgclError> {
        with_stopped_world(|| self.minor_gc_stw_body())
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
            self.minor_gc_stop_the_world()?;
            // `old_occupancy_trigger` was a config knob wired to NOTHING: no
            // automatic major collection existed, so old-gen garbage accumulated
            // until evacuation ran out of regions mid-minor-GC and silently
            // dropped live objects — the heap corruption behind bliss-wc4t
            // ("unbound variable: CONS" after enough stress-mode collections).
            // Honor the knob: when the non-Free region fraction crosses it,
            // run a major collection to sweep old-gen garbage back to Free.
            Self::occupancy_exceeds_trigger()
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
        with_stopped_world(|| self.major_gc_stw_body())
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
    fn major_gc_stw_body(&mut self) -> Result<(), EgclError> {
        let start = std::time::Instant::now();

        // A standalone major collection must first drain nursery state so
        // nursery deaths trigger finalizers and weak-reference clearing too.
        self.minor_gc_stw_body()?;

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
        object_map.reset(heap_base_addr, region_size, region_count, &marked_regions);
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
        scan_external_roots(|slot| {
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
                            if type_id == crate::object::type_id::FUNCTION_INTERPRETED && body_size >= 32 {
                                let payload = unsafe { header_ptr.add(body_offset(header_ptr)) };
                                let name = unsafe { *(payload as *const EgclVal).add(3) };
                                if object_map.is_marked(body_addr) {
                                    live_functions.insert(name);
                                } else {
                                    dead_functions.insert(name);
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

        if let Some(sweep) = FUNCTION_CAPTURE_SWEEP_FN.get() {
            dead_functions.retain(|name| !live_functions.contains(name));
            sweep(&dead_functions.into_iter().collect::<Vec<_>>());
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
                !dead_set.contains(&unsafe { *slot }.to_raw())
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
        for (idx, region) in state.regions.iter().enumerate() {
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
                            .saturating_sub(region.header.alloc_top as usize) >= total_size
                    }) {
                        evacuation_target =
                            Self::allocate_target_region(state, RegionKind::OldGen, 0);
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
        Self::relocate_object_fields(state, heap_base_addr, heap_end);
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
        if is_heap_ref(new_val) {
            state.remembered.insert(slot_addr as usize);
        }
    }
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
    let guard = heap_state().lock().unwrap();
    let Some(state) = guard.as_ref() else {
        return;
    };
    let base = state.heap_base as usize;
    if body < base + OBJECT_HEADER_SIZE || body >= base + state.config.heap_size {
        return;
    }
    // Large-object regions never move, so pinning within one is a no-op (and its
    // header sits at a different offset — never touch it here).
    for region in &state.regions {
        let rb = region.base as usize;
        if body >= rb && body < rb + region.size {
            if region.header.kind == RegionKind::LargeObject {
                return;
            }
            break;
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

/// Invoke every registered external root scanner with `visit`.
fn scan_external_roots(mut visit: impl FnMut(*mut EgclVal)) {
    SCAN_PASS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scanners = root_scanners().lock().unwrap().clone();
    for s in scanners {
        s(&mut visit);
    }
}

/// Inspect host roots for application delivery, replacing registry scanners
/// with the caller's semantic dependency graph.
///
/// # Safety
/// Call only inside `with_heap_snapshot`, without allocating Lisp objects.
/// `excluded` must be covered by the delivery analyzer (including captures).
/// The callback must not mutate slots or let unrooted values escape the snapshot.
pub unsafe fn visit_delivery_host_roots(
    excluded: &[RootScanner],
    visit: &mut dyn FnMut(*mut EgclVal),
) {
    advance_root_scan_pass();
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
/// delivery handles symbol cells separately.
///
/// # Safety
/// `value` must be live, and the call must be inside `with_heap_snapshot`.
/// The callback must neither allocate Lisp objects nor mutate reference slots.
pub unsafe fn visit_delivery_references(value: EgclVal, visit: &mut dyn FnMut(EgclVal)) {
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
            | EgclError::UnboundVariable(datum)
            | EgclError::UndefinedFunction(datum) => datum.trace_host_roots(visit),
            // The already-signalled condition is a live EgclVal and must be
            // relocated with the moving GC (bliss-9kc).
            EgclError::Signalled { condition, .. } => condition.trace_host_roots(visit),
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

/// Default heap for a standalone T0 evaluator, with survivor/old-generation
/// reserve left outside the nursery so moving collections always have space.
fn t0_default_config() -> GcConfig {
    // `EGCL_HEAP_MB` overrides the total heap size (MiB); useful with
    // `EGCL_GC_DISABLE` to reserve enough headroom to run to completion without
    // ever collecting.
    let heap_size = std::env::var("EGCL_HEAP_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|mb| mb * 1024 * 1024)
        .unwrap_or(512 * 1024 * 1024);
    // `EGCL_GC_DISABLE` makes the whole heap a single nursery so a minor GC is
    // never triggered until the heap is exhausted — a diagnostic for isolating
    // GC/heap-corruption bugs (bliss-6b2): if a workload that intermittently
    // corrupts memory runs cleanly with the collector effectively off, the fault
    // is in the collector/write-barrier, not the mutator.
    let gc_disabled = std::env::var_os("EGCL_GC_DISABLE").is_some();
    let nursery_size = if gc_disabled {
        heap_size
    } else {
        64 * 1024 * 1024
    };
    GcConfig {
        heap_size,
        heap_max: heap_size,
        nursery_size,
        tlab_size: 256 * 1024,
        region_size: 1024 * 1024,
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
        let _ = init_heap(&t0_default_config());
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
    // Strided stressing, optionally skipping the first `SKIP` allocations.
    if stride != 0 && n > gc_stress_skip() && n % stride == 0 {
        let _ = collect_t0_minor();
    }
}

pub fn alloc_typed(body_size: usize, type_id: u8) -> Option<*mut u8> {
    if body_size == 0 {
        return None;
    }
    let _ = crate::thread::current_thread_id();
    crate::safepoint::poll_safepoint();
    maybe_gc_stress();
    T0_ALLOCATOR.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            ensure_heap_initialized();
        }
        let epoch = GC_MOVE_EPOCH.load(Ordering::Acquire);
        if guard.as_ref().is_none_or(|(seen, _)| *seen != epoch) {
            *guard = HeapAllocator::without_tlab()
                .ok()
                .map(|allocator| (epoch, allocator));
        }
        let body = {
            let alloc = &mut guard.as_mut()?.1;
            alloc
                .alloc_fast(body_size)
                .or_else(|| alloc.alloc_slow(body_size).ok())?
        };
        // The slow path may have collected and then refilled this allocator.
        // Publish its new epoch so the next allocation reuses that fresh TLAB.
        guard.as_mut()?.0 = GC_MOVE_EPOCH.load(Ordering::Acquire);
        // SAFETY: `body` is a freshly allocated object body of `body_size` bytes.
        unsafe { set_object_type_id(body, body_size, type_id) };
        Some(body)
    })
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
        .expect("GC heap unavailable for double-float");
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
        .expect("GC heap unavailable for character-string");
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
pub fn alloc_pinned_typed(body_size: usize, type_id: u8) -> Option<*mut u8> {
    if body_size == 0 {
        return None;
    }
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
            let body_off = unsafe { write_object_header(ptr, type_id, body_size as u32) };
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
            let body_off = unsafe { write_object_header(ptr, type_id, body_size as u32) };
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
    let mut target_idx = None;
    for &idx in state.pinned_hosts.iter() {
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
    }

    let idx = target_idx?;
    let region = &mut state.regions[idx];
    let ptr = region.header.alloc_top;
    let body_off = unsafe { write_object_header(ptr, type_id, body_size as u32) };
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

/// Trace off-heap captured lexical bindings owned by a live function object.
type FunctionCaptureTraceFn = fn(EgclVal, &mut dyn FnMut(*mut EgclVal));
static FUNCTION_CAPTURE_TRACE_FN: OnceLock<FunctionCaptureTraceFn> = OnceLock::new();

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
pub fn register_deferred_finalizer(
    object: EgclVal,
    finalizer: EgclVal,
) -> Result<(), EgclError> {
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
    /// Dead pinned function/symbol slots, recorded after major-GC reachability.
    /// Each slot remains a walkable filler until reused; live objects never move.
    free_pinned_slots: Vec<(usize, usize)>,
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
        return Err(EgclError::Internal(
            "nursery_size exceeds heap_size".into(),
        ));
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
        free_pinned_slots: Vec::new(),
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
    *heap_state().lock().unwrap() = Some(state);
    HEAP_IDENTITY_EPOCH.fetch_add(1, Ordering::Release);
    GC_MOVE_EPOCH.fetch_add(1, Ordering::Release);
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
    let region = &mut state.regions[idx];
    let header_ptr = region.header.alloc_top;
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
            if matches!(
                kind,
                tid::FOREIGN_POINTER
                    | tid::FOREIGN_LIBRARY
                    | tid::FOREIGN_CALLBACK
                    | tid::PYTHON_OBJECT
            ) {
                region.bytes[body_offset..body_offset + len].fill(0);
            } else if matches!(kind, tid::STREAM | tid::MUTEX | tid::CONDITION_VARIABLE) {
                region.bytes[body_offset..body_offset + len.min(8)].fill(0);
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
        // delivered snapshot is conservative and keeps serialized slots valid.
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
        // but restore it as unavailable, never as a dangling native pointer.
        if matches!(
            type_id,
            crate::object::type_id::FOREIGN_POINTER
                | crate::object::type_id::FOREIGN_LIBRARY
                | crate::object::type_id::FOREIGN_CALLBACK
                // A PyObject address names an object in an interpreter that does
                // not outlive this process, so it must be nulled rather than
                // carried: a restored proxy reports itself dead, which every
                // accessor already handles, instead of dereferencing an address
                // that now means something else entirely.
                | crate::object::type_id::PYTHON_OBJECT
        ) {
            // Neither native addresses nor allocation identities survive an
            // image restart. Restore pointers as null and library tokens as
            // invalid; a restored token must never name a new library.
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
    let required = occupied
        .checked_add(config.nursery_size)
        .ok_or_else(invalid)?;
    config.heap_size = config.heap_size.max(required);
    if config.heap_size > config.heap_max {
        return Err(EgclError::Oom);
    }
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
        let mut mapped = false;
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
    clear_heap_objects(state);
    // The pre-restore region set is gone: every cached per-thread T0 allocator
    // still holds a TLAB carved from it, and the reset region alloc_tops will
    // hand that same space out again — so a post-restore allocation from a stale
    // TLAB (e.g. the re-opened stdio streams) is silently overwritten by a later
    // one (bliss-64r1). Bump the move epoch so each thread lazily discards its
    // allocator and refills from the restored region state.
    GC_MOVE_EPOCH.fetch_add(1, Ordering::Release);

    // Pass 1: materialize every object, recording old-body → new-body. Pre-size
    // to a generous estimate of the object count (each record is a >=13-byte
    // header plus payload) so the per-object inserts never trigger a rehash
    // (bliss-pohq: the rehash storm dominated large restores).
    let mut map = ImageRelocations::with_object_capacity(data.len() / 24 + 16);
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
        let new_body =
            append_serialized_object(state, type_id, &data[offset..offset + size], &mut targets)?;
        map.insert_object(old_body, new_body);
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
    // Pass-2 pin/field-walk below iterates only `map` (real GC-heap objects).
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
                full_map.insert_object(old_body + OBJECT_HEADER_SIZE, new_body + OBJECT_HEADER_SIZE);
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
    for &new_body in map.object_targets() {
        let header = (new_body - OBJECT_HEADER_SIZE) as *const u8;
        unsafe {
            let hdr = (new_body - OBJECT_HEADER_SIZE) as *mut ObjectHeader;
            (*hdr).set_pinned();
        }
        let (_type_id, body_size) = unsafe { read_object_header(header) };
        let mut fo = 0usize;
        while fo + 8 <= body_size as usize {
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
        return Err(EgclError::InvalidImage(
            "relocation table too small".into(),
        ));
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
            bytes.extend_from_slice(&EgclVal::from_fixnum(i as i64).0.to_le_bytes());
            bytes.extend_from_slice(&tail.to_le_bytes());
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
            body[..8].copy_from_slice(&((size - 8) as u64).to_le_bytes());
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
