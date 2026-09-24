# §4.8  Inline Caches

Inline caches (ICs) accelerate polymorphic operations — generic function
dispatch, type checks, slot access, and arithmetic — by caching the result
of a slow lookup directly at the call site in emitted code.  TorCL follows
the HotSpot model: every call site that performs a dynamic lookup is backed
by an IC that transitions through monomorphic → polymorphic → megamorphic
states, and is atomically patchable without stopping the world.

This section specifies IC data structures (D4.03, D4.13), the IC state
machine (A4.11), generic-function dispatch acceleration, type-check
elimination, code-site layout, atomic patching, GC interaction, statistics,
and fallback to the generic dispatch hash table.

---

## 4.8.1  Requirements

| ID | Requirement |
|----|-------------|
| R4.48 | Every call site that performs generic dispatch, type checking, or slot access MUST be backed by an inline cache. |
| R4.49 | The IC subsystem MUST support three states — monomorphic, polymorphic (≤ 8 entries), and megamorphic — with well-defined transitions. |
| R4.50 | IC patching MUST be atomic with respect to concurrent reader threads: a reading thread MUST never observe a half-written IC entry. |
| R4.51 | The GC MUST invalidate all ICs that reference a class or method whose layout, class hierarchy, or method definition has changed. |
| R4.52 | When an IC transitions to megamorphic state, the runtime MUST fall back to a hash-table-based generic dispatch that is still competitive (< 3× monomorphic fast-path latency for 95th-percentile lookups). |

---

## 4.8.2  Data Structures

### D4.03 — `InlineCacheEntry`

A single cached mapping from a guard key to a target.

```rust
/// One slot in an inline cache.
#[repr(C)]
struct InlineCacheEntry {
    /// The class pointer (or type tag) that this entry guards on.
    /// Null sentinel (0) means the slot is empty.
    guard: AtomicU64,   // class pointer or immediate-type tag

    /// Cached target: a function pointer, slot offset, or compiled
    /// method entry point, depending on the IC kind.
    target: AtomicU64,  // target payload (kind-dependent)
}
```

| Field | Width | Semantics |
|-------|-------|-----------|
| `guard` | 64 bits | Class pointer (heap-object ICs) or packed immediate-type tag. A zero guard marks an empty slot. |
| `target` | 64 bits | Dispatch target.  For GF dispatch: entry-point address.  For slot access: slot offset. For type-check: branch target address. |

**Alignment:** Each entry MUST be 16-byte aligned so that a 128-bit
atomic compare-and-swap can update both fields together on x86-64.

**Versioned guard variant:** For T2-optimised code that uses class
version checks (§4.8.8), a wider entry layout is used:

```rust
/// IC entry with class-version guard, used in T2 code.
#[repr(C, align(32))]
struct InlineCacheVersionedEntry {
    /// Class pointer.
    guard_class: AtomicU64,
    /// Expected class version counter.
    guard_version: AtomicU64,
    /// Cached dispatch target.
    target: AtomicU64,
    /// Padding to 32-byte alignment.
    _pad: u64,
}
```

Versioned entries are 32 bytes each and are used only in monomorphic
ICs embedded in T2-compiled code (§4.8.5).  They are NOT stored in the
standard `InlineCacheSite.entries` array; instead, the T2 compiler
emits the versioned guard+target inline as immediate data in the code
stream, checked by the assembly sequence shown in §4.8.8.  The
standard 16-byte `InlineCacheEntry` remains the layout for all
interpreter and T1 IC sites.

### D4.13 — `InlineCacheSite`

The full IC metadata associated with a single call site.

```rust
/// Inline cache for one call site.
#[repr(C)]
struct InlineCacheSite {
    /// IC kind — determines interpretation of guard/target fields.
    kind: ICKind,

    /// Current IC state.
    state: AtomicU8,  // ICState discriminant

    /// Number of valid entries (0..=MAX_POLY_ENTRIES).
    entry_count: AtomicU8,

    /// Per-site spinlock used to serialise concurrent miss handlers.
    /// 0 = unlocked, 1 = locked.  See §4.8.7 Patching Protocol and
    /// §4.8.12 Concurrency.
    lock: AtomicU8,

    /// Padding to align entries array to a 16-byte boundary.
    /// Preceding fields: kind (1) + state (1) + entry_count (1)
    /// + lock (1) = 4 bytes; 12 bytes of padding → offset 16.
    _pad: [u8; 12],

    /// Inline entry slots.  Monomorphic uses entries[0] only.
    /// Polymorphic uses entries[0..entry_count].
    entries: [InlineCacheEntry; MAX_POLY_ENTRIES],

    /// Pointer to the megamorphic dispatch hash table (§4.8.10).
    /// NULL when state ≠ Megamorphic.  Set by `build_dispatch_hash_table`
    /// during the Polymorphic → Megamorphic transition.
    dispatch_table: *const DispatchHashTable,

    /// Pointer to the slow-path / generic dispatch stub.
    slow_path: *const u8,

    /// Back-pointer to the compiled code object that owns this IC,
    /// used by the GC for invalidation scanning.
    owner_code: *const CodeObject,

    /// Byte offset from the start of the owning CodeObject to the
    /// patchable instruction that jumps to the IC fast path.
    patch_offset: u32,

    /// Statistics: total number of times the slow path was entered.
    miss_count: AtomicU32,

    /// Statistics: total number of times the fast path was taken.
    /// Updated by sampling (see §4.8.9), NOT on every fast-path hit.
    hit_count: AtomicU32,
}
```

```rust
const MAX_POLY_ENTRIES: usize = 8;

#[repr(u8)]
enum ICKind {
    GenericFunctionDispatch = 0,
    SlotRead               = 1,
    SlotWrite              = 2,
    TypeCheck              = 3,
    ArithmeticOp           = 4,
}

#[repr(u8)]
enum ICState {
    Uninitialized = 0,
    Monomorphic   = 1,
    Polymorphic   = 2,
    Megamorphic   = 3,
}
```

| Constant | Value | Rationale |
|----------|-------|-----------|
| `MAX_POLY_ENTRIES` | 8 | Empirically, > 8 unique receiver types at one site is rare and does not benefit from linear scan vs. hash lookup. Matches V8 and HotSpot defaults. |

---

## 4.8.3  IC State Machine (A4.11)

The IC progresses through four states.  Transitions are monotonic during
normal execution; only GC invalidation and re-compilation can reset state.

```text
                   first miss
  ┌──────────────┐ ──────────► ┌──────────────┐
  │ Uninitialized│             │ Monomorphic  │
  └──────────────┘             └──────┬───────┘
                                      │ miss with different guard
                                      ▼
                               ┌──────────────┐
                               │ Polymorphic  │ ◄──┐
                               │ (2..8 entries)│ ───┘ miss with new guard,
                               └──────┬───────┘      entry_count < 8
                                      │ miss with new guard,
                                      │ entry_count == 8
                                      ▼
                               ┌──────────────┐
                               │ Megamorphic  │
                               └──────────────┘
```

### Transition Rules

| From | Condition | To | Action |
|------|-----------|----|--------|
| Uninitialized | First miss | Monomorphic | Write guard+target into `entries[0]`; set `entry_count = 1`; patch call site to IC fast-path stub. |
| Monomorphic | Miss; guard ≠ `entries[0].guard` | Polymorphic | Append new entry at `entries[1]`; set `entry_count = 2`; rewrite stub to linear-scan check. |
| Polymorphic | Miss; new guard not in existing entries; `entry_count < MAX_POLY_ENTRIES` | Polymorphic | Append new entry at `entries[entry_count]`; increment `entry_count`. |
| Polymorphic | Miss; `entry_count == MAX_POLY_ENTRIES` | Megamorphic | Replace fast-path stub with hash-table dispatch stub; set `state = Megamorphic`. |
| Megamorphic | (terminal) | Megamorphic | All dispatches go through hash-table fallback. |
| Any | GC class/method invalidation | Uninitialized | Clear all entries; set `entry_count = 0`; restore call site to slow-path trampoline. |

### Transition Algorithm (pseudocode)

```text
function ic_miss(site: &InlineCacheSite, receiver_class: ClassPtr, args...):
    // Look up correct target via slow path.
    target = generic_lookup(site.kind, receiver_class, args...)

    // Attempt to add this mapping to the IC.
    match site.state:
        Uninitialized:
            site.entries[0] = { guard: receiver_class, target: target }
            site.entry_count = 1
            site.state = Monomorphic
            patch_call_site(site, monomorphic_stub_for(site))

        Monomorphic:
            site.entries[1] = { guard: receiver_class, target: target }
            site.entry_count = 2
            site.state = Polymorphic
            patch_call_site(site, polymorphic_stub_for(site))

        Polymorphic:
            n = site.entry_count
            // For multi-argument dispatch (§4.8.4), each logical entry
            // occupies `slots_per_entry` physical slots (e.g. 2 for
            // 2-argument GFs).  Effective capacity is reduced accordingly.
            slots_per_entry = dispatch_arity_slots(site)
            effective_max = MAX_POLY_ENTRIES / slots_per_entry
            if n < effective_max:
                write_entry(site, n, slots_per_entry, receiver_class, target)
                site.entry_count = n + 1
                // stub already handles linear scan; no re-patch needed
                // unless entry count crossed a stub-variant threshold
            else:
                site.state = Megamorphic
                site.dispatch_table = build_dispatch_hash_table(site)
                patch_call_site(site, megamorphic_stub)

        Megamorphic:
            // Insert into hash table only; no IC entry change.
            insert_hash_entry(site.dispatch_table, receiver_class, target)

    site.miss_count.fetch_add(1, Relaxed)
    return target
```

### Guard Checks

The fast-path stub performs a guard check appropriate to the IC kind:

| IC Kind | Guard Value | Check |
|---------|------------|-------|
| GenericFunctionDispatch | Receiver's class pointer | `receiver.header.class_ptr == guard` |
| SlotRead / SlotWrite | Receiver's class pointer | `receiver.header.class_ptr == guard` |
| TypeCheck | Type tag or class pointer | Tag-bits check for immediates; class-pointer check for heap objects |
| ArithmeticOp | Packed (lhs_tag, rhs_tag) pair | Both operand tags match the packed guard |

---

## 4.8.4  Generic Function Dispatch Acceleration

CLOS generic functions are the primary consumer of ICs.  Each `CALL-NEXT-METHOD`
or generic function call site receives its own `InlineCacheSite` with
`kind = GenericFunctionDispatch`.

### Monomorphic GF Fast Path

When a generic function call site is monomorphic (single receiver class):

```asm
; x86-64 monomorphic GF IC stub
; rdi = receiver (TorclVal, tag = 010 heap object)
  mov   rax, [rdi - 2]          ; load object header (subtract heap tag)
  mov   rax, [rax + 8]          ; load class pointer from header
  cmp   rax, QWORD [rip + ic_guard]  ; compare against cached class
  jne   .ic_miss                ; → slow path
  jmp   QWORD [rip + ic_target] ; → cached method entry point
.ic_miss:
  jmp   ic_miss_trampoline      ; → runtime ic_miss handler
```

### Polymorphic GF Path

A linear scan over `entry_count` entries (unrolled for ≤ 4 entries,
loop for 5–8):

```asm
; x86-64 polymorphic IC stub (unrolled, 2 entries shown)
  mov   rax, [rdi - 2]
  mov   rax, [rax + 8]          ; class pointer
  cmp   rax, QWORD [rip + ic_entries + 0]
  je    .hit0
  cmp   rax, QWORD [rip + ic_entries + 16]
  je    .hit1
  ; ... more entries ...
  jmp   ic_miss_trampoline
.hit0:
  jmp   QWORD [rip + ic_entries + 8]
.hit1:
  jmp   QWORD [rip + ic_entries + 24]
```

### Multi-method Dispatch

For generic functions specializing on multiple arguments, the guard
is extended:

- **2-argument dispatch:** guard = `(class_ptr_arg1, class_ptr_arg2)`,
  stored across two consecutive `InlineCacheEntry` slots that are
  treated as a single logical entry (effective `MAX_POLY_ENTRIES / 2 = 4`).
- **3+ argument dispatch:** guard is a hash of all specializer class
  pointers; collision → slow path re-check.  Polymorphic capacity
  remains 8 entries (hash collisions are rare in practice).

---

## 4.8.5  Type-Check Elimination via Monomorphic ICs

When the optimising compiler (T2, §4) detects a monomorphic IC with a
high hit count and zero miss count over a profiling window, it MAY
speculate that the type is stable and eliminate the type check entirely:

1. Compiler emits an **uncommon trap** (deoptimisation point) in place
   of the guard check.
2. If the guard would ever fail at runtime, the uncommon trap fires,
   deoptimises the frame (§4.7 OSR), and resets the IC to Uninitialized.
3. The function is re-compiled with the IC in its new state.

**Requirement (from R4.48):** Speculative type-check elimination MUST
preserve correctness — any deoptimisation MUST restore full program
state and retry the operation through the generic path.

### Conditions for Speculation

| Condition | Threshold |
|-----------|-----------|
| IC state is monomorphic | required |
| `hit_count` ≥ 10,000 | required |
| `miss_count` == 0 during profiling window | required |
| Function is at T2 tier | required |
| Guard class is not `BUILT-IN-CLASS T` | required (too broad to specialize on) |

---

## 4.8.6  IC Site Layout in Emitted Code

Each IC site in compiled code consists of three regions:

```text
┌────────────────────────────────────────────────┐
│  Inline fast path (in code stream)             │
│  ┌──────────────────────────────────────────┐  │
│  │  load receiver class                     │  │
│  │  cmp class, [ic_data + guard]            │  │
│  │  jne slow_path_trampoline                │  │
│  │  jmp [ic_data + target]                  │  │
│  └──────────────────────────────────────────┘  │
├────────────────────────────────────────────────┤
│  IC data (in side table, not in code stream)   │
│  ┌──────────────────────────────────────────┐  │
│  │  InlineCacheSite (D4.13)                 │  │
│  └──────────────────────────────────────────┘  │
├────────────────────────────────────────────────┤
│  Slow-path trampoline (shared per IC kind)     │
│  ┌──────────────────────────────────────────┐  │
│  │  push ic_site_ptr                        │  │
│  │  call ic_miss_handler                    │  │
│  │  jmp resolved_target                     │  │
│  └──────────────────────────────────────────┘  │
└────────────────────────────────────────────────┘
```

**Code-object metadata:** Every `CodeObject` (§4.6) embeds an array of
`InlineCacheSite` pointers as a side table.  The `patch_offset` field
records the byte offset of the patchable instruction from the code
object base, enabling the runtime to locate and rewrite it.

**IC data placement:** IC data (`InlineCacheSite`) is allocated in a
separate non-executable, GC-visible memory region.  This ensures:

- The IC data is writable without making code pages writable (W⊕X).
- The GC can scan IC data for object references during marking.
- IC patching only modifies data, not instruction bytes (except for
  the initial stub selection, which patches a single jump target).

---

## 4.8.7  Atomic IC Patching

IC updates MUST be atomic with respect to concurrent threads that may
be executing the fast-path check.  The atomicity strategy is
architecture-dependent.

### x86-64

- **Entry write (guard + target):** Use `LOCK CMPXCHG16B` (128-bit CAS)
  to atomically write both the 64-bit guard and 64-bit target.  This
  requires 16-byte alignment of `InlineCacheEntry` (guaranteed by D4.03).
- **Jump target patch:** Overwriting an 8-byte aligned `MOV` immediate
  or a RIP-relative `JMP` target is naturally atomic on x86-64 when
  the target is 8-byte aligned.
- **Ordering:** IC data writes use `Release` ordering.  The fast-path
  guard load uses `Acquire` ordering.  This ensures a thread that
  observes the new guard also observes the new target.

```rust
impl InlineCacheEntry {
    fn cas_update(&self, old_guard: u64, new_guard: u64, new_target: u64) -> bool {
        // Atomic 128-bit CAS on x86-64.
        // old = (old_guard, old_target_dont_care)
        // new = (new_guard, new_target)
        unsafe {
            let old_pair = u128::from(old_guard);  // upper 64 = don't care
            let new_pair = (new_target as u128) << 64 | new_guard as u128;
            core::arch::x86_64::cmpxchg16b(
                self as *const _ as *mut u128,
                old_pair,
                new_pair,
            )
        }
    }
}
```

### AArch64

- **Entry write:** Use `LDXP` / `STXP` (load-exclusive pair / store-
  exclusive pair) for 128-bit atomic updates.
- **Instruction visibility:** After patching a jump target in IC data,
  execute `ISB` (Instruction Synchronization Barrier) on the patching
  thread.  Other cores will see the updated data through normal cache
  coherency; `ISB` is only needed if the code stream itself was
  modified (initial stub selection).
- **Data barrier:** `DMB ISH` (Data Memory Barrier, Inner Shareable)
  after store-exclusive to ensure visibility to other cores.

```text
; AArch64 IC entry update
    ldxp  x2, x3, [x0]         ; load-exclusive pair (guard, target)
    cmp   x2, x4               ; compare guard with expected
    b.ne  .retry
    stxp  w1, x5, x6, [x0]    ; store-exclusive pair (new_guard, new_target)
    cbnz  w1, .retry           ; retry if exclusive monitor lost
    dmb   ish                   ; ensure store visible to all cores
```

### Patching Protocol

1. **Acquire IC lock** — a per-site spinlock (stored in `InlineCacheSite`)
   to serialise concurrent miss handlers for the same site.
2. **Re-check state** — another thread may have already handled the same
   miss; if the guard now matches, return the existing target.
3. **Write new entry** atomically (CAS / exclusive pair).
4. **Update state/count** with `Release` store.
5. **Patch code jump target** if the stub variant changed (Uninitialized →
   Monomorphic, Mono → Poly, Poly → Mega).
6. **Release IC lock.**

---

## 4.8.8  GC Interaction and IC Invalidation

### Object Movement

When the GC relocates objects (§3 compaction), it MUST update all IC
entries that reference moved objects:

- **Guard pointers:** If a class object moves, every IC entry whose
  guard matches the old address MUST be updated to the new address.
- **Target pointers:** If a method's compiled code is relocated, every
  IC entry whose target points into that code MUST be updated.

The GC performs this by scanning the IC side-table of every live
`CodeObject`.  Each `InlineCacheSite` is registered in a global
`ic_sites` weak set, enabling the GC to iterate all live IC sites
without walking all code objects.

### Class / Method Invalidation

When a class definition changes (slot layout, inheritance, method
addition/removal), all ICs that cached results depending on the old
definition MUST be invalidated:

| Event | Invalidation Scope |
|-------|-------------------|
| `DEFCLASS` redefining an existing class | All ICs whose guard matches the class or any subclass |
| `DEFMETHOD` adding/removing/replacing a method | All ICs with `kind = GenericFunctionDispatch` for the affected GF |
| `CHANGE-CLASS` on an instance | IC for that instance's class at dispatch sites it may reach (conservative: invalidate all ICs for the old class) |
| Class finalization changing slot layout | All `SlotRead`/`SlotWrite` ICs for the class and subclasses |

### Invalidation Mechanism

Each class maintains a **class version counter** (`class_version: AtomicU64`).
IC guards optionally embed the expected version:

```rust
struct ClassVersionedGuard {
    class_ptr: u64,
    class_version: u64,
}
```

For monomorphic ICs in T2-optimised code, the guard check includes
a version comparison:

```asm
  mov   rax, [rdi - 2]          ; object header
  mov   rbx, [rax + 8]          ; class pointer
  cmp   rbx, QWORD [rip + ic_guard_class]
  jne   .ic_miss
  mov   rcx, [rbx + CLASS_VERSION_OFFSET]
  cmp   rcx, QWORD [rip + ic_guard_version]
  jne   .ic_miss                ; class was redefined → miss
  jmp   QWORD [rip + ic_target]
```

When a class is redefined, its version counter is incremented.  All
ICs with the old version automatically miss on next invocation and
re-resolve through the slow path, which caches the new version.

### Bulk Invalidation

For operations that invalidate many ICs (e.g., redefining a base class
with many subclasses), the runtime MAY perform **epoch-based bulk
invalidation**:

1. Increment a global `ic_epoch: AtomicU64`.
2. Each IC miss handler checks `ic_epoch` against a locally cached
   `last_epoch`.  If they differ, the IC clears all entries before
   re-resolving.
3. This avoids O(n) scanning of all IC sites for bulk invalidation
   events.

---

## 4.8.9  IC Statistics and Monitoring

The runtime collects per-site and aggregate IC statistics for adaptive
optimisation and developer tooling.

### Per-Site Counters

| Counter | Type | Updated By |
|---------|------|-----------|
| `hit_count` | `AtomicU32` | Profiling subsystem (§4.9): sampled periodically via timer-based profiling interrupts, NOT incremented on every fast-path hit.  When `TORCL_IC_STATS_ENABLED` is `false`, hit counting is disabled entirely. |
| `miss_count` | `AtomicU32` | `ic_miss` handler (always incremented; miss path is already slow). |

### Aggregate Metrics

The profiling subsystem (§4.9) periodically samples IC sites and
maintains:

| Metric | Description |
|--------|-------------|
| `ic_mono_rate` | Fraction of IC sites currently monomorphic |
| `ic_poly_rate` | Fraction currently polymorphic |
| `ic_mega_rate` | Fraction currently megamorphic |
| `ic_miss_rate` | Global miss rate (misses / total accesses) over a sampling window |
| `ic_transitions` | Count of state transitions per window (for detecting pathological churn) |

### Monitoring Interface

```lisp
;; Retrieve IC statistics for a function.
(torcl-ext:function-ic-stats #'my-generic-function)
;; => (:sites 12
;;     :monomorphic 9
;;     :polymorphic 2
;;     :megamorphic 1
;;     :total-hits 1482930
;;     :total-misses 47
;;     :miss-rate 0.000031)

;; Dump all megamorphic IC sites (for performance debugging).
(torcl-ext:dump-megamorphic-ics :stream *trace-output*)
```

### Tier Promotion Feedback

The IC statistics feed back into tier promotion decisions (§4.9):

- A function with high `ic_miss_rate` is deprioritised for T2
  promotion (optimising code with unstable ICs wastes compile time).
- A function whose ICs are all monomorphic and stable is a prime
  candidate for T2 with speculative type-check elimination (§4.8.5).

---

## 4.8.10  Megamorphic Fallback: Generic Dispatch Hash Table

When an IC transitions to megamorphic state, subsequent dispatches
use a hash table instead of linear scan.

### Hash Table Structure

```rust
/// Per-megamorphic-site dispatch table.
struct DispatchHashTable {
    /// Power-of-two capacity.
    capacity: usize,

    /// Number of live entries.
    len: AtomicUsize,

    /// Open-addressing table: (guard, target) pairs.
    /// Empty slots have guard == 0.
    slots: Box<[InlineCacheEntry]>,
}
```

- **Hash function:** The class pointer is hashed using a
  multiplicative hash (`ptr.wrapping_mul(GOLDEN_RATIO_U64) >> shift`)
  for fast, low-collision distribution of aligned pointers.
- **Collision resolution:** Linear probing with a maximum probe length
  of 8.  If probe length is exceeded, the entry falls through to the
  full generic lookup.
- **Load factor:** The table is resized (doubled) when load exceeds 75%.
- **Concurrency:** Reads are lock-free (atomic loads of guard+target
  pairs with `Acquire` ordering).  Writes acquire the per-site
  spinlock and use `Release` stores.

### Performance Target

Per R4.52, the megamorphic hash-table dispatch MUST be no slower than
3× the monomorphic single-comparison fast path for 95th-percentile
lookups.  Benchmark: a tight loop dispatching on 20 distinct receiver
classes through a single call site.

### Rehashing

When the dispatch table is resized:

1. Allocate new table at 2× capacity.
2. Copy all live entries (atomic reads of old table).
3. CAS-swap the `slots` pointer in `DispatchHashTable`.
4. Old table is freed after a quiescent period (epoch-based
   reclamation, same epoch as GC safepoints).

---

## 4.8.11  Error Handling

| Failure Mode | Detection | Recovery |
|-------------|-----------|----------|
| CAS failure during IC update | Return value of CAS instruction | Retry (bounded to 8 attempts, then fall through to slow path) |
| IC data corruption (guard ≠ any valid class) | Guard check fails for all known classes | Reset IC to Uninitialized; log warning |
| Hash table probe exceeds max length | Probe counter ≥ 8 | Fall through to generic lookup; do not insert (table is too full) |
| Stale target (code was freed) | Code liveness check before jump | Reset IC entry; re-resolve through slow path |

---

## 4.8.12  Concurrency

- IC data modifications are serialised by a per-site spinlock (1 byte
  in `InlineCacheSite`).
- Fast-path reads are lock-free: atomic loads with `Acquire` ordering.
- The per-site lock is held only during `ic_miss` handling (typically
  < 1 μs).  No nested locking.  The lock order is: IC site lock →
  (if needed) class table read-lock → (if needed) GF dispatch table
  read-lock.
- GC invalidation acquires the IC site lock before clearing entries,
  ensuring no concurrent miss handler writes stale data.

---

## 4.8.13  Configuration

| Knob | Type | Default | Description |
|------|------|---------|-------------|
| `TORCL_IC_MAX_POLY` | `usize` | 8 | Maximum polymorphic entries before megamorphic transition |
| `TORCL_IC_SPECULATE_THRESHOLD` | `u32` | 10,000 | Minimum hit count for type-check elimination |
| `TORCL_IC_HASH_INITIAL_CAPACITY` | `usize` | 32 | Initial capacity of megamorphic dispatch hash table |
| `TORCL_IC_HASH_MAX_PROBE` | `usize` | 8 | Maximum linear probe length |
| `TORCL_IC_STATS_ENABLED` | `bool` | `true` | Enable per-site hit/miss counters (disable for minimal overhead) |
| `TORCL_IC_EPOCH_INVALIDATION` | `bool` | `true` | Use epoch-based bulk invalidation |

All knobs are settable via environment variables or the runtime
configuration API (`torcl-ext:set-runtime-option`).

---

## 4.8.14  Test Strategy

| Test Category | Method |
|---------------|--------|
| State transitions | Unit tests exercising Uninitialized → Mono → Poly → Mega with mock class pointers; verify state and entry_count after each miss. |
| Atomicity | Multi-threaded stress test: N threads call the same GF with M distinct receiver classes concurrently; verify no torn reads or crashes. |
| GC invalidation | Trigger class redefinition while IC-using code is running on other threads; verify ICs reset and re-resolve correctly. |
| Speculative elimination | Compile a function at T2 with monomorphic IC; introduce a new receiver class; verify uncommon trap fires and deoptimisation restores correct state. |
| Megamorphic performance | Benchmark dispatch through a megamorphic site with 20 classes; assert latency < 3× monomorphic baseline (R4.52). |
| Hash table correctness | Insert/lookup/resize stress test with concurrent readers and writers; verify no lost entries or infinite probe loops. |
| Epoch invalidation | Redefine a base class with 100 subclasses; verify all ICs invalidated within one epoch bump, not O(n) site scans. |
| Platform parity | Run IC atomicity tests on both x86-64 and AArch64 (CI matrix) to verify CAS / exclusive-pair correctness. |
