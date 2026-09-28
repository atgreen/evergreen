# §2 Runtime Core

The Runtime Core is the lowest Rust-implemented layer of TorCL. It owns
process lifecycle (startup → run → shutdown), the thread model (exposed OS
carrier threads + M:N fibers), stack layout and frame walking, safepoint
synchronisation, POSIX signal handling, and the C-ABI FFI bridge.
Source lives in `crates/torcl-rt/src/` (see §0 directory map).

---

## 2.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R2.01 | The runtime MUST boot to a CL `REPL` prompt in < 50 ms cold on commodity hardware (see G3). | MUST |
| R2.02 | Startup MUST follow the sequence: Rust `main` → parse CLI → init GC heap → load/mmap image → call CL entry point. | MUST |
| R2.03 | The runtime MUST support configurable scheduler groups of exposed OS carrier threads (default carrier count = number of hardware threads). | MUST |
| R2.04 | Fibers MUST be multiplexed M:N onto a scheduler group's carrier threads with cooperative yield at safepoints. | MUST |
| R2.05 | Each fiber MUST maintain its own CL control + value stack, separate from its carrier's Rust shadow stack. | MUST |
| R2.06 | Stack frames MUST be walkable by the debugger and GC without stopping the world (safepoint maps). | MUST |
| R2.07 | Safepoints MUST be implemented via a polling-page mechanism; a safepoint poll MUST be inserted at every loop back-edge and function prologue. | MUST |
| R2.08 | `SIGSEGV` on the poisoned safepoint page MUST be caught and converted to a safepoint trap, not a crash. | MUST |
| R2.09 | `SIGSEGV` on a null-tagged pointer dereference MUST raise a CL `TYPE-ERROR` condition (null-pointer guard pages). | MUST |
| R2.10 | `SIGINT` (Ctrl-C) MUST be delivered as a CL `INTERRUPT-CONDITION` to the foreground fiber, or to the foreground native thread when no fiber is mounted. | MUST |
| R2.11 | FFI calls to C functions MUST use the platform C ABI (System V AMD64 / AAPCS64). | MUST |
| R2.12 | The FFI bridge MUST support callbacks from C into CL (closure trampolines). | MUST |
| R2.13 | Alien type marshalling MUST handle: signed/unsigned integers (8–64 bit), float, double, pointer, struct-by-value, and `void`. | MUST |
| R2.14 | Foreign calls and callbacks MUST use TorCL-generated ABI adapters, including variadic and struct-by-value calls. The runtime MUST NOT depend on `libffi`. Compiled call sites MAY lower the same ABI plan directly. | MUST |
| R2.15 | FFI calls MUST transition the calling fiber to a "native" state that does not block GC safepoints; a plain native thread publishes equivalent root state. | MUST |
| R2.16 | Environment variables listed in §2.8 MUST be read before any heap allocation. | MUST |
| R2.17 | Shutdown MUST run all registered finalizers, finish fibers, join carrier/native threads, and exit with a CL-controlled exit code. | MUST |
| R2.18 | The runtime MUST NOT call `panic!()` in any production code path; all errors propagate via `Result<T, TorclError>`. | MUST NOT |
| R2.19 | The runtime MUST support at least 100 000 simultaneous fibers on a 64-bit system with default stack sizes. | MUST |
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
  │          Fiber scheduler         │
  │ (work-stealing deque per carrier)│
  └──────┬───────┬───────┬──────────┘
         │       │       │
   ┌────▼────┐┌──▼──────┐┌─▼───────┐
   │Carrier 0││Carrier 1││Carrier N │  exposed OS threads (default 1/core)
   └─────────┘└─────────┘└─────────┘
```

| Component | Description |
|-----------|-------------|
| **Carrier thread** | An exposed `TORCL-THREAD` OS-backed thread. A scheduler group marks its carriers and gives each a TLAB, a work-stealing deque, and a Rust call stack. User-created native threads and group-owned carriers share the same public thread type. |
| **Fiber** | A distinct `TORCL-FIBER` lightweight managed execution. Has its own `TorclStack` (§2.4), dynamic state, saved continuation, and lifecycle independent of any one carrier. |
| **Scheduler** | Internal per-carrier run queue (LIFO push/pop) with cross-carrier stealing (FIFO). A public scheduler-group handle controls carrier/fiber lifecycle; scheduling decisions happen only at safepoints (§2.5). |

**R2.21** Native/carrier threads and fibers MUST be distinct public object
types. `TORCL-THREAD:MAKE-THREAD` creates a one-to-one OS-backed thread;
`TORCL-FIBER:MAKE-FIBER` creates a lightweight fiber. Scheduler-group carrier
threads MUST be observable through both thread introspection and the group.

#### Execution-context register — approved direction (2026-09-26)

Generated Lisp code will reserve a register pointing to the **current Lisp
execution context**, not directly to OS-thread TLS. The context identifies the
current fiber, or native-thread Lisp execution when no fiber is mounted, and
provides access to its dynamic bindings, Lisp execution state, current carrier,
and managed/foreign boundary state and published roots. This shared internal
interface does not merge the distinct public thread and fiber types (R2.21).

Fiber-local state follows the fiber across migration; the carrier association
is refreshed before Lisp execution resumes on another OS thread. Foreign-entry
and callback adapters establish or restore the appropriate Lisp context, and
nested Lisp→foreign→Lisp calls restore the enclosing context on exit. Boundary
root publication and GC participation remain governed by §2.7; a context
pointer alone does not make a transition GC-safe.

This is an approved design direction, **not an implemented ABI**. The concrete
context layout, ownership/lifetime and transition protocol, and physical
register assignments remain subject to the T1/T2 and boundary-adapter audit
tracked in `bliss-q861` (§4.7.4.2). This records a register convention only: it
does not by itself define an ABI for embedding another language runtime.
Embedding CPython is recorded separately as an approved direction in §2.7.8; no
JVM integration is implied or planned.

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
  scheduler moves it to Runnable when the fd is ready.
- **Dead:** Returned from its entry function or killed. Resources
  pending finalization.

### 2.3.3 Fiber and Native Thread Creation

```rust
/// D2.01 — Fiber descriptor (abridged; see D9.10).
pub struct Fiber {
    id:          FiberId,             // monotonic u64
    state:       AtomicU8,            // enum FiberState
    stack:       TorclStack,          // §2.4
    entry:       TorclVal,            // CL function to call
    result:      UnsafeCell<TorclVal>,
    join_waker:  AtomicWaker,         // for join semantics
    continuation: FiberContinuation,  // saved SP/FP + callee-saved registers
    tls_slots:   Box<[TorclVal; MAX_TLS]>, // per-fiber dynamic bindings
    handler_stack: Vec<HandlerFrame>,
    restart_stack: Vec<RestartFrame>,
    pin_count:   AtomicU32,
    carrier:     AtomicPtr<NativeThread>,
}
```

Creating a fiber (`TORCL-FIBER:MAKE-FIBER`) allocates a `TorclStack` from a
pool and sets `state = Created`. `SUBMIT-FIBER` transitions it to `Runnable`
and pushes it onto a carrier's deque. Creating a native thread
(`TORCL-THREAD:MAKE-THREAD`) instead creates a dedicated OS thread and does
not allocate or submit a fiber. **R2.19**: with a default CL stack of 512 KiB
(guard-page protected), 100 000 fibers require
~50 GiB of virtual address space — feasible on 64-bit with
overcommit/lazy mapping.

### 2.3.4 Scheduling Policy

1. A carrier pops from its own deque (LIFO — cache-warm).
2. If empty, it attempts to steal from a random other carrier (FIFO —
   fair, avoids starvation).
3. If all deques are empty, the carrier parks on a futex and is woken by
   the next `submit-fiber` or I/O completion event.

A fiber's first submission fixes its owning scheduler group. Synchronization,
deadline, and I/O wakeups requeue it on that group's carrier pool. Waits use a
per-fiber generation token and a carrier-consumed pending-wake handshake, so a
timeout from an old wait cannot wake a new one and an unpark racing with
unmount cannot mount one continuation on two carriers.

Preemption is cooperative: a running fiber yields at the next safepoint poll
when the scheduler sets its yield flag (e.g., time-slice expired or a
higher-priority fiber is ready).

---

## 2.4 Stack Layout

Each fiber owns a `TorclStack`: a contiguous virtual memory
region used for CL control/value frames. The Rust call stack of the
carrier thread (the "shadow stack") is separate.

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

Default usable size: 512 KiB (configurable via `TORCL_STACK_SIZE`).

### 2.4.2 Frame Format

Every CL call pushes a fixed-layout frame header followed by a
variable-size locals area.

```text
 ┌────────────────────────────────────┐  ← FP (frame pointer)
 │ prev_fp        : *mut Frame       │  8 bytes — linked list for walking
 │ return_pc      : *const u8        │  8 bytes — return address in native code
 │ function       : TorclVal         │  8 bytes — calling function (for debugger)
 │ code_info      : *const CodeInfo  │  8 bytes — safepoint map + source loc table
 │ flags          : u32              │  4 bytes — frame type, catch/unwind bits
 │ num_locals     : u16              │  2 bytes
 │ _pad           : u16              │  2 bytes (alignment)
 ├────────────────────────────────────┤
 │ locals[0]      : TorclVal         │
 │ locals[1]      : TorclVal         │
 │ ...                               │
 │ locals[N-1]    : TorclVal         │
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
frame on the fiber's `TorclStack`** (R2.05), in the §2.4.2 format, so a
single call chain freely interleaves tiers and one frame walker (the `prev_fp`
chain) sees them all. This is the key mechanism deviation from HotSpot noted in
§0 §1.1: rather than a template (assembly) interpreter whose frames are native
machine-stack frames, TorCL's baseline interpreter is a host-language (Rust)
loop, but its frames still live on the CL stack — not on the carrier's Rust
"shadow" stack.

The Rust shadow stack therefore holds only **transient, non-CL** activity: the
interpreter dispatch loop itself, GC inner loops, and runtime-internal helpers.
It never holds a durable CL activation, so a fiber can be parked or
migrated by saving its `TorclStack` pointer alone (§2.3) — interpreter state is
not stranded on a shared worker stack. Deep interpreted recursion consumes
`TorclStack` frames and raises `STORAGE-CONDITION` on overflow (R2.20), rather
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
on the one `TorclStack`, so OSR and deoptimisation (§4.6) rebuild or unwind
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
Two realisations satisfy it; TorCL ships the first and keeps the second as an
explicit, deferred option.

| | **Host-loop (baseline)** | **Template (later option)** |
|---|---|---|
| Interpreter body | Rust dispatch loop over bytecode | Generated machine-code stub per bytecode (via the codegen emitter, §4.7) |
| Frames | On `TorclStack` (D2.03), driven by the Rust loop | On `TorclStack`, native-stack frames |
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
`TorclStack` — the two-stack condition bliss-nmq removes).

- The **host-loop baseline above qualifies because it is a _bytecode_ loop**:
  `CALL`/`RETURN` push and pop D2.03 frames on the `TorclStack`, so CL
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
  a lowering pass makes available. TorCL's current `EnvFrame` chain
  (`Rc<RefCell<EnvFrame>>`) matches SBCL's heap-env model; moving *call frames*
  onto the `TorclStack` is the separable step, and the bytecode loop is what
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
carrier thread.

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

The guard regions are pre-registered per fiber at creation and
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

TorCL-generated native code uses the platform C ABI: System V AMD64 on
Linux/macOS x86-64, Win64 on Windows x86-64, AAPCS64 on aarch64, and ELFv2 on
ppc64le. This means CL-compiled functions can be called directly from C without
wrapper overhead when they use fixed-arity, non-variadic signatures.

Outbound scalar calls are implemented for each of those ABIs. Targets without a
per-ABI module — currently s390x — reach foreign code through the bootstrap
dispatcher's fixed set of call shapes, which supports neither mixed
floating-point signatures nor variadic calls. Callbacks (§2.7.5) and aggregate
arguments are implemented for x86-64 only.

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
| `INTEGER` | `Int { signed, bits }` (8–64 bit) | Both | Check the declared C range; return a fixnum or bignum without truncation |
| `SINGLE-FLOAT` | `Float` | Both | Extract from tagged word |
| `DOUBLE-FLOAT` | `Double` | Both | Heap-allocated; pass value |
| `STRING` | `Pointer(Int{8})` | CL→C | UTF-8 copy with null terminator; pinned |
| `(ALIEN *)` | `Pointer` | Both | Raw pointer, no GC tracking |
| `STRUCT` | `Struct` by value | Both | Classified into registers or memory by TorCL's target ABI planner (R2.14) |
| `(SIMPLE-ARRAY (UNSIGNED-BYTE 8))` | `Pointer(Int{8})` + length | Both | Scoped copy-in/copy-out to stable native storage; length passed separately. Lisp storage is never exposed as a stable C address. |
| Numeric vector | `Pointer(<alien>)` + length | Both | Same scoped copy, with explicit scalar element layout rather than inferring C layout from upgraded Lisp array element types. |

### 2.7.4 Call Flow

```text
  CL code                   FFI trampoline              C function
  ───────                   ──────────────              ──────────
  1. marshal args           2. transition thread        4. execute
     into C layout             to Native state
                            3. call via fn ptr ─────►
                            5. transition back  ◄─────  return
  6. unmarshal return          to Runnable
     into TorclVal
```

Step 2 (R2.15): Before entering C code, the fiber sets
`state = Native` and publishes its stack top. This tells the GC that
the thread's CL stack is quiescent and scannable without cooperation.

### 2.7.5 Callbacks (R2.12)

When C code needs to call back into CL:

1. Callback creation retains the Lisp closure as a precise root and generates
   a distinct C entry with stable native context. Code is written and then made
   executable through the W^X allocator; no writable+executable mapping is used.
2. The entry captures C arguments according to the same target ABI plan used by
   outbound calls. Its dispatcher enters runnable Lisp state, roots marshalled
   arguments, and invokes the retained function, independent of its current tier.
3. On return, the dispatcher restores the preceding runtime state, and the entry
   places the marshalled result in the C ABI's return location. Lisp nonlocal exits
   and Rust panics must be contained before they can unwind through foreign frames.

Callback storage has an explicit foreign lifetime. C may retain its entry after
Lisp drops its last wrapper, so GC must not implicitly release executable code or
its closure root. Before explicit release, the caller must retire retained C
references and finish every active invocation.

The `TORCL-FFI` scalar callback API is:

- `(make-callback function result-type argument-types)` retains a function and
  returns an opaque `foreign-callback` handle.
- `(callback-pointer callback)` returns its borrowed C entry pointer.
- `(free-callback callback)` releases the retained function and executable code.
  Reentrant release while active is rejected; callers must also prevent future
  or concurrent C entry before freeing. Raw pointer aliases cannot enforce this.
- `(callback-error callback)` consumes the latest failure's diagnostic string,
  or returns `NIL`. This is diagnostic text, not the original Lisp condition.

A failed callback supplies a zero C result and permits C to finish its frames.
An enclosing foreign call on that thread then signals `FFI-ERROR`; calls on a
foreign-created thread report through `callback-error`. Handled nested call
failures do not poison the outer call. Callbacks restored from images are invalid
and must be recreated, rather than accidentally naming newly allocated entries.

Implementation status: `ffi/callback.rs` generates the SysV AMD64 scalar entry,
sharing ABI classification with outbound calls. `ffi/managed_callback.rs` retains
precise closure roots, coordinates GC state transitions, and contains failures;
the standard-library registry owns entries until explicit release. The evaluator
supplies rooted function dispatch. Aggregate callbacks, other target ABIs, and
replacement of the separate legacy Rust `Callback` bootstrap API remain under
`bliss-124.3` / `bliss-124.5`.

### 2.7.6 Compiled Foreign Signatures (R2.14)

| Path | Mechanism | When |
|------|-----------|------|
| Dynamic calls | Cached TorCL-generated adapter for the target ABI and normalized signature | First use compiles; later uses reuse the adapter |
| Statically known signatures | Direct call lowering using the same ABI classification | Compiler may eliminate the intermediate argument slots |
| Callbacks | A C-entry adapter with an explicitly retained Lisp closure | Foreign code calls a stable entry independent of the closure's current Lisp tier |

The ABI plan assigns each argument and result to registers or memory, including
aggregate classification, stack alignment, and variadic promotions and metadata.
Dynamic signatures are compiled at runtime; they do not require an interpreted
call engine. The target function address is separate from the signature so
different foreign functions can share an outbound adapter.

Adapter code uses the runtime's W^X executable-memory allocator. An active call
retains its adapter even if the cache evicts it. No adapter-cache lock is held
while foreign code executes or reenters Lisp. GC root publication and native
state transitions remain runtime responsibilities at both sides of the boundary.

Implementation status: SysV AMD64 scalar outbound adapters, including variadic
calls, are implemented in `crates/torcl-rt/src/ffi/call.rs`. Variadic calls retain
the named-parameter count, promote only trailing arguments, and supply the SysV
vector-register count. The internal `%ffi-call` primitive accepts an optional
fifth argument for that named-parameter count. The runtime `ffi_call_buffered`
API in `ffi/buffered.rs` supports outbound structs and unions, including packed
layouts, register-bank rollback, hidden result pointers, and variadic calls.
It shares argument placement with scalar adapters through `ffi/abi.rs`.
Callers provide native argument and result buffers with the declared C layouts;
the adapter copies exactly their declared sizes through padded staging storage.
It does not publish the result buffer when a callback reports failure.
Scalar callbacks are implemented as described in §2.7.5. The Lisp buffer interface
is described below; CFFI integration, aggregate callbacks, and additional target ABIs remain work under
`bliss-124`; unsupported
signatures on the generated path signal an FFI error before entering foreign
code. Other targets temporarily retain the pre-existing bootstrap dispatcher
in `ffi/legacy.rs`; it is not an implementation of the generated-adapter contract.

### 2.7.7 Lisp Foreign Memory Interface

`TORCL-FFI` exposes the native memory substrate used by the CFFI port. This
interface takes byte counts and offsets; CFFI supplies its own typed allocation,
string translation, and compound-type abstractions above it.

| Operation | Contract |
|-----------|----------|
| `FOREIGN-ALLOC bytes`, `FOREIGN-FREE pointer` | Explicitly owned, zeroed native storage; free only the original allocation address |
| `POINTERP`, `FOREIGN-POINTER` | Predicate and Lisp type for opaque pointer wrappers |
| `MAKE-POINTER address`, `POINTER-ADDRESS pointer` | Explicit borrowed-address import and full-width integer inspection |
| `NULL-POINTER`, `NULL-POINTER-P`, `POINTER-EQ` | Null construction/test and address equality (not wrapper identity) |
| `INC-POINTER pointer bytes` | Checked address arithmetic retaining known allocation identity |
| `MEM-REF pointer type &optional offset` | Scalar load with known bounds/lifetime validation; offsets need not be aligned |
| `(SETF MEM-REF)`, `MEM-SET value pointer type &optional offset` | Scalar store with integer range checking |
| `FOREIGN-TYPE-SIZE type`, `FOREIGN-TYPE-ALIGNMENT type` | Platform scalar layout, including the native C `long` width |

Foreign pointer wrappers contain no Lisp references. They may move during GC,
but their native storage does not. `FOREIGN-FREE` invalidates all tracked aliases;
GC never implicitly frees foreign storage C may still retain. Pointer results
from C become borrowed wrappers, including a boxed null pointer for address zero.
See §8.3 for borrowed-address safety and image-restart semantics.

Errors from this boundary are catchable as `TORCL-FFI:FFI-ERROR`, a subtype of
`SIMPLE-ERROR`. Public memory entry points and the internal foreign library,
symbol, and call primitives are denied in sandboxed evaluation.

`WITH-POINTER-TO-VECTOR-DATA (pointer vector &optional scalar-type)` evaluates
the vector and type once, copies its entire array storage (ignoring fill pointers)
into a native buffer, and copies back and frees on normal or nonlocal exit.
The default layout is `:UNSIGNED-CHAR`; `MAKE-SHAREABLE-BYTE-VECTOR size` creates
a zero-initialized byte vector. Explicit numeric scalar types permit other C
layouts. Failed copy-in never copies partially initialized native storage back.
The body must not resize the vector, free the buffer, or let C retain the pointer
past the scope. Changes to the Lisp vector during the scope are overwritten by
copy-out. Multiple values from the body are preserved.

`LOAD-FOREIGN-LIBRARY path` returns an opaque `FOREIGN-LIBRARY` object, distinct
from a pointer. `FOREIGN-SYMBOL-POINTER name &optional library` searches the named
library, or the global lookup scope when omitted. In static builds the global
scope is open libraries followed by the loader's explicit host export surface;
dynamic builds use the system loader's default scope. The result is a borrowed
pointer. `CLOSE-FOREIGN-LIBRARY` invalidates its non-reused registry token and
releases its loader reference, running unload destructors when the last reference
is released. Before close, callers must retire all calls, callbacks, and retained
symbol uses from that library. GC does not unload libraries; saved-image library
objects restore with an invalid token, never a token referring to a new provider.

`FOREIGN-CALL pointer return-type argument-types arguments &optional fixed-count`
exposes the generated scalar call path, with a supplied fixed-count selecting
variadic calling. The caller owns signature correctness and symbol lifetime.
Generated scalar callbacks use the ownership API in §2.7.5. The complete CFFI
backend, aggregate callbacks, and remaining target ABIs remain work under
`bliss-124`.

`FOREIGN-CALL-BUFFERED pointer return-type argument-types argument-buffers
result-buffer &optional fixed-count` exposes native by-value calls without
imposing a Lisp representation on C structs. Every argument buffer is a foreign
pointer to the argument's C object representation, including scalars and pointer
arguments (which therefore require a buffer containing the pointer). Result
storage receives the exact declared size. The function returns `result-buffer`,
or `NIL` for `:void`; a void call accepts a null foreign pointer as its result
buffer. An explicitly supplied fixed count selects variadic calling and trailing
scalar promotions. The same caller-owned signature and library lifetime contract
applies as for `FOREIGN-CALL`.

Native descriptors are scalar type names or recursive `(:struct field-type...)`,
`(:packed-struct field-type...)`, and `(:union variant-type...)` lists. Fields have
ordinary C alignment, packed structs use byte alignment, and unions overlay
their variants. `FOREIGN-TYPE-SIZE` and `FOREIGN-TYPE-ALIGNMENT` use the adapter's
checked layout. Empty aggregates, void fields, improper/cyclic lists, unsupported
types, nesting beyond 64 levels and oversized layouts signal `FFI-ERROR`.

Owned native buffers are bounds/lifetime checked and copied into stable staging
storage before entering C. After C returns, copy-back rechecks the destination's
allocation identity: freeing it in a callback signals `FFI-ERROR`, never a stale
write. Callback failure leaves result storage unchanged. No allocation-registry
lock is held across foreign execution. This does not validate borrowed addresses
or extend the lifetimes of pointers embedded in C objects; those remain the
caller's responsibility. Sandbox mode denies this entry in both direct and
function-value dispatch paths. Aggregate descriptors/calls currently target
SysV AMD64; unsupported targets report `FFI-ERROR`.

---

### 2.7.8 Embedded language runtimes — approved direction (2026-09-27)

TorCL intends to embed **CPython** through its C API so that Python is usable as
a second object system rather than as a conventional foreign-function surface:
Lisp owns the process, and callers see `py:import`, `py:call` and `py:getattr`
rather than `PyImport_Import` and `PyObject_Call`. Tracked as `bliss-nnp5e`.

Four properties of this runtime shape the design, and each is a constraint
rather than a preference.

**Transitions reuse the existing boundary mechanism.** A crossing into Python is
a managed→foreign transition (§2.7, §2.5) and publishes Native state exactly as
any other foreign call does, so a collection can proceed while Python runs. The
VM MUST NOT acquire lock-specific knowledge of CPython's concurrency model:
what a crossing does differs between a conventional build and a free-threaded
one, so that belongs behind the transition rather than spread through generated
code. Python-created threads calling into Lisp are attached as Lisp threads by
the same path a foreign callback entry uses.

**Reference counting meets a moving collector.** A Lisp proxy holding a
`PyObject *` owns a Python reference, but the matching release cannot run from
an arbitrary Lisp GC or finalizer context: it may run arbitrary Python code,
including `__del__`, and re-enter CPython at a point where no safe state has
been published. Releases are therefore queued and drained at a safepoint. In the
other direction, a Python object referring to a Lisp object MUST NOT hold a raw
address, because the collector relocates (§3); it holds a stable handle. Cycles
spanning both runtimes are visible to neither collector, and are addressed by
documented ownership and strong/weak handles before any attempt at shared
reachability.

**Signals are arbitrated by the Lisp runtime.** Two runtimes MUST NOT both
believe they own `SIGINT`, `SIGTERM` or `SIGCHLD`. The sharper hazard is
specific to TorCL rather than to CPython: TorCL uses `SIGSEGV` for native-frame
recovery (§2.6.2), so a fault taken inside CPython must not be redirected to an
epilogue that unwinds a Lisp frame. Recovery is disabled while a foreign frame
is on top, and the Python transition is subject to that same rule.

**Zero-copy numeric interchange depends on non-moving storage.** Exposing Lisp
array storage through Python's buffer protocol requires that the storage not
relocate while a view exists, which rests on the pinning and large-object
mechanisms in §3. Strings are copied rather than shared: Python's Unicode
representation and TorCL's string representation are not worth reconciling.

Availability follows the FFI. Lisp→Python works wherever the per-ABI scalar path
does; Python→Lisp requires callbacks (§2.7.5) and is therefore x86-64 only until
those are ported. Staging the two directions separately is deliberate.

**What exists as of 2026-09-27.** The direction above is now partly built, behind
the `python` Cargo feature (CPython is resolved through the foreign-library loader
rather than linked, so a build need not depend on `libpython`, and embedding
requires a glibc target rather than the default static musl):

- The interpreter lifecycle and the transition, with a crossing reentrant so
  Lisp→Python→Lisp→Python works. Startup is explicit rather than lazy at the
  runtime layer, because CPython binds the interpreter to its initialising thread
  and shutdown from any other thread dereferences a thread state that may be gone.
- Reference ownership in both directions: a queued release drained at a crossing,
  and a stable handle table visited by a registered root scanner so a Lisp value
  Python holds survives and is rewritten when the collector moves it.
- The `TORCL-PYTHON` package, nicknamed `PY` — `import`, `resolve`, `call`,
  `call-method`, `getattr`/`setattr`, `type-of`, `typep`, `str`, `repr`, `exec` —
  with `PY:OBJECT` proxies whose references are released through the collector's
  finalizer registry. Documented in `docs/torcl-lisp-api.md`.
- The value policy: numbers by value, strings by copy, `NIL`↔`None`, `T`↔`True`,
  everything else a proxy. An integer beyond a fixnum stays a proxy rather than
  being truncated.
- A Python raise as a first-class condition, `PY:EXCEPTION`, carrying the
  exception's class, its message, the Python frames, and the exception object.
  Its report renders the Python half of a mixed-language backtrace, and
  `PY:BACKTRACE` returns those frames as data for a debugger to interleave.

Still unbuilt, and tracked separately: Python→Lisp callables (`bliss-89axw`,
x86-64 only until callbacks are ported), the zero-copy buffer protocol
(`bliss-s8wrr`), stream binding (`bliss-c4g9u`), and signal arbitration
(`bliss-ztkuw`). The inverse of the exception mapping — a Lisp condition escaping
into Python becoming a Python exception rather than unwinding through CPython
frames, which would leave its reference counts wrong — depends on Python→Lisp calls
and is staged with them. Requirement numbers are deliberately not assigned yet; this
section records a direction and its progress, and gains normative IDs when the
surface stops moving.

No JVM integration is implied or planned.

---

## 2.8 Configuration

### 2.8.1 Environment Variables (R2.16)

| Variable | Default | Description |
|----------|---------|-------------|
| `TORCL_HEAP_SIZE` | `512m` | Initial old-gen heap reservation |
| `TORCL_TLAB_SIZE` | `2m` | Per-thread TLAB size (§3.2.2, `--tlab-size`) |
| `TORCL_NURSERY_SIZE` | `64m` | Total nursery region pool (§3.2.2, `--nursery-size`) |
| `TORCL_STACK_SIZE` | `512k` | CL stack size per fiber |
| `TORCL_WORKERS` | `nproc` | Default scheduler-group carrier count (legacy name) |
| `TORCL_IMAGE` | `torcl.bimg` | Path to boot image |
| `TORCL_GC_LOG` | (none) | Path to GC log file (enables GC logging) |
| `TORCL_JIT_DUMP` | `0` | `1` = emit `jitdump` file for `perf` |
| `TORCL_SAFEPOINT_SPIN` | `1000` | Spin iterations before parking at safepoint |
| `TORCL_FFI_POOL_PAGES` | `4` | Executable pages for callback trampolines |

All environment variables are read once in step 1 of startup (§2.2)
before any heap allocation occurs.

### 2.8.2 CLI Arguments

```text
torcl [options] [-- CL-args...]

Options:
  --image PATH        Override TORCL_IMAGE
  --eval FORM         Evaluate FORM and exit
  --load FILE         Load FILE and exit
  --no-image          Bootstrap from lib/boot.lisp (no image)
  --workers N         Override TORCL_WORKERS
  --heap-size SIZE    Override TORCL_HEAP_SIZE
  --help              Print usage and exit
  --version           Print version and exit
```

Arguments after `--` are passed to CL as
`TORCL-EXT:*COMMAND-LINE-ARGUMENTS*`.

---

## 2.9 Shutdown Lifecycle (R2.17)

```text
 1. CL entry function returns (or EXIT is called with code N).
 2. Run *EXIT-HOOKS* (list of thunks, LIFO order).
 3. Set global shutdown flag (atomic).
 4. Interrupt all fibers → each unwinds via UNWIND-PROTECT.
 5. Wait for all fibers to reach Dead state (timeout 5 s).
 6. Run pending finalizers (§3).
 7. Signal carrier threads to exit; join all carriers and remaining native threads.
 8. Unmap heap, safepoint page, trampoline pool.
 9. Call libc exit(N).
```

If step 5 times out, the runtime logs a warning and proceeds with
forced shutdown (threads are detached, not joined).

---

## 2.10 Error Handling

### 2.10.1 Rust-Side Errors

All runtime-internal functions return `Result<T, TorclError>`.
`TorclError` is a non-exhaustive enum:

```rust
/// D2.04 — Runtime error type.
pub enum TorclError {
    Oom,                          // nursery + old-gen exhausted
    StackOverflow(GreenThreadId),
    InvalidImage(String),
    FfiError(String),
    SignalError(c_int),
    Shutdown,                     // clean shutdown requested
    Internal(String),             // invariant violation (bug)
}
```

`TorclError` MUST NOT be converted to `panic!` (R2.18). At the FFI
boundary, `TorclError` is translated into the appropriate CL condition
class.

### 2.10.2 CL Condition Mapping

| TorclError | CL Condition |
|------------|-------------|
| `Oom` | `STORAGE-CONDITION` |
| `StackOverflow` | `STORAGE-CONDITION` |
| `InvalidImage` | `TORCL-EXT:IMAGE-ERROR` (subclass of `ERROR`) |
| `FfiError` | `TORCL-FFI:FFI-ERROR` (subclass of `ERROR`) |
| `SignalError` | `TORCL-EXT:SIGNAL-ERROR` |
| `Shutdown` | Not signalled — initiates shutdown path |
| `Internal` | `TORCL-EXT:INTERNAL-ERROR` — always a bug, dump & abort |

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
| Startup sequence | Integration test: spawn `torcl --eval '(quit 42)'`, assert exit code 42 and elapsed < 50 ms. |
| Fiber creation / join | Unit test: submit 1 000 fibers each incrementing an atomic counter; assert final value. |
| Safepoint liveness | Unit test: tight loop in T1; verify GC completes within 100 ms. |
| Stack overflow | Unit test: deeply recursive function; assert `STORAGE-CONDITION` raised, stack intact. |
| Signal handling | Integration test: send `SIGINT` to process in `(sleep 10)`; assert `INTERRUPT-CONDITION` caught. |
| FFI round-trip | Unit test: call `strlen` on a CL string; call back into CL from C. |
| Shutdown | Integration test: register exit hook, verify it runs, verify clean exit. |
| Stress | Stress test: 100 000 fibers with FFI callbacks; no crash, no leak (valgrind/ASAN). |

---

## 2.13 Module Map

| Source file | Responsibility | Key types / functions |
|-------------|---------------|-----------------------|
| `crates/torcl-rt/src/startup.rs` | §2.2 boot sequence | `torcl_main()`, `load_image()` |
| `crates/torcl-rt/src/thread.rs` | §2.3 thread model | `GreenThread`, `WorkerThread`, `Scheduler` |
| `crates/torcl-rt/src/stack.rs` | §2.4 stack layout | `TorclStack`, `Frame`, `FrameWalker` |
| `crates/torcl-rt/src/safepoint.rs` | §2.5 safepoint | `SafepointPage`, `poll_safepoint()` |
| `crates/torcl-rt/src/signal.rs` | §2.6 signals | `install_handlers()`, `sigsegv_handler()` |
| `crates/torcl-rt/src/ffi.rs` | §2.7 FFI bridge | `AlienType`, `ffi_call()`, `make_callback()` |
| `crates/torcl-rt/src/config.rs` | §2.8 config | `RuntimeConfig`, `parse_cli()` |
