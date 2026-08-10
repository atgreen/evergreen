# §9  SBCL-Compatible Extensions

**Scope:** Bliss adopts a curated set of SBCL-compatible extensions where
ANSI X3.226-1994 is silent and the extension is widely depended upon by the
portable Common Lisp ecosystem. Each extension MUST have a rationale,
MUST NOT conflict with ANSI semantics, and MUST be documented here.

## 9.1  Adoption Criteria

An SBCL extension is adopted when ALL of the following hold:
1. **Ecosystem demand** — used by ≥3 popular libraries (Quicklisp top-100)
   or essential for practical CL development.
2. **No ANSI conflict** — operates in an unspecified or implementation-defined area.
3. **Clean namespace** — exported from `BLISS-EXT` (not `CL`), with
   `SB-EXT`-compatible symbol names via a compatibility package.
4. **Documented divergence** — any behavioural difference from SBCL listed explicitly.

**R9.01** Bliss MUST provide a `BLISS-EXT` package exporting all adopted extensions.
**R9.02** Bliss MUST provide `SB-EXT`, `SB-THREAD`, `SB-MOP` compatibility
packages that re-export the corresponding Bliss symbols.
**R9.03** Each adopted extension MUST include a test verifying SBCL-compatible behaviour.

---

## 9.2  Threading Extensions (`SB-THREAD` Compatibility)

**R9.04** Bliss MUST provide the following threading API in `BLISS-THREADS`,
re-exported via `SB-THREAD`.

### 9.2.1  Thread Lifecycle

```lisp
(make-thread function &key name arguments ephemeral)
  ;; function   — designator for a function (APPLY'd with ARGUMENTS)
  ;; name       — string or NIL; ephemeral — if T, runtime MAY auto-reap
  ;; Returns → thread (D9.01).  Thread-safety: safe; allocates under GC-safe state.

(join-thread thread &key default timeout)
  ;; Blocks until THREAD terminates.  timeout — real seconds or NIL.
  ;; Returns → (values result status)  status ∈ {:OK :TIMEOUT :ABORTED}
  ;; Multiple threads MAY join the same thread.

(thread-alive-p thread)       ;; → boolean; lock-free atomic read.
(destroy-thread thread)       ;; Sends TERMINATE-THREAD at next safepoint. Idempotent.
(interrupt-thread thread fn)  ;; Calls FN in THREAD at next safepoint (§2.5).
(abort-thread &optional cond) ;; Cleanly unwinds the calling thread.
```

**Special variables:** `*current-thread*` (bound per-thread, read-only),
`(all-threads)` → list of live thread objects.

### 9.2.2  Mutexes

```lisp
(make-mutex &key name)                ;; → mutex (D9.02). Non-recursive (SBCL compat).
(grab-mutex mutex &key waitp timeout) ;; waitp=NIL → try-lock. Returns T/NIL.
(release-mutex mutex &key if-not-owner) ;; if-not-owner ∈ {:WARN :FORCE :ERROR}
(with-mutex (mutex &key waitp timeout) &body body) ;; UNWIND-PROTECT wrapper.
```

### 9.2.3  Condition Variables

```lisp
(make-waitqueue &key name)                              ;; → waitqueue (D9.03)
(condition-wait waitqueue mutex &key timeout)            ;; Atomically release+block. Returns T/NIL.
(condition-notify waitqueue &optional (count 1))         ;; Wake COUNT waiters.
(condition-broadcast waitqueue)                          ;; Wake all waiters.
```

### 9.2.4  Semaphores

```lisp
(make-semaphore &key name count)              ;; count — initial permits (default 0) → D9.04
(signal-semaphore semaphore &optional (n 1))  ;; Increment permits, wake waiters.
(wait-on-semaphore sem &key timeout notification) ;; Block if zero. notification → D9.05.
(semaphore-count semaphore)                   ;; → non-negative integer; lock-free.
(try-semaphore semaphore &optional (n 1))     ;; Non-blocking. Returns T/NIL.
```

### 9.2.5  Memory Barriers

```lisp
(barrier kind) ;; Macro. KIND ∈ {:READ :WRITE :FULL :DATA-DEPENDENCY}. Default :FULL.
```

### 9.2.6  Data Structures

**D9.01 — Thread:** `header(8) | name(8) | state:AtomicU8(1) | ephemeral(1) | pad(6) | os-handle(8) | result(8) | mailbox-head:AtomicPtr(8) | join-waiters:AtomicPtr(8)` — 56 bytes. State ∈ {:BORN :RUNNING :DEAD :ABORTED}.

**D9.02 — Mutex:** `header(8) | name(8) | owner:AtomicU64(8) | state:AtomicU32(4) | futex(4)` — 32 bytes. State: 0=free, 1=locked, 2=contended.

**D9.03 — Waitqueue:** `header(8) | name(8) | queue-head:AtomicPtr(8)` — 24 bytes.

**D9.04 — Semaphore:** `header(8) | name(8) | count:AtomicI64(8) | wait-queue:AtomicPtr(8)` — 32 bytes.

**D9.05 — Semaphore-Notification:** `header(8) | status:AtomicU8(1)` — 16 bytes (padded).

### 9.2.7  Compatibility: Bliss vs SBCL

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `make-thread` | `:name` only | adds `:arguments`, `:ephemeral` | superset |
| Mutex recursion | non-recursive (error) | non-recursive (error) | identical |
| `interrupt-thread` | safe-point delivery | safepoint delivery (§2.5) | identical |
| `destroy-thread` | `terminate-thread` | `destroy-thread` + alias | name differs |
| `condition-wait` spurious | possible | possible | identical |
| `barrier` | internal | exported | Bliss addition |

---

## 9.3  Global Variables

**R9.06** `DEFGLOBAL` — globally special, same value across all threads. Atomic cell.
**R9.07** `DEFINE-LOAD-TIME-GLOBAL` — evaluated once at load time, treated as constant.

```lisp
(defglobal name value &optional doc)            ;; Atomic global cell. Compiler MAY inline.
(define-load-time-global name value &optional doc) ;; Evaluated once; not re-evaluated on reload.
```

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `defglobal` | `sb-ext:defglobal` | `bliss-ext:defglobal` | identical semantics |
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

**D9.06 — Weak-Pointer:** `header(8) | referent:BlissVal(8) | broken-p:AtomicU8(1)` — 24 bytes (padded). GC clears referent to NIL when target unreachable.

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `weak-pointer-value` 2nd val | `t`/`nil` | `t`/`nil` | identical |
| Finalizer thread | dedicated | dedicated | identical |

---

## 9.5  Hash Table Extensions

**R9.10** `:SYNCHRONIZED` → thread-safe hash table with reader-writer lock.
**R9.11** `:WEAKNESS` → `:KEY`, `:VALUE`, `:KEY-AND-VALUE`, `:KEY-OR-VALUE`.
**R9.12** `WITH-LOCKED-HASH-TABLE` → multi-operation atomicity.

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `:synchronized` | RW lock | RW lock | identical |
| Concurrent resize | stop-the-world | incremental rehash | Bliss avoids long pauses |

---

## 9.6  Sequence Extensions

**R9.13** `SB-SEQUENCE:DEFINE-SEQUENCE-CLASS` — user-defined sequences. MAY be deferred to v2.

---

## 9.7  MOP Extensions (`SB-MOP` / Closer-MOP Compatibility)

**R9.14** Bliss MUST export enough MOP symbols for `CLOSER-MOP` to load.
All symbols exported from `BLISS-MOP`, re-exported via `SB-MOP`.

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

| Symbol | Signature | Notes |
|--------|-----------|-------|
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

### 9.7.6  Compatibility: Bliss vs SBCL MOP

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `compute-applicable-methods-using-classes` | 2 values | 2 values | identical |
| `make-method-lambda` | `(gf method lambda env)` | identical | identical |
| `validate-superclass` default | T for standard-class | T for standard-class | identical |
| `set-funcallable-instance-function` | supported | supported | identical |
| `add-method` locking | acquires GF lock | GF lock + IC flush (§4.8) | Bliss also flushes inline caches |
| Closer-MOP test suite | passes | passes (R9.03) | identical |

---

## 9.8  Miscellaneous

**R9.15** `SAVE-LISP-AND-DIE` — dump a heap image and exit (§7).
Bliss name: `SAVE-IMAGE`; compatibility alias provided.
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

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `save-lisp-and-die` | primary name | alias for `save-image` | alias provided |
| `exit :timeout` | not supported | supported | Bliss addition |
| `with-timeout` condition | `sb-ext:timeout` | `bliss-ext:timeout` | `sb-ext` alias provided |
| `definition-source` | struct | plist `(:file :line :form)` | accessor compat macros provided |

---

## 9.9  Extension Versioning

**R9.20** `BLISS-EXT` exports `:BLISS-EXT-VERSION` → `(major minor)` list.
**R9.21** Incompatible changes MUST bump major version and appear in release notes.

---

## 9.10  Timer & Scheduling Extensions

**R9.22** Bliss MUST provide a timer facility in `BLISS-EXT`, compatible with SBCL's `sb-ext` timer API.

### 9.10.1  API

```lisp
(make-timer function &key name thread)
  ;; thread — if supplied, FUNCTION runs in that thread via interrupt-thread;
  ;;          if NIL, runs in the dedicated timer thread.  Returns → timer (D9.07).

(schedule-timer timer time &key repeat-interval absolute-p catch-up)
  ;; time — seconds from now (or universal-time if absolute-p)
  ;; repeat-interval — recurring period or NIL.  catch-up — replay missed iterations.
  ;; Thread-safety: acquires timer-queue lock.

(unschedule-timer timer)  ;; Remove from schedule. Blocks if currently executing. Idempotent.
(timer-scheduled-p timer &key delta)  ;; delta: true only if firing within delta seconds.
(list-all-timers)  ;; → list of all scheduled timers. Diagnostic use.
```

### 9.10.2  Data Structure & Algorithm

**D9.07 — Timer:** `header(8) | name(8) | function(8) | thread(8) | fire-time:u64(8) | repeat-interval:u64(8) | catch-up:u8(1) | scheduled:AtomicU8(1) | pad(6) | next:*Timer(8)` — 64 bytes. Times in monotonic nanoseconds.

**A9.01 — Hierarchical Timing Wheel:** 4 levels (slot widths 1ms/64ms/~4s/~4min), min-heap overflow for >4min. O(1) insertion. Dedicated timer thread sleeps until next tick (1ms resolution) or nearest-deadline wakeup. Expired timers: called directly (timer-thread) or posted via `interrupt-thread`. Repeating timers re-inserted; `catch-up` replays missed intervals synchronously.

### 9.10.3  Compatibility

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `make-timer :thread` | supported | supported | identical |
| `schedule-timer :repeat-interval` | supported | supported | identical |
| `schedule-timer :catch-up` | not supported | supported | Bliss addition |
| Timer resolution | ~10ms (signal) | ~1ms (timing wheel) | Bliss more precise |
| `unschedule-timer` blocks | no | yes (safer cleanup) | Bliss stricter |
| `list-all-timers` | not provided | provided | Bliss addition |

---

## 9.11  Introspection Extensions

**R9.23** Bliss MUST provide introspection for interactive development and IDE integration.

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

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `function-lambda-expression` | source at debug≥1 | source at debug≥1 | identical |
| `function-arglist` | `sb-introspect` package | `bliss-ext` package | package name |
| `who-calls` et al. | `sb-introspect` package | `bliss-ext` package | package name |
| `describe-object` format | SBCL-specific text | SBCL-compatible layout | minor formatting |

---

## 9.12  Compiler-Hint Extensions

**R9.26** Bliss MUST support compiler-hint declarations for guided optimisation.

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
(declaim (bliss-ext:always-bound *variable*))
  ;; Elides UNBOUND-VARIABLE check on every special-variable read.
```

**R9.28** Compiler MUST elide unbound checks. If unbound at runtime: undefined
behaviour (UNBOUND-MARKER treated as valid object; no GC corruption).

### 9.12.3  `freeze-type`

```lisp
(declaim (bliss-ext:freeze-type type-name))
  ;; Compiler MAY inline typep, structure accessors, stamp checks.
```

**R9.29** `freeze-type` MUST enable: (1) inline `typep` as stamp comparison,
(2) inline structure accessors without GF dispatch, (3) constant-fold `subtypep`.

**R9.30** Redefining a frozen type: `STYLE-WARNING` at compile, continuable
`ERROR` at load.

### 9.12.4  Condition Muffling

```lisp
(declaim (bliss-ext:muffle-conditions condition-type))   ;; Suppress matching warnings/notes.
(declaim (bliss-ext:unmuffle-conditions condition-type))  ;; Re-enable.
```

`inhibit-warnings` optimize quality (0–3): 0=all notes, 3=silence all.

### 9.12.5  Compatibility

| Feature | SBCL | Bliss | Divergence |
|---------|------|-------|------------|
| `truly-the` | special form | special form | identical |
| `always-bound` | declaration | declaration | identical |
| `freeze-type` | declaration | declaration | identical |
| `freeze-type` redef | `style-warning` | `style-warning` + continuable `error` at load | Bliss stricter |
| `muffle-conditions` | declaration | declaration | identical |
| Trust violation | undefined | undefined (but GC-safe) | Bliss guarantees no GC corruption |

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
| §9.2 Threading | `test_threading_ext` | make/join/interrupt, mutex contention, condvar |
| §9.4 Weak refs | `test_weak_ext` | GC clearing, finalizer ordering |
| §9.7 MOP | `test_mop_compat.lisp` | Closer-MOP suite |
| §9.10 Timers | `test_timer_ext` | one-shot, repeat, cancel, catch-up |
| §9.11 Introspection | `test_introspect.lisp` | `who-calls`, `function-arglist` |
| §9.12 Compiler hints | `test_compiler_hints.lisp` | `truly-the` elision, `freeze-type` |

**R9.35** Bordeaux-Threads test suite MUST pass as integration gate for §9.2.
**R9.36** Closer-MOP test suite MUST pass as integration gate for §9.7.
