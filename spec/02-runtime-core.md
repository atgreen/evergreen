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

**Image load cost model (step 4):** The `.bimg` format uses
position-independent tagged values (heap offsets, not absolute
pointers). This makes step 4 a true O(1) `mmap` — no relocation
pass is required, and pages are populated on demand by the OS.
The symbol table and package registry are reconstructed from
offset-based indices embedded in the image header, which is a
small fixed-size read (< 4 KiB). This design ensures cold-start
image load is dominated by kernel `mmap` setup (< 1 ms), well
within the R2.01 50 ms budget.

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

### 2.4.4 Rust Shadow Stack Interaction

Interpreted code (T0) and runtime-internal helpers execute on the Rust
stack. These frames are invisible to the CL frame walker. When the
interpreter calls a CL-compiled function, it pushes a **trampoline
frame** (a `CALL` frame with a sentinel `code_info`) that bridges the
two worlds. The GC scans both the CL stack (via the frame chain) and
the Rust stack (via conservative scanning of the worker thread's native
stack between known bounds).

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
| `BLISS_NURSERY_SIZE` | `2m` | Per-thread nursery (TLAB) size |
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
