# §5.4 Condition System

**Scope:** Full ANSI Common Lisp condition system — condition types, handler
binding and dispatch, restart machinery, signalling protocol (`SIGNAL`,
`ERROR`, `WARN`, `CERROR`), debugger entry, and interactions with
`UNWIND-PROTECT`, CLOS, and native threads.

---

## 5.4.1 Requirements

| ID | Requirement |
|--------|-------------|
| R5.203 | When `*BREAK-ON-SIGNALS*` is non-NIL, the signalling functions (`SIGNAL`, `ERROR`, `WARN`, `CERROR`) MUST test `(TYPEP condition *BREAK-ON-SIGNALS*)` and, if true, call `BREAK` before performing handler search. |
| R5.91 | All ANSI condition types listed in §5.4.2 MUST be defined as CLOS classes inheriting from `CONDITION`. |
| R5.92 | `HANDLER-BIND` MUST establish handler clusters without consing; cluster setup MUST be O(1). |
| R5.93 | Handler search MUST be O(n) in the number of established handler clusters, scanning newest-first. |
| R5.94 | Handlers bound via `HANDLER-BIND` MUST execute without unwinding the stack (non-unwound semantics). |
| R5.95 | `HANDLER-CASE` MUST expand into `HANDLER-BIND` + `BLOCK`/`RETURN-FROM` so that the handler body executes after unwinding. |
| R5.96 | `RESTART-BIND` and `RESTART-CASE` MUST establish restart clusters on a thread-local restart stack. |
| R5.97 | `COMPUTE-RESTARTS` MUST return all applicable restarts, newest-first, filtering by any `:test-function`. |
| R5.98 | `FIND-RESTART` MUST locate the most recently established applicable restart of the given name. |
| R5.99 | `INVOKE-RESTART` MUST call the restart's function in the dynamic environment of the `RESTART-BIND`/`RESTART-CASE`. |
| R5.100 | `INVOKE-RESTART-INTERACTIVELY` MUST call the restart's `:interactive` function to obtain arguments, then invoke. |
| R5.101 | `*DEBUGGER-HOOK*` MUST be called before entering the default debugger; it receives the condition and the hook's own value. |
| R5.102 | `SIGNAL` MUST search handlers, invoke matching handlers (non-unwound), and return `NIL` if no handler transfers control. |
| R5.103 | `ERROR` MUST signal and, if no handler transfers control, enter the debugger. It MUST NOT return. |
| R5.104 | `WARN` MUST signal a `WARNING`; if the `MUFFLE-WARNING` restart is invoked the warning is suppressed. If no handler handles, a message MUST be printed to `*ERROR-OUTPUT*`. |
| R5.105 | `CERROR` MUST signal an error condition (typically `SIMPLE-ERROR`) with a `CONTINUE` restart established. If the restart is invoked, `CERROR` returns `NIL`. |
| R5.106 | `UNWIND-PROTECT` cleanup forms MUST execute even when a handler or restart transfers control via non-local exit. |
| R5.107 | Condition objects MUST be full CLOS instances; `DEFINE-CONDITION` MUST expand to `DEFCLASS` with metaclass `CONDITION-CLASS`. |
| R5.108 | Handler stacks and restart stacks MUST be thread-local. Each thread begins with empty stacks. |
| R5.109 | Handler establishment (`HANDLER-BIND`, `HANDLER-CASE`) MUST NOT heap-allocate in the common case; stack-allocated clusters are REQUIRED. |
| R5.110 | The runtime MUST pre-allocate `STORAGE-CONDITION` instances at startup for use when heap memory is exhausted. |

---

## 5.4.2 Condition Class Hierarchy

All conditions are CLOS classes. The metaclass `CONDITION-CLASS` is a subclass
of `STANDARD-CLASS` that enforces ANSI rules (no `:default-initargs` on
`MAKE-CONDITION`, slot access via accessor only, etc.).

### 5.4.2.1 Full ANSI Condition Type Tree

```text
condition
├── serious-condition
│   ├── error
│   │   ├── arithmetic-error
│   │   │   ├── division-by-zero
│   │   │   ├── floating-point-overflow
│   │   │   ├── floating-point-underflow
│   │   │   ├── floating-point-inexact
│   │   │   └── floating-point-invalid-operation
│   │   ├── cell-error
│   │   │   ├── unbound-variable
│   │   │   ├── undefined-function
│   │   │   └── unbound-slot
│   │   ├── control-error
│   │   ├── file-error
│   │   ├── package-error
│   │   ├── parse-error
│   │   ├── print-not-readable
│   │   ├── program-error
│   │   ├── stream-error
│   │   │   ├── end-of-file
│   │   │   └── reader-error       [also inherits parse-error]
│   │   └── type-error
│   │       └── simple-type-error  [also inherits simple-condition]
│   └── storage-condition
├── warning
│   ├── simple-warning             [also inherits simple-condition]
│   └── style-warning
└── simple-condition
    ├── simple-error               [also inherits error]
    ├── simple-warning             [also inherits warning]
    └── simple-type-error          [also inherits type-error]
```

### 5.4.2.2 Slot Definitions per Condition Type

| Condition Type | Slots | Accessors |
|----------------|-------|-----------|
| `condition` | (none — abstract root) | — |
| `simple-condition` | `format-control`, `format-arguments` | `simple-condition-format-control`, `simple-condition-format-arguments` |
| `arithmetic-error` | `operation`, `operands` | `arithmetic-error-operation`, `arithmetic-error-operands` |
| `cell-error` | `name` | `cell-error-name` |
| `file-error` | `pathname` | `file-error-pathname` |
| `package-error` | `package` | `package-error-package` |
| `print-not-readable` | `object` | `print-not-readable-object` |
| `stream-error` | `stream` | `stream-error-stream` |
| `type-error` | `datum`, `expected-type` | `type-error-datum`, `type-error-expected-type` |
| `unbound-slot` | `instance` | `unbound-slot-instance` (plus inherited `name`) |

Types without listed slots (`error`, `serious-condition`, `warning`,
`style-warning`, `control-error`, `program-error`, `parse-error`,
`storage-condition`, `division-by-zero`, `floating-point-*`, etc.) inherit
their ancestors' slots and add none of their own.

---

## 5.4.3 Data Structures

### D5.10 — HandlerCluster

A handler cluster groups the handlers from a single `HANDLER-BIND` form.
Clusters are allocated on the Bliss control stack (not the heap) as a
fixed-size frame.

```rust
/// Stack-allocated handler cluster. Pushed onto the thread-local handler stack.
#[repr(C)]
struct HandlerCluster {
    /// Pointer to the previous cluster on the stack (linked list).
    prev: *const HandlerCluster,
    /// Number of entries in this cluster (known at compile time).
    count: u16,
    /// Inline array of handler entries (variable-length, but stack-allocated
    /// via alloca-style mechanism or fixed-max with overflow to heap).
    entries: [HandlerEntry; /* count */],
}

#[repr(C)]
struct HandlerEntry {
    /// The condition type specifier (a class object pointer / BlissVal).
    type_spec: BlissVal,
    /// The handler function (a BlissVal pointing to a closure or compiled fn).
    handler_fn: BlissVal,
}
```

**Invariants:**
- `prev` forms a singly-linked stack, newest cluster first.
- The thread-local variable `*current-handler-cluster*` always points to the
  top of the stack (or null for an empty stack).
- Cluster allocation and deallocation are implicit in stack frame
  setup/teardown — no malloc/free.

### D5.11 — RestartCluster

Analogous to handler clusters, but for restarts.

```rust
#[repr(C)]
struct RestartCluster {
    prev: *const RestartCluster,
    count: u16,
    entries: [RestartEntry; /* count */],
}

#[repr(C)]
struct RestartEntry {
    /// Restart name (symbol) or NIL for anonymous restarts.
    name: BlissVal,
    /// The restart function.
    function: BlissVal,
    /// :interactive function (or NIL).
    interactive_fn: BlissVal,
    /// :report function (or NIL, or a format string BlissVal).
    report_fn: BlissVal,
    /// :test function (or NIL, meaning always applicable).
    test_fn: BlissVal,
    /// The condition this restart is associated with (or NIL for general).
    associated_condition: BlissVal,
}
```

### D5.12 — ThreadConditionState

Each thread (see §2) maintains its own condition-system state. This structure
is part of the `BlissThread` object (§2).

```rust
struct ThreadConditionState {
    /// Top of the handler cluster stack.
    handler_stack: *const HandlerCluster,
    /// Top of the restart cluster stack.
    restart_stack: *const RestartCluster,
    /// Current *debugger-hook* value for this thread.
    debugger_hook: BlissVal,
}
```

### D5.13 — Pre-allocated Storage Conditions

```rust
/// Allocated at startup (R5.110); never garbage-collected.
static STORAGE_CONDITION_POOL: [BlissVal; 4] = /* ... */;
```

Four `STORAGE-CONDITION` instances are pre-allocated in a pinned, non-GC
region at startup. When the GC or allocator detects OOM, one of these is
signalled instead of attempting allocation.

---

## 5.4.4 Algorithms & Control Flow

### A5.04 — Handler Search (`signal-handler-search`)

Invoked by `SIGNAL`, `ERROR`, `WARN`, and `CERROR`.

```text
function signal-handler-search(condition):
    // R5.203: *break-on-signals* check before handler search
    if *break-on-signals* ≠ NIL:
        bos ← *break-on-signals*
        *break-on-signals* ← NIL   // prevent recursive break
        if typep(condition, bos):
            break(condition)        // enter debugger via BREAK
        *break-on-signals* ← bos   // restore

    cluster ← current-thread.handler_stack
    // Save the handler stack pointer BEFORE searching so we can
    // rebind it during handler invocation (ANSI 9.1.4.1 semantics).
    while cluster ≠ null:
        for i in 0..cluster.count:
            entry ← cluster.entries[i]
            if typep(condition, entry.type_spec):
                // Temporarily pop to cluster.prev so re-signalling
                // from within a handler searches only OLDER handlers.
                saved ← current-thread.handler_stack
                current-thread.handler_stack ← cluster.prev
                result ← funcall(entry.handler_fn, condition)  // non-unwound call
                current-thread.handler_stack ← saved
                // If handler returns normally, continue search.
        cluster ← cluster.prev
    return NIL  // no handler transferred control
```

**Key properties:**
- O(n) in total handler count across all clusters (R5.93).
- The stack pointer rebind prevents infinite handler recursion on the
  same handler cluster.
- If a handler performs a non-local exit (via `RETURN-FROM`, `THROW`, or
  `GO`), the stack unwinds through `UNWIND-PROTECT` cleanup forms normally.

### A5.05 — `HANDLER-BIND` Establishment

```text
function handler-bind-establish(entries):
    cluster ← stack-allocate HandlerCluster
    cluster.prev ← current-thread.handler_stack
    cluster.count ← len(entries)
    cluster.entries ← entries    // inline copy, all on stack
    current-thread.handler_stack ← cluster
    // execute body forms
    // on exit (normal or non-local):
    current-thread.handler_stack ← cluster.prev
```

Establishment is O(1) — a single pointer swap plus a fixed-size copy of
handler entries onto the stack frame (R5.92).

### A5.06 — `HANDLER-CASE` Expansion

`HANDLER-CASE` is a macro that expands into `HANDLER-BIND` + `BLOCK`/`RETURN-FROM`:

```lisp
;; Source:
(handler-case (form)
  (error (e) (recovery e))
  (warning (w) (log-warning w)))

;; Expansion:
(block #:handler-case-block
  (let ((#:tag nil))
    (declare (dynamic-extent #:tag))
    (multiple-value-call
      (lambda (&rest #:vals)
        (case #:tag
          (0 (let ((e (car #:vals))) (recovery e)))
          (1 (let ((w (car #:vals))) (log-warning w)))
          (t (values-list #:vals))))
      (handler-bind
        ((error   (lambda (#:c)
                    (setq #:tag 0)
                    (return-from #:handler-case-block #:c)))
         (warning (lambda (#:c)
                    (setq #:tag 1)
                    (return-from #:handler-case-block #:c))))
        (form)))))
```

The `RETURN-FROM` causes stack unwinding (R5.95), so the handler clause body
runs after all intervening `UNWIND-PROTECT` cleanup forms.

**`:no-error` clause:** When present, is placed in the `(t ...)` branch
of the `CASE` and receives the normal return values.

### A5.07 — `RESTART-BIND` / `RESTART-CASE` Establishment

```text
function restart-bind-establish(entries):
    cluster ← stack-allocate RestartCluster
    cluster.prev ← current-thread.restart_stack
    cluster.count ← len(entries)
    cluster.entries ← entries
    current-thread.restart_stack ← cluster
    // execute body
    // on exit:
    current-thread.restart_stack ← cluster.prev
```

`RESTART-CASE` is analogous to `HANDLER-CASE`: it wraps the body in a
`BLOCK` and each restart function performs `RETURN-FROM` that block with
the restart's return values.

### A5.08 — `COMPUTE-RESTARTS` and `FIND-RESTART`

```text
function compute-restarts(condition-or-nil):
    result ← empty-list
    cluster ← current-thread.restart_stack
    while cluster ≠ null:
        for i in 0..cluster.count:
            entry ← cluster.entries[i]
            // Filter by association: if condition given and restart has
            // an associated-condition, skip unless they match.
            if condition-or-nil ≠ nil
               and entry.associated_condition ≠ nil
               and entry.associated_condition ≠ condition-or-nil:
                continue
            // Filter by :test-function
            if entry.test_fn ≠ nil
               and not funcall(entry.test_fn, condition-or-nil):
                continue
            push restart-object(entry) onto result
        cluster ← cluster.prev
    return nreverse(result)   // newest-first ordering

function find-restart(name, condition-or-nil):
    cluster ← current-thread.restart_stack
    while cluster ≠ null:
        for i in 0..cluster.count:
            entry ← cluster.entries[i]
            if entry.name = name:
                // same association/test filtering as compute-restarts
                if applicable?(entry, condition-or-nil):
                    return restart-object(entry)
        cluster ← cluster.prev
    return NIL
```

### A5.09 — `INVOKE-RESTART` and `INVOKE-RESTART-INTERACTIVELY`

```text
function invoke-restart(restart, &rest args):
    if restart is a symbol:
        restart ← find-restart(restart)
        if restart = NIL: signal CONTROL-ERROR
    funcall(restart.function, args...)

function invoke-restart-interactively(restart):
    if restart is a symbol:
        restart ← find-restart(restart)
        if restart = NIL: signal CONTROL-ERROR
    args ← if restart.interactive_fn ≠ NIL
               then funcall(restart.interactive_fn)
               else ()
    funcall(restart.function, args...)
```

### A5.10 — Signalling Protocols

#### `SIGNAL`

```text
function signal(datum, &rest arguments):
    condition ← coerce-to-condition(datum, arguments, 'simple-condition)
    signal-handler-search(condition)       // A5.04
    return NIL
```

#### `ERROR`

```text
function error(datum, &rest arguments):
    condition ← coerce-to-condition(datum, arguments, 'simple-error)
    signal-handler-search(condition)
    // No handler transferred control → enter debugger.
    invoke-debugger(condition)             // never returns
```

#### `WARN`

```text
function warn(datum, &rest arguments):
    condition ← coerce-to-condition(datum, arguments, 'simple-warning)
    restart-case (muffle-warning () (return-from warn NIL)):
        signal-handler-search(condition)
    // No handler handled → print warning.
    format(*error-output*, "~&WARNING: ~A~%" condition)
    return NIL
```

#### `CERROR`

```text
function cerror(continue-format, datum, &rest arguments):
    condition ← coerce-to-condition(datum, arguments, 'simple-error)
    restart-case
        (continue ()
           :report (lambda (s) (format s continue-format arguments))
           (return-from cerror NIL)):
        signal-handler-search(condition)
        invoke-debugger(condition)
```

### A5.11 — Debugger Entry Protocol

```text
function invoke-debugger(condition):
    hook ← *debugger-hook*
    if hook ≠ NIL:
        saved ← hook
        *debugger-hook* ← NIL      // prevent recursive entry
        funcall(saved, condition, saved)
    // If hook returns (should not, but guard):
    default-debugger(condition)     // enters interactive debugger (§6)
    // default-debugger never returns; if somehow it does:
    abort-to-toplevel()
```

---

## 5.4.5 Interaction with UNWIND-PROTECT

When a handler or restart performs a non-local exit (via `RETURN-FROM`,
`THROW`, or `GO`), the standard stack unwinding machinery executes all
`UNWIND-PROTECT` cleanup forms between the current frame and the target
frame (R5.106).

**Implementation:** `UNWIND-PROTECT` pushes a cleanup-frame on the control
stack. The non-local-exit instruction (`NLX`) walks the stack, invoking
each cleanup-frame's thunk. This is the same machinery used by `THROW`/`CATCH`
(see §2 runtime core). The condition system does not add any special NLX
handling — it relies entirely on the general NLX + UNWIND-PROTECT infrastructure.

**Signalling during cleanup:** If a condition is signalled during an
`UNWIND-PROTECT` cleanup form, the handler search starts from the handlers
established within that cleanup form (the dynamically active handlers at
that point). This is standard dynamic-extent semantics.

---

## 5.4.6 Interaction with CLOS

### 5.4.6.1 Conditions as CLOS Instances (R5.107)

- Every condition type is a CLOS class whose metaclass is `CONDITION-CLASS`.
- `CONDITION-CLASS` is a subclass of `STANDARD-CLASS` with the following
  restrictions enforced at class finalization time:
  - No `:default-initargs` for `MAKE-CONDITION`.
  - Slot access MUST go through defined accessors; `SLOT-VALUE` on condition
    slots is unspecified (per ANSI) but Bliss permits it as an extension.
  - Multiple inheritance is allowed (e.g., `SIMPLE-TYPE-ERROR` inherits
    both `SIMPLE-CONDITION` and `TYPE-ERROR`).
- `DEFINE-CONDITION` is a macro that expands to `DEFCLASS` with
  `(:metaclass condition-class)` and generates the accessor functions.

### 5.4.6.2 `MAKE-CONDITION`

```lisp
(defun make-condition (type &rest slot-initializations)
  (apply #'make-instance type slot-initializations))
```

The `coerce-to-condition` helper (used by `SIGNAL`, `ERROR`, etc.) converts:
- A condition object → used as-is.
- A symbol (condition type name) + initargs → `(apply #'make-condition symbol args)`.
- A format string + args → `(make-condition default-type :format-control string :format-arguments args)`.

---

## 5.4.7 Thread-Local Handler and Restart Stacks (R5.108)

Each Bliss thread (§2) maintains independent handler and restart stacks
via `ThreadConditionState` (D5.12). There is no sharing of handler or
restart registrations between threads.

**Thread creation:** A new thread starts with empty handler and restart stacks.
The creating thread's handlers/restarts are NOT inherited.

**Thread-local binding:** The handler/restart stack pointers are stored in
the `BlissThread` structure, accessed via a thread-local pointer (TLS on
the platform level). This avoids any locking on handler establishment or
search.

**`*DEBUGGER-HOOK*`:** Is a thread-local special variable. Each thread may
bind its own debugger hook independently.

---

## 5.4.8 Performance Requirements

| Operation | Requirement | Mechanism |
|-----------|-------------|-----------|
| Handler establishment (`HANDLER-BIND`) | O(1), zero heap allocation (R5.92, R5.109) | Stack-allocated cluster, single pointer swap |
| Handler search | O(n) in handler count (R5.93) | Linear walk of cluster linked list |
| Restart establishment | O(1), zero heap allocation | Stack-allocated cluster, single pointer swap |
| `COMPUTE-RESTARTS` | O(n) in restart count | Linear walk + filter; result list is heap-allocated |
| `FIND-RESTART` | O(n) in restart count (early exit) | Linear walk, returns on first match |
| `MAKE-CONDITION` | Normal CLOS allocation cost | `MAKE-INSTANCE` with `CONDITION-CLASS` |
| Condition signalling (no handler matches) | O(n) handler search + return | No allocation beyond the condition itself |

**Optimization notes:**
- The compiler SHOULD recognize `HANDLER-BIND` with constant type specifiers
  and emit inline cluster construction (no closure allocation for the
  cluster frame itself).
- For `HANDLER-CASE`, the compiler SHOULD emit a single `BLOCK` with an
  NLX catch-tag rather than per-clause blocks when possible.
- The baseline interpreter (T0) MAY use a slightly different representation
  (a Rust `Vec`-based stack) for simplicity, but MUST maintain the same
  O(1)/O(n) complexity guarantees.

---

## 5.4.9 Error Handling

| Failure Mode | Response |
|--------------|----------|
| `INVOKE-RESTART` with non-existent restart name | Signal `CONTROL-ERROR` |
| `ERROR` with no handler and no debugger available | Print condition to stderr, call `(ABORT)` restart; if none, `std::process::exit(1)` |
| Stack overflow during handler search | Use pre-allocated `STORAGE-CONDITION` (R5.110), attempt abbreviated handler search with reduced stack |
| Heap exhaustion during `MAKE-CONDITION` | Use pre-allocated `STORAGE-CONDITION` pool (D5.13); pool is 4 instances, recycled via flag |
| Recursive debugger entry | `*DEBUGGER-HOOK*` is set to `NIL` before calling the hook (A5.11), preventing infinite recursion |
| Condition signalled during `UNWIND-PROTECT` cleanup | Normal handler search from the cleanup's dynamic environment; does not interfere with the in-progress unwind |

---

## 5.4.10 Concurrency Considerations

- Handler and restart stacks are strictly thread-local — no
  synchronization is needed for establishment or search (R5.108).
- Condition objects themselves are normal CLOS instances on the heap and
  are subject to normal GC rules. A condition object MAY be shared between
  threads (e.g., passed via a channel), but signalling it in another thread
  will use that thread's handlers.
- `*DEBUGGER-HOOK*` is a thread-local special variable; binding it in one
  thread does not affect other threads.
- The pre-allocated `STORAGE-CONDITION` pool (D5.13) uses atomic
  compare-and-swap to claim an instance, ensuring thread-safety during
  OOM signalling.

---

## 5.4.11 Configuration

| Knob | Default | Description |
|------|---------|-------------|
| `BLISS_MAX_HANDLER_DEPTH` | 1024 | Maximum handler cluster nesting depth before signalling `CONTROL-ERROR` (stack overflow guard). |
| `BLISS_STORAGE_CONDITION_POOL_SIZE` | 4 | Number of pre-allocated `STORAGE-CONDITION` instances. |
| `*DEBUGGER-HOOK*` | `NIL` | Per-thread; user-settable hook before default debugger. |
| `*BREAK-ON-SIGNALS*` | `NIL` | Type specifier; matching signals enter debugger before handler search. |

---

## 5.4.12 Test Strategy

| Category | Method |
|----------|--------|
| ANSI compliance | Run `ansi-test` condition-related tests (section 19); all MUST pass. |
| Handler ordering | Unit tests verifying newest-first dispatch, correct stack rebinding during handler execution. |
| Handler-case unwinding | Tests confirming `UNWIND-PROTECT` cleanup runs between signal point and handler-case clause. |
| Restart machinery | Tests for `COMPUTE-RESTARTS` ordering, `:test-function` filtering, `INVOKE-RESTART-INTERACTIVELY`. |
| Thread isolation | Multi-threaded tests establishing handlers in one thread, signalling in another, verifying no cross-thread leakage. |
| OOM signalling | Stress test exhausting heap, verifying `STORAGE-CONDITION` is signalled from pre-allocated pool. |
| Performance | Microbenchmark: 10M handler-bind/signal cycles; verify zero heap allocation via GC counter. |
| Recursive signalling | Test signalling within a handler, verifying older-handler-only search. |
| `*BREAK-ON-SIGNALS*` | Test that matching conditions enter debugger before handler search. |
