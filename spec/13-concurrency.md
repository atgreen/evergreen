# §13 Concurrency Model

**Scope:** Consolidates concurrency specification: memory model, lock
hierarchy, lock-free structures, green-thread scheduling, GC
safepoint/thread-suspension interaction, async signal safety.
Supersedes fragments in §2.11, §3.11, §5.1.5, §5.4.7, §8.8.

Source: `crates/bliss-rt/src/{thread,scheduler,safepoint,signal}.rs`,
`crates/bliss-rt/src/gc/`.

---

## 13.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R13.01 | Bliss MUST provide sequential consistency for reads and writes to special (dynamically-bound) variables within a single thread. | MUST |
| R13.02 | Reads and writes to global symbol value cells from different threads MUST use acquire-release ordering. | MUST |
| R13.03 | Reads and writes to CLOS slot values from different threads MAY use relaxed ordering unless the slot is declared `(:synchronized t)`. | MAY |
| R13.04 | Atomic compare-and-swap operations (`BLISS-EXT:CAS`) MUST provide acquire-release semantics. | MUST |
| R13.05 | A complete lock-ordering hierarchy MUST be defined spanning all subsystems; acquiring locks out of order MUST be detected in debug builds (see also R8.16). | MUST |
| R13.06 | The lock hierarchy MUST be documented as a single total order with numeric levels; every lock in the system MUST be assigned a level. | MUST |
| R13.07 | Symbol value cells, inline-cache (IC) patch words, and profiling counters MUST be implemented as lock-free data structures. | MUST |
| R13.08 | The green-thread scheduler MUST use a work-stealing deque (Chase-Lev) per OS worker thread with LIFO push/pop for local work and FIFO steal for cross-worker balancing. | MUST |
| R13.09 | The scheduler MUST support cooperative preemption via per-thread yield flags checked at safepoints; time-slice expiry MUST set the yield flag. | MUST |
| R13.10 | GC stop-the-world requests MUST be coordinated via the safepoint polling-page mechanism (§2.5, §3.9); no thread MUST be suspended asynchronously. | MUST |
| R13.11 | Threads in the `Native` state (executing FFI calls) MUST be excluded from safepoint handshake; their CL stacks MUST be scannable without cooperation. | MUST |
| R13.12 | Threads in the `Blocked` state MUST publish their stack top before blocking; the GC MUST be able to scan their stacks without waking them. | MUST |
| R13.13 | All signal handlers MUST be async-signal-safe; non-trivial work MUST be deferred to the next safepoint via per-thread flags (see also R8.10). | MUST |
| R13.14 | The runtime MUST support a `SIGUSR1` fallback to interrupt threads blocked in long-running syscalls, but the handler MUST only set a flag — it MUST NOT suspend the thread. | MUST |
| R13.15 | The runtime MUST provide `BLISS-EXT:WITH-ATOMIC` — a macro that disables thread preemption (yield-flag checks) for its dynamic extent, to allow short critical sections without locks. | MUST |
| R13.16 | `WITH-ATOMIC` sections MUST NOT span safepoints; the compiler MUST reject code that places a safepoint poll inside a `WITH-ATOMIC` body if the body exceeds an estimated 10 µs execution bound. | MUST |
| R13.17 | The runtime MUST support thread-local dynamic bindings: each green thread has its own binding stack for special variables. Binding a special variable in one thread MUST NOT affect other threads. | MUST |
| R13.18 | The runtime MUST provide `BLISS-EXT:MAKE-THREAD`, `BLISS-EXT:JOIN-THREAD`, `BLISS-EXT:THREAD-YIELD`, `BLISS-EXT:DESTROY-THREAD`, `BLISS-EXT:CURRENT-THREAD`, and `BLISS-EXT:ALL-THREADS`. | MUST |
| R13.19 | Mutex, condition-variable, read-write-lock, and semaphore primitives MUST be provided in the `BLISS-THREAD` package. | MUST |
| R13.20 | The runtime SHOULD detect potential deadlocks (lock-wait-graph cycle detection) in debug builds (see also R8.21). | SHOULD |

---

## 13.2 Memory Model

Bliss exposes a memory model to CL code that balances safety with
performance. It is **not** sequentially consistent by default — relaxed
access is permitted for slot reads to allow the JIT to avoid memory
fences on hot paths.

### 13.2.1 Ordering Guarantees

| Access Type | Ordering | Rationale |
|-------------|----------|-----------|
| Special-variable read/write (thread-local binding stack) | **Sequential consistency** (per-thread) | Binding stack is thread-local; no cross-thread visibility. Each thread sees its own bindings in program order. (R13.01) |
| Global symbol value cell read | **Acquire** | Reader must see all writes preceding the store. (R13.02) |
| Global symbol value cell write | **Release** | Writer must publish all prior mutations before the cell update is visible. (R13.02) |
| CLOS slot read (unsynchronized) | **Relaxed** | No ordering guarantee across threads. User must use explicit synchronisation if sharing mutable objects. (R13.03) |
| CLOS slot read (`:synchronized t`) | **Acquire** | Compiler emits an acquire fence after the load. |
| CLOS slot write (`:synchronized t`) | **Release** | Compiler emits a release fence before the store. |
| `BLISS-EXT:CAS` | **Acquire-Release** (AcqRel) | Both success and failure paths use AcqRel. (R13.04) |
| `BLISS-EXT:ATOMIC-INCF` / `ATOMIC-DECF` | **Acquire-Release** | Fetch-and-add with AcqRel ordering. |
| Hash-table entry read (synchronized table) | **Acquire** (via striped lock) | Lock acquisition provides the fence. |
| Hash-table entry write (synchronized table) | **Release** (via striped lock) | Lock release provides the fence. |
| Write-barrier stores (SATB buffer, card table) | **Relaxed** | Benign races acceptable (§3.11). |

### 13.2.2 Data-Race Semantics

A data race on a non-atomic CL value MUST NOT crash the process. All
`BlissVal` slots are `AtomicU64` at the Rust level with relaxed
loads/stores for unsynchronized paths. A racy read MAY return a stale
value but MUST NOT return a value with an invalid tag. The type tag is
always written atomically with the payload (naturally aligned 64-bit
writes are tear-free on x86-64 and aarch64).

### 13.2.3 Fences and Compiler Barriers

| Primitive | Mapping |
|-----------|---------|
| `BLISS-EXT:MEMORY-BARRIER` | Full `SeqCst` fence. |
| `BLISS-EXT:LOAD-BARRIER` | `fence(Acquire)`. |
| `BLISS-EXT:STORE-BARRIER` | `fence(Release)`. |

The JIT MUST NOT reorder memory accesses across these barriers.

---

## 13.3 Lock Ordering Protocol

### 13.3.1 Complete Lock Hierarchy

All locks in the system are assigned a numeric level. A thread MUST
acquire locks in strictly ascending level order. Acquiring a lock at
level ≤ the highest level currently held by the thread is a violation
and MUST trigger a `debug_assert!` panic in debug builds (R13.05,
R13.06).

| Level | Lock | Protects | Subsystem | Notes |
|-------|------|----------|-----------|-------|
| 0 | (thread-local — no lock) | TLAB, dynamic bindings, handler bindings, SATB buffer | §2, §3 | Never contended. |
| 1 | Per-stream mutex | Stream state and buffer | §5.4 | Ordered by `stream-id` when multiple streams needed. |
| 2 | Per-bucket hash-table stripe; per-symbol plist RwLock | Hash-table entries; symbol property lists | §5.5, §13.4.1 | Ordered by bucket index within a table. Plist locks are rarely contended. |
| 3 | Per-package RwLock (internal, external) | Symbol tables within a package | §5.1 | `external` before `internal` (Rule L4, §5.1.5). |
| 4 | Global package registry RwLock | Package creation/deletion/rename | §5.1 | Must precede per-package locks (Rule L1). |

> **Note:** §5.1.5 defines a local hierarchy (Level 0 = PackageRegistry,
> Level 1 = Package, Level 2 = SymbolTable) for the package subsystem.
> The global hierarchy here (levels 3–4) supersedes those local levels.
> The relative ordering is preserved: registry (global level 4) before
> per-package (global level 3), matching Rule L1 in §5.1.5.
| 5 | Compiler code-cache lock | JIT code installation, IC patching coordination | §4 | — |
| 6 | Profiling-data lock | Tier-promotion decisions, counters rollup | §4.9 | — |
| 7 | GC world-stop mutex | Stop-the-world coordination | §3.9 | Only the GC controller acquires this. |
| 8 | Thread registry RwLock | Global list of all green threads | §2.3 | Must precede per-thread locks. |
| 9 | Image save mutex | Heap serialisation to `.bimg` | §7 | Highest level — no other lock may be held. |

### 13.3.2 Multi-Package Lock Ordering (from §5.1.5)

When an operation (e.g., `USE-PACKAGE`, `IMPORT`) requires write locks
on multiple packages, the packages MUST be sorted by `Package.id`
(ascending), deduplicated, and locked in that order (A5.03). This
sub-ordering operates within level 3 of the global hierarchy.

### 13.3.3 Multi-Stream Lock Ordering (from §5.4.7.3)

Composite streams that delegate to components acquire the composite
stream's lock first, then each component's lock in ascending `stream-id`
order. This sub-ordering operates within level 1 of the global
hierarchy.

### 13.3.4 Hash-Table Stripe Ordering

Synchronized hash tables use striped locking (one lock per N buckets).
When a resize requires acquiring multiple stripes, they MUST be
acquired in ascending stripe index order. This sub-ordering operates
within level 2.

### 13.3.5 Debug Lock-Stack (D13.01)

A thread-local `Vec<(u8, &'static str)>` records all locks currently
held. Before every lock acquisition in debug builds,
`assert_lock_order(level, name)` checks that `level` strictly exceeds
the top of the stack; violation triggers a panic with the held and
requested lock names.

### 13.3.6 Deadlock Detection (R13.20)

In debug builds, a background watchdog thread (interval:
`BLISS_DEADLOCK_WATCHDOG_MS`, default 5 s) scans the lock-wait graph
for cycles. The graph is constructed from per-thread lock stacks
(locks held) and a `LOCK_WAITERS` table (thread → waited-for lock).
A cycle implies deadlock; the watchdog logs thread IDs, lock names, and
levels to stderr and the crash log directory.

---

## 13.4 Lock-Free Data Structures

### 13.4.1 Symbol Value Cells (R13.07)

Global symbol value cells are `AtomicU64` fields storing `BlissVal`.
Operations:

| Operation | Implementation | Ordering |
|-----------|---------------|----------|
| Read global value | `cell.load(Acquire)` | Acquire |
| Write global value | `cell.store(val, Release)` | Release |
| CAS global value | `cell.compare_exchange(old, new, AcqRel, Acquire)` | AcqRel/Acquire |
| Thread-local binding | Binding-stack lookup (no atomic needed) | Thread-local |

Symbol `function` cells use the same pattern. The `plist` field is
separately protected by a per-symbol `RwLock` (level 2, same as
hash-table stripes — plists are rarely contended).

### 13.4.2 Inline Cache Patching (R13.07)

ICs (§4.8) are patched atomically. D13.02 — `MonomorphicIC`: two
`AtomicU64` fields (`class_id`, `method`), 16-byte aligned.

**Patching protocol (A13.01):** Writer stores `method` then `class_id`
(both `Release`). Reader loads `class_id` then `method` (both
`Acquire`). If `class_id` matches the receiver, call `method`;
otherwise fall through to the megamorphic stub. Stale reads are
benign — a mismatch simply takes the slow path. Polymorphic ICs use a
small fixed-size array of `MonomorphicIC` slots scanned linearly.

### 13.4.3 Profiling Counters (R13.07)

D13.03 — `FunctionProfile`: `call_count: AtomicU32`,
`loop_count: AtomicU32`, `deopt_count: AtomicU16`, `tier: AtomicU8`.
All updated with `Relaxed` fetch-and-add. Exact counts are not required
— counters are heuristics for tier promotion (§4.9). Slight
undercounting from relaxed ordering is acceptable.

### 13.4.4 Work-Stealing Deque (D13.04)

Chase-Lev deque: `buffer: AtomicPtr<CircularBuffer<T>>`,
`bottom: AtomicIsize` (owner only), `top: AtomicIsize` (stealers via
CAS). Push: `Release`. Pop: `SeqCst` fence. Steal:
`top.compare_exchange(t, t+1, SeqCst, Relaxed)`.

### 13.4.5 SATB Buffer Flush Queue (D13.05)

Lock-free MPSC linked list. Mutators enqueue full SATB buffers via CAS
on the head pointer; the GC marker drains by swapping head to null and
walking the chain. Each node holds `buffer: [BlissVal; SATB_BUFFER_SIZE]`,
`len: usize`, `next: AtomicPtr`.

---

## 13.5 Green Thread Scheduling

### 13.5.1 Architecture Overview

Each OS worker owns a Chase-Lev deque and a TLAB. Workers steal from
random peers when idle. A global overflow queue and an I/O poller
(epoll/kqueue) feed tasks into the system.

### 13.5.2 Scheduling Algorithm (A13.02)

```text
ALGORITHM worker_loop(worker_id):
  loop:
    // 1. Try local deque (LIFO — cache-warm)
    task ← worker.deque.pop()
    if task ≠ EMPTY:
      run(task)
      continue

    // 2. Try global overflow queue
    task ← global_queue.try_dequeue()
    if task ≠ EMPTY:
      run(task)
      continue

    // 3. Try stealing from a random peer (FIFO — fair)
    victim ← random_worker(excluding: worker_id)
    task ← victim.deque.steal()
    if task ≠ EMPTY:
      run(task)
      continue

    // 4. Try I/O poller for ready tasks
    ready ← io_poller.poll_nonblocking()
    if ready is not empty:
      for task in ready:
        worker.deque.push(task)
      continue

    // 5. Park — no work available
    worker.park_on_futex(timeout: 1ms)
    // Woken by: new task push, I/O completion, or timeout
```

### 13.5.3 Task Lifecycle

| Event | Action |
|-------|--------|
| `MAKE-THREAD` | Allocate `GreenThread` (D2.01), push to current worker's deque. Wake a parked worker if any. |
| Yield (cooperative) | Push current task back to deque tail, pop next task. |
| Block (mutex/condvar) | Remove task from deque, set state `Blocked`, record waker. |
| I/O submit | Set state `Waiting`, publish stack top, register with I/O poller. Treated like `Blocked` for GC purposes (§13.6.1). |
| I/O ready | I/O poller marks task ready, set state `Runnable`, push to worker deque. |
| Unblock (notify) | Set state `Runnable`, push to notifier's deque (or global queue if cross-worker). |
| FFI call | Set state `Native`, publish stack top. Task remains logically on the worker but does not block scheduling. |
| FFI return | Set state `Runnable`; if worker's deque is empty, resume immediately. |
| Death | Set state `Dead`, wake join waiters, return stack to pool. |

### 13.5.4 Cooperative Preemption (R13.09)

A per-worker timer (1 ms, `timer_create` / `CLOCK_MONOTONIC`) sets a
per-green-thread `yield_flag`. The safepoint poll checks this flag;
if set, the green thread is pushed to the back of the local deque and
the next task is popped. Default time slice: 1 ms (`BLISS_TIME_SLICE_US`).

### 13.5.5 Priority Levels

Four levels: `:low` (0, background/finalizers), `:normal` (1, default),
`:high` (2, interactive/I/O), `:critical` (3, GC/signals). Stealers
prefer higher-priority tasks; FIFO within a level.

---

## 13.6 GC Safepoints and Thread Suspension

### 13.6.1 Stop-the-World Protocol (A13.03)

```text
ALGORITHM stop_the_world():
  // Precondition: caller holds GC world-stop mutex (level 7)

  // 1. Poison the safepoint page
  mprotect(safepoint_page, PROT_NONE)

  // 2. Wait for all threads to reach a safe state
  for thread in thread_registry:
    match thread.state:
      Runnable →
        // Thread will fault on next safepoint poll.
        // Wait until thread.at_safepoint flag is set.
        wait_for(thread.at_safepoint, timeout: 100ms)
        if timeout:
          // Fallback: send SIGUSR1 to interrupt syscall
          send_sigusr1(thread.os_thread_id)
          wait_for(thread.at_safepoint, timeout: 1s)
          if timeout:
            panic!("Thread {} did not reach safepoint", thread.id)

      Blocked →
        // Stack already published. Scannable without cooperation.
        // No action needed. (R13.12)

      Native →
        // Executing FFI. CL stack is quiescent and published.
        // Skip this thread for handshake. (R13.11)
        // When FFI returns, thread will see poisoned page and
        // enter safepoint before resuming CL code.

      Waiting →
        // Blocked on I/O. Stack published. Treat like Blocked.

      Dead →
        // No action needed.

  // 3. All threads are now either at safepoint, Blocked, Native,
  //    Waiting, or Dead. GC may proceed.

ALGORITHM resume_all_threads():
  // 1. Un-poison the safepoint page
  mprotect(safepoint_page, PROT_READ)

  // 2. Wake all parked threads
  for thread in thread_registry:
    if thread.at_safepoint:
      thread.at_safepoint ← false
      wake(thread)
```

### 13.6.2 Native-to-Runnable Transition

On FFI return (§2.7.4 step 5), the thread checks the safepoint page.
If poisoned, it enters the safepoint immediately before resuming CL
code. If not, it resumes normally.

### 13.6.3 Blocked Thread Stack Scanning (R13.12)

A thread transitioning to `Blocked` publishes `sp` and `fp` to its
descriptor. The GC can then walk its stack without synchronisation.
On unblock, the thread checks for a pending safepoint before resuming.

### 13.6.4 Safepoint and Scheduler Interaction

Safepoint checks and scheduling decisions are co-located. At every
safepoint, the thread performs (in order, expanding §2.5.3):

1. Publish `sp` and `fp`.
2. Set `at_safepoint` flag.
3. If GC pending → park until GC completes.
4. Clear `at_safepoint` flag.
5. If `yield_flag` set → yield to scheduler (§13.5.4).
6. If `interrupt_flag` set → deliver pending interrupt condition.
7. If `timeout_flag` set → signal `TIMEOUT-CONDITION` (sandbox, §8.4.4).
8. Resume CL execution.

---

## 13.7 Async Signal Safety (R13.13)

### 13.7.1 Signal Handler Rules

All Bliss signal handlers MUST obey POSIX async-signal-safety:

1. **No heap allocation.** Signal handlers MUST NOT call `malloc`,
   `mmap`, or any GC allocator path.
2. **No lock acquisition.** Signal handlers MUST NOT acquire any mutex,
   rwlock, or spinlock.
3. **No stdio.** Signal handlers MUST NOT call `write(2)` to buffered
   streams. Exception: the crash handler (§8.9.3) may call `write(2)`
   directly to fd 2 as a last-resort diagnostic.
4. **Flag-only.** Each signal handler sets a per-thread `sig_atomic_t`
   flag and returns. The flag is checked at the next safepoint.

### 13.7.2 Signal-to-Safepoint Mapping

| Signal | Flag Set | Safepoint Action |
|--------|----------|-----------------|
| `SIGINT` | `pending_interrupt` | Signal `INTERRUPT-CONDITION` to foreground green thread |
| `SIGSEGV` (safepoint page) | N/A — handler rewrites PC | Thread enters safepoint directly via PC rewrite in `ucontext_t` |
| `SIGSEGV` (null guard) | N/A — handler rewrites PC | Raise `TYPE-ERROR` via PC rewrite |
| `SIGSEGV` (stack guard) | N/A — handler rewrites PC | Raise `STORAGE-CONDITION` via PC rewrite |
| `SIGFPE` | `pending_fpe` | Signal `ARITHMETIC-ERROR` |
| `SIGPIPE` | `pending_pipe` | Signal `STREAM-ERROR` on next I/O operation |
| `SIGUSR1` | `pending_safepoint` | Enter safepoint (fallback for syscall-blocked threads, R13.14) |
| `SIGTERM` | `shutdown_requested` | Initiate orderly shutdown (§2.9) |

### 13.7.3 SIGSEGV Handler Fast Path (A13.04)

The `SIGSEGV` handler is the one exception to the "flag-only" rule. It
rewrites the saved PC in `ucontext_t` to redirect the faulting thread
to a safepoint stub or condition-raising stub (see §2.6.2 for the
dispatch logic). This is async-signal-safe because it only writes to
the `ucontext_t` structure and returns; the redirected code runs
outside the signal handler context.

### 13.7.4 SIGUSR1 Fallback (R13.14)

`SIGUSR1` is sent to threads blocked in syscalls after a 100 ms timeout.
The handler sets `pending_safepoint = true`; when the syscall returns
(`EINTR`), the thread enters the safepoint. Per R3.14, this handler
MUST NOT suspend the thread; it only sets a flag. The `SIGUSR1`
mechanism is strictly for flag-setting — thread suspension is
prohibited (§3, R3.14). This is a last-resort — the polling-page
handles >99.9% of cases.

---

## 13.8 Thread-Local Dynamic Bindings (R13.17)

Each green thread owns a **binding stack**: a stack of `(symbol,
previous_value)` pairs representing dynamically-bound special variables.

### 13.8.1 Binding Protocol

`bind_special`: load global cell (`Acquire`), push `(symbol,
old_value)` to binding stack, set `thread.tls_map[symbol] = new_value`.
`unbind_special`: pop binding stack, remove from `tls_map`. The global
cell is never modified — thread-local bindings shadow it.

**Reading a special** checks `tls_map` first; if absent, falls back to
the global cell with `Acquire` ordering.

### 13.8.2 Inherited Bindings

New green threads inherit a snapshot of the parent's `*standard-input*`,
`*standard-output*`, `*error-output*`, `*debug-io*`, `*query-io*`,
`*trace-output*`, `*terminal-io*`, and `*package*` bindings. Other
specials start with global values.

---

## 13.9 Synchronisation Primitives (R13.19)

### 13.9.1 Primitive Types

| Type | CL Name | Implementation |
|------|---------|----------------|
| Mutex | `BLISS-THREAD:MUTEX` | Futex-based, non-reentrant by default; `:recursive t` option available. |
| RW-Lock | `BLISS-THREAD:RW-LOCK` | Reader-writer lock with writer preference to prevent starvation. |
| Condition Variable | `BLISS-THREAD:CONDITION-VARIABLE` | Paired with a mutex; supports `WAIT`, `NOTIFY`, `NOTIFY-ALL`. |
| Semaphore | `BLISS-THREAD:SEMAPHORE` | Counting semaphore; `WAIT` decrements, `SIGNAL` increments. |
| Barrier | `BLISS-THREAD:BARRIER` | N-thread barrier with `WAIT` that blocks until all N threads arrive. |

### 13.9.2 Green-Thread–Aware Blocking

Blocking on a sync primitive transitions the green thread to `Blocked`
(§2.3.2), freeing the OS worker for other green threads. Unblocking
pushes the green thread back onto a worker's run queue.

---

## 13.10 Configuration

| Parameter | Default | Env Var | Description |
|-----------|---------|---------|-------------|
| Worker threads | `nproc` | `BLISS_WORKERS` | OS worker-thread count. |
| Time slice | 1000 µs | `BLISS_TIME_SLICE_US` | Cooperative preemption interval. |
| Safepoint spin iterations | 1000 | `BLISS_SAFEPOINT_SPIN` | Spin count before parking at safepoint. |
| Deadlock watchdog interval | 5 s | `BLISS_DEADLOCK_WATCHDOG_MS` | Debug-build only. 0 = disabled. |
| SIGUSR1 timeout | 100 ms | `BLISS_SIGUSR1_TIMEOUT_MS` | Delay before sending SIGUSR1 fallback. |
| Green-thread stack size | 512 KiB | `BLISS_STACK_SIZE` | Per-green-thread CL stack. |
| Max green threads | 100 000 | `BLISS_MAX_THREADS` | Upper limit (R2.19). |

---

## 13.11 Error Handling

| Failure | Response |
|---------|----------|
| Lock-order violation (debug) | `debug_assert!` panic with diagnostic. |
| Deadlock detected (debug) | Log cycle to stderr and crash log; no abort (informational). |
| Safepoint timeout (>1 s) | Panic — indicates a bug (thread stuck in unsafe code). |
| Thread creation failure | Signal `BLISS-EXT:THREAD-ERROR` with `:reason :limit-exceeded` or `:reason :oom`. |
| SIGUSR1 delivery failure | Log warning; GC proceeds — thread will eventually reach a poll. |

---

## 13.12 Test Strategy

| Test | Method | Acceptance |
|------|--------|------------|
| Memory ordering | Stress test: two threads communicating via global value cells with release-acquire; verify no stale reads in 10⁶ iterations. | Zero failures. |
| Lock-order enforcement | Intentionally acquire locks out of order in debug build; verify panic. | Panic with descriptive message. |
| Deadlock detection | Create a synthetic deadlock (A holds L1, B holds L2, A waits L2, B waits L1); verify watchdog detects cycle within 10 s. | Cycle logged. |
| Work-stealing deque | Concurrent push/pop/steal from 8 threads; verify all tasks complete, no lost items. | 100% completion. |
| Safepoint liveness | Tight loop in T1 code; request STW; verify all threads reach safepoint within 10 ms. | p99 < 10 ms. |
| Native-thread GC safety | Thread in FFI call during GC; verify CL stack scanned correctly. | No GC crash. |
| Blocked-thread GC safety | Thread blocked on mutex during GC; verify stack scanned correctly. | No GC crash. |
| Cooperative preemption | 100 CPU-bound green threads on 4 workers; verify all make progress (no starvation) within 100 ms. | All threads run. |
| SIGUSR1 fallback | Thread blocked in `read(2)` on a pipe; request STW; verify thread reaches safepoint via SIGUSR1 within 200 ms. | Safepoint reached. |
| WITH-ATOMIC correctness | Verify preemption is suppressed inside `WITH-ATOMIC`; verify GC safepoint is NOT suppressed (compile-time check). | Correct behaviour. |
| Binding stack isolation | Two threads bind the same special to different values; verify no cross-thread leakage. | Thread-local isolation. |

---

## 13.13 Module Map

| Source File | Responsibility | Key Types / Functions |
|-------------|---------------|-----------------------|
| `crates/bliss-rt/src/thread.rs` | Green-thread descriptor, state machine | `GreenThread`, `ThreadState`, `GreenThreadId` |
| `crates/bliss-rt/src/scheduler.rs` | Work-stealing scheduler, worker loop | `Scheduler`, `WorkerThread`, `WorkStealingDeque` |
| `crates/bliss-rt/src/safepoint.rs` | Safepoint page, STW protocol | `SafepointPage`, `stop_the_world()`, `resume_all()` |
| `crates/bliss-rt/src/sync/mutex.rs` | Green-thread-aware mutex | `BlissMutex`, `BlissRwLock` |
| `crates/bliss-rt/src/sync/condvar.rs` | Condition variables | `BlissCondVar` |
| `crates/bliss-rt/src/sync/semaphore.rs` | Counting semaphore, barrier | `BlissSemaphore`, `BlissBarrier` |
| `crates/bliss-rt/src/sync/atomic.rs` | CAS, atomic-incf, memory barriers | `bliss_cas()`, `atomic_incf()` |
| `crates/bliss-rt/src/signal.rs` | Signal handlers, SIGUSR1 fallback | `install_handlers()`, `sigsegv_handler()` |
| `crates/bliss-rt/src/gc/safepoint.rs` | GC-side safepoint coordination | `request_safepoint()`, `wait_at_safepoint()` |
| `crates/bliss-rt/src/profiling.rs` | Lock-free profiling counters | `FunctionProfile` |
| `crates/bliss-compiler/src/ic.rs` | Inline cache patching | `MonomorphicIC`, `patch_ic()` |
