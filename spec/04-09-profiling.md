# §4.9 Profiling Infrastructure

## Scope

The profiling infrastructure collects runtime execution data—invocation
counts, back-edge (loop) counts, and receiver-type profiles—to drive
tier-promotion decisions (§1.1) and to feed the T2 optimising compiler
with speculative type information. Design mirrors HotSpot's profiling
model: lightweight in-line counters updated by T0/T1 code, read
lock-free by the compilation scheduler, with optional JIT-dump and
`perf` integration for external tooling.

This section specifies the counter mechanisms, type-profile recording,
the promotion trigger algorithm, external profiling integration, and
runtime configurability. Data structures D4.04, D4.14 and algorithm
A4.12 are defined herein.

---

## 4.9.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R4.53 | Every compiled function MUST maintain a monotonically-increasing invocation counter and a per-back-edge iteration counter, both readable without acquiring a lock. | MUST |
| R4.54 | The runtime MUST record receiver-type profiles for polymorphic call sites in a fixed-size ring buffer (D4.14), capturing class-wrapper pointers and hit counts. | MUST |
| R4.55 | Counter reads by the compilation scheduler (T2 compiler thread) MUST use lock-free access (relaxed or acquire atomics); no mutex, spinlock, or CAS loop SHALL be required for reads. | MUST |
| R4.56 | Tier-promotion from T1→T2 MUST be triggered when either the invocation counter or any back-edge counter of a function exceeds a configurable threshold (A4.12). | MUST |
| R4.57 | On Linux the runtime SHOULD emit JIT-dump files (`jit-<pid>.dump`) compatible with `perf inject --jit`, and SHOULD register JIT code via `/tmp/perf-<pid>.map` for symbol resolution. | SHOULD |
| R4.58 | Total profiling overhead (counter increments + type recording) MUST NOT exceed 5% wall-clock slowdown relative to profiling-disabled execution on representative benchmarks (cl-bench arithmetic, gabriel). | MUST |

---

## 4.9.2 Data Structures

### D4.04 — `MethodCounters`

Per-function profiling counters, allocated alongside the compiled-code
metadata (§4.3). One `MethodCounters` instance exists per function that
has been compiled to at least T1.

```rust
/// Per-function profiling counters.
/// Alignment: 64 bytes (one cache line on x86-64 / aarch64).
#[repr(C, align(64))]
pub struct MethodCounters {
    /// Monotonic invocation counter.  Incremented in the function
    /// prologue of T1 code.  Accessed with Relaxed ordering.
    pub invoke_count: AtomicU32,

    /// Overflow / tier-transition flag.  Set to 1 by the mutator
    /// when `invoke_count` or any back-edge counter crosses the
    /// promotion threshold.  Read by the compilation scheduler.
    pub hot_flag: AtomicU8,

    /// Padding to push back-edge counters to their own cache line
    /// region and avoid false-sharing with invoke_count.
    _pad0: [u8; 27],

    /// Back-edge counters.  One per loop header in the function's
    /// control-flow graph, up to MAX_BACKEDGE_COUNTERS.  Each counter
    /// is incremented at the loop latch.  Excess loops share the last
    /// slot (saturation, not wrap).
    pub backedge_counts: [AtomicU32; MAX_BACKEDGE_COUNTERS],
}

/// Maximum number of individually tracked back-edge counters per
/// function.  Functions with more loops fold excess into the last slot.
pub const MAX_BACKEDGE_COUNTERS: usize = 8;
```

**Layout (64 bytes total):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | `invoke_count` |
| 4 | 1 | `hot_flag` |
| 5 | 27 | padding |
| 32 | 4×8 = 32 | `backedge_counts[0..8]` |

**Invariants:**

- `invoke_count` is monotonically non-decreasing. The hardware `lock inc`
  instruction wraps at `u32::MAX` → 0; this is harmless because any
  value near `u32::MAX` has long since exceeded every promotion threshold,
  and wrapping to 0 merely delays (never causes) a redundant re-promotion.
- `hot_flag` transitions 0→1 exactly once per compilation cycle. Reset
  to 0 only by the compilation scheduler after enqueuing the function
  for T2 compilation.
- `backedge_counts[i]` values are monotonically non-decreasing within a
  single compilation cycle; the scheduler MAY reset them to 0 when T2
  code is installed.

**Ownership:**

- Allocated by the T1 code installer, owned by the function's code
  metadata block.
- Freed when the function's compiled code is collected by GC (§3).

---

### D4.14 — `TypeProfileRing`

Fixed-size ring buffer recording receiver types at polymorphic call
sites. Each call site in T1 code that performs a generic dispatch is
assigned one `TypeProfileRing`.

```rust
/// Ring-buffer type profile for a single call site.
/// Stores the most recent TYPE_PROFILE_RING_SIZE observed receiver
/// class-wrapper pointers and their hit counts.
#[repr(C, align(32))]
pub struct TypeProfileRing {
    /// Ring entries.  Each entry holds a class-wrapper pointer
    /// (BlissVal with tag 010) and a hit count.
    pub entries: [TypeProfileEntry; TYPE_PROFILE_RING_SIZE],

    /// Write cursor (index into `entries`).  Updated with Relaxed
    /// ordering by the mutator; stale reads by the compiler are
    /// tolerable (they only reduce profile accuracy, never correctness).
    pub cursor: AtomicU8,

    /// Total number of calls through this site (including those whose
    /// type was not recorded because the ring was full).
    pub total_count: AtomicU32,

    /// Number of distinct types observed since the last reset.
    /// Saturates at 255.  Used to classify the site as monomorphic
    /// (1), polymorphic (2–4), or megamorphic (>4).
    pub distinct_types: AtomicU8,
}

#[repr(C)]
pub struct TypeProfileEntry {
    /// Class-wrapper pointer (tagged heap pointer, §1).
    /// 0 means unused slot.
    pub class_ptr: AtomicU64,

    /// Number of times this class was observed at this call site.
    pub count: AtomicU32,

    /// Padding to align entries to 16 bytes.
    _pad: u32,
}

pub const TYPE_PROFILE_RING_SIZE: usize = 4;
```

**Layout per entry (16 bytes):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 8 | `class_ptr` |
| 8 | 4 | `count` |
| 12 | 4 | padding |

**Ring semantics:**

1. On a generic call, T1 stub code loads the receiver's class-wrapper
   pointer and scans `entries[0..TYPE_PROFILE_RING_SIZE]`.
2. If a matching `class_ptr` is found, its `count` is incremented
   (Relaxed atomic add).
3. If no match and an empty slot (`class_ptr == 0`) exists, the new
   class pointer is stored (Relaxed CAS) and `distinct_types`
   incremented.
4. If no match and no empty slot, `cursor` advances (wrapping) and the
   oldest entry is overwritten. `distinct_types` increments (if not
   saturated at 255).
5. `total_count` is always incremented (Relaxed atomic add).

**Ownership:**

- Allocated per call site by the T1 code installer.
- Stored in the function's profiling data block, adjacent to the
  `MethodCounters`.
- Read by the T2 compiler during inlining and speculative devirtualisation.
- Reset (all entries zeroed, cursor reset) when T2 code is installed
  and the call site is deoptimised back to T1 (§4.8 OSR / deopt).

---

## 4.9.3 Counter Instrumentation

### 4.9.3.1 Invocation Counters

The T1 baseline compiler (§4.4) emits an invocation counter increment
in every function prologue:

```asm
; x86-64 T1 prologue — invocation counter
;   r13 = pointer to MethodCounters (loaded from code metadata)
    lock inc dword [r13 + OFFSET_INVOKE_COUNT]
    cmp     dword [r13 + OFFSET_INVOKE_COUNT], INVOKE_THRESHOLD
    jae     .slow_path_promote
```

On aarch64, `ldadd` (LSE atomics) or a `ldxr`/`stxr` loop is used:

```asm
; aarch64 T1 prologue — invocation counter
;   x20 = pointer to MethodCounters
    mov     w8, #1
    ldadd   w8, w9, [x20, #OFFSET_INVOKE_COUNT]
    add     w9, w9, #1
    cmp     w9, INVOKE_THRESHOLD
    b.hs    .slow_path_promote
```

The slow path sets `hot_flag` and enters the promotion trigger (A4.12).

### 4.9.3.2 Back-Edge Counters

Each loop latch (the branch that jumps back to the loop header)
contains a counter increment for the corresponding `backedge_counts[i]`
slot:

```asm
; x86-64 back-edge counter for loop i
    lock inc dword [r13 + OFFSET_BACKEDGE_BASE + i*4]
    cmp     dword [r13 + OFFSET_BACKEDGE_BASE + i*4], BACKEDGE_THRESHOLD
    jae     .slow_path_osr
```

Back-edge overflow triggers an OSR (On-Stack Replacement) request
(§4.8) in addition to setting `hot_flag`.

### 4.9.3.3 T0 Bytecode Interpreter Counters

The T0 bytecode interpreter maintains invocation counts in the function
metadata and increments bytecode back-edge counters at `BACK_EDGE`
instructions.  Counter storage follows the same `FnMeta` layout used by T1
so the tiering scheduler, OSR machinery, and profiler read one shared
profile shape across tiers.  When the invocation count reaches the T0→T1
threshold, the interpreter requests T1 compilation and continues interpreting
until T1 code is installed.  When a bytecode back-edge reaches the OSR
threshold, the interpreter may request direct OSR into T2 once suitable OSR
entry metadata exists.

---

## 4.9.4 Counter Reading Protocol (Lock-Free)

### R4.55 compliance

The compilation scheduler thread reads profiling data without any
synchronisation primitive beyond atomic loads:

```rust
impl MethodCounters {
    /// Read the invocation count.  Safe to call from any thread.
    /// May return a slightly stale value; this is acceptable because
    /// promotion thresholds are heuristic, not safety-critical.
    #[inline]
    pub fn read_invoke_count(&self) -> u32 {
        self.invoke_count.load(Ordering::Relaxed)
    }

    /// Read a back-edge counter.
    #[inline]
    pub fn read_backedge_count(&self, idx: usize) -> u32 {
        debug_assert!(idx < MAX_BACKEDGE_COUNTERS);
        self.backedge_counts[idx].load(Ordering::Relaxed)
    }

    /// Set the hot flag (called by mutator on threshold crossing).
    /// Release ordering ensures that the counter increment that
    /// triggered this store is visible to the scheduler's subsequent
    /// Acquire load.
    #[inline]
    pub fn set_hot(&self) {
        self.hot_flag.store(1, Ordering::Release);
    }

    /// Check and clear the hot flag (called by scheduler).
    /// Uses Acquire ordering so that counter reads that follow
    /// observe at least the values that preceded the flag set.
    pub fn check_and_clear_hot(&self) -> bool {
        self.hot_flag.swap(0, Ordering::AcqRel) != 0
    }
}
```

**Correctness argument:**

- Counters are monotonically non-decreasing. A stale read can only
  *undercount*, which delays promotion—never causes premature promotion.
- The `hot_flag` is the synchronisation point: the mutator stores it
  with Release after the counter increment that triggered it; the
  scheduler loads it with Acquire via `check_and_clear_hot`, establishing
  a happens-before relationship.

### Reader protocol for `TypeProfileRing`

The T2 compiler reads type profiles as a snapshot: it copies the
`entries` array, `cursor`, `total_count`, and `distinct_types` in a
single pass. Because all fields use Relaxed atomics, the snapshot may
be slightly inconsistent (e.g., `total_count` may not exactly equal the
sum of individual entry counts). This is benign:

- Speculative type guards are always protected by a deoptimisation trap
  (§4.8). An inaccurate profile results in a less-optimal but
  still-correct guard.

---

## 4.9.5 Algorithm A4.12 — Tier Promotion Trigger

The promotion trigger runs on two paths: the **mutator slow path**
(inline, synchronous) and the **scheduler poll** (background thread).

### A4.12 Pseudocode

```text
ALGORITHM A4.12: TierPromotionTrigger

INPUT:  fn       — function whose counter overflowed
        counters — fn's MethodCounters
        kind     — INVOKE | BACKEDGE

CONSTANTS (configurable, see §4.9.7):
  INVOKE_THRESHOLD       = 5000    (default)
  BACKEDGE_THRESHOLD     = 10000   (default)
  OSR_BACKEDGE_THRESHOLD = 50000   (default)

— Mutator slow path (called from T1 code) —

1.  IF counters.hot_flag is already 1:
        RETURN  // another counter already triggered
2.  SET counters.hot_flag ← 1   (Release)
3.  LET request = CompileRequest {
        fn,
        tier: T2,
        osr: (kind == BACKEDGE),
        profile: snapshot(fn.type_profiles),
        priority: compute_priority(counters),
    }
4.  Enqueue request into the CompileQueue (lock-free MPSC queue)
5.  RETURN to caller (no blocking)

— Scheduler poll (background, every SCHEDULER_POLL_MS) —

6.  FOR EACH fn in active_function_set:
7.      IF fn.counters.check_and_clear_hot():
8.          // Already enqueued by mutator slow path
9.          CONTINUE
10.     LET ic = fn.counters.read_invoke_count()
11.     LET max_be = MAX(fn.counters.read_backedge_count(i)
                         for i in 0..fn.backedge_count)
12.     IF ic ≥ INVOKE_THRESHOLD OR max_be ≥ BACKEDGE_THRESHOLD:
13.         SET fn.counters.hot_flag ← 1
14.         Enqueue CompileRequest as in step 3
15. END FOR

FUNCTION compute_priority(counters) → u32:
    // Higher priority = compile sooner.
    // Weight invocations more heavily than back-edges, because
    // a function called many times with short loops benefits more
    // from optimisation than a rarely-called function with one
    // hot loop (which is handled via OSR).
    LET ic = counters.read_invoke_count()
    LET be = MAX(counters.backedge_counts)
    RETURN ic * 2 + be
```

### Priority queue

The `CompileQueue` is a lock-free MPSC (multi-producer, single-consumer)
FIFO queue. The single consumer is the T2 compilation thread. The queue
itself imposes no ordering beyond insertion order; when the T2 thread
drains the queue it sorts the batch of pending requests by `priority`
(highest first) before processing them. Duplicate requests for the same
function are deduplicated by checking the function's `hot_flag` state
before starting compilation.

### Demotion / deoptimisation feedback

If T2-compiled code is deoptimised (§4.8), the function's
`MethodCounters` are reset and the function reverts to T1 code. A
**penalty multiplier** (default 2×) is applied to the promotion
thresholds for that function to avoid rapid re-promotion of unstable
code. After 3 consecutive deoptimisations the function is **blacklisted**
from T2 promotion for the remainder of the session.

---

## 4.9.6 JIT-Dump and Perf Integration

### R4.57 — Linux `perf` support

#### JIT-dump file (`jit-<pid>.dump`)

On Linux, when the environment variable `BLISS_PERF_JITDUMP=1` is set,
the runtime:

1. Opens `/tmp/jit-<pid>.dump` at startup.
2. Writes a JIT-dump header (magic `0x4A695444`, version 1, ELF machine
   type, pid, timestamp).
3. On each T1 or T2 code installation, writes a `JIT_CODE_LOAD` record:
   - code name (function name or `lambda-<id>`)
   - code address, code size
   - timestamp (monotonic clock)
   - the raw machine code bytes
4. On code deallocation (GC), writes a `JIT_CODE_MOVE` or
   `JIT_CODE_CLOSE` record as appropriate.

#### Perf map file (`/tmp/perf-<pid>.map`)

Concurrently, and unconditionally on Linux when
`BLISS_PERF_MAP=1` (default off), the runtime maintains a text file
`/tmp/perf-<pid>.map` with lines of the form:

```text
<hex-start-addr> <hex-size> <symbol-name>
```

Updated on code install/dealloc. Writes are append-only; the file is
rewritten periodically (every 60 s or 1000 entries, whichever comes
first) to remove stale entries.

#### macOS dtrace integration

On macOS, when `BLISS_DTRACE=1` is set, the runtime fires USDT probes:

| Probe | Arguments |
|-------|-----------|
| `bliss:jit:compile` | fn-name, tier, code-addr, code-size |
| `bliss:jit:deopt` | fn-name, reason, code-addr |
| `bliss:gc:start` | generation, reason |
| `bliss:gc:end` | generation, duration-ns, bytes-reclaimed |

---

## 4.9.7 Configuration

All profiling tunables are exposed as environment variables and as
CL special variables (in the `BLISS-PROFILER` package).

| Env Variable | CL Variable | Type | Default | Description |
|---|---|---|---|---|
| `BLISS_T0_T1_THRESHOLD` | `*t0-t1-threshold*` | u32 | 10 | Invocations before T0→T1 |
| `BLISS_T1_T2_INVOKE_THRESHOLD` | `*t1-t2-invoke-threshold*` | u32 | 5000 | Invocations before T1→T2 |
| `BLISS_T1_T2_BACKEDGE_THRESHOLD` | `*t1-t2-backedge-threshold*` | u32 | 10000 | Back-edge iterations before T1→T2 |
| `BLISS_OSR_BACKEDGE_THRESHOLD` | `*osr-backedge-threshold*` | u32 | 50000 | Back-edge iterations before OSR |
| `BLISS_DEOPT_PENALTY_MULTIPLIER` | `*deopt-penalty-multiplier*` | f32 | 2.0 | Threshold multiplier after deopt |
| `BLISS_DEOPT_BLACKLIST_LIMIT` | `*deopt-blacklist-limit*` | u32 | 3 | Consecutive deopts before blacklist |
| `BLISS_SCHEDULER_POLL_MS` | `*scheduler-poll-ms*` | u32 | 50 | Scheduler poll interval (ms) |
| `BLISS_PERF_JITDUMP` | `*perf-jitdump*` | bool | false | Enable JIT-dump file on Linux |
| `BLISS_PERF_MAP` | `*perf-map*` | bool | false | Enable perf map file on Linux |
| `BLISS_DTRACE` | `*dtrace*` | bool | false | Enable USDT probes on macOS |
| `BLISS_PROFILING_DISABLED` | `*profiling-disabled*` | bool | false | Disable all counter instrumentation |
| `BLISS_TYPE_PROFILE_RING_SIZE` | — | u8 | 4 | Ring size (compile-time only) |

When `BLISS_PROFILING_DISABLED=1`, the T1 compiler omits counter
increments and type-recording stubs entirely, yielding zero profiling
overhead but disabling tier promotion (functions remain at T1
indefinitely).

Setting thresholds at runtime (via the CL variables) takes effect for
newly compiled functions; already-compiled T1 code retains the threshold
it was compiled with.

---

## 4.9.8 Profiling Overhead Constraints

### R4.58 — ≤5% overhead budget

The overhead budget is allocated as follows:

| Component | Budget | Mechanism |
|-----------|--------|-----------|
| Invocation counter increment | ≤1% | Single `lock inc` (x86-64) or `ldadd` (aarch64) per call |
| Back-edge counter increment | ≤2% | One `lock inc` per loop iteration; hot loops move to T2 quickly |
| Type profile recording | ≤1.5% | Linear scan of 4-entry ring; executed only at polymorphic call sites |
| Scheduler poll | ≤0.5% | Background thread; reads only, no contention with mutators |

**Design mitigations to stay within budget:**

1. **Cache-line alignment** — `MethodCounters` is aligned to 64 bytes so
   counter increments do not cause false-sharing with adjacent data.
2. **Relaxed atomics** — All mutator-side stores use `Relaxed` ordering;
   no memory fences on the fast path.
3. **Short ring buffer** — The 4-entry type profile ring keeps the
   linear scan to 4 iterations (≤64 bytes of data, fits in one cache
   line).
4. **Threshold-gated slow path** — The promotion slow path (A4.12
   steps 1–5) executes at most once per function per compilation cycle.
5. **T2 eliminates overhead** — Once a function is compiled to T2,
   profiling counters are removed from the optimised code. Only
   deoptimisation stubs re-enable profiling.

---

## 4.9.9 Error Handling

| Failure | Handling |
|---------|----------|
| `MethodCounters` allocation failure (OOM) | Function remains at T0/T1 without counters; no promotion possible. Log warning. |
| JIT-dump file open/write failure | Log warning; disable JIT-dump for the session. Execution continues normally. |
| Perf map file write failure | Log warning; disable perf map. No effect on execution. |
| CompileQueue full (back-pressure) | Drop lowest-priority request; log at debug level. Function stays at T1 until next poll cycle. |
| Counter wrap (`u32::MAX` → 0) | Benign; function is already far past promotion threshold. No corrective action needed (see §4.9.2 invariants). |

---

## 4.9.10 Concurrency

| Resource | Writer(s) | Reader(s) | Synchronisation |
|----------|-----------|-----------|-----------------|
| `MethodCounters.invoke_count` | Mutator thread (T1 code) | Scheduler thread | `AtomicU32`, Relaxed stores, Relaxed loads |
| `MethodCounters.hot_flag` | Mutator (Release) | Scheduler (Acquire via swap) | `AtomicU8`, AcqRel swap |
| `MethodCounters.backedge_counts` | Mutator thread | Scheduler thread | `AtomicU32`, Relaxed |
| `TypeProfileRing.entries` | Mutator thread | T2 compiler thread | `AtomicU64`/`AtomicU32`, Relaxed; snapshot copy |
| `TypeProfileRing.cursor` | Mutator thread | T2 compiler thread | `AtomicU8`, Relaxed |
| `CompileQueue` | Multiple mutator threads | Single scheduler/T2 thread | Lock-free MPSC queue |
| JIT-dump file | Single writer (code installer, holding install lock) | External tools (read-only) | File-level; installer holds code-install mutex |
| Perf map file | Single writer (code installer) | External tools | Atomic rename on rewrite |

**Lock ordering** (§2 global lock order extended):

- The code-install mutex (§4.7) is acquired *before* writing to the
  JIT-dump or perf-map files. No profiling lock is ever held while
  acquiring the code-install mutex.

---

## 4.9.11 Test Strategy

| Test | Type | Validates |
|------|------|-----------|
| `test_invoke_counter_increments` | Unit | `MethodCounters` increment semantics, saturation at `u32::MAX` |
| `test_backedge_counter_increments` | Unit | Per-slot back-edge counting, excess-loop folding into last slot |
| `test_type_profile_ring_insert` | Unit | Ring insert, eviction, `distinct_types` saturation |
| `test_type_profile_ring_snapshot` | Unit | Snapshot consistency under concurrent writes (stress test with loom) |
| `test_hot_flag_acqrel` | Unit | Acquire-Release ordering of `hot_flag` (loom model checker) |
| `test_promotion_trigger` | Integration | T1 function promotes to T2 after threshold invocations |
| `test_osr_promotion` | Integration | Long-running loop triggers OSR at back-edge threshold |
| `test_deopt_penalty` | Integration | Threshold multiplier applied after deoptimisation; blacklist after 3 |
| `test_jit_dump_format` | Integration | JIT-dump file parses correctly with `perf inject --jit` |
| `test_perf_map_format` | Integration | Perf map entries match installed code regions |
| `test_profiling_disabled` | Integration | Zero counter overhead when `BLISS_PROFILING_DISABLED=1` |
| `bench_profiling_overhead` | Benchmark | Wall-clock overhead ≤5% on gabriel/cl-bench suite |

---

## 4.9.12 Module Map

```
crates/bliss-compiler/src/
├── profile/
│   ├── mod.rs              # re-exports, MethodCounters, TypeProfileRing
│   ├── counters.rs         # D4.04 MethodCounters impl
│   ├── type_profile.rs     # D4.14 TypeProfileRing impl
│   ├── trigger.rs          # A4.12 TierPromotionTrigger
│   ├── scheduler.rs        # background scheduler poll loop
│   ├── jit_dump.rs         # JIT-dump file writer (Linux)
│   ├── perf_map.rs         # perf map file writer (Linux)
│   └── dtrace.rs           # USDT probe definitions (macOS)
└── baseline.rs             # T1 codegen — emits counter stubs (§4.9.3)
```
