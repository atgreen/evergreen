# §2 Runtime Core

The Runtime Core is the lowest Rust-implemented layer of Bliss. It owns
process lifecycle (startup → run → shutdown), the thread model (OS worker
pool + M:N green threads), stack layout and frame walking, safepoint
synchronisation, POSIX signal handling, and the C-ABI FFI bridge.
Source lives in `crates/bliss-rt/src/` (see §0 directory map).

---

## 2.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R2.01 | The runtime MUST boot to a CL `REPL` prompt in < 50 ms cold on commodity hardware (see G3). | MUST |
| R2.02 | Startup MUST follow the sequence: Rust `main` → parse CLI → init GC heap → load/mmap image → call CL entry point. | MUST |
| R2.03 | The runtime MUST support a configurable OS worker-thread pool (default = number of hardware threads). | MUST |
| R2.04 | Green threads (fibers) MUST be multiplexed M:N onto the worker pool with cooperative yield at safepoints. | MUST |
| R2.05 | Each green thread MUST maintain its own CL control + value stack, separate from the Rust shadow stack. | MUST |
| R2.06 | Stack frames MUST be walkable by the debugger and GC without stopping the world (safepoint maps). | MUST |
| R2.07 | Safepoints MUST be implemented via a polling-page mechanism; a safepoint poll MUST be inserted at every loop back-edge and function prologue. | MUST |
| R2.08 | `SIGSEGV` on the poisoned safepoint page MUST be caught and converted to a safepoint trap, not a crash. | MUST |
| R2.09 | `SIGSEGV` on a null-tagged pointer dereference MUST raise a CL `TYPE-ERROR` condition (null-pointer guard pages). | MUST |
| R2.10 | `SIGINT` (Ctrl-C) MUST be delivered as a CL `INTERRUPT-CONDITION` to the foreground green thread. | MUST |
| R2.11 | FFI calls to C functions MUST use the platform C ABI (System V AMD64 / AAPCS64). | MUST |
| R2.12 | The FFI bridge MUST support callbacks from C into CL (closure trampolines). | MUST |
| R2.13 | Alien type marshalling MUST handle: signed/unsigned integers (8–64 bit), float, double, pointer, struct-by-value, and `void`. | MUST |
| R2.14 | The runtime SHOULD use `libffi` for variadic and struct-by-value calls; hand-rolled stubs MAY be used for hot-path leaf calls. | SHOULD |
| R2.15 | FFI calls MUST transition the calling green thread to a "native" state that does not block GC safepoints. | MUST |
| R2.16 | Environment variables listed in §2.8 MUST be read before any heap allocation. | MUST |
| R2.17 | Shutdown MUST run all registered finalizers, join worker threads, and exit with a CL-controlled exit code. | MUST |
| R2.18 | The runtime MUST NOT call `panic!()` in any production code path; all errors propagate via `Result<T, BlissError>`. | MUST NOT |
| R2.19 | The runtime MUST support at least 100 000 simultaneous green threads on a 64-bit system with default stack sizes. | MUST |
| R2.20 | Stack overflow on a CL stack MUST raise `STORAGE-CONDITION`, not a process-killing signal. | MUST |

---

## 2.2 Process Startup Sequence

```text
 ┌─────────────────────────────────────────────────────────────┐
 │ 1. Rust main()                                              │
 │    └─ parse CLI args + env vars (§2.8)                      │
 │                                                             │
 │ 2. Platform init                                            │
 │    ├─ install signal handlers (§2.6)                        │
 │    ├─ allocate safepoint page (§2.5)                        │
 │    └─ seed PRNG, detect CPU features                        │
 │                                                             │
 │ 3. GC init (§3)                                             │
 │    ├─ mmap nursery + old-gen regions                        │
 │    └─ init write-barrier metadata                           │
 │                                                             │
 │ 4. Image load                                               │
 │    ├─ mmap .bimg (§0.5.4) or fall back to bootstrap reader  │
 │    ├─ verify image header + magic number                    │
 │    └─ restore symbol table + package registry (offset-based)│
 │                                                             │
 │ 5. Worker-pool start                                        │
 │    └─ spawn N OS threads (§2.3), each with TLAB (§3)        │
 │                                                             │
 │ 6. CL entry                                                 │
 │    ├─ run *INIT-HOOKS*                                      │
 │    ├─ if --eval / --load: execute, then exit                │
 │    └─ else: enter REPL loop                                 │
 └─────────────────────────────────────────────────────────────┘
```

**R2.02** is satisfied by executing steps 1–6 in strict order.
Steps 1–3 target < 5 ms total; step 5 is bounded by
`pthread_create` latency.

**Image load cost model (step 4):** The `.bimg` format stores
absolute heap pointers.  Step 4 `mmap`s the heap section and, if the
mapped base differs from `original_base` recorded in the image header,
executes an O(n) pointer relocation pass (A7.01, §7.2.10) over the
relocation table.  When the OS maps the image at the original base
(the common case), relocation is skipped and load is a near-O(1)
`mmap`.  The symbol table and package registry are stored as separate
image sections (§7.2.7, §7.2.8) and are read via fixed-size directory
entries.  Under typical conditions (no relocation, 64 MB image) the
cold-start image load completes in < 50 ms (R7.02), well within the
R2.01 budget.

### 2.2.1 Bootstrap vs Image Boot

On first build (no `.bimg` exists), step 4 falls back to the
**bootstrap reader**: a minimal Rust-implemented `READ`/`EVAL` loop
that loads `lib/boot.lisp`. The bootstrap path is slower (hundreds of
ms) and is only used during development and cross-compilation (§0.4.5).

---

## 2.3 Thread Model

### 2.3.1 Architecture

```text
  ┌──────────────────────────────────┐
  │        Green-thread scheduler    │
  │  (work-stealing deque per worker)│
  └──────┬───────┬───────┬──────────┘
         │       │       │
    ┌────▼──┐┌───▼───┐┌──▼────┐
    │Worker0││Worker1││WorkerN│    OS threads (1 per core)
    └───────┘└───────┘└───────┘
```

| Component | Description |
|-----------|-------------|
| **Worker thread** | A pinned OS thread. Owns a TLAB (§3), a work-stealing deque, and the Rust call stack. |
| **Green thread (fiber)** | A CL-visible thread. Has its own `BlissStack` (§2.4), a state machine (`Runnable` / `Blocked` / `Native` / `Dead`), and a continuation pointer. |
| **Scheduler** | Per-worker run-queue (LIFO push/pop) with cross-worker stealing (FIFO). Scheduling decisions happen only at safepoints (§2.5). |

### 2.3.2 State Machine

```text
                    mutex/condvar wait
 ┌──────────┐ ──────────────────────► ┌──────────┐
 │ Runnable │ ◄────────────────────── │ Blocked  │
 └──────────┘     notify/unlock       └──────────┘
   │      ▲                              
   │      │ FFI return /                 
   │      │ callback entry               
   │      │                              
   │  ┌───┴──────┐                       
   │  │  Native  │                       
   │  └──────────┘                       
   │      ▲                              
   │      │ FFI call                     
   │      │                              
   │  ┌───┴──────┐     async I/O         
   ├──┤          │ ◄── submit ───────── (from Runnable)
   │  └──────────┘                       
   │                                     
   │  async I/O          ┌──────────┐    
   ├─── submit ────────► │ Waiting  │    
   │                     └────┬─────┘    
   │                          │ I/O ready
   │ ◄────────────────────────┘          
   │                                     
   │  entry fn returns / kill            
   ▼                                     
 ┌──────────┐
 │   Dead   │
 └──────────┘
```

Transitions summary:

- **Runnable → Blocked:** mutex lock, condvar wait, channel receive.
- **Blocked → Runnable:** mutex unlock, condvar notify, channel send.
- **Runnable → Native:** FFI call (§2.7).
- **Native → Runnable:** FFI return, or callback entry back into CL.
- **Runnable → Waiting:** async I/O submit.
- **Waiting → Runnable:** I/O ready (epoll/kqueue notification).
- **any → Dead:** entry function returns or thread is killed.

- **Runnable:** On a worker's run-queue; may be executing.
- **Blocked:** Waiting on a mutex, condition variable, or channel.
- **Native:** Executing a C FFI call (§2.7); does not hold GC
  handshake obligations — the safepoint scanner skips it until it
  transitions back.
- **Waiting:** Blocked on async I/O (epoll/kqueue fd). An I/O poller
  green thread moves it to Runnable when the fd is ready.
- **Dead:** Returned from its entry function or killed. Resources
  pending finalization.

### 2.3.3 Green Thread Creation

```rust
/// D2.01 — Green thread descriptor.
pub struct GreenThread {
    id:          GreenThreadId,       // monotonic u64
    state:       AtomicU8,            // enum ThreadState
    stack:       BlissStack,          // §2.4
    entry:       BlissVal,            // CL function to call
    result:      UnsafeCell<BlissVal>,
    join_waker:  AtomicWaker,         // for join semantics
    tls_slots:   Box<[BlissVal; MAX_TLS]>, // per-green-thread TLS
}
```

Creating a green thread (`BLISS-THREAD:MAKE-THREAD`) allocates a
`BlissStack` from a pool, sets `state = Runnable`, and pushes the
descriptor onto the current worker's deque. **R2.19**: with a default
CL stack of 512 KiB (guard-page protected), 100 000 threads require
~50 GiB of virtual address space — feasible on 64-bit with
overcommit/lazy mapping.

### 2.3.4 Scheduling Policy

1. A worker pops from its own deque (LIFO — cache-warm).
2. If empty, it attempts to steal from a random other worker (FIFO —
   fair, avoids starvation).
3. If all deques are empty, the worker parks on a futex and is woken by
   the next `make-thread` or I/O completion event.

Preemption is cooperative: a running green thread yields at the next
safepoint poll when the scheduler sets a per-thread yield flag (e.g.,
time-slice expired, higher-priority thread ready).

---

## 2.4 Stack Layout

Each green thread owns a `BlissStack`: a contiguous virtual memory
region used for CL control/value frames. The Rust call stack of the
OS worker thread (the "shadow stack") is separate.

### 2.4.1 Stack Regions

```text
 High address
 ┌──────────────────────┐
 │   Guard page (4 KiB) │  ← SIGSEGV on overflow → STORAGE-CONDITION (R2.20)
 ├──────────────────────┤
 │                      │
 │   CL frames (grow ↓) │
 │                      │
 ├──────────────────────┤
 │   Red zone (4 KiB)   │  ← overflow handler frame space
 ├──────────────────────┤
 │   Guard page (4 KiB) │  ← hard limit
 └──────────────────────┘
 Low address
```

Default usable size: 512 KiB (configurable via `BLISS_STACK_SIZE`).

### 2.4.2 Frame Format

Every CL call pushes a fixed-layout frame header followed by a
variable-size locals area.

```text
 ┌────────────────────────────────────┐  ← FP (frame pointer)
 │ prev_fp        : *mut Frame       │  8 bytes — linked list for walking
 │ return_pc      : *const u8        │  8 bytes — return address in native code
 │ function       : BlissVal         │  8 bytes — calling function (for debugger)
 │ code_info      : *const CodeInfo  │  8 bytes — safepoint map + source loc table
 │ flags          : u32              │  4 bytes — frame type, catch/unwind bits
 │ num_locals     : u16              │  2 bytes
 │ _pad           : u16              │  2 bytes (alignment)
 ├────────────────────────────────────┤
 │ locals[0]      : BlissVal         │
 │ locals[1]      : BlissVal         │
 │ ...                               │
 │ locals[N-1]    : BlissVal         │
 └────────────────────────────────────┘  ← SP (stack pointer)
```

**D2.02 — Frame header:** 40 bytes fixed. The `prev_fp` chain allows
O(n) stack walks without requiring metadata side-tables (R2.06).

### 2.4.3 Frame Types

| `flags` bits 1:0 | Type | Description |
|-------------------|------|-------------|
| `00` | `CALL` | Normal function call |
| `01` | `CATCH` | `CATCH` / `HANDLER-BIND` frame — holds tag + handler |
| `10` | `UNWIND` | `UNWIND-PROTECT` cleanup frame |
| `11` | `SPECIAL` | Special-variable binding frame (dynamic binding stack) |

### 2.4.4 Unified Control Stack (Interpreted + Compiled Frames)

**Every CL activation — interpreted (T0) or compiled (T1/T2) — lives as a
frame on the green thread's `BlissStack`** (R2.05), in the §2.4.2 format, so a
single call chain freely interleaves tiers and one frame walker (the `prev_fp`
chain) sees them all. This is the key mechanism deviation from HotSpot noted in
§0 §1.1: rather than a template (assembly) interpreter whose frames are native
machine-stack frames, Bliss's baseline interpreter is a host-language (Rust)
loop, but its frames still live on the CL stack — not on the OS worker's Rust
"shadow" stack.

The Rust shadow stack therefore holds only **transient, non-CL** activity: the
interpreter dispatch loop itself, GC inner loops, and runtime-internal helpers.
It never holds a durable CL activation, so a green thread can be parked or
migrated by saving its `BlissStack` pointer alone (§2.3) — interpreter state is
not stranded on a shared worker stack. Deep interpreted recursion consumes
`BlissStack` frames and raises `STORAGE-CONDITION` on overflow (R2.20), rather
than overflowing the Rust stack.

#### D2.03 — Interpreter Frame

An interpreted (T0) frame is a `CALL` frame (§2.4.3) whose `code_info` marks it
interpreted and whose locals area is laid out as the bytecode function's slots
followed by its operand-stack slots:

```text
 │ …§2.4.2 header (code_info → InterpCodeInfo)… │
 ├────────────────────────────────────────────┤
 │ bcp        : *const u8   │ current bytecode PC (the stable OSR/deopt coord) │
 │ constants  : *const …    │ constant pool / reference table for this fn      │
 │ sp_top     : u16         │ operand-stack depth within the locals area       │
 ├────────────────────────────────────────────┤
 │ local[0..n_locals]       │ lexical slots                                    │
 │ opstack[0..max_stack]    │ operand stack (grows within the frame)           │
 └────────────────────────────────────────────┘
```

The operand stack lives **inside the frame**, not in a separate `ValueStack`
object; `bcp` is the same bytecode PC used for source maps, profiling, OSR
entry, and deopt resume (§4.4.3).

#### D2.04 — Interpreter↔Compiled Adapters (i2c / c2i)

Crossing tiers is an argument-shuffle, not a stack switch. A **c2i adapter**
(compiled→interpreted) moves register/stack ABI arguments into the callee's
interpreter frame slots; an **i2c adapter** (interpreted→compiled) moves
operand-stack arguments into the compiled calling convention. Adapters are the
single-stack analog of a cross-world trampoline: control and both frames stay
on the one `BlissStack`, so OSR and deoptimisation (§4.6) rebuild or unwind
frames in place without bridging two stacks.

#### GC of the control stack

CL frames are scanned **precisely** via the frame chain: compiled frames use
their safepoint stack maps (§4.7), and interpreter frames use per-`bcp` operand
maps derived by abstract interpretation of the bytecode (the interpreter knows
which slots hold references). Conservative scanning is confined to the Rust
shadow stack's transient VM frames, and never pins CL data. This removes the
conservative pinning of interpreter values implied by an all-shadow-stack T0.

### 2.4.5 Interpreter Realisation: Host-Loop vs. Template

The unified-stack model above is independent of *how* the interpreter is coded.
Two realisations satisfy it; Bliss ships the first and keeps the second as an
explicit, deferred option.

| | **Host-loop (baseline)** | **Template (later option)** |
|---|---|---|
| Interpreter body | Rust dispatch loop over bytecode | Generated machine-code stub per bytecode (via the codegen emitter, §4.7) |
| Frames | On `BlissStack` (D2.03), driven by the Rust loop | On `BlissStack`, native-stack frames |
| Cost | No codegen prerequisite; portable; debuggable | Assembly interpreter per ISA (x86-64 + aarch64); largest single component |
| Payoff | One stack, exact maps, cheap fiber park, mixed-tier chains | The above **plus** raw interpreter throughput / full HotSpot fidelity |

The host-loop realisation already delivers the architectural wins (single stack,
precise maps, cheap M:N parking, uniform OSR/deopt). A template interpreter is
warranted only if interpreter throughput becomes a hard requirement, at which
point it is a localized swap because the frame format (D2.03), adapters (D2.04),
and maps are already defined here.

**Note — two axes decide whether the interpreter shares the compiled stack.**
Whether interpreted activations land on the one control stack depends on two
independent properties: (1) *recursive* (a tree-walker: one host frame per
subform) vs. *explicit-stack* (a bytecode loop that pushes/pops its own frames),
and (2) *host-language* (Rust) vs. *compiled to native code*. A true single stack
requires **explicit-stack _or_ compiled** — the only combination that fails is
*recursive and host-language*, which is exactly today's tree-walker (its Rust
recursion puts CL activations on the Rust stack, a separate stack from
`BlissStack` — the two-stack condition bliss-nmq removes).

- The **host-loop baseline above qualifies because it is a _bytecode_ loop**:
  `CALL`/`RETURN` push and pop D2.03 frames on the `BlissStack`, so CL
  activations live there and only the single dispatch-loop frame (plus transient
  helpers) sits on the Rust stack. It is host-language but not recursive.
- **SBCL reaches the same single control stack from the other axis.** Its
  `sb-fasteval` interpreter is a *recursive* code-walker, but it is **compiled
  native Lisp** — interpreted functions are `funcallable-structure` objects
  (`sbcl/src/interpreter/function.lisp`), so their activations are ordinary
  compiled frames on the one per-thread control stack
  (`sbcl/src/runtime/thread.h`, `C_STACK_IS_CONTROL_STACK`). Notably SBCL still
  keeps interpreter **lexical environments on the heap** (a parent-linked
  `basic-env` chain — `sbcl/src/interpreter/basic-env.lisp`: "never
  stack-allocates any ENV"). So **call-frame unification and locals-in-frame are
  separable**: SBCL unifies the control stack while heap-allocating bindings,
  whereas D2.03's in-frame locals are a bytecode-VM / compiled-code property that
  a lowering pass makes available. Bliss's current `EnvFrame` chain
  (`Rc<RefCell<EnvFrame>>`) matches SBCL's heap-env model; moving *call frames*
  onto the `BlissStack` is the separable step, and the bytecode loop is what
  achieves it without the interpreter itself being native code.

---

## 2.5 Safepoint Mechanism

### 2.5.1 Polling Page

A single page (`safepoint_page`) is `mmap`'d read-only at startup. At
every safepoint poll site, generated code performs:

```asm
; x86-64 safepoint poll (4 bytes — fits in instruction cache)
test  [rip + safepoint_page_offset], eax   ; load from polling page
```

- **Normal operation:** Page is mapped readable; the `test` is a no-op
  (1 cycle, no side effects).
- **Safepoint requested:** The GC (or scheduler) calls
  `mprotect(safepoint_page, PROT_NONE)`. The next poll triggers
  `SIGSEGV`, which the signal handler (§2.6) converts into a safepoint
  trap.

### 2.5.2 Poll-Site Placement (R2.07)

Safepoint polls are inserted by the compiler at:

1. **Function prologues** — ensures that a long call chain eventually
   reaches a safepoint.
2. **Loop back-edges** — ensures that tight loops cannot prevent GC.
3. **Allocation slow-paths** — TLAB exhaustion already checks in.

The interpreter (T0) checks the safepoint flag at the top of its
dispatch loop.

### 2.5.3 Safepoint Actions

When a thread reaches a safepoint it executes (in order):

1. **Record stack top** — publish `sp` and `fp` to thread descriptor.
2. **Signal "at safepoint"** — atomic flag visible to GC coordinator.
3. **Wait if GC pending** — spin/park until GC completes.
4. **Check yield flag** — yield to scheduler if preemption requested.
5. **Resume** — return to CL code.

### 2.5.4 Time-to-Safepoint Guarantee

The maximum time between safepoint polls MUST be bounded:

| Tier | Guarantee |
|------|-----------|
| T0 (interpreter) | Every bytecode dispatch (< 1 µs) |
| T1 (baseline) | Every back-edge + prologue (< 10 µs typical) |
| T2 (optimising) | Same, but compiler may hoist polls out of counted loops with known bounds ≤ 1024 iterations |

---

## 2.6 Signal Handling

All signal handlers are installed in step 2 of startup (§2.2) using
`sigaction` with `SA_SIGINFO | SA_ONSTACK` on a dedicated alt-stack per
OS worker thread.

### 2.6.1 Signal Table

| Signal | Source | Action |
|--------|--------|--------|
| `SIGSEGV` | Safepoint page fault | Enter safepoint trap (§2.5) |
| `SIGSEGV` | Null-guard page fault | Raise CL `TYPE-ERROR` (R2.09) |
| `SIGSEGV` | Stack guard page | Raise CL `STORAGE-CONDITION` (R2.20) |
| `SIGSEGV` | Other | Abort with diagnostic dump |
| `SIGBUS`  | Alignment / bad mmap | Abort with diagnostic dump |
| `SIGINT`  | Ctrl-C | Deliver `INTERRUPT-CONDITION` to foreground thread (R2.10) |
| `SIGTERM` | Process termination | Initiate graceful shutdown (§2.9) |
| `SIGPIPE` | Broken pipe | Ignored (`SIG_IGN`); I/O functions check `EPIPE`. |

### 2.6.2 Distinguishing SIGSEGV Sources (R2.08, R2.09, R2.20)

The `SIGSEGV` handler inspects `siginfo_t.si_addr`:

```rust
fn sigsegv_handler(sig: c_int, info: &siginfo_t, uctx: &mut ucontext_t) {
    let addr = info.si_addr as usize;
    if is_in_safepoint_page(addr) {
        enter_safepoint_from_signal(uctx);     // redirect PC to safepoint stub
    } else if is_in_null_guard_region(addr) {
        raise_cl_type_error(uctx);             // unwind to CL handler
    } else if is_in_stack_guard(addr) {
        raise_cl_storage_condition(uctx);
    } else {
        crash_with_dump(sig, info, uctx);      // unrecoverable
    }
}
```

The guard regions are pre-registered per green thread at creation and
per heap region at allocation, allowing O(1) range checks via a sorted
interval table.

### 2.6.3 Async-Signal-Safety

Signal handlers MUST only call async-signal-safe functions (per POSIX).
All condition-raising paths defer to the CL stack by rewriting the
saved PC in `ucontext_t` to point at a stub that runs outside the
signal handler.

---

## 2.7 FFI Bridge

### 2.7.1 Calling Convention (R2.11)

Bliss-generated native code uses the platform C ABI (System V AMD64 on
Linux/macOS x86-64; AAPCS64 on aarch64). This means CL-compiled
functions can be called directly from C without wrapper overhead when
they use fixed-arity, non-variadic signatures.

### 2.7.2 Alien Type System (R2.13)

```text
 D2.03 — Alien type descriptors
 ┌──────────────────────────────────────────────────────┐
 │ AlienType enum                                       │
 │  ├─ Void                                             │
 │  ├─ Int { signed: bool, bits: u8 }   (8,16,32,64)   │
 │  ├─ Float                                            │
 │  ├─ Double                                           │
 │  ├─ Pointer(Box<AlienType>)                          │
 │  ├─ Struct { fields: Vec<AlienType>, packed: bool }  │
 │  ├─ Union  { variants: Vec<AlienType> }              │
 │  └─ FnPtr  { ret: Box<AlienType>,                    │
 │              args: Vec<AlienType>,                    │
 │              variadic: bool }                         │
 └──────────────────────────────────────────────────────┘
```

### 2.7.3 Marshalling

| CL type | Alien type | Direction | Notes |
|---------|-----------|-----------|-------|
| `FIXNUM` | `Int { signed: true, bits: 64 }` | Both | Unbox tag, reapply on return |
| `SINGLE-FLOAT` | `Float` | Both | Extract from tagged word |
| `DOUBLE-FLOAT` | `Double` | Both | Heap-allocated; pass value |
| `STRING` | `Pointer(Int{8})` | CL→C | UTF-8 copy with null terminator; pinned |
| `(ALIEN *)` | `Pointer` | Both | Raw pointer, no GC tracking |
| `STRUCT` | `Struct` by value | Both | Stack copy via `libffi` (R2.14) |
| `(SIMPLE-ARRAY (UNSIGNED-BYTE 8))` | `Pointer(Int{8})` + length | CL→C | Data pointer into the array's backing store (pinned for duration of call); length passed as a separate `size_t` argument. Caller must declare layout via `DEFINE-ALIEN-ROUTINE`. |
| `(SIMPLE-ARRAY <element-type>)` | `Pointer(<alien>)` + length | CL→C | Same pin-and-pass strategy; element type maps per this table. The C side receives a raw pointer to contiguous element data. |

### 2.7.4 Call Flow

```text
  CL code                   FFI trampoline              C function
  ───────                   ──────────────              ──────────
  1. marshal args           2. transition thread        4. execute
     into C layout             to Native state
                            3. call via fn ptr ─────►
                            5. transition back  ◄─────  return
  6. unmarshal return          to Runnable
     into BlissVal
```

Step 2 (R2.15): Before entering C code, the green thread sets
`state = Native` and publishes its stack top. This tells the GC that
the thread's CL stack is quiescent and scannable without cooperation.

### 2.7.5 Callbacks (R2.12)

When C code needs to call back into CL:

1. `BLISS-FFI:MAKE-CALLBACK` allocates an executable trampoline (a
   small code stub on a writable+executable page).
2. The trampoline saves C callee-save registers, transitions the
   thread from `Native` to `Runnable`, pushes a trampoline frame on
   the CL stack, and calls the CL closure.
3. On return, the trampoline restores C registers, transitions back
   to `Native`, and returns the marshalled result.

Trampolines are allocated from a pool of executable pages (one page =
~100 trampolines at 40 bytes each). They are freed when the CL
callback object is GC'd (weak reference + destructor).

### 2.7.6 libffi vs Hand-Rolled Stubs (R2.14)

| Path | Mechanism | When |
|------|-----------|------|
| General calls (variadic, struct-by-value, unknown arity) | `libffi` `ffi_call` | Default path |
| Known-arity leaf calls (≤ 6 integer/pointer args, no struct) | Direct `call` via fn ptr | Compiler emits inline stub when alien type signature is static |

The compiler decides at compile time based on the `DEFINE-ALIEN-ROUTINE`
declaration. Hand-rolled stubs avoid the ~50 ns overhead of
`libffi_call` for hot foreign calls.

---

## 2.8 Configuration

### 2.8.1 Environment Variables (R2.16)

| Variable | Default | Description |
|----------|---------|-------------|
| `BLISS_HEAP_SIZE` | `512m` | Initial old-gen heap reservation |
| `BLISS_TLAB_SIZE` | `2m` | Per-thread TLAB size (§3.2.2, `--tlab-size`) |
| `BLISS_NURSERY_SIZE` | `64m` | Total nursery region pool (§3.2.2, `--nursery-size`) |
| `BLISS_STACK_SIZE` | `512k` | CL stack size per green thread |
| `BLISS_WORKERS` | `nproc` | OS worker thread count |
| `BLISS_IMAGE` | `bliss.bimg` | Path to boot image |
| `BLISS_GC_LOG` | (none) | Path to GC log file (enables GC logging) |
| `BLISS_JIT_DUMP` | `0` | `1` = emit `jitdump` file for `perf` |
| `BLISS_SAFEPOINT_SPIN` | `1000` | Spin iterations before parking at safepoint |
| `BLISS_FFI_POOL_PAGES` | `4` | Executable pages for callback trampolines |

All environment variables are read once in step 1 of startup (§2.2)
before any heap allocation occurs.

### 2.8.2 CLI Arguments

```text
bliss [options] [-- CL-args...]

Options:
  --image PATH        Override BLISS_IMAGE
  --eval FORM         Evaluate FORM and exit
  --load FILE         Load FILE and exit
  --no-image          Bootstrap from lib/boot.lisp (no image)
  --workers N         Override BLISS_WORKERS
  --heap-size SIZE    Override BLISS_HEAP_SIZE
  --help              Print usage and exit
  --version           Print version and exit
```

Arguments after `--` are passed to CL as
`BLISS-EXT:*COMMAND-LINE-ARGUMENTS*`.

---

## 2.9 Shutdown Lifecycle (R2.17)

```text
 1. CL entry function returns (or EXIT is called with code N).
 2. Run *EXIT-HOOKS* (list of thunks, LIFO order).
 3. Set global shutdown flag (atomic).
 4. Interrupt all green threads → each unwinds via UNWIND-PROTECT.
 5. Wait for all green threads to reach Dead state (timeout 5 s).
 6. Run pending finalizers (§3).
 7. Signal worker threads to exit; join all workers.
 8. Unmap heap, safepoint page, trampoline pool.
 9. Call libc exit(N).
```

If step 5 times out, the runtime logs a warning and proceeds with
forced shutdown (threads are detached, not joined).

---

## 2.10 Error Handling

### 2.10.1 Rust-Side Errors

All runtime-internal functions return `Result<T, BlissError>`.
`BlissError` is a non-exhaustive enum:

```rust
/// D2.04 — Runtime error type.
pub enum BlissError {
    Oom,                          // nursery + old-gen exhausted
    StackOverflow(GreenThreadId),
    InvalidImage(String),
    FfiError(String),
    SignalError(c_int),
    Shutdown,                     // clean shutdown requested
    Internal(String),             // invariant violation (bug)
}
```

`BlissError` MUST NOT be converted to `panic!` (R2.18). At the FFI
boundary, `BlissError` is translated into the appropriate CL condition
class.

### 2.10.2 CL Condition Mapping

| BlissError | CL Condition |
|------------|-------------|
| `Oom` | `STORAGE-CONDITION` |
| `StackOverflow` | `STORAGE-CONDITION` |
| `InvalidImage` | `BLISS-EXT:IMAGE-ERROR` (subclass of `ERROR`) |
| `FfiError` | `BLISS-FFI:FFI-ERROR` (subclass of `ERROR`) |
| `SignalError` | `BLISS-EXT:SIGNAL-ERROR` |
| `Shutdown` | Not signalled — initiates shutdown path |
| `Internal` | `BLISS-EXT:INTERNAL-ERROR` — always a bug, dump & abort |

---

## 2.11 Concurrency Contracts

| Resource | Synchronisation | Lock order |
|----------|----------------|------------|
| Green-thread run-queue (per worker) | Lock-free Chase-Lev deque | N/A |
| Global thread registry | `RwLock` | Before any per-thread lock |
| Safepoint page | `mprotect` (kernel-serialised) | N/A |
| Trampoline pool | `Mutex` | After thread registry |
| GC heap metadata | See §3 | See §3 |

Lock ordering MUST be respected to prevent deadlock.  Any new lock
MUST be added to this table with its position in the total order.

---

## 2.12 Test Strategy

| What | How |
|------|-----|
| Startup sequence | Integration test: spawn `bliss --eval '(quit 42)'`, assert exit code 42 and elapsed < 50 ms. |
| Green thread creation / join | Unit test: spawn 1 000 green threads each incrementing an atomic counter; assert final value. |
| Safepoint liveness | Unit test: tight loop in T1; verify GC completes within 100 ms. |
| Stack overflow | Unit test: deeply recursive function; assert `STORAGE-CONDITION` raised, stack intact. |
| Signal handling | Integration test: send `SIGINT` to process in `(sleep 10)`; assert `INTERRUPT-CONDITION` caught. |
| FFI round-trip | Unit test: call `strlen` on a CL string; call back into CL from C. |
| Shutdown | Integration test: register exit hook, verify it runs, verify clean exit. |
| Stress | Stress test: 100 000 green threads with FFI callbacks; no crash, no leak (valgrind/ASAN). |

---

## 2.13 Module Map

| Source file | Responsibility | Key types / functions |
|-------------|---------------|-----------------------|
| `crates/bliss-rt/src/startup.rs` | §2.2 boot sequence | `bliss_main()`, `load_image()` |
| `crates/bliss-rt/src/thread.rs` | §2.3 thread model | `GreenThread`, `WorkerThread`, `Scheduler` |
| `crates/bliss-rt/src/stack.rs` | §2.4 stack layout | `BlissStack`, `Frame`, `FrameWalker` |
| `crates/bliss-rt/src/safepoint.rs` | §2.5 safepoint | `SafepointPage`, `poll_safepoint()` |
| `crates/bliss-rt/src/signal.rs` | §2.6 signals | `install_handlers()`, `sigsegv_handler()` |
| `crates/bliss-rt/src/ffi.rs` | §2.7 FFI bridge | `AlienType`, `ffi_call()`, `make_callback()` |
| `crates/bliss-rt/src/config.rs` | §2.8 config | `RuntimeConfig`, `parse_cli()` |
