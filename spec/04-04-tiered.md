# §4.4  Tiered Compilation

**Scope.**  Bliss executes Common Lisp code through three tiers modelled on
the HotSpot JVM's interpreter → C1 → C2 pipeline.  This section specifies
each tier's internal mechanics, the decision logic for tier transitions, the
compilation queue, and the interaction with the profiling subsystem (§4.5).

---

## 4.4.1  Requirements

| ID | Requirement |
|----|-------------|
| R4.23 | Every user-defined function MUST begin execution at T0 (tree-walk interpreter). |
| R4.24 | The runtime MUST promote a function from T0 to T1 when its invocation counter reaches the T0→T1 threshold (default 10). |
| R4.25 | The runtime MUST promote a function from T1 to T2 when its invocation counter reaches the T1→T2 threshold (default 5 000) **or** a back-edge counter in the function reaches the loop-heat threshold (default 10 000). |
| R4.26 | T1 compilation MUST complete synchronously on the calling thread before the function's next invocation executes compiled code.  T2 compilation MUST execute on a background compiler thread. |
| R4.27 | While T2 compilation is in progress, the function MUST continue executing its T1 code; the switch to T2 code MUST be atomic (single pointer store, visible at the next call site). |
| R4.28 | If T2 compilation fails (e.g., unsupported construct, resource exhaustion), the function MUST remain at T1 permanently and the failure MUST be logged. |
| R4.29 | Tier thresholds MUST be configurable at startup via environment variables (`BLISS_T0_T1_THRESHOLD`, `BLISS_T1_T2_THRESHOLD`, `BLISS_LOOP_HEAT_THRESHOLD`). |
| R4.30 | The compilation queue MUST be bounded (default 64 entries); when full, new compilation requests MUST be dropped with a counter increment, not block the caller. |

---

## 4.4.2  Tier Overview

```text
                ┌─────────────┐
                │  T0  tree   │  first call
                │  walk eval  │──────────┐
                └─────────────┘          │ invoke_count ≥ T0_T1_THRESHOLD
                                         ▼
                                ┌─────────────┐
                                │ T1 baseline  │  synchronous compile
                                │  compiler    │──────────┐
                                └─────────────┘          │ invoke_count ≥ T1_T2_THRESHOLD
                                                         │  OR  back_edge_count ≥ LOOP_HEAT
                                                         ▼
                                                ┌─────────────┐
                                                │ T2 optimising│ background compile
                                                │  compiler    │
                                                └─────────────┘
```

Each tier is described in detail below.

---

## 4.4.3  T0 — Tree-Walk Interpreter

### 4.4.3.1  Purpose

T0 provides immediate execution with zero compilation latency.  Every
function starts here.  T0 collects invocation counts so the runtime can
decide when compilation is worthwhile.

### 4.4.3.2  Data Structures

#### ValueStack

The interpreter uses an explicit operand stack rather than relying on the
Rust call stack for recursive evaluation.

```rust
/// Interpreter operand stack (per green-thread).
pub struct ValueStack {
    /// Bump-allocated backing store; grows toward higher addresses.
    slots: Vec<BlissVal>,
    /// Index of the first free slot.
    sp: usize,
}

impl ValueStack {
    pub fn push(&mut self, v: BlissVal);
    pub fn pop(&mut self) -> BlissVal;
    pub fn peek(&self, depth: usize) -> BlissVal;
    /// Reset to a saved stack pointer (for non-local exits).
    pub fn unwind_to(&mut self, saved_sp: usize);
}
```

| Field | Type | Description |
|-------|------|-------------|
| `slots` | `Vec<BlissVal>` | Growable backing array, default capacity 1 024 |
| `sp` | `usize` | Current stack pointer (index into `slots`) |

**Invariant:** `sp <= slots.len()`.  If `push` would exceed capacity, the
vector doubles via `Vec::reserve`, up to a configurable maximum depth
(default 65 536 slots, set via `BLISS_MAX_STACK_DEPTH`).

**Overflow handling:** If `push` would exceed the maximum depth, the
interpreter signals a `STORAGE-CONDITION` (ANSI CL §9.1) with a restart
`ABORT` that unwinds to the nearest `CATCH` or top-level REPL.  This
prevents unbounded memory growth, which is especially important when many
green threads each have their own `ValueStack` — without a cap, deep or
infinite recursion across N green threads could exhaust process memory.
The maximum depth SHOULD be set conservatively; users can raise it via the
environment variable when workloads legitimately require deep stacks.

#### Environment Chain

Lexical environments form a singly-linked chain of `EnvFrame` nodes.
Each frame is heap-allocated when closures may capture it; stack-allocated
via arena otherwise (escape analysis in the interpreter is trivial: if the
body contains `LAMBDA`/`FLET`/`LABELS`, heap-allocate).

```rust
pub struct EnvFrame {
    /// Parent scope (None for the global environment).
    parent: Option<Arc<EnvFrame>>,
    /// Bindings: name → slot index.
    bindings: FxHashMap<SymbolId, BlissVal>,
}
```

### 4.4.3.3  Eval Loop

The core interpreter is a tail-recursive loop driven by an explicit
work stack that avoids deep Rust recursion.  Signature:

```rust
pub fn eval_loop(
    form: BlissVal, env: Arc<EnvFrame>,
    stack: &mut ValueStack, thread: &mut BlissThread,
) -> Result<BlissVal, BlissError>;
```

The loop classifies `current` and dispatches:

| `classify(current)` | Action |
|----------------------|--------|
| `SelfEval` | Return value immediately |
| `Symbol` | Look up in environment chain |
| `SpecialForm(kind)` | Handle (see §4.4.3.4); tail-position forms loop back via TCO |
| `FunctionCall` | Evaluate args, bump `invoke_count`, dispatch to interpreted body (tail-call → loop), compiled entry, or Rust builtin |
| `MacroCall` | `macroexpand_1`, then loop |

On every function call, the eval loop atomically increments the callee's
`invoke_count` and checks against `T0_T1_THRESHOLD`; if reached, it calls
`request_t1_compilation` synchronously (§4.4.4.2).

### 4.4.3.4  Special Form Handling

The interpreter recognises the following special forms directly (all others
are macro-expanded before reaching the eval loop):

| Special Form | Behaviour |
|-------------|-----------|
| `IF` | Evaluate test; tail-call into consequent or alternative |
| `LET` / `LET*` | Extend environment, tail-call body |
| `PROGN` | Evaluate forms in sequence; last form is tail-call |
| `SETQ` | Mutate binding in nearest enclosing `EnvFrame` |
| `QUOTE` | Return datum directly |
| `FUNCTION` | Close over current environment |
| `LAMBDA` | Create interpreted closure |
| `BLOCK` / `RETURN-FROM` | Establish / transfer to non-local exit via `ValueStack::unwind_to` |
| `TAGBODY` / `GO` | Looping via restart of tagged section |
| `CATCH` / `THROW` | Dynamic non-local exit |
| `UNWIND-PROTECT` | Cleanup form always runs |
| `MULTIPLE-VALUE-CALL/BIND/PROG1` | Multiple-value protocol |
| `THE` | Type declaration (ignored at T0, recorded for T2) |
| `LOCALLY` | Declarations wrapper |
| `LOAD-TIME-VALUE` | Evaluate once and cache |
| `EVAL-WHEN` | Conditional evaluation per situation |

### 4.4.3.5  Invocation Counter Maintenance

Every `FnMeta` struct (shared across tiers) contains:

```rust
pub struct FnMeta {
    pub name: SymbolId,
    /// Atomically incremented by T0 eval loop and T1 prologue stub.
    pub invoke_count: AtomicU32,
    /// Atomically incremented by T1 back-edge stubs.
    pub back_edge_count: AtomicU32,
    /// Current tier (read with Acquire, written with Release).
    pub tier: AtomicU8,           // 0, 1, or 2
    /// Pointer to the active entry point; updated atomically on tier change.
    pub entry: AtomicPtr<u8>,
    /// Flags: QUEUED_FOR_T2, T2_FAILED, NEVER_COMPILE, ...
    pub flags: AtomicU16,
}
```

At T0, only `invoke_count` is bumped (one `fetch_add` per call).  Back-edge
counting begins at T1 where profiling stubs are emitted.

---

## 4.4.4  T1 — Baseline Compiler

### 4.4.4.1  Purpose

T1 eliminates interpretation overhead via a fast, single-pass compilation to
native code.  It trades code quality for compilation speed (target: < 100 µs
per function for typical sizes).

### 4.4.4.2  Compilation Trigger

When `invoke_count` reaches `T0_T1_THRESHOLD` (R4.24), the T0 eval loop calls
`request_t1_compilation(meta, thread)`.  T1 compilation executes **synchronously**
on the calling thread (R4.26):

```rust
fn request_t1_compilation(meta: &FnMeta, thread: &mut BlissThread) {
    // Prevent double compilation.
    let prev = meta.tier.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    if prev.is_err() { return; }

    let code = baseline_compile(meta, thread.runtime);
    match code {
        Ok(compiled) => {
            let ptr = thread.runtime.code_cache.install(compiled);
            meta.entry.store(ptr, Ordering::Release);
        }
        Err(e) => {
            log::warn!("T1 compilation of {:?} failed: {}", meta.name, e);
            meta.tier.store(0, Ordering::Release); // fall back to T0
            if e.is_structural() {
                // Unsupported construct (e.g., inline assembly) — never retry.
                meta.flags.fetch_or(NEVER_COMPILE, Ordering::Release);
            }
            // Transient failures (e.g., OOM) leave NEVER_COMPILE unset,
            // allowing retry on next threshold hit as the counter keeps
            // incrementing.
        }
    }
}
```

### 4.4.4.3  Single-Pass AST Walk

The baseline compiler performs a single depth-first walk of the expanded AST.
For each node it emits native code directly into a mutable `CodeBuffer` — no
intermediate representation is constructed.

```text
baseline_compile(fn_meta) → CompiledCode:
    buf ← new CodeBuffer
    emit_prologue(buf, fn_meta)        // stack frame setup + invoke counter stub
    walk(buf, fn_body, fn_env_shape)   // recursive code emission
    emit_epilogue(buf)                 // return sequence
    buf.finalise()                     // flush icache, set executable
```

**No optimisation** is performed at T1: no register allocation beyond a fixed
set of scratch registers, no constant folding, no dead code elimination.  Values
flow through the stack frame's spill area.

### 4.4.4.4  Calling Convention

T1 (and T2) compiled code uses a Bliss-internal calling convention to avoid
the overhead of the platform C ABI on every Lisp-to-Lisp call:

| Register (x86-64) | Purpose |
|--------------------|---------|
| `rax` | Return value (primary) |
| `rdx` | Secondary return value / values count |
| `rdi` | Argument 0 (first arg) |
| `rsi` | Argument 1 |
| `rcx` | Argument 2 |
| `r8` | Argument 3 |
| `r9` | Argument 4 |
| `r10` | Scratch / argument 5 |
| `r11` | Scratch |
| `rbx` | Thread pointer (`*mut BlissThread`) — callee-saved |
| `r12` | Heap allocation pointer (TLAB bump ptr) — callee-saved |
| `r13` | Heap allocation limit — callee-saved |
| `r14` | Frame pointer (Bliss) — callee-saved |
| `r15` | Safepoint page address — callee-saved |
| `rbp` | Platform frame pointer (for unwinding) — callee-saved |
| `rsp` | Stack pointer |

Arguments beyond index 5 are passed on the stack.  `&REST` arguments are
collected into a freshly allocated list at the callee's prologue.

### 4.4.4.5  Stack Frame Layout (D4.08)

```text
       ┌──────────────────────────────┐  ← caller's rsp before CALL
       │      return address          │  [rbp + 8]
       ├──────────────────────────────┤
       │      saved rbp               │  [rbp]       ← rbp points here
       ├──────────────────────────────┤
       │      saved callee-saves      │  [rbp - 8..] (rbx, r12-r15 if used)
       ├──────────────────────────────┤
       │      FnMeta pointer          │  [rbp - K]   for profiling / deopt
       ├──────────────────────────────┤
       │      spill slot 0            │  [rbp - K-8]
       │      spill slot 1            │  [rbp - K-16]
       │      ...                     │
       │      spill slot N-1          │  [rbp - K-8N]
       ├──────────────────────────────┤
       │      overflow arg area       │  (args 6+, if any)
       ├──────────────────────────────┤
       │      alignment padding       │  (16-byte alignment before next CALL)
       └──────────────────────────────┘  ← rsp
```

**D4.07 — Compilation Request** is defined in §4.4.6 below (compilation queue).
**D4.08 — Stack Frame Layout** is the diagram above.

The `FnMeta` pointer stored in the frame allows the profiling subsystem (§4.5)
and the debugger (§6) to identify the function for any frame on the stack.

### 4.4.4.6  Profiling Stub Insertion

T1 emits lightweight profiling stubs at three sites:

1. **Function prologue** — `lock inc [fn_meta + INVOKE_OFFSET]`; compare
   against `T1_T2_THRESHOLD`; branch to cold path on `jge`.
2. **Loop back-edges** — `lock inc [fn_meta + BACKEDGE_OFFSET]`; compare
   against `LOOP_HEAT_THRESHOLD`; branch to cold path on `jge`.
3. **Call-site type feedback** — At each call site, the stub records the
   observed argument types into a `TypeProfile` attached to the call site's
   `FnMeta`.  Each `TypeProfile` is a fixed-size array (default 4 entries) of
   `AtomicU64` slots, each packing a `TypeTag` (upper 8 bits) and a saturating
   hit count (lower 56 bits).  The stub executes after argument evaluation and
   before the actual call:
   - For each argument (up to the first 4), load the runtime type tag.
   - Atomically update the corresponding `TypeProfile` slot: if the tag
     matches, increment the count; otherwise, if a free slot exists, CAS the
     tag in.  If all slots are occupied and none match, set a
     `MEGAMORPHIC` flag on the slot (T2 will not speculate on that argument).
   Type feedback stubs are emitted inline for ≤ 4 arguments; for functions
   with more arguments, only the first 4 are profiled.

The cold path (`.request_t2`) calls into the runtime to enqueue the function
for background T2 compilation (§4.4.6).

---

## 4.4.5  T2 — Optimising Compiler

### 4.4.5.1  Purpose

T2 produces high-quality native code for hot functions.  It operates on a
sea-of-nodes SSA intermediate representation (§4.4 overview, §4.3 IR details)
and runs a full optimisation pass pipeline.

### 4.4.5.2  IR Construction

T2 constructs its IR from the expanded AST (not T1 machine code).  SSA form
is built directly using the Braun et al. (2013) algorithm — no
dominance-frontier-based φ insertion.  `build_ir(ast, env_shape) → IRGraph`
walks the AST, creating blocks and parameter nodes, then validates SSA
invariants.

IR node categories: Constants, Arithmetic (`Add`/`Sub`/`Mul`/`Div`/`Mod`),
Comparison (`Eq`/`Lt`/`NumEq`), Control (`If`/`Jump`/`Return`/`Phi`/`Call`/`TailCall`),
Memory (`Load`/`Store`/`Alloc`/`WriteBarrier`), Type ops (`TypeCheck`/`Box`/`Unbox`),
CL-specific (`ConsCreate`/`Car`/`Cdr`/`Values`/`MvBind`).

### 4.4.5.3  Optimisation Pass Pipeline

Passes execute in a fixed order.  Each pass MUST be idempotent and MUST leave
the IR in valid SSA form.

| Order | Pass | Description |
|-------|------|-------------|
| 1 | Type propagation | Forward dataflow: infer types from declarations, `THE`, constant values, and call return types. Narrows type lattice at each node. |
| 2 | Inlining | Inline small callees (body ≤ 30 IR nodes or marked `INLINE`). Respects `NOTINLINE`. Budget cap: 500 nodes per function after inlining. |
| 3 | Escape analysis | Identify heap allocations that do not escape; replace with stack-allocated frames or scalar replacement. |
| 4 | Constant folding / algebraic simplification | Fold compile-time constants, simplify `(+ x 0)` → `x`, etc. |
| 5 | Dead code elimination | Remove nodes with no uses and no side effects. |
| 6 | Loop-invariant code motion | Hoist invariant computations above loop headers. |
| 7 | Strength reduction | Replace multiplication by constants with shifts/adds where profitable. |
| 8 | Common subexpression elimination | Hash-consing of pure nodes. |
| 9 | Type-check elimination | Remove redundant type checks proven safe by pass 1. |
| 10 | Lowering | Replace CL-level nodes with machine-level nodes (addressing modes, specific register constraints). |
| 11 | Register allocation | Linear-scan allocator over lowered IR. Spill slots assigned to the frame layout (§4.4.4.5). |
| 12 | Code emission | Walk scheduled nodes, emit machine code into `CodeBuffer`. Insert safepoint polls at back-edges. |

### 4.4.5.4  Background Thread Model

T2 compilation runs on a dedicated **compiler thread pool** (default: 2 OS
threads, configurable via `BLISS_T2_THREADS`).  Each thread loops:
`pop_blocking()` → `t2_compile()` → `install_code()` or `mark_t2_failed()`.

Compiler thread constraints:
- MUST NOT allocate into the Lisp heap (avoids triggering GC).
- MUST use a private bump allocator for IR nodes, freed in bulk post-compilation.
- MUST check the safepoint flag at pass boundaries and yield if GC is pending.

### 4.4.5.5  Code Installation via Atomic Swap

When T2 compilation completes, the new code is installed atomically:

```rust
fn install_code(meta: &FnMeta, compiled: CompiledCode) {
    let code_ptr = runtime.code_cache.install(compiled);
    // Atomic pointer store; subsequent calls pick up T2 code.
    meta.entry.store(code_ptr, Ordering::Release);
    meta.tier.store(2, Ordering::Release);
    // The old T1 code is NOT freed immediately — it may still be on
    // active stack frames. It becomes reclaimable at the next GC
    // safepoint when no frame references it (§3 code-cache GC).
}
```

**Semantics:** Because `entry` is updated with `Release` ordering and call
sites load it with `Acquire`, no thread can observe a partially-written
code pointer.  At worst, a thread executes one more call through T1 code
before seeing the T2 update — this is acceptable (R4.27).

---

## 4.4.6  Compilation Queue Management

### 4.4.6.1  Data Structure — D4.07 (CompilationRequest)

```rust
pub struct CompilationRequest {
    /// The function to compile.
    pub fn_meta: Arc<FnMeta>,
    /// Priority score (higher = compile sooner).
    pub priority: u32,
    /// Timestamp of request (monotonic, for age-based tiebreaking).
    pub enqueued_at: u64,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `fn_meta` | `Arc<FnMeta>` | Shared reference to function metadata |
| `priority` | `u32` | Computed from invocation count + back-edge heat |
| `enqueued_at` | `u64` | Monotonic timestamp for FIFO tiebreaking |

### 4.4.6.2  Queue Implementation

The compilation queue is a **bounded max-heap** (`BinaryHeap<CompilationRequest>`)
protected by a `Mutex` + `Condvar`, with capacity default 64 (R4.30) and an
`AtomicU64` drop counter.

**Enqueue semantics (R4.30):** If at capacity, increment `dropped` and return
`false` (never block).  If the function is already queued, update priority if
higher.  Otherwise push and `notify_one`.  Compiler threads call
`pop_blocking()` which waits on the condvar.

---

## 4.4.7  Tier Transition Decision Logic

### Algorithm A4.08 — Tier Promotion Decision

The promotion decision is split into two checks, both embedded in
profiling stubs emitted by T1 (§4.4.4.6) and the T0 eval loop
(§4.4.3.3):

```text
ALGORITHM A4.08  tier_promotion_decision(fn_meta)

Input:  fn_meta — FnMeta of the function under consideration
Output: action ∈ { NONE, COMPILE_T1, ENQUEUE_T2 }

1.  tier ← fn_meta.tier.load(Acquire)
2.  IF tier = 0:
3.      IF fn_meta.invoke_count.load(Relaxed) ≥ T0_T1_THRESHOLD:
4.          RETURN COMPILE_T1       // synchronous T1 compilation
5.      RETURN NONE
6.  IF tier = 1:
7.      IF fn_meta.flags contains T2_FAILED or NEVER_COMPILE:
8.          RETURN NONE             // do not re-attempt
9.      IF fn_meta.flags contains QUEUED_FOR_T2:
10.         RETURN NONE             // already in queue
11.     invoke_hot ← fn_meta.invoke_count.load(Relaxed) ≥ T1_T2_THRESHOLD
12.     loop_hot   ← fn_meta.back_edge_count.load(Relaxed) ≥ LOOP_HEAT_THRESHOLD
13.     IF invoke_hot OR loop_hot:
14.         SET fn_meta.flags |= QUEUED_FOR_T2
15.         priority ← fn_meta.invoke_count + 2 × fn_meta.back_edge_count
16.         ENQUEUE CompilationRequest { fn_meta, priority, now() }
17.         RETURN ENQUEUE_T2
18.     RETURN NONE
19. // tier = 2: already at max tier
20. RETURN NONE
```

**Notes:**
- Step 15: back-edge count is weighted 2× because loop-heavy functions
  benefit more from optimisation than call-heavy functions.
- The `QUEUED_FOR_T2` flag (step 9) prevents duplicate enqueues.  It is
  cleared by the compiler thread after compilation completes (or fails).

---

## 4.4.8  Profiling Subsystem Interaction

The tiered compilation system depends on §4.5 (Profiling) for:

| Interaction Point | Direction | Description |
|-------------------|-----------|-------------|
| Invocation counters | Tier → Profile | T0 eval loop and T1 prologue stubs increment `FnMeta.invoke_count` |
| Back-edge counters | Tier → Profile | T1 back-edge stubs increment `FnMeta.back_edge_count` |
| Type feedback | Profile → T2 | T1 profiling stubs record observed argument types into a `TypeProfile` attached to each call site; T2 uses this for speculative type specialisation |
| Deoptimisation | T2 → T0/T1 | If a speculative type guard in T2 code fails, the runtime deoptimises the frame back to T1 (or T0 for debugging) via on-stack replacement (§4.6) |
| Counter reset | Profile → Tier | On deoptimisation, invoke and back-edge counters are halved (not zeroed) to allow re-promotion without immediate re-trigger |

---

## 4.4.9  Error Handling

| Failure | Effect | Recovery |
|---------|--------|----------|
| T1 compilation of unsupported form | Function stays at T0 | Log warning; flag `NEVER_COMPILE` if structural (e.g., inline assembly) |
| T1 compilation OOM | Function stays at T0 | Log; retry on next threshold hit (counter keeps incrementing) |
| T2 compilation of unsupported construct | Function stays at T1 | Flag `T2_FAILED`; log (R4.28) |
| T2 compilation OOM | Function stays at T1 | Log; clear `QUEUED_FOR_T2`; may retry if counter doubles |
| Code cache full | Evict coldest T2 code (LRU) | Evicted function drops to T1; counters reset |
| Compilation queue full | Request dropped (R4.30) | `dropped` counter incremented; function re-eligible on next threshold hit |

---

## 4.4.10  Concurrency

| Resource | Synchronisation | Lock Order |
|----------|-----------------|------------|
| `FnMeta` fields | Lock-free atomics (`AtomicU32`, `AtomicPtr`, etc.) | N/A |
| `CompilationQueue.heap` | `Mutex` + `Condvar` | Before code-cache lock |
| Code cache installation | `RwLock` (write during install, read during execution) | After queue lock |
| IR node arena (per-compilation) | Thread-local bump allocator | No lock |
| Type profile slots | Per-slot `AtomicU64` (packed type + count) | N/A |

**Invariant:** No lock is ever held while running user code.  Compiler threads
hold the queue mutex only during enqueue/dequeue (microseconds).

---

## 4.4.11  Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `BLISS_T0_T1_THRESHOLD` | 10 | Invocation count to trigger T1 compilation |
| `BLISS_T1_T2_THRESHOLD` | 5 000 | Invocation count to trigger T2 compilation |
| `BLISS_LOOP_HEAT_THRESHOLD` | 10 000 | Back-edge count to trigger T2 compilation |
| `BLISS_T2_THREADS` | 2 | Number of background compiler threads |
| `BLISS_COMPILE_QUEUE_SIZE` | 64 | Maximum entries in the T2 compilation queue |
| `BLISS_INLINE_LIMIT` | 30 | Maximum IR node count for inlining in T2 |
| `BLISS_T2_NODE_BUDGET` | 500 | Maximum IR nodes per function post-inlining |
| `BLISS_MAX_STACK_DEPTH` | 65 536 | Maximum ValueStack slots per green thread (overflow signals `STORAGE-CONDITION`) |

---

## 4.4.12  Test Strategy

| Test | Scope | Method |
|------|-------|--------|
| Tier transition correctness | Unit | Call a function N times; assert `fn_meta.tier` progresses 0 → 1 → 2 |
| T0/T1 semantic equivalence | Integration | Run ansi-test under forced T0 and forced T1 (`BLISS_T0_T1_THRESHOLD=1`); diff results |
| T1/T2 semantic equivalence | Integration | Run ansi-test under forced T1 and forced T2 (`BLISS_T1_T2_THRESHOLD=1`); diff results |
| Compilation queue overflow | Unit | Fill queue to capacity; assert `try_enqueue` returns `false` and `dropped` increments |
| Concurrent T2 install | Stress | N threads calling hot function while T2 installs; no crashes or wrong results |
| Deoptimisation round-trip | Integration | Force type guard failure; assert correct fallback to T1 and eventual re-promotion |
| Code cache eviction | Unit | Fill code cache; assert LRU eviction and tier demotion |
| Threshold configurability | Unit | Set env vars; assert runtime reads correct values |
