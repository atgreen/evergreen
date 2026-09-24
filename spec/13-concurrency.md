# §13 Concurrency Model

**Scope:** Consolidates concurrency specification: memory model, lock
hierarchy, lock-free structures, fiber scheduling, GC
safepoint/thread-suspension interaction, async signal safety.
Supersedes fragments in §2.11, §3.11, §5.1.5, §5.4.7, §8.8.

Source: `crates/torcl-rt/src/{thread,scheduler,safepoint,signal}.rs`,
`crates/torcl-rt/src/gc/`.

---

## 13.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R13.01 | TorCL MUST provide sequential consistency for reads and writes to special (dynamically-bound) variables within a single thread. | MUST |
| R13.02 | Reads and writes to global symbol value cells from different threads MUST use acquire-release ordering. | MUST |
| R13.03 | Reads and writes to CLOS slot values from different threads MAY use relaxed ordering unless the slot is declared `(:synchronized t)`. | MAY |
| R13.04 | Atomic compare-and-swap operations (`TORCL-EXT:CAS`) MUST provide acquire-release semantics. | MUST |
| R13.05 | A complete lock-ordering hierarchy MUST be defined spanning all subsystems; acquiring locks out of order MUST be detected in debug builds (see also R8.16). | MUST |
| R13.06 | The lock hierarchy MUST be documented as a single total order with numeric levels; every lock in the system MUST be assigned a level. | MUST |
| R13.07 | Symbol value cells, inline-cache (IC) patch words, and profiling counters MUST be implemented as lock-free data structures. | MUST |
| R13.08 | The fiber scheduler MUST use a work-stealing deque (Chase-Lev) per exposed OS carrier thread with LIFO push/pop for local work and FIFO steal for cross-carrier balancing. | MUST |
| R13.09 | The scheduler MUST support cooperative preemption via per-fiber yield flags checked at safepoints; time-slice expiry MUST set the yield flag. | MUST |
| R13.10 | GC stop-the-world requests MUST be coordinated via the safepoint polling-page mechanism (§2.5, §3.9); no thread MUST be suspended asynchronously. | MUST |
| R13.11 | Fibers or native threads in the `Native` state (executing FFI calls) MUST be excluded from safepoint handshake; their CL stacks MUST be scannable without cooperation. | MUST |
| R13.12 | Fibers or native threads in the `Blocked` state MUST publish their stack top before blocking; the GC MUST be able to scan their stacks without waking them. | MUST |
| R13.13 | All signal handlers MUST be async-signal-safe; non-trivial work MUST be deferred to the next safepoint via per-thread flags (see also R8.10). | MUST |
| R13.14 | The runtime MUST support a `SIGUSR1` fallback to interrupt threads blocked in long-running syscalls, but the handler MUST only set a flag — it MUST NOT suspend the thread. | MUST |
| R13.15 | The runtime MUST provide `TORCL-EXT:WITH-ATOMIC` — a macro that disables thread preemption (yield-flag checks) for its dynamic extent, to allow short critical sections without locks. | MUST |
| R13.16 | `WITH-ATOMIC` sections MUST NOT span safepoints; the compiler MUST reject code that places a safepoint poll inside a `WITH-ATOMIC` body if the body exceeds an estimated 10 µs execution bound. | MUST |
| R13.17 | The runtime MUST support execution-local dynamic bindings: each native thread and each fiber has its own binding stack for special variables. Binding a special variable in one execution MUST NOT affect another. | MUST |
| R13.18 | The runtime MUST provide exposed OS-backed thread lifecycle and introspection in `TORCL-THREAD`, plus a distinct lightweight fiber lifecycle and introspection API in `TORCL-FIBER` (§9.2). A thread and a fiber MUST NOT be aliases or accepted interchangeably. | MUST |
| R13.19 | Mutex, condition-variable, read-write-lock, and semaphore primitives MUST be provided in the `TORCL-THREAD` package. | MUST |
| R13.20 | The runtime SHOULD detect potential deadlocks (lock-wait-graph cycle detection) in debug builds (see also R8.21). | SHOULD |

---

## 13.2 Memory Model

TorCL exposes a memory model to CL code that balances safety with
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
| `TORCL-EXT:CAS` | **Acquire-Release** (AcqRel) | Both success and failure paths use AcqRel. (R13.04) |
| `TORCL-EXT:ATOMIC-INCF` / `ATOMIC-DECF` | **Acquire-Release** | Fetch-and-add with AcqRel ordering. |
| Hash-table entry read (synchronized table) | **Acquire** (via striped lock) | Lock acquisition provides the fence. |
| Hash-table entry write (synchronized table) | **Release** (via striped lock) | Lock release provides the fence. |
| Write-barrier stores (SATB buffer, card table) | **Relaxed** | Benign races acceptable (§3.11). |

### 13.2.2 Data-Race Semantics

A data race on a non-atomic CL value MUST NOT crash the process. All
`TorclVal` slots are `AtomicU64` at the Rust level with relaxed
loads/stores for unsynchronized paths. A racy read MAY return a stale
value but MUST NOT return a value with an invalid tag. The type tag is
always written atomically with the payload (naturally aligned 64-bit
writes are tear-free on x86-64 and aarch64).

### 13.2.3 Fences and Compiler Barriers

| Primitive | Mapping |
|-----------|---------|
| `TORCL-EXT:MEMORY-BARRIER` | Full `SeqCst` fence. |
| `TORCL-EXT:LOAD-BARRIER` | `fence(Acquire)`. |
| `TORCL-EXT:STORE-BARRIER` | `fence(Release)`. |

The JIT MUST NOT reorder memory accesses across these barriers.

---

## 13.3 Lock Ordering Protocol

### 13.3.1 Complete Lock Hierarchy

All locks in the system are assigned a numeric level. A thread MUST acquire
locks in ascending level order. Locks at the same level may nest only where
the table defines a stable sub-order, and that sub-order MUST strictly
increase. Any other acquisition at a level less than or equal to the highest
level currently held is a violation and MUST panic in debug builds (R13.05,
R13.06).

| Level | Lock | Protects | Subsystem | Notes |
|-------|------|----------|-----------|-------|
| 0 | (thread-local — no lock) | TLAB, dynamic bindings, handler bindings, SATB buffer | §2, §3 | Never contended. |
| 1 | Per-stream mutex | Stream state and buffer | §5.4 | Ordered by `stream-id` when multiple streams are needed. |
| 2 | Hash-table/table-stripe lock; per-symbol plist RwLock | Hash-table entries; symbol property lists | §5.5, §13.4.1 | Ordered by table identity then bucket index. |
| 3 | Global package registries | Package creation/deletion/rename and registry lookup | §5.1 | Acquired before a package object (Rule L1). Multiple registries use their declared sub-order. |
| 4 | Per-package RwLock | Package metadata and symbol tables | §5.1 | Multiple packages are ordered by `Package.id`. |
| 5 | Interned-string table | Canonical string-object identities | §5.4 | Package operations may prepare string keys while holding a package lock; no allocation may occur while holding level 6 or above. |
| 6 | Compiler/code-cache and debugger-metadata locks | Code installation, compiler registries, IC/debug trap coordination | §4, §6 | — |
| 7 | Profiling-data locks | Sampling, allocation, instrumentation and tier-promotion data | §4.9, §6 | Ordered by the declared profiler sub-order. |
| 8 | GC world and external-root locks | Heap state, root scanners, weak/finalizer registries, all side tables containing relocatable `TorclVal`s | §3.9 | The heap lock has sub-order 1; root side tables have larger sub-orders. |
| 9 | Execution and scheduler registries | Native-thread/fiber registries, scheduler groups, timer/I/O registries, any IDE-backend connection registry (owned by the loaded SLIME/SLY backend, §6.1.5, when it runs multi-threaded) | §2.3, §13.7 | Must precede per-execution objects. |
| 10 | Per-execution object locks | Native-thread/fiber state, stacks and individual connection objects | §2.3 | Ordered by stable object identity when multiple objects are needed. |
| 11 | Image save mutex | Heap serialisation to `.bimg` | §7 | Highest level — no other lock may be acquired while held. |

The implementation names these levels in
`torcl_rt::lock_order::LockLevel`. Long-lived cross-subsystem locks use
`OrderedMutex` or `OrderedRwLock`. Condvar wait cells, user-visible Lisp
locks, and a stream's private I/O mutex are protocol locks: they are assigned
the level of their owning object but MUST be leaf acquisitions (no other
runtime lock may be acquired while they are held). They remain ordinary Rust
mutexes where `Condvar` interoperability is required; the owning operation
must release the wait cell before entering another subsystem.

### 13.3.2 Multi-Package Lock Ordering (from §5.1.5)

When an operation (e.g., `USE-PACKAGE`, `IMPORT`) requires write locks
on multiple packages, the packages MUST be sorted by `Package.id`
(ascending), deduplicated, and locked in that order (A5.03). This
sub-ordering operates within level 4 of the global hierarchy.

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

A thread-local stack records `(level, sub-order, lock identity, name)` for
each ordered lock currently held. Before acquisition in debug builds, the
wrapper checks that the requested level is greater than the top level, or
that it has a strictly greater non-zero sub-order within the same level.
Violation panics with both lock names, levels, and sub-orders. Guards pop the
stack in reverse acquisition order, including during unwinding.

### 13.3.6 Deadlock Detection (R13.20)

In debug builds, a background watchdog thread (interval:
`TORCL_DEADLOCK_WATCHDOG_MS`, default 5 s) scans the lock-wait graph
for cycles. The graph is constructed from per-thread lock stacks
(locks held) and a `LOCK_WAITERS` table (thread → waited-for lock).
A cycle implies deadlock; the watchdog reports thread IDs, lock names,
levels, and sub-orders to stderr. It is informational and does not abort.
Setting the interval to `0` disables the watchdog.

---

## 13.4 Lock-Free Data Structures

### 13.4.1 Symbol Value Cells (R13.07)

Global symbol value cells are `AtomicU64` fields storing `TorclVal`.
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
walking the chain. Each node holds `buffer: [TorclVal; SATB_BUFFER_SIZE]`,
`len: usize`, `next: AtomicPtr`.

---

## 13.5 Fiber Scheduling on Exposed Carrier Threads

### 13.5.1 Architecture Overview

Each exposed carrier thread in a scheduler group owns an internal Chase-Lev
deque and a TLAB. Carriers steal from random peers when idle. A global overflow queue and an I/O poller
(epoll/kqueue) feed tasks into the system.

### 13.5.2 Scheduling Algorithm (A13.02)

```text
ALGORITHM carrier_loop(carrier_id):
  loop:
    // 1. Try local deque (LIFO — cache-warm)
    task ← carrier.deque.pop()
    if task ≠ EMPTY:
      run(task)
      continue

    // 2. Try global overflow queue
    task ← global_queue.try_dequeue()
    if task ≠ EMPTY:
      run(task)
      continue

    // 3. Try stealing from a random peer (FIFO — fair)
    victim ← random_carrier(excluding: carrier_id)
    task ← victim.deque.steal()
    if task ≠ EMPTY:
      run(task)
      continue

    // 4. Try I/O poller for ready tasks
    ready ← io_poller.poll_nonblocking()
    if ready is not empty:
      for task in ready:
        carrier.deque.push(task)
      continue

    // 5. Park — no work available
    carrier.park_on_futex(timeout: 1ms)
    // Woken by: new task push, I/O completion, or timeout
```

### 13.5.3 Task Lifecycle

| Event | Action |
|-------|--------|
| `MAKE-FIBER` | Allocate a `Fiber` (D2.01) in `Created`; do not schedule it. |
| `SUBMIT-FIBER` | Associate a created fiber with one scheduler group, mark `Runnable`, push to a carrier deque, and wake a parked carrier. |
| `MAKE-THREAD` | Create a dedicated exposed native OS thread. It is not submitted to a fiber run queue. |
| Yield (cooperative) | Push current task back to deque tail, pop next task. |
| Block (mutex/condvar) | Remove task from deque, set state `Blocked`, record waker. |
| I/O submit | Set state `Waiting`, publish stack top, register with I/O poller. Treated like `Blocked` for GC purposes (§13.6.1). |
| I/O ready | I/O poller marks task ready, set state `Runnable`, push to worker deque. |
| Unblock (notify) | Set state `Runnable`, push to notifier's deque (or global queue if cross-worker). |
| FFI call | Set state `Native`, publish stack top. Task remains logically on the worker but does not block scheduling. |
| FFI return | Set state `Runnable`; if worker's deque is empty, resume immediately. |
| Death | Set state `Dead`, wake join waiters, return stack to pool. |

### 13.5.4 Public Scheduler Groups and Carriers

The scheduler-group object is public; its per-carrier scheduler and deque are
not. `START-FIBERS` creates the configured number of exposed
`TORCL-THREAD` carrier objects and `SCHEDULER-GROUP-CARRIERS` returns a stable
snapshot of them. Consequently thread monitoring, naming, interruption, and
backtraces work uniformly for application-created platform threads and runtime
carriers, while fiber monitoring uses the separate `TORCL-FIBER` API.

Fibers may migrate between carriers only while unpinned. A fiber is mounted on
at most one carrier at a time, and a carrier runs at most one fiber at a time.
The fiber's current/last carrier reference is updated before its state becomes
`:RUNNING`, using release ordering; introspection reads it with acquire ordering.

### 13.5.5 Cooperative Preemption (R13.09)

A scheduler-group service timer (1 ms by default) reads each carrier's
atomically published mounted-fiber ID and sets that fiber's `yield_flag`. The
timer never suspends a carrier. Every safepoint poll checks this flag;
if set, the fiber is pushed to the back of the local deque and
the next task is popped. Default time slice: 1 ms (`TORCL_TIME_SLICE_US`).

### 13.5.6 Priority Levels

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

All TorCL signal handlers MUST obey POSIX async-signal-safety:

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
| `SIGINT` | `pending_interrupt` | Signal `INTERRUPT-CONDITION` to foreground fiber/native thread |
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

Each fiber and native thread owns a **binding stack**: a stack of `(symbol,
previous_value)` pairs representing dynamically-bound special variables.

### 13.8.1 Binding Protocol

`bind_special`: load global cell (`Acquire`), push `(symbol,
old_value)` to binding stack, set `thread.tls_map[symbol] = new_value`.
`unbind_special`: pop binding stack, remove from `tls_map`. The global
cell is never modified — thread-local bindings shadow it.

**Reading a special** checks `tls_map` first; if absent, falls back to
the global cell with `Acquire` ordering.

### 13.8.2 Inherited Bindings

New fibers and native threads inherit a snapshot of the parent's `*standard-input*`,
`*standard-output*`, `*error-output*`, `*debug-io*`, `*query-io*`,
`*trace-output*`, `*terminal-io*`, and `*package*` bindings. Other
specials start with global values.

---

## 13.9 Synchronisation Primitives (R13.19)

### 13.9.1 Primitive Types

| Type | CL Name | Implementation |
|------|---------|----------------|
| Mutex | `TORCL-THREAD:MUTEX` | Futex-based, non-reentrant by default; `:recursive t` option available. |
| RW-Lock | `TORCL-THREAD:RW-LOCK` | Reader-writer lock with writer preference to prevent starvation. |
| Condition Variable | `TORCL-THREAD:CONDITION-VARIABLE` | Paired with a mutex; supports `WAIT`, `NOTIFY`, `NOTIFY-ALL`. |
| Semaphore | `TORCL-THREAD:SEMAPHORE` | Counting semaphore; `WAIT` decrements, `SIGNAL` increments. |
| Barrier | `TORCL-THREAD:BARRIER` | N-thread barrier with `WAIT` that blocks until all N threads arrive. |

`TORCL-THREADS` is a deprecated compatibility nickname for `TORCL-THREAD`; new
APIs and examples use `TORCL-THREAD`.

### 13.9.1.1 Public Lambda Lists

```lisp
(torcl-thread:make-mutex &key name recursive) -> mutex
(torcl-thread:grab-mutex mutex &key (waitp t) timeout) -> boolean
(torcl-thread:release-mutex mutex &key (if-not-owner :error)) -> nil
(torcl-thread:with-mutex (mutex &key (waitp t) timeout) body*) -> values

(torcl-thread:make-rw-lock &key name) -> rw-lock
(torcl-thread:grab-rw-lock-read rw-lock &key (waitp t) timeout) -> boolean
(torcl-thread:grab-rw-lock-write rw-lock &key (waitp t) timeout) -> boolean
(torcl-thread:release-rw-lock-read rw-lock &key (if-not-owner :error)) -> nil
(torcl-thread:release-rw-lock-write rw-lock &key (if-not-owner :error)) -> nil
(torcl-thread:with-rw-lock-read (rw-lock &key (waitp t) timeout) body*) -> values
(torcl-thread:with-rw-lock-write (rw-lock &key (waitp t) timeout) body*) -> values

(torcl-thread:make-condition-variable &key name) -> condition-variable
(torcl-thread:condition-wait condition-variable mutex &key timeout) -> boolean
(torcl-thread:condition-notify condition-variable &optional (count 1)) -> count
(torcl-thread:condition-broadcast condition-variable) -> count

(torcl-thread:make-semaphore &key name (count 0)) -> semaphore
(torcl-thread:signal-semaphore semaphore &optional (count 1)) -> nil
(torcl-thread:wait-on-semaphore semaphore &key timeout notification) -> boolean
(torcl-thread:try-semaphore semaphore &optional (count 1)) -> boolean
(torcl-thread:semaphore-count semaphore) -> non-negative-integer

(torcl-thread:make-barrier count &key name) -> barrier
(torcl-thread:barrier-wait barrier &key timeout) -> index, status
(torcl-thread:barrier-count barrier) -> positive-integer
(torcl-thread:barrier-waiting-count barrier) -> non-negative-integer
(torcl-thread:reset-barrier barrier) -> nil
```

`TIMEOUT` is `NIL` or a non-negative real number of seconds. `WAITP NIL`
performs a non-blocking try. `IF-NOT-OWNER` accepts `:ERROR`, `:WARN`, or
`:IGNORE`. Barrier wait status is `:OK`, `:TIMEOUT`, or `:RESET`.

### 13.9.2 Green-Thread–Aware Blocking

Blocking on a sync primitive parks an unpinned fiber (§2.3.2), freeing its
carrier for other fibers. Unblocking pushes the fiber back onto a carrier's run
queue. A plain native thread, or a pinned fiber under the configured policy,
uses the OS-blocking primitive.

Each blocking operation receives a monotonically increasing per-fiber wait
token. Timer and I/O completions carry that token, so a completion left over
from an earlier timeout cannot wake a later wait. Enqueue and transition to
`Blocked`/`Waiting` occur before releasing the primitive's wait-queue lock; a
wake that races with the carrier-side unmount is recorded as pending and is
consumed by the carrier after the context switch. This closes both the
enqueue-to-park lost-wakeup window and the early-unpark double-mount window.

Deadline waits share a runtime timer service rather than creating one native
thread per sleeping fiber. Descriptor readiness uses a shared epoll poller on
Linux and kqueue on BSD-family systems, with a `poll(2)` helper fallback on
other Unix targets. A readiness or deadline completion requeues the fiber on
the scheduler group that accepted its first submission; a fiber MUST NOT be
submitted to a second scheduler group.

---

## 13.10 Configuration

| Parameter | Default | Env Var | Description |
|-----------|---------|---------|-------------|
| Carrier threads | `nproc` | `TORCL_WORKERS` | Default scheduler-group carrier count. |
| Time slice | 1000 µs | `TORCL_TIME_SLICE_US` | Cooperative preemption interval. |
| Safepoint spin iterations | 1000 | `TORCL_SAFEPOINT_SPIN` | Spin count before parking at safepoint. |
| Deadlock watchdog interval | 5 s | `TORCL_DEADLOCK_WATCHDOG_MS` | Debug-build only. 0 = disabled. |
| SIGUSR1 timeout | 100 ms | `TORCL_SIGUSR1_TIMEOUT_MS` | Delay before sending SIGUSR1 fallback. |
| Fiber stack size | 512 KiB | `TORCL_STACK_SIZE` | Per-fiber CL stack. |
| Max fibers | 100 000 | `TORCL_MAX_FIBERS` | Upper limit (legacy `TORCL_MAX_THREADS` accepted; R2.19). |
| Pinned blocking | `:WARN` | `TORCL_PINNED_BLOCKING_ACTION` | `WARN`, `ERROR`, or `NIL`/`NATIVE` fallback policy (R9.43). |

---

## 13.11 Error Handling

| Failure | Response |
|---------|----------|
| Lock-order violation (debug) | `debug_assert!` panic with diagnostic. |
| Deadlock detected (debug) | Log cycle to stderr and crash log; no abort (informational). |
| Safepoint timeout (>1 s) | Panic — indicates a bug (thread stuck in unsafe code). |
| Native-thread creation failure | Signal `TORCL-THREAD:THREAD-ERROR` with `:reason :limit-exceeded` or `:reason :oom`. |
| Fiber creation/submission failure | Signal `TORCL-FIBER:FIBER-ERROR` with `:reason :limit-exceeded`, `:already-submitted`, `:closed-group`, or `:oom`. |
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
| Cooperative preemption | 100 CPU-bound fibers on 4 exposed carriers; verify all make progress (no starvation) within 100 ms. | All fibers run. |
| SIGUSR1 fallback | Thread blocked in `read(2)` on a pipe; request STW; verify thread reaches safepoint via SIGUSR1 within 200 ms. | Safepoint reached. |
| WITH-ATOMIC correctness | Verify preemption is suppressed inside `WITH-ATOMIC`; verify GC safepoint is NOT suppressed (compile-time check). | Correct behaviour. |
| Binding stack isolation | Two threads bind the same special to different values; verify no cross-thread leakage. | Thread-local isolation. |

---

## 13.13 Module Map

| Source File | Responsibility | Key Types / Functions |
|-------------|---------------|-----------------------|
| `crates/torcl-rt/src/thread.rs` | Native-thread/fiber descriptors, continuation switching, carrier pool | `NativeThread`, `Fiber`, `FiberContinuation`, `ChaseLevDeque` |
| `crates/torcl-rt/src/scheduler.rs` | Public scheduler-group lifecycle over exposed carriers | `SchedulerGroup`, `SchedulerConfig` |
| `crates/torcl-rt/src/safepoint.rs` | Safepoint page, STW protocol | `SafepointPage`, `stop_the_world()`, `resume_all()` |
| `crates/torcl-rt/src/sync/mutex.rs` | Green-thread-aware mutex | `TorclMutex`, `TorclRwLock` |
| `crates/torcl-rt/src/sync/condvar.rs` | Condition variables | `TorclCondVar` |
| `crates/torcl-rt/src/sync/semaphore.rs` | Counting semaphore, barrier | `TorclSemaphore`, `TorclBarrier` |
| `crates/torcl-rt/src/sync/timer.rs` | Shared deadline scheduler | per-fiber generation-tagged timer wakeups |
| `crates/torcl-rt/src/sync/io.rs` | Async descriptor readiness | epoll/kqueue poller; poll fallback |
| `crates/torcl-rt/src/sync/atomic.rs` | CAS, atomic-incf, memory barriers | `torcl_cas()`, `atomic_incf()` |
| `crates/torcl-rt/src/{runtime,safepoint,thread}.rs` | Signal handlers, directed SIGUSR1 fallback | `install_signal_handlers()`, `sigusr1_handler()`, participant signalling |
| `crates/torcl-rt/src/gc/safepoint.rs` | GC-side safepoint coordination | `request_safepoint()`, `wait_at_safepoint()` |
| `crates/torcl-rt/src/profiling.rs` | Lock-free profiling counters | `FunctionProfile` |
| `crates/torcl-compiler/src/ic.rs` | Inline cache patching | `MonomorphicIC`, `patch_ic()` |
