# §3  Memory Management & Garbage Collection

EGCL uses a generational, region-based garbage collector inspired by
HotSpot's G1 and ZGC. The design targets sub-10 ms GC pauses for heaps
up to 32 GB, concurrent old-generation marking, and per-thread
bump-pointer allocation for zero-contention fast paths. This chapter
specifies heap layout, allocation, minor and major GC algorithms, write
barriers, remembered sets, finalization, safepoint protocol, and tuning
knobs.

---

## 3.1  Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R3.01 | The heap MUST be divided into nursery (young-gen) and old-gen regions. | MUST |
| R3.02 | Each mutator thread MUST own a Thread-Local Allocation Buffer (TLAB) of configurable size (default ~2 MB). | MUST |
| R3.03 | Allocation within a TLAB MUST use a bump-pointer and require no locks or atomic operations. | MUST |
| R3.04 | When a TLAB is exhausted the allocator MUST refill from the nursery or fall back to old-gen (§3.3.2). | MUST |
| R3.05 | Objects larger than half a region (default >1 MB) MUST be allocated directly in dedicated large-object regions in old-gen. | MUST |
| R3.06 | Minor GC MUST be stop-the-world and copy live nursery objects into survivor space or promote to old-gen. | MUST |
| R3.07 | Minor GC SHOULD complete in < 2 ms for a 64 MB nursery on typical workloads. | SHOULD |
| R3.08 | Old-gen marking MUST be concurrent with mutator execution. | MUST |
| R3.09 | Old-gen marking MUST use a tri-colour incremental algorithm with a Yuasa snapshot-at-the-beginning (SATB) write barrier. | MUST |
| R3.10 | Old-gen compaction MUST use region evacuation: select regions with highest garbage ratio and copy live objects to fresh regions. | MUST |
| R3.11 | Cross-generation pointers MUST be tracked via a remembered set so minor GC does not scan old-gen. | MUST |
| R3.12 | Finalization MUST be deferred: objects with finalizers are enqueued on a finalization queue after becoming unreachable, and finalizers run on a dedicated thread. | MUST |
| R3.13 | The runtime MUST support weak references (weak pointers) that are cleared atomically during GC. | MUST |
| R3.14 | All GC activity MUST respect safepoints; mutator threads MUST NOT be suspended via asynchronous signals (e.g. `SIGUSR1`). Safepoint polling MAY use a synchronous page-fault trap (`mprotect` / `SIGSEGV`) initiated by the thread itself. | MUST |
| R3.15 | Heap size, nursery size, pause-time target, and promotion threshold MUST be configurable at startup. | MUST |
| R3.16 | The GC MUST maintain a consistent heap even if a finalizer signals a CL condition; finalizer errors MUST NOT corrupt GC state. | MUST |
| R3.17 | GC marking state (mark-white/black, mark-grey) MUST be tracked in side tables (the global mark bitmap, §3.4), not in object headers, to allow concurrent marking without racing with mutator field writes.  Structural GC bits that are only written by stop-the-world phases (forwarded, pinned, remembered-set) MAY reside in the `ObjectHeader.gc_bits` field (D1.02, §1.3.1). | MUST |
| R3.18 | The GC MUST support heap walking for image serialisation (§7) and debugging (§6). | MUST |
| R3.19 | Large-object regions MUST NOT be moved; they are freed in bulk when unmarked. | MUST |
| R3.20 | The runtime SHOULD provide GC statistics (pause times, bytes promoted, regions evacuated) via an introspection API (§6). | SHOULD |

---

## 3.2  Heap Layout

### 3.2.1  Region Model

The heap is a contiguous (or virtually contiguous via `mmap`) arena
divided into fixed-size **regions**.

| Parameter | Default | Notes |
|-----------|---------|-------|
| Region size | 2 MB | Power-of-two, aligned; configurable 1–32 MB |
| Max regions | 16 384 | → 32 GB max heap at 2 MB regions |
| Region alignment | Region size | Enables fast region-index lookup via shift |

**D3.01 — RegionHeader** (one per region, stored in a side array):

```rust
#[repr(C)]
struct RegionHeader {
    kind: RegionKind,          // Nursery | Survivor | OldGen | LargeObject | Free
    gen_age: u8,               // 0 = nursery, 1..N = survivor age
    live_bytes: AtomicU32,     // updated during marking
    alloc_top: AtomicPtr<u8>,  // bump pointer (nursery/survivor only)
    alloc_limit: *const u8,    // end of allocatable area
    next_free: AtomicU32,      // index of next region in free-list (Free regions)
    mark_bitmap_offset: u32,   // byte offset into global mark-bitmap
}

enum RegionKind {
    Free,
    Nursery,
    Survivor,
    OldGen,
    LargeObject,
}
```

### 3.2.2  Nursery

The nursery is a pool of regions (default: 32 regions = 64 MB) reserved
exclusively for young allocation. Each mutator thread carves out a TLAB
from a nursery region.

**D3.02 — TLAB (Thread-Local Allocation Buffer):**

```rust
struct Tlab {
    cursor: *mut u8,   // current bump-pointer
    limit:  *const u8, // end of this TLAB slice
    region_idx: u16,   // owning nursery region index
}
```

- Default TLAB size: 2 MB (one full region) but may be a sub-region
  slice when regions are shared under high thread counts.
- When `cursor == limit`, the thread requests a new TLAB (§3.3.2).

### 3.2.3  Survivor Spaces

Survivor space is composed of regions drawn from the global region pool,
tagged `Survivor`. Two logical semi-spaces (from-space/to-space) are
maintained, each targeting ~25 % of nursery capacity. At the default
configuration (nursery = 32 regions = 64 MB), each semi-space comprises
8 regions (16 MB). Regions are added or released dynamically as survivor
occupancy fluctuates; the 25 % target is a soft guideline, not a hard
limit.

Objects surviving a minor GC are copied into to-space regions with
incremented `gen_age`. When `gen_age ≥ promotion_threshold` (default 4),
the object is promoted to old-gen instead.

### 3.2.4  Old Generation

Remaining regions form old-gen. Allocation within old-gen uses a
free-list of regions; within a region, allocation is bump-pointer.
Old-gen regions track `live_bytes` to enable evacuation selection.

### 3.2.5  Large-Object Regions

Objects exceeding `region_size / 2` are allocated in one or more
contiguous regions tagged `LargeObject`. These regions:

- Are allocated directly in old-gen (R3.05).
- Are never moved (R3.19).
- Are freed atomically when the contained object is unmarked after a
  major GC cycle.

---

## 3.3  Allocation

### 3.3.1  Fast Path (TLAB Bump-Pointer)

The overwhelmingly common allocation path is a single pointer bump with
no synchronisation:

```rust
// Inlined by JIT at every allocation site.
fn alloc_fast(tlab: &mut Tlab, size: usize) -> Option<*mut u8> {
    let aligned = align_up(size, 16);   // 16-byte alignment
    let new_cursor = tlab.cursor.wrapping_add(aligned);
    if new_cursor <= tlab.limit {
        let ptr = tlab.cursor;
        tlab.cursor = new_cursor;
        Some(ptr)
    } else {
        None  // → slow path
    }
}
```

On x86-64 this compiles to ~5 instructions (load, add, compare, store,
branch). R3.03 is satisfied: no locks, no atomics.

### 3.3.2  Slow Path

When `alloc_fast` returns `None`:

1. **TLAB refill:** Request a new TLAB from the nursery region pool
   (atomic bump of the region's `alloc_top`). If the current nursery
   region is full, acquire a fresh `Free` region and convert it to
   `Nursery`.
2. **Nursery exhaustion:** If no free regions remain for nursery, trigger
   a minor GC (§3.5), then retry.
3. **Old-gen fallback:** If allocation is for a large object (R3.05) or
   if the nursery is completely full after GC, allocate directly from
   old-gen.
4. **Out-of-memory:** If old-gen is also full, signal a CL
   `STORAGE-CONDITION` (§5, §8).

### 3.3.3  Object Initialization

After bump-pointer allocation, the runtime writes:

1. The **object header** (see §1 D1.02) at the start of the allocated
   block.
2. Zero-initialises payload fields (MUST, to prevent the GC from
   tracing uninitialised pointers).

---

## 3.4  Mark Bitmap & Side Metadata

Marking state is stored in a **global mark bitmap**, one bit per 16-byte
granule (the minimum object alignment). For a 32 GB heap this bitmap is
256 MB — allocated lazily via `mmap` with `MAP_NORESERVE`.

```text
bit_index = (object_addr - heap_base) / 16
byte_index = bit_index / 8
bit_offset = bit_index % 8
```

A second bitmap, the **TAMS (Top-at-Mark-Start)** pointer per region,
records the `alloc_top` at the start of a concurrent mark cycle. Objects
allocated above TAMS are implicitly live (allocated during marking).

---

## 3.5  Minor GC — Nursery Collection

Minor GC is stop-the-world and uses a parallel scavenge (Cheney-style
breadth-first copying).

### A3.01 — Minor GC Algorithm

```text
ALGORITHM minor_gc():
  // Phase 1: Stop the world
  request_safepoint(ALL_THREADS)          // §3.9
  wait_until_all_threads_at_safepoint()

  // Phase 2: Identify roots
  roots ← scan_thread_stacks()           // stack slots, register spills
         ∪ scan_remembered_set()          // old→young pointers (§3.7)
         ∪ scan_global_roots()            // symbol value cells, specials

  // Phase 3: Parallel scavenge
  work_queues ← partition(roots, N_GC_WORKERS)
  parallel_for worker in 1..N_GC_WORKERS:
    while item ← work_queues[worker].pop():
      if item.addr in nursery_or_survivor_from_space:
        if already_forwarded(item):
          update_reference(item, forwarding_addr(item))
        else:
          dest ← choose_destination(item)   // survivor-to or old-gen
          copy_object(item, dest)
          install_forwarding_ptr(item, dest)
          update_reference(item, dest)
          scan_fields(dest, work_queues[worker])  // enqueue children

  // Phase 4: Reclaim
  for region in nursery_regions ∪ survivor_from_space:
    region.kind ← Free
    region.alloc_top ← region.base
  swap(survivor_from_space, survivor_to_space)

  // Phase 5: Refill TLABs and resume
  for thread in all_mutator_threads:
    thread.tlab ← allocate_new_tlab()
  resume_all_threads()
```

**`choose_destination(obj)`:**

```text
if obj.gen_age ≥ PROMOTION_THRESHOLD:
  allocate_in(old_gen)          // promote
  add_to_remembered_set_if_needed(obj)
else:
  allocate_in(survivor_to_space)
  obj.gen_age += 1
```

---

## 3.6  Old-Gen Concurrent Marking

Old-gen uses a tri-colour incremental mark with Yuasa SATB write
barriers, closely following the SATB approach used in G1.

### 3.6.1  Tri-Colour Invariant

- **White:** Not yet visited — potentially garbage.
- **Grey:** Visited but children not yet scanned.
- **Black:** Visited and all children scanned.

The strong tri-colour invariant is maintained: a black object MUST NOT
point directly to a white object. The SATB write barrier ensures this by
recording the old value of any overwritten pointer.

### 3.6.2  SATB Write Barrier

Whenever a mutator overwrites a reference field, the **old** value is
logged to a per-thread SATB buffer:

```rust
// Inserted by compiler at every reference store (§4).
#[inline(always)]
fn satb_write_barrier(slot: *mut EgclVal, old_val: EgclVal) {
    if gc_marking_in_progress() {
        if is_heap_pointer(old_val) {
            let buf = current_thread().satb_buffer;
            buf.push(old_val);
            if buf.is_full() {
                flush_satb_buffer(buf); // enqueue for GC threads
            }
        }
    }
}
```

The `gc_marking_in_progress()` check reads a global flag (a single byte
in a cache-friendly location) — cost: one load + test-and-branch on the
fast path when GC is idle.

### A3.02 — Concurrent Mark Algorithm

```text
ALGORITHM concurrent_mark():
  // -- Initialisation (STW pause ~0.5 ms) --
  request_safepoint(ALL_THREADS)
  set gc_marking_in_progress ← true
  for each region R in old_gen:
    R.tams ← R.alloc_top          // record Top-at-Mark-Start
  initial_roots ← scan_thread_stacks() ∪ scan_global_roots()
  mark_grey(initial_roots)
  resume_all_threads()

  // -- Concurrent tracing (mutators running) --
  while grey_set is not empty:
    obj ← grey_set.pop()
    for each reference field F in obj:
      child ← load(F)
      if child is heap_ptr AND not marked(child):
        mark_grey(child)
    mark_black(obj)

    // Periodically drain SATB buffers from mutators
    if satb_queue.has_pending():
      for val in satb_queue.drain():
        if not marked(val):
          mark_grey(val)

  // -- Remark (STW pause ~1 ms) --
  request_safepoint(ALL_THREADS)
  drain all remaining SATB buffers
  scan_thread_stacks() for new roots → mark transitively
  set gc_marking_in_progress ← false
  resume_all_threads()

  // -- Cleanup --
  compute_live_bytes(each old_gen region)
  update_region_headers()
  build_collection_set()          // → §3.8 evacuation
```

### 3.6.3  Mark Termination Protocol

GC worker threads use a **termination protocol** (work-stealing with
termination detection): each worker attempts to steal from other
workers' queues before declaring itself idle. All workers idle + empty
SATB queue = mark phase complete.

---

## 3.6.4  Minor GC During Concurrent Marking

A minor GC (§3.5) may be triggered while a concurrent old-gen mark cycle
(§3.6) is in progress. The two must interact correctly:

1. **Concurrent marker is paused during minor GC.** Minor GC is STW, so
   all threads — including GC marker threads — are at a safepoint. The
   marker resumes when mutators resume.

2. **Promoted objects are implicitly marked.** Objects promoted from
   nursery/survivor to old-gen during a minor GC are allocated above the
   region's TAMS pointer (or in fresh regions whose TAMS is their base).
   Per the TAMS rule (§3.4), objects above TAMS are implicitly live —
   they need not be explicitly grey-marked. This ensures promoted
   objects are not incorrectly reclaimed by the in-progress marking
   cycle.

3. **Minor GC updates remembered sets consistently.** When minor GC
   copies objects and updates references, any old→young references that
   become old→old (because the young object was promoted) are reflected
   in card-table state. Cards containing promoted-to regions are not
   dirtied unless they contain genuine old→young pointers post-GC.

4. **SATB buffers are flushed.** As part of the minor GC STW pause, all
   per-thread SATB buffers are flushed to the global SATB queue. This
   ensures that reference overwrites performed by the minor GC's
   forwarding-pointer installation are captured by the concurrent
   marker when it resumes.

This interaction mirrors HotSpot G1's approach: young GCs are
orthogonal to concurrent marking and can occur freely during any phase
of the old-gen mark cycle.

---

## 3.7  Remembered Sets

Cross-generation pointers (old→young) must be tracked so minor GC can
find roots without scanning all of old-gen.

### 3.7.1  Card Table

A byte array covering old-gen, one byte per 512-byte card:

```text
card_index = (addr - old_gen_base) / 512
```

| Card state | Value | Meaning |
|------------|-------|---------|
| Clean | `0x00` | No old→young pointers in this card |
| Dirty | `0xFF` | Card may contain old→young pointer; must be scanned |

**Combined write barrier:** Both the SATB barrier (§3.6.2) and the card
barrier fire on reference stores. To avoid two separate conditional
branches per store, the compiler emits a single combined barrier
sequence:

```rust
/// Emitted by the JIT at every reference-store site.
/// Single function, two fast-path checks, to minimise branch overhead.
#[inline(always)]
fn combined_write_barrier(
    slot_addr: *mut EgclVal,
    old_val: EgclVal,
    new_val: EgclVal,
) {
    // 1. SATB barrier (only when concurrent marking is active)
    if gc_marking_in_progress() {
        if is_heap_pointer(old_val) {
            let buf = current_thread().satb_buffer;
            buf.push(old_val);
            if buf.is_full() {
                flush_satb_buffer(buf);
            }
        }
    }

    // 2. Card barrier (unconditional — independent of marking state)
    if is_young_pointer(new_val) && is_old_gen(slot_addr) {
        let idx = (slot_addr as usize - OLD_GEN_BASE) / CARD_SIZE;
        CARD_TABLE[idx] = DIRTY;
    }
}
```

When marking is **not** in progress, the SATB branch is a single
load + test-and-branch that falls through, leaving only the card check
on the fast path. When marking **is** active, the combined cost is two
conditional branches plus (rarely) the SATB buffer push. This is
comparable to HotSpot G1's post-write barrier overhead.

At minor GC, dirty cards are scanned to discover old→young references
and add them to the root set (A3.01 step 2). After scanning, cards are
reset to clean.

### 3.7.2  Per-Region Remembered Sets (Future Optimisation)

For large heaps (>8 GB) the card table may be replaced by per-region
remembered sets (hash sets of slot addresses) to reduce scanning cost.
This is a MAY-level optimisation for v2.

---

## 3.8  Old-Gen Compaction — Region Evacuation

After concurrent marking, old-gen regions with low `live_bytes` are
selected for evacuation (G1-style mixed collection).

### 3.8.1  Collection Set Selection

```text
ALGORITHM select_collection_set(pause_target_ms):
  candidates ← old_gen_regions
    .filter(r → r.live_bytes < r.size * 0.65)
    .sort_by(r → r.live_bytes ASC)   // most garbage first

  collection_set ← []
  estimated_pause ← 0
  for r in candidates:
    cost ← estimate_evac_cost(r)     // ~proportional to live_bytes
    if estimated_pause + cost ≤ pause_target_ms:
      collection_set.append(r)
      estimated_pause += cost
    else:
      break
  return collection_set
```

### 3.8.2  Evacuation (STW)

Evacuation is stop-the-world (may be parallelised across GC workers):

1. For each region in collection set, copy live objects to fresh
   old-gen regions.
2. Update all references (stack roots, other heap objects via remembered
   set entries) to point to new locations.
3. Free evacuated regions.

Future: incremental/concurrent evacuation with read barriers (ZGC-style)
is a MAY for v2+.

---

## 3.9  Safepoint Protocol

Mutator threads must reach a **safepoint** before GC can proceed. EGCL
uses a polling-based safepoint mechanism (R3.14 — no asynchronous
signal-based suspension). The mechanism uses a synchronous page-fault
trap: the GC thread `mprotect`s a polling page, causing the mutator
thread itself to fault into a handler that parks the thread. This is a
self-initiated synchronous trap, not an externally-delivered async
signal like `SIGUSR1`.

### 3.9.1  Safepoint Polls

The compiler inserts safepoint polls at:

- Function prologues (every function entry).
- Loop back-edges (every iteration of `LOOP`, `DO`, `DOTIMES`, etc.).
- Allocation slow paths (automatically, before requesting a new TLAB).

A safepoint poll reads a single **safepoint page**:

```rust
// Inserted by JIT; compiles to one load instruction.
fn safepoint_poll() {
    unsafe { core::ptr::read_volatile(SAFEPOINT_PAGE as *const u8); }
}
```

To trigger a safepoint, the GC thread `mprotect`s the safepoint page as
`PROT_NONE`, causing any poll to fault into a signal handler that parks
the thread. This is one-instruction overhead on the fast path
(no branch, no test).

### 3.9.2  Time-to-Safepoint Guarantee

Because polls exist at every back-edge and function entry, the maximum
time-to-safepoint is bounded by the longest straight-line code between
polls. The compiler MUST ensure no basic-block sequence between two
safepoint polls exceeds an estimated 100 µs of execution (R3.14).

### 3.9.3  Safepoint State

At a safepoint, each thread's state is fully walkable:

- Stack frames enumerable via frame pointer chain.
- Live reference set known via stack maps (§4) emitted by the compiler.
- Register-spilled references stored in known spill slots.

---

## 3.10  Finalization & Weak References

### 3.10.1  Finalizers

- Objects with finalizers (registered via `sb-ext:finalize`-compatible
  API, §9) are tracked in a global **finalization registry**.
- During marking, finalisable objects found unreachable are moved to a
  **finalization queue** rather than reclaimed immediately (R3.12).
- A dedicated **finalizer thread** drains the queue, calling each
  finalizer in an unspecified order.
- Finalizer execution MUST NOT block GC; the finalizer thread runs
  concurrently with mutators.
- Errors in finalizers are caught and logged; they MUST NOT corrupt GC
  state (R3.16). A `HANDLER-BIND` wrapping each finalizer call converts
  unhandled conditions to warnings.

### 3.10.2  Weak Pointers

**D3.03 — WeakPointer:**

```rust
struct WeakPointer {
    header: ObjectHeader,
    referent: AtomicU64,   // holds a EgclVal (tagged 64-bit value); cleared to NIL when referent dies
    broken: AtomicBool,
}
```

- During the remark STW pause, the GC scans the global weak-pointer
  list. Any `WeakPointer` whose referent is unmarked has its `referent`
  set to `NIL` and `broken` set to `true` (R3.13).
- `WEAK-POINTER-VALUE` returns `(values referent broken-p)`.

### 3.10.3  Weak Hash Tables

Hash tables created with `:weakness :key`, `:value`, or `:key-and-value`
use the same mechanism: during remark, entries whose weak side is
unmarked are removed.

---

## 3.11  Concurrency Contracts

| Concern | Contract |
|---------|----------|
| TLAB allocation | Single-threaded per TLAB; no synchronisation required. |
| Nursery region pool | Lock-free CAS on `next_free` region index. |
| SATB buffers | Per-thread buffer; flushed to global queue under lock. |
| Card table | Unsynchronised byte writes (benign races: worst case = redundant scanning). |
| Mark bitmap | Atomic bit-set (compare-and-swap per word). |
| Region headers | `live_bytes` updated with `fetch_add`; `kind` written only under STW. |
| Safepoint page | Modified only by the GC controller thread via `mprotect`. |
| Finalization queue | Lock-free MPSC queue (mutators enqueue, finalizer thread dequeues). |

---

## 3.12  Error Handling

| Failure | Response |
|---------|----------|
| Nursery exhaustion | Trigger minor GC; if still insufficient, promote to old-gen. |
| Old-gen exhaustion | Trigger concurrent mark + evacuate; if still insufficient, signal `STORAGE-CONDITION`. |
| `mmap` failure (heap expansion) | Signal `STORAGE-CONDITION` with `:REASON :MMAP-FAILED`. |
| Finalizer error | Catch, log warning, continue to next finalizer (R3.16). |
| Stack overflow during GC | GC threads use pre-allocated, fixed-size stacks; overflow → abort with diagnostic. |
| Corrupted forwarding pointer | Debug-mode assertion; release-mode: undefined (programming error). |

---

## 3.13  Configuration & Tuning

| Parameter | Flag / Env Var | Default | Description |
|-----------|---------------|---------|-------------|
| Initial heap | `--heap-size` | 256 MB | Initial `mmap` reservation |
| Max heap | `--heap-max` | 8 GB | Hard limit; `STORAGE-CONDITION` if exceeded |
| Nursery size | `--nursery-size` | 64 MB | Total nursery region pool |
| TLAB size | `--tlab-size` | 2 MB | Per-thread allocation buffer |
| Region size | `--region-size` | 2 MB | Must be power of two, 1–32 MB |
| Promotion threshold | `--promo-threshold` | 4 | Survivor generations before promotion |
| Pause target | `--gc-pause-target` | 10 ms | Target max STW pause for evacuation |
| GC workers | `--gc-workers` | `num_cpus / 2` | Parallel GC threads |
| Large-object threshold | (derived) | `region_size / 2` | Objects above this go to large-object regions |
| SATB buffer size | `--satb-buf-size` | 1024 entries | Per-thread SATB buffer capacity |
| Card size | (compile-time) | 512 bytes | Granularity of card table |
| Concurrent mark auto-trigger | `--gc-old-occupancy` | 45 % | Old-gen occupancy threshold to start marking |

---

## 3.14  Test Strategy

| Test | Method | Acceptance |
|------|--------|------------|
| TLAB allocation throughput | Microbenchmark: single-thread alloc loop | ≥ 1 GB/s allocation rate |
| Minor GC correctness | Allocate linked structures, trigger minor GC, verify graph integrity | Zero corruption across 10⁶ cycles |
| Promotion correctness | Age objects through survivor spaces, verify promotion at threshold | Objects in old-gen after N minor GCs |
| Concurrent mark correctness | Concurrent mutator stress test (producer/consumer) during marking | Heap walk after mark finds no dangling pointers |
| SATB barrier correctness | Inject pointer mutations during mark, verify SATB buffer contents | All pre-values captured |
| Evacuation correctness | Fill old-gen with fragmented data, trigger evacuation, verify references updated | Post-compaction heap integrity |
| Weak reference clearing | Create weak pointers, drop strong refs, trigger GC, check broken flag | `broken-p` = T after collection |
| Finalizer ordering | Register finalizers, drop refs, verify all called on finalizer thread | All finalizers invoked; errors logged, not propagated |
| Pause time | Run cl-bench under default config, measure STW pauses | p99 pause < `gc-pause-target` × 2 |
| OOM handling | Set `--heap-max` small, allocate until exhaustion | `STORAGE-CONDITION` signalled, no crash |

---

## 3.15  Module Map

GC implementation lives under `crates/egcl-rt/src/gc/`:

```
gc/
├── mod.rs          # public API: init_heap(), alloc(), collect(), gc_stats()
├── region.rs       # RegionHeader, region pool, free-list management
├── tlab.rs         # TLAB struct, refill logic
├── nursery.rs      # minor GC: parallel scavenge, survivor management
├── mark.rs         # concurrent marker: tri-colour, SATB processing
├── evacuate.rs     # collection-set selection, region evacuation
├── card_table.rs   # card table, dirty-card scanning
├── barrier.rs      # write-barrier helpers (SATB + card), called from JIT stubs
├── bitmap.rs       # mark bitmap, TAMS tracking
├── weak.rs         # weak pointers, weak hash-table support
├── finalize.rs     # finalization registry, finalizer thread
├── safepoint.rs    # safepoint page, polling, thread parking
└── stats.rs        # GC statistics, pause-time histograms
```

Cross-references: allocation fast path is JIT-inlined (§4), object
header layout is defined in §1 (D1.02), thread stack walking uses
stack maps from §4, CL-level `STORAGE-CONDITION` is defined in §5.
