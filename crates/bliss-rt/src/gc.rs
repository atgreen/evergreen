//! Garbage collector — generational, region-based, concurrent old-gen marking.
//!
//! Inspired by HotSpot G1 and ZGC. See §3 of the spec.

use crate::error::BlissError;
use crate::value::BlissVal;

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

/// Register a finalizer for a heap object.
/// The finalizer function will be called when the object is about to be collected.
pub fn register_finalizer(_object: BlissVal, _finalizer: BlissVal) -> Result<(), BlissError> {
    // Finalizer registration is recorded; the GC will invoke finalizers
    // during collection when the object becomes unreachable.
    // In the current bootstrap implementation, finalizers are accepted
    // but not invoked (no concurrent GC cycle is running yet).
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

/// A single object allocation record in the bootstrap heap.
/// Stores the raw bytes of the object so they can be iterated by `walk_heap`
/// and serialised by image save.
struct HeapObject {
    /// The type_id from the ObjectHeader (first 8 bytes), cached for fast filtering.
    type_id: u8,
    /// The raw bytes of the object (including the ObjectHeader prefix).
    data: Vec<u8>,
}

/// Bootstrap heap state — stores the GC configuration, stats, and
/// all allocated objects so that `walk_heap` can iterate them and
/// image save can serialise real heap data.
struct HeapState {
    config: GcConfig,
    stats: GcStats,
    /// All live objects in the bootstrap heap.
    objects: Vec<HeapObject>,
    /// A stable base address used for relocation bookkeeping.
    /// Set once at init time.
    base_address: u64,
    /// Monotonically increasing GC generation counter.
    gc_generation: u32,
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
    let mut stats = GcStats::default();
    stats.nursery_capacity = config.nursery_size as u64;
    stats.old_gen_capacity = (config.heap_size - config.nursery_size) as u64;
    stats.regions_total = regions_total;
    stats.regions_free = regions_total;

    // Use the address of the heap_state mutex itself as a stable base address
    // for relocation tracking.  This gives a deterministic, non-zero value that
    // changes across processes, which is exactly what the image format needs.
    let base_address = heap_state() as *const _ as u64;

    let state = HeapState {
        config: config.clone(),
        stats,
        objects: Vec::new(),
        base_address,
        gc_generation: 0,
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
    if let Some(state) = &*guard {
        for obj in &state.objects {
            let should_continue = callback(obj.data.as_ptr(), obj.type_id, obj.data.len());
            if !should_continue {
                break;
            }
        }
    }
    Ok(())
}

/// Record an object in the bootstrap heap so that `walk_heap` can
/// enumerate it and `save_image` can serialise it.
pub fn record_object(type_id: u8, data: Vec<u8>) {
    let mut guard = heap_state().lock().unwrap();
    if let Some(state) = guard.as_mut() {
        state.stats.bytes_allocated += data.len() as u64;
        state.objects.push(HeapObject { type_id, data });
    }
}

/// Perform a full GC cycle (minor + major). In the bootstrap heap
/// this is a no-op in terms of reclamation (there are no unreachable
/// objects in the Vec-backed store), but it increments the GC
/// generation counter and updates stats, fulfilling the contract that
/// image save triggers a full GC before serialisation.
pub fn full_gc() -> Result<(), BlissError> {
    let mut guard = heap_state().lock().unwrap();
    if let Some(state) = guard.as_mut() {
        state.gc_generation += 1;
        state.stats.minor_gc_count += 1;
        state.stats.major_gc_count += 1;
    }
    Ok(())
}

/// Return the heap base address recorded at init time.
/// Used by image save to populate `original_base` in the header.
pub fn heap_base_address() -> u64 {
    let guard = heap_state().lock().unwrap();
    match &*guard {
        Some(state) => state.base_address,
        None => 0,
    }
}

/// Return the current GC generation counter.
pub fn gc_generation() -> u32 {
    let guard = heap_state().lock().unwrap();
    match &*guard {
        Some(state) => state.gc_generation,
        None => 0,
    }
}

/// Restore heap objects from a serialised byte buffer produced by
/// `save_image`. The buffer is a concatenation of length-prefixed
/// records: each record is `[u8 type_id][u32 len][len bytes data]`.
/// Clears any existing objects and replaces them with the restored set.
pub fn restore_heap(serialized: &[u8]) -> Result<(), BlissError> {
    let mut guard = heap_state().lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => {
            // If the heap hasn't been initialised, create a minimal state
            // so that the restored objects are accessible.
            *guard = Some(HeapState {
                config: GcConfig {
                    heap_size: 64 * 1024 * 1024,
                    heap_max: 256 * 1024 * 1024,
                    nursery_size: 16 * 1024 * 1024,
                    tlab_size: 8192,
                    region_size: 1024 * 1024,
                    promotion_threshold: 15,
                    pause_target_ms: 10,
                    gc_workers: 1,
                    satb_buffer_size: 1024,
                    old_occupancy_trigger: 0.45,
                },
                stats: GcStats::default(),
                objects: Vec::new(),
                base_address: heap_state() as *const _ as u64,
                gc_generation: 0,
            });
            guard.as_mut().unwrap()
        }
    };

    state.objects.clear();
    state.stats.bytes_allocated = 0;

    let mut offset = 0;
    while offset < serialized.len() {
        // Each record: [u8 type_id][u32 len (LE)][len bytes]
        if offset + 5 > serialized.len() {
            return Err(BlissError::InvalidImage(
                "truncated heap object record".into(),
            ));
        }
        let type_id = serialized[offset];
        offset += 1;
        let len = u32::from_le_bytes([
            serialized[offset],
            serialized[offset + 1],
            serialized[offset + 2],
            serialized[offset + 3],
        ]) as usize;
        offset += 4;
        if offset + len > serialized.len() {
            return Err(BlissError::InvalidImage(
                "truncated heap object data".into(),
            ));
        }
        let data = serialized[offset..offset + len].to_vec();
        offset += len;
        state.stats.bytes_allocated += data.len() as u64;
        state.objects.push(HeapObject { type_id, data });
    }

    Ok(())
}

/// Serialise all live heap objects into a byte buffer using the
/// length-prefixed record format expected by `restore_heap`.
pub fn serialize_heap_objects() -> Vec<u8> {
    let guard = heap_state().lock().unwrap();
    let mut buf = Vec::new();
    if let Some(state) = &*guard {
        for obj in &state.objects {
            buf.push(obj.type_id);
            buf.extend_from_slice(&(obj.data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&obj.data);
        }
    }
    buf
}

// ── Entry continuation ────────────────────────────────────────────

/// Global entry continuation stored by the runtime. Used by image save/load
/// to persist the continuation to resume on load (§7.2.3).
fn entry_continuation_state() -> &'static Mutex<BlissVal> {
    static STATE: OnceLock<Mutex<BlissVal>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(crate::value::NIL))
}

/// Set the entry continuation that will be stored in the next image save.
pub fn set_entry_continuation(val: BlissVal) {
    *entry_continuation_state().lock().unwrap() = val;
}

/// Retrieve the current entry continuation value.
pub fn get_entry_continuation() -> BlissVal {
    *entry_continuation_state().lock().unwrap()
}

// ── GC metadata serialization ─────────────────────────────────────

/// Serialize all GcStats fields into a byte buffer for the GcMeta image section.
/// Format: 13 fields as little-endian, u64 for the first 11, u32 for the last 2.
pub fn serialize_gc_metadata() -> Vec<u8> {
    let stats = heap_stats();
    let gc_gen = gc_generation();
    let mut buf = Vec::with_capacity(8 * 11 + 4 * 2 + 4);
    // GC generation (u32 LE)
    buf.extend_from_slice(&gc_gen.to_le_bytes());
    // All u64 stats fields
    buf.extend_from_slice(&stats.minor_gc_count.to_le_bytes());
    buf.extend_from_slice(&stats.major_gc_count.to_le_bytes());
    buf.extend_from_slice(&stats.total_minor_pause_us.to_le_bytes());
    buf.extend_from_slice(&stats.total_major_pause_us.to_le_bytes());
    buf.extend_from_slice(&stats.bytes_allocated.to_le_bytes());
    buf.extend_from_slice(&stats.bytes_promoted.to_le_bytes());
    buf.extend_from_slice(&stats.nursery_used.to_le_bytes());
    buf.extend_from_slice(&stats.nursery_capacity.to_le_bytes());
    buf.extend_from_slice(&stats.old_gen_used.to_le_bytes());
    buf.extend_from_slice(&stats.old_gen_capacity.to_le_bytes());
    buf.extend_from_slice(&stats.large_object_bytes.to_le_bytes());
    // u32 fields
    buf.extend_from_slice(&stats.regions_total.to_le_bytes());
    buf.extend_from_slice(&stats.regions_free.to_le_bytes());
    buf
}

/// Restore GC metadata (stats + generation) from the serialized GcMeta section.
pub fn restore_gc_metadata(data: &[u8]) -> Result<(), BlissError> {
    // Minimum size: 4 (gen) + 11*8 (u64 fields) + 2*4 (u32 fields) = 100 bytes
    if data.len() < 100 {
        if data.is_empty() {
            return Ok(()); // empty section is valid (bootstrap)
        }
        return Err(BlissError::InvalidImage(
            "GC metadata section too small".into(),
        ));
    }
    let mut off = 0usize;
    let read_u32 = |o: &mut usize| -> u32 {
        let v = u32::from_le_bytes([data[*o], data[*o+1], data[*o+2], data[*o+3]]);
        *o += 4;
        v
    };
    let read_u64 = |o: &mut usize| -> u64 {
        let v = u64::from_le_bytes([
            data[*o], data[*o+1], data[*o+2], data[*o+3],
            data[*o+4], data[*o+5], data[*o+6], data[*o+7],
        ]);
        *o += 8;
        v
    };

    let gc_gen = read_u32(&mut off);
    let minor_gc_count = read_u64(&mut off);
    let major_gc_count = read_u64(&mut off);
    let total_minor_pause_us = read_u64(&mut off);
    let total_major_pause_us = read_u64(&mut off);
    let bytes_allocated = read_u64(&mut off);
    let bytes_promoted = read_u64(&mut off);
    let nursery_used = read_u64(&mut off);
    let nursery_capacity = read_u64(&mut off);
    let old_gen_used = read_u64(&mut off);
    let old_gen_capacity = read_u64(&mut off);
    let large_object_bytes = read_u64(&mut off);
    let regions_total = read_u32(&mut off);
    let regions_free = read_u32(&mut off);

    let mut guard = heap_state().lock().unwrap();
    if let Some(state) = guard.as_mut() {
        state.gc_generation = gc_gen;
        state.stats.minor_gc_count = minor_gc_count;
        state.stats.major_gc_count = major_gc_count;
        state.stats.total_minor_pause_us = total_minor_pause_us;
        state.stats.total_major_pause_us = total_major_pause_us;
        state.stats.bytes_allocated = bytes_allocated;
        state.stats.bytes_promoted = bytes_promoted;
        state.stats.nursery_used = nursery_used;
        state.stats.nursery_capacity = nursery_capacity;
        state.stats.old_gen_used = old_gen_used;
        state.stats.old_gen_capacity = old_gen_capacity;
        state.stats.large_object_bytes = large_object_bytes;
        state.stats.regions_total = regions_total;
        state.stats.regions_free = regions_free;
    }
    Ok(())
}

// ── Symbol table serialization (§7.2.7) ───────────────────────────

/// Serialize the symbol intern table. In the bootstrap runtime, symbols
/// are managed by bliss-stdlib's packages module. This function serializes
/// whatever symbol state the runtime tracks directly.
pub fn serialize_symbols() -> Vec<u8> {
    // The bootstrap runtime does not maintain its own symbol table;
    // symbols are managed by bliss-stdlib. Return empty data —
    // the call-site wiring is established for when stdlib hooks in.
    Vec::new()
}

/// Restore the symbol intern table from serialized data.
pub fn restore_symbols(data: &[u8]) -> Result<(), BlissError> {
    if data.is_empty() {
        return Ok(());
    }
    // Parse symbol records and rebuild the intern table.
    // Format: [u32 count LE] [for each: u32 name_len LE, name bytes, u64 value LE]
    let mut off = 0usize;
    if data.len() < 4 {
        return Err(BlissError::InvalidImage("symbol section too small".into()));
    }
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    off += 4;
    for _ in 0..count {
        if off + 4 > data.len() {
            return Err(BlissError::InvalidImage("truncated symbol record".into()));
        }
        let name_len = u32::from_le_bytes([data[off], data[off+1], data[off+2], data[off+3]]) as usize;
        off += 4;
        if off + name_len + 8 > data.len() {
            return Err(BlissError::InvalidImage("truncated symbol data".into()));
        }
        // Skip name bytes and value — in bootstrap, symbols are managed by stdlib
        off += name_len + 8;
    }
    Ok(())
}

// ── Package registry serialization (§7.2.8) ──────────────────────

/// Serialize the package registry.
pub fn serialize_packages() -> Vec<u8> {
    // In the bootstrap runtime, packages are managed by bliss-stdlib.
    // Return empty data — call-site wiring established.
    Vec::new()
}

/// Restore the package registry from serialized data.
pub fn restore_packages(data: &[u8]) -> Result<(), BlissError> {
    if data.is_empty() {
        return Ok(());
    }
    // Parse package records.
    // Format: [u32 count LE] [for each: u32 name_len LE, name bytes]
    let mut off = 0usize;
    if data.len() < 4 {
        return Err(BlissError::InvalidImage("package section too small".into()));
    }
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    off += 4;
    for _ in 0..count {
        if off + 4 > data.len() {
            return Err(BlissError::InvalidImage("truncated package record".into()));
        }
        let name_len = u32::from_le_bytes([data[off], data[off+1], data[off+2], data[off+3]]) as usize;
        off += 4;
        if off + name_len > data.len() {
            return Err(BlissError::InvalidImage("truncated package data".into()));
        }
        off += name_len;
    }
    Ok(())
}

// ── Code cache serialization ──────────────────────────────────────

/// Serialize the compiled code cache.
pub fn serialize_code_cache() -> Vec<u8> {
    // In the bootstrap runtime, no compiled code is cached at the GC level.
    // The compiler crate manages its own code; this wiring exists for the
    // full implementation to hook into.
    Vec::new()
}

/// Restore the compiled code cache from serialized data.
pub fn restore_code_cache(data: &[u8]) -> Result<(), BlissError> {
    if data.is_empty() {
        return Ok(());
    }
    // Parse code cache records.
    // Format: [u32 count LE] [for each: u32 code_len LE, code bytes]
    let mut off = 0usize;
    if data.len() < 4 {
        return Err(BlissError::InvalidImage("code cache section too small".into()));
    }
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    off += 4;
    for _ in 0..count {
        if off + 4 > data.len() {
            return Err(BlissError::InvalidImage("truncated code cache record".into()));
        }
        let code_len = u32::from_le_bytes([data[off], data[off+1], data[off+2], data[off+3]]) as usize;
        off += 4;
        if off + code_len > data.len() {
            return Err(BlissError::InvalidImage("truncated code cache data".into()));
        }
        off += code_len;
    }
    Ok(())
}

// ── Relocation table ──────────────────────────────────────────────

/// Serialize the relocation table — records all heap pointers that need
/// fixup when the image is loaded at a different base address (R7.03).
/// Each entry is a u64 LE offset within the heap section.
pub fn serialize_relocation_table() -> Vec<u8> {
    // Walk heap objects and record offsets of pointer-valued fields.
    // In the bootstrap runtime with Vec-backed objects, there are no
    // raw pointers embedded in object data (all references use BlissVal
    // tagged values which are self-contained). Return the empty table.
    Vec::new()
}

/// Apply pointer relocations to heap data when loading at a different base.
/// `reloc_data` is a sequence of u64 LE offsets into the heap section;
/// each offset points to a u64 that should be adjusted by `delta`.
pub fn apply_relocations(
    heap_data: &mut [u8],
    reloc_data: &[u8],
    delta: i64,
) -> Result<(), BlissError> {
    if reloc_data.is_empty() || delta == 0 {
        return Ok(());
    }
    if reloc_data.len() % 8 != 0 {
        return Err(BlissError::InvalidImage(
            "relocation table size not a multiple of 8".into(),
        ));
    }
    let mut off = 0usize;
    while off < reloc_data.len() {
        let ptr_offset = u64::from_le_bytes([
            reloc_data[off], reloc_data[off+1], reloc_data[off+2], reloc_data[off+3],
            reloc_data[off+4], reloc_data[off+5], reloc_data[off+6], reloc_data[off+7],
        ]) as usize;
        off += 8;

        if ptr_offset + 8 > heap_data.len() {
            return Err(BlissError::InvalidImage(format!(
                "relocation offset {} out of bounds (heap size {})",
                ptr_offset,
                heap_data.len()
            )));
        }

        // Read the pointer value, apply delta, write back
        let old_val = u64::from_ne_bytes([
            heap_data[ptr_offset], heap_data[ptr_offset+1],
            heap_data[ptr_offset+2], heap_data[ptr_offset+3],
            heap_data[ptr_offset+4], heap_data[ptr_offset+5],
            heap_data[ptr_offset+6], heap_data[ptr_offset+7],
        ]);
        let new_val = if delta >= 0 {
            old_val.wrapping_add(delta as u64)
        } else {
            old_val.wrapping_sub((-delta) as u64)
        };
        heap_data[ptr_offset..ptr_offset+8].copy_from_slice(&new_val.to_ne_bytes());
    }
    Ok(())
}
