# §9  SBCL-Compatible Extensions

**Scope:** TorCL adopts a curated set of SBCL-compatible extensions where
ANSI X3.226-1994 is silent and the extension is widely depended upon by the
portable Common Lisp ecosystem. Each extension MUST have a rationale,
MUST NOT conflict with ANSI semantics, and MUST be documented here.

## 9.1  Adoption Criteria

An SBCL extension is adopted when ALL of the following hold:
1. **Ecosystem demand** — used by ≥3 popular libraries (Quicklisp top-100)
   or essential for practical CL development.
2. **No ANSI conflict** — operates in an unspecified or implementation-defined area.
3. **Clean namespace** — exported from `TORCL-EXT` (not `CL`), with
   `SB-EXT`-compatible symbol names via a compatibility package.
4. **Documented divergence** — any behavioural difference from SBCL listed explicitly.

**R9.01** TorCL MUST provide a `TORCL-EXT` package exporting all adopted extensions.
**R9.02** TorCL MUST provide `SB-EXT`, `SB-THREAD`, `SB-MOP` compatibility
packages that re-export the corresponding TorCL symbols.
**R9.03** Each adopted extension MUST include a test verifying SBCL-compatible behaviour.

---

## 9.2  Native Threads and Fibers (`SB-THREAD` Compatibility)

**Design decision (2026-08-20).** TorCL adopts the scheduler-group and fiber
lifecycle shape described by the SBCL Fibers proposal, but deliberately uses a
JVM-style two-level public model: OS-backed platform/carrier threads and
lightweight fibers are separate, exposed object types. This preserves honest
resource and blocking semantics—code can tell whether it owns an OS thread or
a schedulable fiber—while still allowing carrier introspection. It rejects the
earlier design in which `MAKE-THREAD` secretly returned a managed fiber.

**R9.04** TorCL MUST expose OS-backed platform/carrier threads through the
`TORCL-THREAD` package and lightweight managed fibers through the distinct
`TORCL-FIBER` package. `TORCL-THREADS` is a deprecated nickname for
`TORCL-THREAD`. `SB-THREAD` re-exports the compatible native-thread and
synchronisation symbols; it MUST NOT make a fiber appear to be an OS thread.

### 9.2.1  Native/Carrier Thread Lifecycle

```lisp
(make-thread function &key name arguments ephemeral)
  ;; function   — designator for a function (APPLY'd with ARGUMENTS)
  ;; name       — string or NIL; ephemeral — if T, runtime MAY auto-reap
  ;; Returns → native thread (D9.01), backed one-to-one by an OS thread.

(join-thread thread &key default timeout)
  ;; Blocks until THREAD terminates.  timeout — real seconds or NIL.
  ;; Returns → (values result status)  status ∈ {:OK :TIMEOUT :ABORTED}
  ;; Multiple threads MAY join the same thread.

(thread-alive-p thread)       ;; → boolean; lock-free atomic read.
(destroy-thread thread)       ;; Sends TERMINATE-THREAD at next safepoint. Idempotent.
(interrupt-thread thread fn)  ;; Calls FN in THREAD at next safepoint (§2.5).
(abort-thread &optional cond) ;; Cleanly unwinds the calling thread.
```

**Special variables:** `*current-thread*` (bound per-native-thread, read-only),
`(all-threads)` → a snapshot of live native thread objects. Scheduler carrier
threads are ordinary visible native thread objects and also appear in
`(scheduler-group-carriers group)` and `(all-threads)`.

`MAKE-THREAD` and `JOIN-THREAD` are native-thread operations, not compatibility
aliases for fibers. A native thread created by `MAKE-THREAD` runs its function
directly. A scheduler group creates additional native threads as carriers and
multiplexes fibers over them. Per-carrier schedulers, work-stealing deques, and
OS handles remain implementation details even though the carrier thread object
itself is public.

### 9.2.2  Fiber Lifecycle and Scheduler Groups

**R9.42** TorCL MUST provide the following API in `TORCL-FIBER`:

```lisp
(make-fiber function &key name arguments stack-size initial-bindings)
  ;; Create a fiber in :CREATED state without starting it. Returns D9.10.

(fiber-yield)                         ;; Yield the current unpinned fiber.
(fiber-sleep seconds)                 ;; Cooperative deadline wait.
(fiber-park predicate &key timeout)   ;; Predicate or timeout; T iff predicate won.
(fiber-join fiber &key timeout)       ;; Cooperatively wait; return entry values.

(run-fibers fibers &key carrier-count idle-hook) ;; Blocking convenience API.
(start-fibers fibers &key carrier-count idle-hook) ;; → scheduler-group (D9.11)
(submit-fiber group fiber)            ;; Thread-safe dynamic submission.
(finish-fibers group)                 ;; Join carriers; results in submission order.
(fiber-group-done-p group)            ;; Non-blocking completion predicate.
(scheduler-group-carriers group)      ;; Snapshot of exposed TORCL-THREAD objects.
```

A fiber belongs to at most one scheduler group. `START-FIBERS` creates an
explicit group and its exposed carrier threads; `RUN-FIBERS` is equivalent to
`START-FIBERS` followed by `FINISH-FIBERS`. `SUBMIT-FIBER` accepts work until
finishing begins. `FINISH-FIBERS` closes submission, waits for all submitted
fibers, shuts down and joins the group's carrier threads, and is idempotent.
Results preserve submission order. `carrier-count` defaults to the number of
available processors and MUST be at least one.

**R9.43** Fiber pinning MUST use a balanced per-fiber counter:

```lisp
(fiber-pin &optional fiber)
(fiber-unpin &optional fiber)
(fiber-can-yield-p &optional fiber)
(with-fiber-pinned ((&optional fiber)) &body body)
*pinned-blocking-action* ; :WARN (default), :ERROR, or NIL
```

Pinning prevents unmounting/migration from the current carrier. Explicit
`FIBER-YIELD` while pinned signals `PROGRAM-ERROR`. A fiber-aware blocking
primitive parks an unpinned fiber without blocking its carrier. If the fiber is
pinned, `:WARN` warns then uses the OS-blocking operation, `:ERROR` signals an
error before blocking, and `NIL` silently uses the OS-blocking operation.
`WITH-FIBER-PINNED` MUST unpin with `UNWIND-PROTECT` semantics.
The bootstrap runtime maps `*PINNED-BLOCKING-ACTION*` to
`TORCL_PINNED_BLOCKING_ACTION`; `NIL` and `NATIVE` select the silent native
fallback spelling.

**R9.44** Fiber introspection MUST include:

```lisp
(current-fiber)                    ;; Current fiber or NIL on a plain native thread.
(list-all-fibers)                  ;; Snapshot of all non-reclaimed fiber objects.
(fiber-state fiber)                ;; :CREATED/:RUNNABLE/:RUNNING/:SUSPENDED/:DEAD
(fiber-name fiber)                 ;; String or NIL.
(fiber-result fiber)               ;; Result values as a list after :DEAD.
(fiber-error-p fiber)              ;; Whether death followed an unhandled condition.
(fiber-alive-p fiber)              ;; State is not :DEAD.
(fiber-carrier-thread fiber)       ;; Current/last exposed carrier, or NIL.
(print-fiber-backtrace fiber &key stream count)
```

`PRINT-FIBER-BACKTRACE` supports `:CREATED`, `:RUNNABLE`, `:SUSPENDED`, and
`:DEAD` fibers from their saved continuation. A running fiber MUST first be
cooperatively brought to a safepoint or the operation signals
`FIBER-STILL-RUNNING`; its live stack MUST NOT be walked concurrently.

### 9.2.3  Mutexes

```lisp
(make-mutex &key name)                ;; → mutex (D9.02). Non-recursive (SBCL compat).
(grab-mutex mutex &key waitp timeout) ;; waitp=NIL → try-lock. Returns T/NIL.
(release-mutex mutex &key if-not-owner) ;; if-not-owner ∈ {:WARN :FORCE :ERROR}
(with-mutex (mutex &key waitp timeout) &body body) ;; UNWIND-PROTECT wrapper.
```

### 9.2.4  Condition Variables

```lisp
(torcl-thread:make-condition-variable &key name) ;; → condition-variable (D9.03)
(torcl-thread:condition-wait condition-variable mutex &key timeout) ;; Atomically release+block. Returns T/NIL.
(torcl-thread:condition-notify condition-variable &optional (count 1)) ;; Wake COUNT waiters.
(torcl-thread:condition-broadcast condition-variable) ;; Wake all waiters.
```

### 9.2.5  Semaphores

```lisp
(make-semaphore &key name count)              ;; count — initial permits (default 0) → D9.04
(signal-semaphore semaphore &optional (n 1))  ;; Increment permits, wake waiters.
(wait-on-semaphore sem &key timeout notification) ;; Block if zero. notification → D9.05.
(semaphore-count semaphore)                   ;; → non-negative integer; lock-free.
(try-semaphore semaphore &optional (n 1))     ;; Non-blocking. Returns T/NIL.
```

### 9.2.6  Memory Barriers

```lisp
(torcl-ext:memory-barrier &optional (kind :full)) ;; KIND ∈ {:READ :WRITE :FULL :DATA-DEPENDENCY}.
(torcl-ext:load-barrier)
(torcl-ext:store-barrier)
```

### 9.2.7  Atomic Operations (CAS)

**R9.37** TorCL MUST provide SBCL-compatible atomic operations in `TORCL-EXT`,
re-exported via `SB-EXT` and, where SBCL compatibility requires it,
`SB-THREAD`.

```lisp
(torcl-ext:cas place old new)
  ;; Macro. Atomically: if PLACE holds OLD (by EQ), store NEW, return OLD value.
  ;; If PLACE does not hold OLD, return actual current value; no store.
  ;; Applicable place types: special variables, structure slots (defstruct),
  ;;   SVREF, CAR, CDR, SYMBOL-PLIST, SYMBOL-VALUE, SLOT-VALUE.
  ;; Expansion: compiler-generated CAS intrinsic per place type.
  ;; Thread-safety: lock-free; full memory barrier on success.

(torcl-ext:atomic-incf place &optional (delta 1))
  ;; Atomically increment PLACE by DELTA (a fixnum). Returns previous value.
  ;; Applicable places: fixnum-typed special variables, structure slots
  ;;   declared (type fixnum), SVREF of (simple-array fixnum).
  ;; Thread-safety: lock-free fetch-and-add.

(torcl-ext:atomic-decf place &optional (delta 1))
  ;; Atomically decrement PLACE by DELTA (a fixnum). Returns previous value.
  ;; Same applicable places and semantics as ATOMIC-INCF.
```

**D9.08 — CAS Expansion:** The `CAS` macro expands to a `%COMPARE-AND-SWAP` compiler
intrinsic selected by place type. For struct slots, the compiler resolves the slot
offset at compile time. For special variables, CAS operates on the TLS cell (§2.4)
with fallback to the global cell. Overflow from fixnum arithmetic in `ATOMIC-INCF`/
`ATOMIC-DECF` signals `ARITHMETIC-ERROR` (not undefined behaviour).

### 9.2.8  Recursive Locks

**R9.38** TorCL MUST provide recursive mutexes compatible with SBCL's `sb-thread:make-lock`.

```lisp
(make-lock &key name)
  ;; → recursive-lock (D9.09). A lock may be acquired multiple times by the
  ;; owning thread without deadlock; each GRAB-LOCK must be matched by RELEASE-LOCK.

(grab-lock lock &key waitp timeout)
  ;; waitp=NIL → try-lock. Returns T on success, NIL on failure/timeout.
  ;; If already held by current thread, increments recursion count.

(release-lock lock &key if-not-owner)
  ;; Decrements recursion count; releases when count reaches zero.
  ;; if-not-owner ∈ {:WARN :FORCE :ERROR} (default :ERROR).

(with-lock (lock &key waitp timeout) &body body)
  ;; UNWIND-PROTECT wrapper; releases one recursion level on exit.
```

**D9.09 — Recursive-Lock:** `header(8) | name(8) | owner:AtomicU64(8) | state:AtomicU32(4) | recursion-count:u32(4) | futex(4) | pad(4)` — 40 bytes.

### 9.2.9  Data Structures

**D9.01 — Native Thread:** `header(8) | name(8) | state:AtomicU8(1) | ephemeral(1) | carrier(1) | pad(5) | os-handle(8) | result(8) | mailbox-head:AtomicPtr(8) | join-waiters:AtomicPtr(8)` — 56 bytes. State ∈ {:BORN :RUNNING :BLOCKED :NATIVE :DEAD :ABORTED}. `carrier` identifies a thread owned by a scheduler group; it does not change object identity or visibility.

**D9.02 — Mutex:** `header(8) | name(8) | owner:AtomicU64(8) | state:AtomicU32(4) | futex(4)` — 32 bytes. State: 0=free, 1=locked, 2=contended.

**D9.03 — Waitqueue:** `header(8) | name(8) | queue-head:AtomicPtr(8)` — 24 bytes.

**D9.04 — Semaphore:** `header(8) | name(8) | count:AtomicI64(8) | wait-queue:AtomicPtr(8)` — 32 bytes.

**D9.05 — Semaphore-Notification:** `header(8) | status:AtomicU8(1)` — 16 bytes (padded).

**D9.10 — Fiber:** A GC-managed descriptor containing identity/name, lifecycle
state, entry and result, control/value stack, saved continuation, dynamic TLS
bindings, handler/restart/unwind state, pin count, wake predicate/deadline,
published SP/FP, scheduler-group membership, and current/last carrier thread.

**D9.11 — Fiber Scheduler Group:** A GC-managed lifecycle handle containing its
exposed carrier-thread vector, submitted fibers in stable order, active count,
submission-closed flag, and join notification. Each carrier owns an internal
work-stealing deque and scheduler state (§13.5).

### 9.2.10  Compatibility: TorCL vs SBCL

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `make-thread` | OS thread | exposed OS native/carrier thread; adds `:arguments`, `:ephemeral` | superset; never creates a fiber |
| Fiber API | experimental SBCL proposal | `TORCL-FIBER` | separate package; JVM-style type distinction |
| Mutex recursion | non-recursive (error) | non-recursive (error) | identical |
| `interrupt-thread` | safe-point delivery | safepoint delivery (§2.5) | identical |
| `destroy-thread` | `terminate-thread` | `destroy-thread` + alias | name differs |
| `condition-wait` spurious | possible | possible | identical |
| `memory-barrier` | internal | `torcl-ext:memory-barrier` | TorCL addition |
| `cas` | `sb-ext:cas` macro | `torcl-ext:cas` macro | identical semantics |
| `atomic-incf`/`decf` | `sb-ext:atomic-incf` | `torcl-ext:atomic-incf` | identical; overflow signals error |
| CAS places | specials, struct, svref, car/cdr | same set | identical |
| `make-lock` (recursive) | `sb-thread:make-lock` | `torcl-thread:make-lock` | identical; `TORCL-THREADS` is a deprecated nickname |
| Recursive lock semantics | re-entrant, counted | re-entrant, counted | identical |

---

## 9.3  Global Variables

**R9.06** `DEFGLOBAL` — globally special, same value across all threads. Atomic cell.
**R9.07** `DEFINE-LOAD-TIME-GLOBAL` — evaluated once at load time, treated as constant.

```lisp
(defglobal name value &optional doc)            ;; Atomic global cell. Compiler MAY inline.
(define-load-time-global name value &optional doc) ;; Evaluated once; not re-evaluated on reload.
```

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `defglobal` | `sb-ext:defglobal` | `torcl-ext:defglobal` | identical semantics |
| Thread visibility | seq-cst | seq-cst | identical |

---

## 9.4  Weak References and Finalizers

**R9.08** `MAKE-WEAK-POINTER` / `WEAK-POINTER-VALUE` — GC may clear if unreachable.
**R9.09** `FINALIZE` / `CANCEL-FINALIZATION` — finalizers run in dedicated thread, never during GC (§3).

```lisp
(make-weak-pointer object)              ;; → weak-pointer (D9.06)
(weak-pointer-value wp)                 ;; → (values object alive-p)
(finalize object function &key dont-save) ;; dont-save: skip image serialization
(cancel-finalization object)            ;; Remove all finalizers. Idempotent.
```

**D9.06 — Weak-Pointer:** `header(8) | referent:TorclVal(8) | broken-p:AtomicU8(1)` — 24 bytes (padded). GC clears referent to NIL when target unreachable.

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `weak-pointer-value` 2nd val | `t`/`nil` | `t`/`nil` | identical |
| Finalizer thread | dedicated | dedicated | identical |

---

## 9.5  Hash Table Extensions

**R9.10** `:SYNCHRONIZED` → thread-safe hash table with reader-writer lock.
**R9.11** `:WEAKNESS` → `:KEY`, `:VALUE`, `:KEY-AND-VALUE`, `:KEY-OR-VALUE`.
**R9.12** `WITH-LOCKED-HASH-TABLE` → multi-operation atomicity.

### 9.5.1  API

```lisp
(make-hash-table &key test size rehash-size rehash-threshold
                      hash-function synchronized weakness)
  ;; Extended lambda list — SBCL-compatible keyword args:
  ;; synchronized — if T, all accesses protected by an internal RW lock.
  ;; weakness     — NIL | :KEY | :VALUE | :KEY-AND-VALUE | :KEY-OR-VALUE
  ;;   :KEY            — entry cleared when key is unreachable
  ;;   :VALUE          — entry cleared when value is unreachable
  ;;   :KEY-AND-VALUE  — cleared when both are unreachable
  ;;   :KEY-OR-VALUE   — cleared when either is unreachable
  ;; hash-function — (function (t) (values fixnum)) overriding default for TEST.
  ;; Returns → hash-table.

(with-locked-hash-table (hash-table) &body body)
  ;; Acquires exclusive (write) lock on HASH-TABLE for the dynamic extent of BODY.
  ;; If HASH-TABLE is not :SYNCHRONIZED, executes BODY without locking.
  ;; Returns → the values of the last form in BODY.
  ;; Thread-safety: re-entrant for the owning thread (recursive lock).

(hash-table-synchronized-p hash-table)  ;; → boolean
(hash-table-weakness hash-table)        ;; → NIL | weakness keyword
```

### 9.5.2  Compatibility

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `:synchronized` | RW lock | RW lock | identical |
| `:weakness` keywords | 4 modes | 4 modes | identical |
| `with-locked-hash-table` | exclusive lock | exclusive lock | identical |
| `hash-table-synchronized-p` | supported | supported | identical |
| `hash-table-weakness` | supported | supported | identical |
| `:hash-function` | supported | supported | identical |
| Concurrent resize | stop-the-world | incremental rehash | TorCL avoids long pauses |

---

## 9.6  Sequence Extensions

**R9.13** Extensible sequences via `SB-SEQUENCE:DEFINE-SEQUENCE-CLASS`.

**Status:** Out-of-scope for v1. **Rationale:** The SBCL extensible sequence
protocol requires deep integration with every standard sequence function
(~40 functions). Ecosystem usage is limited (primarily `trivial-extensible-sequences`,
few Quicklisp libraries depend on it). The implementation cost is disproportionate
to adoption benefit for v1. Will be revisited for v2 based on user demand.

**R9.40** When user code calls `sb-sequence:define-sequence-class` in TorCL v1,
the macro MUST signal a `TORCL-EXT:NOT-YET-IMPLEMENTED` error with a descriptive
message referencing v2.

---

## 9.7  MOP Extensions (`SB-MOP` / Closer-MOP Compatibility)

**R9.14** TorCL MUST export enough MOP symbols for `CLOSER-MOP` to load.
All symbols exported from `TORCL-MOP`, re-exported via `SB-MOP`.

### 9.7.1  Class Introspection

| Symbol | Signature | Returns | Thread-safety |
|--------|-----------|---------|---------------|
| `class-name` | `(class)` | symbol | safe |
| `(setf class-name)` | `(new class)` | new-name | acquires class lock |
| `class-direct-superclasses` | `(class)` | list | safe |
| `class-direct-subclasses` | `(class)` | list | safe |
| `class-precedence-list` | `(class)` | list | safe after finalization |
| `class-direct-slots` | `(class)` | list of direct-slot-def | safe |
| `class-slots` | `(class)` | list of effective-slot-def | safe after finalization |
| `class-default-initargs` | `(class)` | list of (name form fn) | safe |
| `class-direct-default-initargs` | `(class)` | list of (name form fn) | safe |
| `class-finalized-p` | `(class)` | boolean | safe |
| `class-prototype` | `(class)` | instance | lazy-alloc; safe |

### 9.7.1b  MOP Metaclasses

**R9.39** TorCL MUST export the following metaclass and slot-definition classes
from `TORCL-MOP` (re-exported via `SB-MOP`):

| Class | Superclass | Purpose |
|-------|------------|---------|
| `funcallable-standard-class` | `standard-class` | Metaclass for funcallable instances (GFs) |
| `forward-referenced-class` | `class` | Placeholder for not-yet-defined superclasses |
| `standard-direct-slot-definition` | `direct-slot-definition` | Default direct slot-def class |
| `standard-effective-slot-definition` | `effective-slot-definition` | Default effective slot-def class |

`funcallable-standard-class` instances support `set-funcallable-instance-function` (§9.7.5).
`forward-referenced-class` is replaced by the real class on `defclass`; `finalize-inheritance`
signals an error if any superclass remains forward-referenced.

### 9.7.2  Slot Definition Accessors

| Symbol | Signature | Returns |
|--------|-----------|---------|
| `slot-definition-name` | `(slotd)` | symbol |
| `slot-definition-initform` | `(slotd)` | form |
| `slot-definition-initfunction` | `(slotd)` | function or NIL |
| `slot-definition-type` | `(slotd)` | type specifier |
| `slot-definition-allocation` | `(slotd)` | `:instance` or `:class` |
| `slot-definition-initargs` | `(slotd)` | list of symbols |
| `slot-definition-readers` | `(direct-slotd)` | list of function names |
| `slot-definition-writers` | `(direct-slotd)` | list of function names |
| `slot-definition-location` | `(effective-slotd)` | fixnum or cons |

### 9.7.3  Generic Function & Method Protocol

| Symbol | Signature | Returns | Thread-safety |
|--------|-----------|---------|---------------|
| `generic-function-name` | `(gf)` | name | safe |
| `generic-function-methods` | `(gf)` | list | acquires gf lock |
| `generic-function-lambda-list` | `(gf)` | lambda-list | safe |
| `generic-function-method-combination` | `(gf)` | method-combination | safe |
| `generic-function-method-class` | `(gf)` | class | safe |
| `generic-function-argument-precedence-order` | `(gf)` | list | safe |
| `method-function` | `(method)` | function | safe |
| `method-generic-function` | `(method)` | gf or NIL | safe |
| `method-lambda-list` | `(method)` | lambda-list | safe |
| `method-specializers` | `(method)` | list | safe |
| `method-qualifiers` | `(method)` | list | safe |
| `accessor-method-slot-definition` | `(method)` | slot-def | safe |

### 9.7.4  Instance & Dispatch Protocol

| Symbol | Signature | Returns |
|--------|-----------|---------|
| `compute-applicable-methods-using-classes` | `(gf classes)` | (values methods ok-p) |
| `compute-applicable-methods` | `(gf arguments)` | list of methods |
| `compute-effective-method` | `(gf mc methods)` | effective-method form |
| `make-method-lambda` | `(gf method lambda env)` | (values lambda initargs) |
| `compute-discriminating-function` | `(gf)` | function |
| `slot-value-using-class` | `(class inst slotd)` | value |
| `(setf slot-value-using-class)` | `(new class inst slotd)` | new |
| `slot-boundp-using-class` | `(class inst slotd)` | boolean |
| `slot-makunbound-using-class` | `(class inst slotd)` | instance |
| `intern-eql-specializer` | `(object)` | eql-specializer |
| `eql-specializer-object` | `(spec)` | object |
| `specializer-direct-methods` | `(specializer)` | list |
| `specializer-direct-generic-functions` | `(specializer)` | list |
| `add-direct-method` | `(specializer method)` | unspecified |
| `remove-direct-method` | `(specializer method)` | unspecified |

### 9.7.5  Class Finalization & Mutation

**Finalization protocol:** `finalize-inheritance` invokes the following generic
functions in order: `compute-class-precedence-list`, `compute-slots`,
`compute-effective-slot-definition` (per slot), `compute-default-initargs`.
Each is a documented customization point.

| Symbol | Signature | Notes |
|--------|-----------|-------|
| `compute-class-precedence-list` | `(class)` | → list of classes; called by `finalize-inheritance` |
| `compute-slots` | `(class)` | → list of effective-slot-defs; collects direct slots from CPL |
| `compute-effective-slot-definition` | `(class name direct-slots)` | → single effective-slot-def |
| `compute-default-initargs` | `(class)` | → list of (name form function) triples |
| `finalize-inheritance` | `(class)` | compute CPL + effective slots |
| `ensure-class` | `(name &rest initargs)` | find-or-create class |
| `ensure-class-using-class` | `(class name &rest initargs)` | with existing class |
| `validate-superclass` | `(class super)` | → boolean |
| `allocate-instance` | `(class &rest initargs)` | raw instance |
| `ensure-generic-function` | `(name &rest args)` | find-or-create gf |
| `ensure-generic-function-using-class` | `(gf name &rest args)` | with existing gf |
| `add-method` | `(gf method)` | recomputes discriminator |
| `remove-method` | `(gf method)` | recomputes discriminator |
| `set-funcallable-instance-function` | `(fin function)` | set FIN's function |

### 9.7.6  Compatibility: TorCL vs SBCL MOP

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `compute-applicable-methods-using-classes` | 2 values | 2 values | identical |
| `make-method-lambda` | `(gf method lambda env)` | identical | identical |
| `validate-superclass` default | T for standard-class | T for standard-class | identical |
| `set-funcallable-instance-function` | supported | supported | identical |
| `add-method` locking | acquires GF lock | GF lock + IC flush (§4.8) | TorCL also flushes inline caches |
| `compute-class-precedence-list` | supported | supported | identical |
| `compute-slots` | supported | supported | identical |
| `compute-effective-slot-definition` | supported | supported | identical |
| `compute-default-initargs` | supported | supported | identical |
| `funcallable-standard-class` | supported | supported | identical |
| `forward-referenced-class` | supported | supported | identical |
| `standard-direct-slot-definition` | supported | supported | identical |
| `standard-effective-slot-definition` | supported | supported | identical |
| Closer-MOP test suite | passes | passes (R9.03) | identical |

---

## 9.8  Miscellaneous

**R9.15** `SAVE-LISP-AND-DIE` — dump a heap image and exit (§7).
TorCL name: `SAVE-IMAGE`; compatibility alias provided.
**R9.16** `QUIT` / `EXIT` — process exit with status code.

```lisp
(exit &key code abort timeout) ;; code: integer (default 0); abort: skip cleanup; timeout: thread join wait
(quit &key unix-status recklessly-p) ;; SBCL alias: unix-status→code, recklessly-p→abort
```

**R9.17** `WITH-TIMEOUT` — wall-clock time limit; signals `TIMEOUT` on expiry.

```lisp
(with-timeout (seconds &body timeout-forms) &body body)
  ;; Implementation: schedules a timer (§9.10) that interrupts the thread.
```

**R9.18** `NATIVE-NAMESTRING` — OS-native path string for a pathname (§5.7).
**R9.19** Source-location tracking: `DEFINITION-SOURCE` → file, line, form-number (§6).

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `save-lisp-and-die` | primary name | alias for `save-image` | alias provided |
| `exit :timeout` | not supported | supported | TorCL addition |
| `with-timeout` condition | `sb-ext:timeout` | `torcl-ext:timeout` | `sb-ext` alias provided |
| `definition-source` | struct | plist `(:file :line :form)` | accessor compat macros provided |

---

## 9.9  Extension Versioning

**R9.20** `TORCL-EXT` exports `:TORCL-EXT-VERSION` → `(major minor)` list.
**R9.21** Incompatible changes MUST bump major version and appear in release notes.

---

## 9.10  Timer & Scheduling Extensions

**R9.22** TorCL MUST provide a timer facility in `TORCL-EXT`, compatible with SBCL's `sb-ext` timer API.

### 9.10.1  API

```lisp
(make-timer function &key name thread)
  ;; thread — if supplied, FUNCTION runs in that thread via interrupt-thread;
  ;;          if NIL, runs in the dedicated timer thread.  Returns → timer (D9.07).

(schedule-timer timer time &key repeat-interval absolute-p catch-up)
  ;; time — seconds from now (real number), or universal-time if absolute-p is T.
  ;; repeat-interval — recurring period in seconds, or NIL.
  ;; catch-up — if T, replay missed iterations synchronously on wake.
  ;; Thread-safety: acquires timer-queue lock.
  ;;
  ;; Clock conversion (absolute-p = T):
  ;;   The universal-time value is converted to monotonic nanoseconds at schedule
  ;;   time via: fire-time = monotonic-now + (time - get-universal-time) * 1e9.
  ;;   This means the timer is immune to NTP clock adjustments after scheduling.
  ;;   Wall-clock skew (NTP jumps) that occurs between schedule and fire does NOT
  ;;   affect the timer — it fires at the monotonic equivalent of the wall-clock
  ;;   instant that was current when scheduled. This matches SBCL's real-time
  ;;   semantics in practice (SBCL uses CLOCK_REALTIME but is also subject to
  ;;   drift). Note: D9.07 stores fire-time as monotonic nanoseconds.

(unschedule-timer timer)  ;; Remove from schedule. Blocks if currently executing. Idempotent.
(timer-scheduled-p timer &key delta)  ;; delta: true only if firing within delta seconds.
(list-all-timers)  ;; → list of all scheduled timers. Diagnostic use.
```

### 9.10.2  Data Structure & Algorithm

**D9.07 — Timer:** `header(8) | name(8) | function(8) | thread(8) | fire-time:u64(8) | repeat-interval:u64(8) | catch-up:u8(1) | scheduled:AtomicU8(1) | pad(6) | next:*Timer(8)` — 64 bytes. Times in monotonic nanoseconds.

**A9.01 — Hierarchical Timing Wheel:** 4 levels (slot widths 1ms/64ms/~4s/~4min), min-heap overflow for >4min. O(1) insertion. Dedicated timer thread sleeps until next tick (1ms resolution) or nearest-deadline wakeup. Expired timers: called directly (timer-thread) or posted via `interrupt-thread`. Repeating timers re-inserted; `catch-up` replays missed intervals synchronously.

### 9.10.3  Compatibility

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `make-timer :thread` | supported | supported | identical |
| `schedule-timer :repeat-interval` | supported | supported | identical |
| `schedule-timer :catch-up` | not supported | supported | TorCL addition |
| Timer resolution | ~10ms (signal) | ~1ms (timing wheel) | TorCL more precise |
| `unschedule-timer` blocks | no | yes (safer cleanup) | TorCL stricter |
| `list-all-timers` | not provided | provided | TorCL addition |

---

## 9.11  Introspection Extensions

**R9.23** TorCL MUST provide introspection for interactive development and IDE integration.

### 9.11.1  Function Introspection

**R9.24** `function-lambda-expression` MUST return the source lambda expression
for any function compiled with `debug` ≥ 1.

```lisp
(function-lambda-expression fn)  ;; → (values lambda-expr closure-p name)
(function-type fn)               ;; → declared/inferred ftype or NIL
(function-arglist fn)            ;; → lambda-list or :NOT-AVAILABLE (SLIME/SLY arg hints)
```

### 9.11.2  `describe-object` Protocol

**R9.25** `describe-object` generic function per ANSI, with SBCL-compatible output.
Default methods for: `standard-object`, `structure-object`, `function`, `symbol`,
`cons`, `number`, `string`, `array`, `hash-table`, `package`, `pathname`, `stream`.

```lisp
(describe-object object stream)  ;; Generic function. Output serialized by stream lock.
(describe object &optional (stream *standard-output*))  ;; Wrapper with header/footer.
```

### 9.11.3  Cross-Reference Database

```lisp
(who-calls function-name &key package)     ;; → list of (caller . source-location)
(who-binds variable-name)                  ;; → callers that LET-bind the special
(who-references variable-name)             ;; → callers that read the special
(who-sets variable-name)                   ;; → callers that write the special
(who-macroexpands macro-name)              ;; → callers that expanded the macro
```

Cross-reference data retained when `debug` ≥ 1. Requires compiler xref pass (§4).

### 9.11.4  Compatibility

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `function-lambda-expression` | source at debug≥1 | source at debug≥1 | identical |
| `function-arglist` | `sb-introspect` package | `torcl-ext` package | package name |
| `who-calls` et al. | `sb-introspect` package | `torcl-ext` package | package name |
| `describe-object` format | SBCL-specific text | SBCL-compatible layout | minor formatting |

---

## 9.12  Compiler-Hint Extensions

**R9.26** TorCL MUST support compiler-hint declarations for guided optimisation.

### 9.12.1  `truly-the`

```lisp
(truly-the type form)
  ;; Like THE but trusted unconditionally — no runtime check even at (safety 3).
  ;; Wrong assertion → undefined behaviour (but GC-safe: tagged values remain valid).
```

**R9.27** `truly-the` MUST suppress all type checks. Compiler MAY use the
asserted type for downstream propagation and specialised code selection.

### 9.12.2  `always-bound`

```lisp
(declaim (torcl-ext:always-bound *variable*))
  ;; Elides UNBOUND-VARIABLE check on every special-variable read.
```

**R9.28** Compiler MUST elide unbound checks. If unbound at runtime: undefined
behaviour (UNBOUND-MARKER treated as valid object; no GC corruption).

**R9.41** The compiler's declaration-identifier resolver MUST recognise both
`torcl-ext:always-bound` and `sb-ext:always-bound` as equivalent declaration
identifiers. Declarations are resolved by symbol identity (via the compatibility
package re-export in R9.02), so `(declaim (sb-ext:always-bound *var*))` MUST
work identically. The same applies to `freeze-type` and `muffle-conditions` /
`unmuffle-conditions` — all four declaration identifiers MUST be recognised
under both `TORCL-EXT` and `SB-EXT` prefixes.

### 9.12.3  `freeze-type`

```lisp
(declaim (torcl-ext:freeze-type type-name))
  ;; Compiler MAY inline typep, structure accessors, stamp checks.
```

**R9.29** `freeze-type` MUST enable: (1) inline `typep` as stamp comparison,
(2) inline structure accessors without GF dispatch, (3) constant-fold `subtypep`.

**R9.30** Redefining a frozen type: `STYLE-WARNING` at compile, continuable
`ERROR` at load.

### 9.12.4  Condition Muffling

```lisp
(declaim (torcl-ext:muffle-conditions condition-type))   ;; Suppress matching warnings/notes.
(declaim (torcl-ext:unmuffle-conditions condition-type))  ;; Re-enable.
```

`inhibit-warnings` optimize quality (0–3): 0=all notes, 3=silence all.

### 9.12.5  Compatibility

| Feature | SBCL | TorCL | Divergence |
|---------|------|-------|------------|
| `truly-the` | special form | special form | identical |
| `always-bound` | declaration (`sb-ext:`) | declaration (`torcl-ext:` + `sb-ext:` alias) | identical; both prefixes recognised |
| `freeze-type` | declaration (`sb-ext:`) | declaration (`torcl-ext:` + `sb-ext:` alias) | identical; both prefixes recognised |
| `freeze-type` redef | `style-warning` | `style-warning` + continuable `error` at load | TorCL stricter |
| `muffle-conditions` | declaration (`sb-ext:`) | declaration (`torcl-ext:` + `sb-ext:` alias) | identical; both prefixes recognised |
| Trust violation | undefined | undefined (but GC-safe) | TorCL guarantees no GC corruption |

---

## 9.13  Concurrency & Error Contracts

**R9.31** Extension functions accepting thread args MUST signal `TYPE-ERROR` for non-threads.
**R9.32** Operations on dead/finalized objects MUST signal a descriptive condition.
**R9.33** Lock ordering: `timer-queue-lock < thread-mailbox-lock < gc-safepoint-lock`.
Debug builds MUST assert correct ordering (§8).

---

## 9.14  Test Strategy

**R9.34** Each section (§9.2–§9.12) MUST have a corresponding test module:

| Section | Test file | Key coverage |
|---------|-----------|-------------|
| §9.2 Threading | `test_threading_ext` | make/join/interrupt, mutex contention, condvar, CAS, atomic-incf/decf, recursive locks |
| §9.4 Weak refs | `test_weak_ext` | GC clearing, finalizer ordering |
| §9.7 MOP | `test_mop_compat.lisp` | Closer-MOP suite |
| §9.10 Timers | `test_timer_ext` | one-shot, repeat, cancel, catch-up |
| §9.11 Introspection | `test_introspect.lisp` | `who-calls`, `function-arglist` |
| §9.12 Compiler hints | `test_compiler_hints.lisp` | `truly-the` elision, `freeze-type` |

**R9.35** Bordeaux-Threads test suite MUST pass as integration gate for §9.2.
**R9.36** Closer-MOP test suite MUST pass as integration gate for §9.7.
