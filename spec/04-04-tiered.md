# §4.4  Tiered Compilation

**Scope.**  EGCL executes Common Lisp code through three tiers modelled on
the HotSpot JVM's interpreter → C1 → C2 pipeline.  This section specifies
each tier's internal mechanics, the decision logic for tier transitions, the
compilation queues, and the interaction with the profiling subsystem (§4.9).

**Implementation status.** The worker lifecycle below is the target design.
`bliss-ifnlh` tracks asynchronous T1 and `bliss-hrvj9` tracks removing
compilation-length GC exclusion. This specification does not claim that these
gaps are implemented or tested; existing coverage must be audited against the
new contracts.

---

## 4.4.1  Requirements

| ID | Requirement |
|----|-------------|
| R4.23 | Every user-defined function MUST begin execution at T0 (portable bytecode interpreter). |
| R4.24 | The runtime MUST request promotion of a function from T0 to T1 when its invocation counter reaches the T0→T1 threshold (default 10). |
| R4.25 | The runtime MUST request promotion of a function from T1 to T2 when its invocation counter reaches the T1→T2 threshold (default 4 096) **or** a back-edge counter in the function reaches the loop-heat threshold (default 10 000). |
| R4.26 | T1 and T2 native compilation MUST execute on dedicated background compiler workers from already-expanded bytecode, without repeating source-level compile-time effects. Expensive compiler work MUST permit moving GC to complete concurrently; a whole-compilation GC exclusion is forbidden. |
| R4.27 | While native compilation is queued or running, ordinary execution MUST continue in its current tier. Publication MUST validate definition generation and dependencies, register complete code/GC metadata, and atomically expose the entry at subsequent dispatch or matching OSR polls; stale results MUST be discarded. |
| R4.28 | If native compilation fails, the function MUST retain its usable tier and the failure MUST be logged. Structural failures MUST suppress retries for the same definition and compilation kind; transient failures MAY retry with backoff. Every terminal outcome MUST release request ownership and roots. |
| R4.29 | Tier thresholds MUST be configurable at startup via environment variables (`EGCL_T0_T1_THRESHOLD`, `EGCL_T1_T2_THRESHOLD`, `EGCL_LOOP_HEAT_THRESHOLD`). |
| R4.30 | T1 and T2 compilation queues MUST be separate and bounded (default 64 entries each). Requests MUST be deduplicated through publication by function identity, definition generation, target tier, and OSR entry where applicable. A full queue MUST drop new automatic requests with a counter increment, without blocking or leaving the request marked pending. |

---

## 4.4.2  Tier Overview

```text
Managed source processing → expanded bytecode → T0 execution
                                                │ hotness request
                                                ▼
                                           T1 bounded queue
                                                │ dedicated worker
                                                ▼
                                      validated T1 publication
                                                │ further heat
                                                ▼
                                           T2 bounded queue
                                                │ dedicated workers
                                                ▼
                                      validated T2 publication

Execution continues in its current tier while requests are pending.
Loop heat may request an OSR entry before invocation-driven promotion.
```

Each tier is described in detail below.

---

## 4.4.3  T0 — Bytecode Interpreter

### 4.4.3.1  Purpose

T0 provides architecture-independent cold-code execution with low front-end
latency.  Every executable form is first lowered to portable EGCL bytecode;
every user-defined function starts by executing that bytecode.  T0 collects
invocation and bytecode back-edge counts so the runtime can decide when
native compilation is worthwhile.

This bytecode layer is the stable execution contract below source forms and
above native code.  It is the coordinate system for source maps, debugger
locations, profiling counters, OSR entry, deoptimisation resume, and portable
`.bfasl` files.

### 4.4.3.2  Data Structures

#### BytecodeFunction

The compiler front-end lowers a macroexpanded lambda/body form to a
`BytecodeFunction`.

```rust
pub struct BytecodeFunction {
    /// Encoded bytecode instructions.
    code: Vec<u8>,
    /// Literal constants referenced by bytecode operands.
    constants: Vec<EgclVal>,
    /// Symbol, package, class, and function references.
    references: Vec<BytecodeRef>,
    /// Bytecode PC -> source location mapping.
    source_map: Vec<SourceMapEntry>,
    /// Exception, cleanup, catch/tagbody, and restart metadata.
    unwind_table: Vec<UnwindEntry>,
    /// Bytecode PCs eligible for safepoints, back-edge counting, OSR, and deopt.
    pc_table: Vec<PcInfo>,
    /// Maximum operand stack depth required by verifier.
    max_stack: u16,
    /// Number of lexical local slots.
    n_locals: u16,
}
```

The bytecode verifier MUST check stack depth, local-slot bounds, branch
targets, literal/reference indices, unwind-table ranges, and safepoint/PC
metadata before a `BytecodeFunction` can execute or be serialized to FASL.

#### ValueStack

The bytecode interpreter uses an explicit operand stack rather than relying
on the Rust call stack for recursive evaluation.

```rust
/// Interpreter operand stack (per green-thread).
pub struct ValueStack {
    /// Bump-allocated backing store; grows toward higher addresses.
    slots: Vec<EgclVal>,
    /// Index of the first free slot.
    sp: usize,
}

impl ValueStack {
    pub fn push(&mut self, v: EgclVal);
    pub fn pop(&mut self) -> EgclVal;
    pub fn peek(&self, depth: usize) -> EgclVal;
    /// Reset to a saved stack pointer (for non-local exits).
    pub fn unwind_to(&mut self, saved_sp: usize);
}
```

| Field | Type | Description |
|-------|------|-------------|
| `slots` | `Vec<EgclVal>` | Growable backing array, default capacity 1 024 |
| `sp` | `usize` | Current stack pointer (index into `slots`) |

**Invariant:** `sp <= slots.len()`.  If `push` would exceed capacity, the
vector doubles via `Vec::reserve`, up to a configurable maximum depth
(default 65 536 slots, set via `EGCL_MAX_STACK_DEPTH`).

**Overflow handling:** If `push` would exceed the maximum depth, the
interpreter signals a `STORAGE-CONDITION` (ANSI CL §9.1) with a restart
`ABORT` that unwinds to the nearest `CATCH` or top-level REPL.  This
prevents unbounded memory growth, which is especially important when many
green threads each have their own `ValueStack` — without a cap, deep or
infinite recursion across N green threads could exhaust process memory.
The maximum depth SHOULD be set conservatively; users can raise it via the
environment variable when workloads legitimately require deep stacks.

#### Lexical Frame

Lexical environments are represented as frame slots addressed by bytecode
operands.  Closed-over slots are boxed into heap-allocated closure cells; all
other locals live in the current bytecode frame and are visible to the GC
through the frame's stack map.

```rust
pub struct BytecodeFrame {
    function: *const BytecodeFunction,
    pc: u32,
    locals: Vec<EgclVal>,
    stack_base: usize,
}
```

### 4.4.3.3  Eval Loop

The core interpreter is a dispatch loop over bytecode instructions.
Signature:

```rust
pub fn bytecode_loop(
    function: &BytecodeFunction,
    frame: &mut BytecodeFrame,
    stack: &mut ValueStack, thread: &mut EgclThread,
) -> Result<EgclVal, EgclError>;
```

Representative bytecodes:

| Bytecode | Behaviour |
|----------|-----------|
| `CONST k` | Push `constants[k]` |
| `LOAD_LOCAL i` / `STORE_LOCAL i` | Read/write lexical local slot |
| `LOAD_SPECIAL s` / `STORE_SPECIAL s` | Read/write dynamic value cell |
| `CALL argc` | Call function value with `argc` arguments |
| `TAIL_CALL argc` | Tail-call without growing the bytecode frame chain |
| `RETURN n` | Return `n` values |
| `BR pc` / `BR_IF_FALSE pc` | Branch to bytecode PC |
| `PUSH_UNWIND idx` / `POP_UNWIND` | Establish/remove cleanup or non-local-exit metadata |
| `THROW` / `RETURN_FROM` / `GO` | Transfer using the unwind table |
| `SAFEPOINT` | Poll GC/yield/interruption and publish frame roots |
| `BACK_EDGE pc` | Increment back-edge counter, poll safepoint, optionally request OSR |

On every function entry, the bytecode loop atomically increments the
callee's `invoke_count` and checks against `T0_T1_THRESHOLD`; if reached, it
requests background T1 compilation and continues in T0 (§4.4.4.2).  On every
`BACK_EDGE`, it increments the function's back-edge counter and checks OSR
eligibility.

### 4.4.3.4  Source-to-Bytecode Lowering

Special forms are handled by the bytecode compiler, not by the runtime
dispatch loop.  All macro expansion completes before lowering.  The compiler
must lower at least:

| Special Form | Lowering |
|-------------|----------|
| `IF` | Conditional branch bytecodes |
| `LET` / `LET*` | Local slot allocation and stores |
| `PROGN` | Sequential bytecode emission |
| `SETQ` | Local/special/global store bytecode |
| `QUOTE` | Constant-pool reference |
| `FUNCTION` | Closure object with bytecode or compiled entry |
| `LAMBDA` | Nested `BytecodeFunction` constant plus closure creation |
| `BLOCK` / `RETURN-FROM` | Unwind-table entries and transfer bytecodes |
| `TAGBODY` / `GO` | Label PCs and unwind-table entries |
| `CATCH` / `THROW` | Dynamic non-local-exit entries |
| `UNWIND-PROTECT` | Cleanup form always runs |
| `MULTIPLE-VALUE-CALL/BIND/PROG1` | Multiple-value protocol |
| `THE` | Type metadata attached to bytecode PC / value slot |
| `LOCALLY` | Declaration scope metadata |
| `LOAD-TIME-VALUE` | Load-time constant cell |
| `EVAL-WHEN` | Conditional compile/load/eval behavior before lowering |

A leading parameter `TYPE` declaration MUST be retained on the saved bytecode
function rather than discarded. At positive safety the T0/T1/T2 call boundary
MUST validate the assertion before executing the body and signal `TYPE-ERROR`
on failure. The successful edge supplies typed parameter metadata to the T2 SSA
entry block. An optimisation that bypasses the call boundary (including body
inlining or a compiled register entry) MUST preserve an equivalent assertion or
decline that transformation.

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
    /// Per-definition compilation state complements the request-key registry.
    pub flags: AtomicU16,
}
```

At T0, `invoke_count` is bumped at bytecode function entry and
`back_edge_count` is bumped by `BACK_EDGE` bytecodes.  This lets hot loops OSR
directly from bytecode to T2 even before a function has reached the call-count
threshold for T1. New dispatches stop counting while matching T2 code
is installed, including calls that fall back because of native depth limits.
It resumes after that code is removed or the function is redefined. Thus
`FUNCTION-INVOKE-COUNT` exposes tiering heat, not a complete lifetime total.
Explicit `EGCL-EXT:PROFILE` invalidates compiled linkage and pins selected
functions to T0 for exact counts (§4.9.3.1). Back-edge counting is unchanged.

---

## 4.4.4  T1 — Baseline Compiler

### 4.4.4.1  Purpose

T1 eliminates bytecode dispatch overhead via a fast, single-pass compilation
from bytecode to native code.  It trades code quality for compilation speed
(target: < 100 µs per function for typical sizes).

### 4.4.4.2  Compilation Trigger

When `invoke_count` reaches `T0_T1_THRESHOLD` (R4.24), the T0 eval loop
requests T1 compilation through the shared lifecycle (§4.4.6). Enqueueing
neither changes the installed tier nor waits for the result. Later dispatches
or matching OSR polls install completed artifacts and select the new entry.

An explicit forced-compilation operation MAY wait for its request. Such a
wait MUST be cooperative with safepoints and fiber scheduling, hold no lock
needed by the compiler or collector, and avoid waiting recursively on work
that requires the same occupied worker. It uses the same worker and validated
publication path; an automatic hotness request never waits for compilation.

### 4.4.4.3  Single-Pass Bytecode Lowering

The baseline compiler walks the saved bytecode and emits native code into a
private `CodeBuffer`, without constructing T2 SSA IR or re-expanding source.

```text
baseline_compile(snapshot) → RelocatableArtifact:
    buf ← new CodeBuffer
    emit_prologue(buf, snapshot.frame_shape)
    lower_bytecode(buf, snapshot.instructions, snapshot.reference_indices)
    emit_epilogue(buf)
    return buf + relocations + stack_maps + OSR_metadata
```

Values flow through the stack frame's spill area. Compilation returns an
unpublished artifact; resolving movable constants, registering GC metadata,
and making the entry executable belong to installation (§4.4.5.5).

### 4.4.4.4  Calling Convention

T1 (and T2) compiled code uses a EGCL-internal calling convention to avoid
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
| `rbx` | Thread pointer (`*mut EgclThread`) — callee-saved |
| `r12` | Heap allocation pointer (TLAB bump ptr) — callee-saved |
| `r13` | Heap allocation limit — callee-saved |
| `r14` | Frame pointer (EGCL) — callee-saved |
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

The `FnMeta` pointer stored in the frame allows the profiling subsystem (§4.9)
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
block-based SSA intermediate representation (§4.4 overview, §4.3 IR details)
and runs a full optimisation pass pipeline.

### 4.4.5.2  IR Construction

T2 constructs its IR from the macroexpanded **bytecode** (`egcl_rt::bytecode`,
§4.4) — the same bytecode T0 interprets and T1 compiles, so deopt/OSR `bcp`
coordinates line up by construction — not from the AST or T1 machine code. SSA
form is built directly using the Braun et al. (2013) algorithm — no
dominance-frontier-based φ insertion. `build_ir(bytecode_fn) → Function`
abstractly interprets the bytecode (operand-stack slots and locals become SSA
values; jump targets start blocks), then validates SSA invariants (§4.3).

Instruction categories: Constants, Arithmetic (`Add`/`Sub`/`Mul`/`Div`/`Mod`),
Comparison (`Eq`/`Lt`/`NumEq`), Terminators (`Brif`/`Jump`/`Return`/`TailCall`/`Trap`),
Call, Memory (`Load`/`Store`/`Alloc`/`WriteBarrier`), Type ops
(`TypeCheck`/`Box`/`Unbox`/`Guard`), CL-specific (`ConsCreate`/`Car`/`Cdr`/`Values`/`MvBind`).
Control-flow merges are block parameters, not instructions.

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

T1 uses one dedicated OS compiler thread; T2 uses a separate pool (default:
2 OS threads, configurable via `EGCL_T2_THREADS`). Separate queues keep a
long optimizing job from occupying baseline compilation capacity. Both tiers
share request ownership, completion, cancellation and publication rules.
Each worker loops: wait for a request → compile → return a relocatable artifact.
The requesting managed execution installs completions at dispatch or a
back-edge poll, respecting ownership of thread-local registries.

The worker's expensive phase operates on owned, immutable compiler data.
Bytecode instructions, control-flow metadata and profile snapshots can be
copied; movable constants and definition dependencies use stable handles or
indices into registered, GC-updated roots. A copied `EgclVal`, raw pointer,
or `Vec<EgclVal>` alone is not such a handle. Workers MUST NOT dereference
movable objects outside a bounded managed-access phase, nor retain such a
borrow across that phase. Resolving a handle requires the same discipline.
Code buffers carry relocations rather than embedded stale object addresses.

During expensive compilation, workers are outside the mutator rendezvous in
a GC-safe state. They MUST NOT hold a GC-excluding lock until the job finishes;
yielding only at pass boundaries is insufficient. Root lifetimes cover queued,
running, and completed-but-unpublished jobs. Snapshot preparation and
installation use bounded managed phases and safepoint cooperation; neither
may hide an unbounded traversal under GC exclusion.

Workers MUST NOT invoke Lisp expanders, source readers, user constant-folding
functions, or other Lisp callbacks. Missing bytecode or an unsupported
optimization causes deferral or a normal compilation failure, never hidden
source processing on a worker. See [§4.2](04-02-macroexpand.md) for the managed
source-processing boundary. A future compiler written in Lisp would instead
need managed compiler threads with ordinary roots and safepoints.

### 4.4.5.5  Validated Code Publication

Installation is a short managed transaction for either tier:

1. Verify the function identity, definition generation, compilation kind, and
   recorded dependencies still match. Reject an obsolete artifact or one
   superseded by an already-installed higher tier.
2. Resolve constant relocations from current rooted handles. Register code,
   stack maps, unwind/deoptimization maps and OSR entries before exposing code.
3. Under synchronization shared with redefinition/invalidation, revalidate
   assumptions and publish the entry with release ordering. Dispatch loads
   with acquire ordering; tier diagnostics must describe that published entry.
4. Complete the request and release its roots, retaining the installed code's
   own roots. Rejected or failed installation releases unpublished resources.

No caller may observe partially initialized metadata. Redefinition cannot race
between the final validity check and publication. Old code remains live while
active frames reference it and is reclaimed under code-cache GC (§3).
Existing activations continue until a matching, validated OSR entry is ready.

---

## 4.4.6  Compilation Queue Management

### 4.4.6.1  Data Structure — D4.07 (CompilationRequest)

The conceptual request contains:

| Field | Description |
|-------|-------------|
| Key | Stable function identity, definition generation, target tier, optional OSR bytecode PC/entry kind |
| Input | Owned bytecode and profile snapshot, stable constant handles, dependency generations |
| Owner | Managed publication destination and completion/cancellation state |
| Priority | Invocation count + twice back-edge heat |
| Enqueued time | Monotonic timestamp for FIFO tiebreaking |

Function identity is not merely its printable name or a movable heap address.
A generation distinguishes redefinitions even when the name or owner is reused.

### 4.4.6.2  Queue Implementation

Each tier uses a bounded maximum-priority queue with a short-held mutex and
condition variable, default capacity 64, and a drop counter. Workers release
the queue lock before compiling. Completed artifacts also require bounded
retention so a stalled owner cannot grow memory without limit.

Admission atomically reserves the request key. A duplicate queued, running,
or awaiting-publication request shares the existing work; a queued request
may gain priority. If admission fails or the queue is full, release the
reservation and increment the appropriate counter. The current tier remains
usable, and later heat can retry. Workers sleep on the queue condition while
idle, without participating as active mutators.

The reservation remains live through compilation and publication, and is
released on success, failure, stale-result rejection, cancellation or owner
shutdown. Cancellation disposes of queued/completed artifacts and lets running
jobs retire safely; it cannot free roots still used by a worker.

---

## 4.4.7  Tier Transition Decision Logic

### Algorithm A4.08 — Tier Promotion Decision

Invocation and back-edge paths select a candidate without changing the
installed tier:

```text
ALGORITHM A4.08 tier_promotion_decision(function, optional_osr_entry)
1. Read current definition generation, installed tier, and hotness.
2. Choose eligible target tier and compilation kind from hotness/OSR policy.
3. If no target, structurally failed for this key, or in retry backoff: return.
4. Form key = (identity, generation, target tier, optional OSR entry).
5. Try to reserve key and enqueue its owned snapshot in that tier's queue.
6. On duplicate: retain existing work; on full: release reservation, count drop.
7. Continue current execution; poll completions at later dispatch/OSR points.
```

Queue eligibility is not evidence of promotion. Only a successfully published
artifact changes the installed tier, and only an actual entry proves execution
in that tier.

---

## 4.4.8  Profiling Subsystem Interaction

The tiered compilation system depends on §4.9 (Profiling) for:

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
| Unsupported construct | Keep current tier | Log; suppress retries for this definition and compilation kind |
| Allocation/resource failure | Keep current tier | Log; release pending state; retry with backoff |
| Stale definition or dependency | Discard unpublished artifact | Release pending state and roots; new generation remains eligible |
| Code cache full | Keep current usable tier | Reclaim eligible inactive code; back off if still full |
| Compilation queue full | Request dropped (R4.30) | Increment drop counter; release reservation; remain eligible |
| Owner shutdown/cancellation | Discard request/result | Release roots when workers can no longer access them |

---

## 4.4.10  Concurrency

| Resource | Synchronisation | Lock Order |
|----------|-----------------|------------|
| `FnMeta` fields | Lock-free atomics (`AtomicU32`, `AtomicPtr`, etc.) | N/A |
| Per-tier compilation queue | `Mutex` + `Condvar` | Released before compilation or installation |
| Code publication and invalidation | Shared synchronization for generation check and entry publication | Never nested under queue lock; no lock held across code execution |
| IR node arena (per-compilation) | Thread-local bump allocator | No lock |
| Type profile slots | Per-slot `AtomicU64` (packed type + count) | N/A |

**Invariant:** No lock is ever held while running user code.  Compiler threads
hold the queue mutex only during enqueue/dequeue (microseconds).

---

## 4.4.11  Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `EGCL_T0_T1_THRESHOLD` | 10 | Invocation count to trigger T1 compilation |
| `EGCL_T1_T2_THRESHOLD` | 4 096 | Invocation count to trigger T2 compilation |
| `EGCL_LOOP_HEAT_THRESHOLD` | 10 000 | Back-edge count to trigger T2 compilation |
| `EGCL_DISABLE_T2` | unset | Set to a true value to keep hot functions at T1 for debugging/differential testing |
| `EGCL_T2_THREADS` | 2 | Number of T2 workers (T1 has a separate worker) |
| `EGCL_COMPILE_QUEUE_SIZE` | 64 | Maximum queued entries per tier |
| `EGCL_INLINE_LIMIT` | 30 | Maximum IR node count for inlining in T2 |
| `EGCL_T2_NODE_BUDGET` | 500 | Maximum IR nodes per function post-inlining |
| `EGCL_MAX_STACK_DEPTH` | 65 536 | Maximum ValueStack slots per green thread (overflow signals `STORAGE-CONDITION`) |

T2 is enabled during normal execution; it is not gated by an opt-in variable.
The profiling-specific spellings `EGCL_T1_T2_INVOKE_THRESHOLD` and
`EGCL_T1_T2_BACKEDGE_THRESHOLD` are accepted aliases for
`EGCL_T1_T2_THRESHOLD` and `EGCL_LOOP_HEAT_THRESHOLD`. The historical
`EGCL_T1_THRESHOLD` and `EGCL_T2_THRESHOLD` spellings remain compatibility
aliases. `EGCL_T2=1` remains a force/debug shorthand for making T2 eligible
immediately after T1, while `EGCL_T2=0` is a compatibility off-switch.

---

## 4.4.12  Test Strategy

| Test | Scope | Method |
|------|-------|--------|
| Tier transition correctness | Unit | Drive hotness, await bounded completion, and prove native entry at each installed tier; a counter or disassembly alone is insufficient |
| T0/T1 semantic equivalence | Integration | Run ansi-test under forced T0 and forced T1 (`EGCL_T0_T1_THRESHOLD=1`); diff results |
| T1/T2 semantic equivalence | Integration | Run ansi-test under forced T1 and forced T2 (`EGCL_T1_T2_THRESHOLD=1`); diff results |
| Compilation queue overflow | Unit | Fill queue to capacity; assert `try_enqueue` returns `false` and `dropped` increments |
| Concurrent T2 install | Stress | N threads calling hot function while T2 installs; no crashes or wrong results |
| Deoptimisation round-trip | Integration | Force type guard failure; assert correct fallback to T1 and eventual re-promotion |
| Code cache eviction | Unit | Fill code cache; assert LRU eviction and tier demotion |
| Threshold configurability | Unit | Set env vars; assert runtime reads correct values |

The worker-boundary regressions require deterministic coordination rather than
sleep-based timing:

- Pause a real T1 job, then a real T2 job, inside expensive compilation. While
  paused, prove another managed execution progresses and a moving collection
  completes. Verify relocation of a relevant rooted constant by comparing
  addresses before/after, then resume compilation, install, and execute native
  code with the correct constant/result. Stress/poison success without actual
  relocation does not establish this property.
- Redefine the function and invalidate a captured dependency while a job is
  paused; its old result must never replace the new definition. Exercise a
  slower T1 completion after T2 publication as well.
- Exercise duplicate requests while queued, running and awaiting publication;
  full queues; structural and transient failures; cancellation and owner
  shutdown. Verify bounded retention, released roots/reservations, and retry
  eligibility. Cover both invocation and OSR request kinds.
- Record macro, compiler-macro and compile-time evaluation activity during
  source processing. Subsequent T1/T2 promotion must leave that record unchanged.
  Check requesting dynamic bindings, handlers/restarts, and recursive source
  compilation under moving GC, including a cooperative explicit compilation wait.

These are acceptance criteria, not citations to existing test coverage.
