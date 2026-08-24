# Bliss-specific Lisp API

This manual is the user-facing reference for Lisp interfaces that are specific
to Bliss. It covers the callable bootstrap API and the extension packages in
the staged Bliss specification. ANSI Common Lisp is outside its scope.

The implementation is still staged. An API appearing in the specification does
not necessarily mean that the bootstrap evaluator can call it today. Every
section therefore carries one of these labels:

| Label | Meaning |
| --- | --- |
| **Available** | Callable through the current `bliss-cli` evaluator. |
| **Partial** | Some underlying behavior exists, but the documented public contract is incomplete. |
| **Specified** | The public contract is defined by the spec, but no Lisp entry point is currently wired. |
| **Deferred** | Deliberately outside the v1 implementation target. |

The status in this document reflects Stage 5 (`spec/stages.json`) on
2026-08-20. The technical specification remains normative for future behavior;
this manual is authoritative about what users can call in the current
bootstrap.

## Package map

| Package | Purpose | Current status | Compatibility |
| --- | --- | --- | --- |
| `BLISS-EXT` | General runtime, compiler, process, package, image, atomic, and memory-barrier extensions | Package and selected functions available | Planned `SB-EXT` re-exports |
| `BLISS` | Deprecated nickname for `BLISS-EXT` | Specified compatibility alias; not created today | Older spelling only |
| `BLISS-THREAD` | OS-backed native threads and synchronization objects | Specified | Planned `SB-THREAD` re-exports |
| `BLISS-THREADS` | Deprecated nickname for `BLISS-THREAD` | Specified compatibility alias; not created today | Older spelling only |
| `BLISS-FIBER` | Lightweight M:N fibers and scheduler groups | Specified | Deliberately distinct from native threads |
| `BLISS-MOP` | Metaobject protocol | Partial internals; public package specified | Planned `SB-MOP` re-exports |
| `BLISS-CLTL2` | Compile-time environment inspection | Specified | Planned `SB-CLTL2` alias |
| `BLISS-GRAY-STREAMS` | Gray stream classes and generic functions | Partial internal dispatch | Intended to be re-exported from `COMMON-LISP` |
| `BLISS-FFI` | Typed foreign calls and callbacks | Partial Rust runtime; Lisp package not yet installed | CFFI/SB-ALIEN migration surface |
| `BLISS-DEBUG` | Breakpoints, watchpoints, and debugger plumbing | Partial Rust library; Lisp API specified | Bliss-specific |
| `BLISS-PROFILER` | Runtime profiling controls and reports | Specified; tier counters have separate available accessors | Bliss-specific |
| `BLISS-GC` | GC counters used by developer tools | Specified | Bliss-specific |
| `BLISS-SYS` | OS timing and page-fault counters | Specified | Bliss-specific |
| `BLISS-SBCL-COMPAT` | Opt-in SBCL migration re-exports | Specified | Transition aid, not a new semantic API |

`COMMON-LISP-USER` uses `COMMON-LISP` and `BLISS-EXT`, but portable code
should still qualify extension names. The canonical public names are
`BLISS-EXT`, `BLISS-THREAD`, `BLISS-FIBER`, and `BLISS-FFI`. Older `BLISS` and
`BLISS-THREADS` references are compatibility spellings only; new spec text must
not introduce fresh primary APIs under those package names. Compatibility
packages are not yet created by the bootstrap package initializer.

`BLISS-INTERNALS`/`BI` is an unstable implementation package and is
intentionally not a supported API.

## Currently available API

### Environment and working directory

#### `bliss-ext:getenv`

```lisp
(bliss-ext:getenv name) -> string-or-nil
```

Returns the process environment variable named by `name`, or `NIL` when it is
unset or cannot be represented by the host environment API.

```lisp
(or (bliss-ext:getenv "HOME") "no home directory")
```

The current bootstrap does not apply the specified sandbox `:ENV` capability
check to this function yet.

#### `bliss-ext:getcwd`

```lisp
(bliss-ext:getcwd) -> directory-namestring-or-nil
```

Returns the current working directory as a string. The bootstrap appends a
trailing slash, which lets ASDF treat the result as a directory namestring.
Returns `NIL` if the host query fails.

```lisp
(format t "Working in ~A~%" (bliss-ext:getcwd))
```

### Subprocesses

#### `bliss-ext:run-program`

```lisp
(bliss-ext:run-program command) -> exit-code, stdout, stderr
```

Runs a subprocess synchronously and captures both output streams.

- A string command is executed by `/bin/sh -c`.
- A list of strings is executed directly. Its first element is the program and
  the remaining elements are arguments.
- `exit-code` is `-1` if the process ended without a host exit code.
- Captured byte output is decoded with replacement for invalid UTF-8.
- Host launch failures signal `FILE-ERROR`.
- Sandbox mode denies the operation with a sandbox violation.

Prefer the list form when shell parsing is not required:

```lisp
(multiple-value-bind (code output error-output)
    (bliss-ext:run-program '("printf" "%s" "hello"))
  (list code output error-output))
;; => (0 "hello" "")
```

The string form intentionally permits shell syntax:

```lisp
(bliss-ext:run-program "printf 'one\\ntwo\\n' | wc -l")
```

### Command-line arguments

#### `bliss-ext:raw-command-line-arguments`

```lisp
(bliss-ext:raw-command-line-arguments) -> list-of-strings
```

Returns the host process argument vector, including the Bliss executable as the
first element. This is the low-level interface used by ASDF/UIOP.

#### `*command-line-args*`

```lisp
*command-line-args* -> list-of-strings
```

When running a script as

```sh
bliss script.lisp -- first second
```

the variable contains `("first" "second")`. It is `NIL` when no arguments
follow `--`. The current bootstrap name is unqualified
`*COMMAND-LINE-ARGS*`; the runtime spec's future spelling
`BLISS-EXT:*COMMAND-LINE-ARGUMENTS*` (also written
`BLISS:*COMMAND-LINE-ARGUMENTS*` in the migration guide) is not wired yet.

### Tiering and compiler diagnostics

These interfaces are diagnostic. Their results are observations, not controls,
and application correctness must not depend on when a function changes tier.
A function designator may be a function-name symbol or a function object.

| Function | Result |
| --- | --- |
| `(bliss-ext:function-tier function)` | `0`, `1`, or `2` for T0 bytecode/interpreter, T1 baseline native, or T2 optimized native; `NIL` if the designator is not a tiered function |
| `(bliss-ext:function-invoke-count function)` | Monotonic invocation count, or `NIL` for an unrecognized designator |
| `(bliss-ext:function-back-edge-count function)` | Aggregate loop back-edge count, or `NIL` for an unrecognized designator |
| `(bliss-ext:deopt-count)` | Process-wide number of speculative native deoptimizations |
| `(bliss-ext:bail-report)` | Prints bytecode-lowering bail reasons and counts, then returns the number of distinct reasons |

`FUNCTION-TIER` polls for completed background compilation before reading the
tier. `BAIL-REPORT` is useful when the process was started with
`BLISS_BAIL_TRACE=1`; without collected reasons it prints nothing and returns
zero.

```lisp
(defun sum-to (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum i)))))

(dotimes (i 20) (sum-to 1000))
(list (bliss-ext:function-tier 'sum-to)
      (bliss-ext:function-invoke-count 'sum-to)
      (bliss-ext:function-back-edge-count 'sum-to)
      (bliss-ext:deopt-count))
```

### Bootstrap image writer

#### `save-image` — **Partial**

```lisp
(save-image path) -> t
```

Writes restorable Lisp definitions for interpreted functions to `path`. The
current implementation is a minimal source-form snapshot, not the relocatable
heap image specified for `BLISS-EXT:SAVE-IMAGE`, and it does not exit the
process. A write failure signals `FILE-ERROR`.

Do not use it as a persistence boundary for arbitrary objects, compiled code,
dynamic state, or deoptimization data.

## Specified general extensions

Unless individually marked otherwise, the APIs below are **Specified**, not
yet callable through the bootstrap evaluator.

### Package-local nicknames

These CDR-10-style functions are exported from `BLISS-EXT`:

| Function | Contract |
| --- | --- |
| `(add-package-local-nickname local-name actual-package &optional (package *package*))` | Add a nickname visible only while resolving names from `package`; signal `PACKAGE-ERROR` for a conflicting global name belonging to another package |
| `(remove-package-local-nickname local-name &optional (package *package*))` | Remove and return the previously associated package, or `NIL` |
| `(package-local-nicknames &optional (package *package*))` | Return an alist of `(nickname . package)` |
| `(package-locally-nicknamed-by-list package)` | Return packages that locally nickname `package` |

Local nicknames shadow global package names and nicknames during
`FIND-PACKAGE` lookup from the owning package.

### Conduit packages

```lisp
(bliss-ext:defconduit name
  (:use package*)
  (:extends source-package*))

(bliss-ext:refresh-conduit conduit-package)
```

`DEFCONDUIT` defines a package that imports and re-exports the external symbols
of every `:EXTENDS` package. `REFRESH-CONDUIT` synchronizes it after a source
package changes.

### Global definitions

```lisp
(bliss-ext:defglobal name value &optional documentation)
(bliss-ext:define-load-time-global name value &optional documentation)
```

`DEFGLOBAL` creates one process-global atomic value rather than a per-thread
special binding. `DEFINE-LOAD-TIME-GLOBAL` evaluates its initializer once and
does not re-evaluate it when the defining file is reloaded.

### Weak references and finalizers

```lisp
(bliss-ext:make-weak-pointer object) -> weak-pointer
(bliss-ext:weak-pointer-value weak-pointer) -> object, alive-p
(bliss-ext:finalize object function &key dont-save) -> unspecified
(bliss-ext:cancel-finalization object) -> unspecified
```

The collector may clear a weak pointer once its referent is otherwise
unreachable. Finalizers run on a dedicated thread, outside GC, and cancellation
is idempotent.

### Extended hash tables

```lisp
(make-hash-table &key test size rehash-size rehash-threshold
                      hash-function synchronized weakness)
(bliss-ext:with-locked-hash-table (table) body*)
(bliss-ext:hash-table-synchronized-p table) -> boolean
(bliss-ext:hash-table-weakness table) -> nil-or-keyword
```

`:SYNCHRONIZED T` requests internal reader/writer locking. `:WEAKNESS` accepts
`:KEY`, `:VALUE`, `:KEY-AND-VALUE`, or `:KEY-OR-VALUE`.
`WITH-LOCKED-HASH-TABLE` holds the table's exclusive recursive lock for the
dynamic extent. The current `MAKE-HASH-TABLE` accepts extra keyword pairs but
only `:TEST` affects the constructed table, so the extended options remain
specified-only.

### Images, exit, timeout, and pathname helpers

| API | Contract |
| --- | --- |
| `(bliss-ext:save-image path &key executable compression purify)` | Save a full relocatable heap image and return its pathname; `:COMPRESSION` is `:NONE` or `:ZSTD`; deprecated alias `BLISS:SAVE-IMAGE` |
| `(sb-ext:save-lisp-and-die path &rest options)` | Compatibility alias that saves and exits |
| `(bliss-ext:exit &key (code 0) abort timeout)` | Orderly process exit; `:ABORT` skips cleanup and `:TIMEOUT` bounds joins |
| `(bliss-ext:quit &key (unix-status 0) recklessly-p)` | SBCL-compatible spelling of `EXIT` |
| `(bliss-ext:with-timeout (seconds timeout-form*) body*)` | Run `body`; on expiry evaluate timeout forms and signal `TIMEOUT` |
| `(bliss-ext:native-namestring pathname)` | Return the host-native pathname representation |
| `(bliss-ext:definition-source name)` | Return source location as `(:FILE ... :LINE ... :FORM ...)` |

### Extension metadata and conditions

`BLISS-EXT:*BLISS-EXT-VERSION*` is specified as a `(major minor)` list.
The extension condition/type surface comprises:

- `BLISS-EXT:IMAGE-ERROR`, `BLISS-EXT:CORRUPT-IMAGE-ERROR`,
  `BLISS-EXT:SIGNAL-ERROR`, and `BLISS-EXT:INTERNAL-ERROR`;
- `BLISS-EXT:STACK-OVERFLOW-ERROR`, `BLISS-EXT:TIMEOUT`, and
  `BLISS-EXT:TIMEOUT-CONDITION`;
- `BLISS-EXT:NOT-YET-IMPLEMENTED`;
- `BLISS-EXT:FOREIGN-POINTER`, an opaque validated foreign-address wrapper.

`BLISS-EXT:INTERNAL-ERROR` denotes a Bliss bug and is not a recoverable
application condition. `BLISS:IMAGE-ERROR` is a deprecated compatibility
spelling.

The deprecated `BLISS` compatibility package additionally specifies:

| API | Contract |
| --- | --- |
| `(bliss:gc &rest options)` | Older-package spelling of `BLISS-EXT:GC` |
| `bliss:*gc-run-time*` | Older-package spelling of `BLISS-EXT:*GC-RUN-TIME*` |
| `(bliss:defglobal name value &optional documentation)` | Older-package spelling of `BLISS-EXT:DEFGLOBAL` |
| `(bliss:make-weak-pointer object)` | Older-package spelling of `BLISS-EXT:MAKE-WEAK-POINTER` |
| `(bliss:exit &key code abort timeout)` | Older-package spelling of `BLISS-EXT:EXIT` |
| `(bliss:struct-slot-offset structure-type slot-name)` | Older-package spelling of `BLISS-EXT:STRUCT-SLOT-OFFSET` |
| `(bliss:deprecated-symbols)` | Return `(symbol replacement removal-version)` entries |

The public tuning specials are `BLISS-EXT:*DEFAULT-EXTERNAL-FORMAT*` (default
`:UTF-8`), `BLISS-EXT:*HASH-TABLE-DEFAULT-SIZE*` (16),
`BLISS-EXT:*HASH-TABLE-SYNCHRONIZED-DEFAULT*` (`NIL`),
`BLISS-EXT:*SORT-PARALLEL-THRESHOLD*` (10000), and
`BLISS-EXT:*READER-SOURCE-TRACKING*` (`T`). They are **Specified** and are not
installed by the bootstrap today.

### Sandboxing

```lisp
(bliss-ext:with-sandbox
    (:capabilities capabilities
     :path-whitelist path-patterns
     :net-whitelist host-port-patterns
     :max-heap-mb megabytes
     :max-cpu-seconds seconds)
  body*)
```

The specified capabilities are `:FILE-READ`, `:FILE-WRITE`, `:NET-CONNECT`,
`:NET-LISTEN`, `:FFI`, `:PROCESS`, `:ENV`, `:EVAL`, `:THREADS`, and `:IMAGE`.
Child threads inherit the context and may not widen it. Read-time evaluation
with `#.` is always disabled inside a sandbox.

## Native threads and synchronization

All entries in this section are **Specified**. The runtime contains native
thread and synchronization machinery, but the bootstrap package initializer
does not yet expose `BLISS-THREAD` Lisp objects or functions.

### Native thread lifecycle

```lisp
(bliss-thread:make-thread function &key name arguments ephemeral) -> thread
(bliss-thread:join-thread thread &key default timeout) -> result, status
(bliss-thread:thread-alive-p thread) -> boolean
(bliss-thread:destroy-thread thread) -> unspecified
(bliss-thread:interrupt-thread thread function) -> unspecified
(bliss-thread:abort-thread &optional condition) -> does-not-return
(bliss-thread:all-threads) -> list-of-threads
bliss-thread:*current-thread*
```

`MAKE-THREAD` creates one dedicated OS thread. It never creates a fiber.
`ARGUMENTS` is applied to `FUNCTION`. `JOIN-THREAD` status is `:OK`,
`:TIMEOUT`, or `:ABORTED`; multiple joiners are permitted. Interrupt and
destroy requests take effect at safepoints.

### Non-recursive mutexes

```lisp
(bliss-thread:make-mutex &key name) -> mutex
(bliss-thread:grab-mutex mutex &key (waitp t) timeout) -> boolean
(bliss-thread:release-mutex mutex &key (if-not-owner :error))
(bliss-thread:with-mutex (mutex &key (waitp t) timeout) body*)
```

`WAITP NIL` performs a try-lock. `IF-NOT-OWNER` accepts `:WARN`, `:FORCE`, or
`:ERROR`. `WITH-MUTEX` releases with `UNWIND-PROTECT` semantics.

### Condition variables

```lisp
(bliss-thread:make-condition-variable &key name) -> condition-variable
(bliss-thread:condition-wait condition-variable mutex &key timeout) -> boolean
(bliss-thread:condition-notify condition-variable &optional (count 1)) -> count
(bliss-thread:condition-broadcast condition-variable) -> count
```

`CONDITION-WAIT` atomically releases the mutex and waits, then reacquires it.
As with SBCL condition variables, callers must recheck their predicate after
waking. `TIMEOUT` is either `NIL` (wait indefinitely) or a non-negative real
number of seconds; the result is true if notified and `NIL` on timeout.

`MAKE-WAITQUEUE`, `CONDITION-WAIT`, `CONDITION-NOTIFY`, and
`CONDITION-BROADCAST` are accepted as deprecated aliases for the same
condition-variable API.

### Read-write locks

```lisp
(bliss-thread:make-rw-lock &key name) -> rw-lock
(bliss-thread:grab-rw-lock-read rw-lock &key (waitp t) timeout) -> boolean
(bliss-thread:grab-rw-lock-write rw-lock &key (waitp t) timeout) -> boolean
(bliss-thread:release-rw-lock-read rw-lock &key (if-not-owner :error)) -> nil
(bliss-thread:release-rw-lock-write rw-lock &key (if-not-owner :error)) -> nil
(bliss-thread:with-rw-lock-read (rw-lock &key (waitp t) timeout) body*) -> values
(bliss-thread:with-rw-lock-write (rw-lock &key (waitp t) timeout) body*) -> values
```

RW-locks use writer preference to prevent writer starvation. `WAITP NIL`
performs a try-lock. `TIMEOUT` has the same meaning as for condition variables.
`IF-NOT-OWNER` accepts `:ERROR`, `:WARN`, or `:IGNORE`; write-lock ownership is
tracked per thread, while read-lock release only verifies that the current
thread holds at least one read acquisition.

### Semaphores

```lisp
(bliss-thread:make-semaphore &key name (count 0)) -> semaphore
(bliss-thread:signal-semaphore semaphore &optional (count 1))
(bliss-thread:wait-on-semaphore semaphore &key timeout notification) -> boolean
(bliss-thread:semaphore-count semaphore) -> non-negative-integer
(bliss-thread:try-semaphore semaphore &optional (count 1)) -> boolean
```

### Recursive locks

```lisp
(bliss-thread:make-lock &key name) -> recursive-lock
(bliss-thread:grab-lock lock &key (waitp t) timeout) -> boolean
(bliss-thread:release-lock lock &key (if-not-owner :error))
(bliss-thread:with-lock (lock &key (waitp t) timeout) body*)
```

Every successful recursive acquisition must be matched by a release.

### Atomics and barriers

```lisp
(bliss-ext:cas place old new) -> previous-value
(bliss-ext:atomic-incf place &optional (delta 1)) -> previous-value
(bliss-ext:atomic-decf place &optional (delta 1)) -> previous-value
(bliss-ext:memory-barrier &optional (kind :full)) -> nil
(bliss-ext:load-barrier) -> nil
(bliss-ext:store-barrier) -> nil
(bliss-ext:with-atomic () body*)
```

`CAS` compares with `EQ`, stores only on a match, and returns the actual old
value. Specified places are special variables, structure slots, `SVREF`,
`CAR`, `CDR`, `SYMBOL-PLIST`, `SYMBOL-VALUE`, and `SLOT-VALUE`.
`ATOMIC-INCF` and `ATOMIC-DECF` support fixnum cells and return the value before
the update; overflow signals `ARITHMETIC-ERROR`.

Memory-barrier kinds are `:READ`, `:WRITE`, `:FULL`, and
`:DATA-DEPENDENCY`.
`WITH-ATOMIC` suppresses scheduler preemption for a short dynamic extent but
does not suppress GC safepoints.

### N-party barriers

```lisp
(bliss-thread:make-barrier count &key name) -> barrier
(bliss-thread:barrier-wait barrier &key timeout) -> index, status
(bliss-thread:barrier-count barrier) -> positive-integer
(bliss-thread:barrier-waiting-count barrier) -> non-negative-integer
(bliss-thread:reset-barrier barrier) -> nil
```

`COUNT` is the number of arrivals required to release a generation. `WAIT`
returns a zero-based arrival index and status `:OK`, or `NIL, :TIMEOUT` if the
timeout expires. `RESET-BARRIER` breaks the current generation and starts a new
one; waiters released by reset receive `NIL, :RESET`.

The fully qualified type names are `BLISS-THREAD:MUTEX`,
`BLISS-THREAD:RW-LOCK`, `BLISS-THREAD:CONDITION-VARIABLE`,
`BLISS-THREAD:SEMAPHORE`, and `BLISS-THREAD:BARRIER`. Lifecycle and
synchronization errors signal `BLISS-THREAD:THREAD-ERROR`. Older migration text
uses `BLISS-THREADS:MAKE-THREAD` and `BLISS-THREADS:MAKE-LOCK`; the current
contract keeps `BLISS-THREADS` only as a deprecated nickname for
`BLISS-THREAD`.

## Fibers

All entries in `BLISS-FIBER` are **Specified**. The Rust scheduler and fiber
data structures are under active development, but there is no public Lisp
package or callable fiber object in the bootstrap yet.

Fibers are lightweight managed executions multiplexed over a scheduler group's
OS carrier threads. A fiber is not a thread, and `BLISS-THREAD` functions do
not accept fiber objects.

### Lifecycle and scheduler groups

```lisp
(bliss-fiber:make-fiber function
  &key name arguments stack-size initial-bindings) -> fiber

(bliss-fiber:run-fibers fibers &key carrier-count idle-hook) -> results
(bliss-fiber:start-fibers fibers &key carrier-count idle-hook) -> group
(bliss-fiber:submit-fiber group fiber) -> unspecified
(bliss-fiber:finish-fibers group) -> results
(bliss-fiber:fiber-group-done-p group) -> boolean
(bliss-fiber:scheduler-group-carriers group) -> list-of-threads
```

`MAKE-FIBER` leaves the object in `:CREATED`; it does not schedule it.
First submission permanently assigns a fiber to one group. `START-FIBERS`
starts the carrier threads, while `RUN-FIBERS` is the blocking convenience
equivalent of start followed by finish. `FINISH-FIBERS` closes submission,
waits for every submitted fiber, returns results in submission order, joins the
carriers, and is idempotent.

`CARRIER-COUNT` defaults to the available processor count and must be at least
one. Carriers are ordinary visible `BLISS-THREAD` objects.

```lisp
;; Intended API; not runnable in the current bootstrap.
(let* ((fibers (list (bliss-fiber:make-fiber (lambda () 20))
                     (bliss-fiber:make-fiber (lambda () 22))))
       (group (bliss-fiber:start-fibers fibers :carrier-count 2)))
  (bliss-fiber:finish-fibers group))
;; => (20 22)
```

### Yielding, sleeping, parking, and joining

```lisp
(bliss-fiber:fiber-yield)
(bliss-fiber:fiber-sleep seconds)
(bliss-fiber:fiber-park predicate &key timeout) -> predicate-won-p
(bliss-fiber:fiber-join fiber &key timeout) -> entry-values
```

These operations park an unpinned fiber without blocking its carrier. Timer
and wake generations prevent an expired earlier wait from waking a later one.

### Pinning

```lisp
(bliss-fiber:fiber-pin &optional fiber)
(bliss-fiber:fiber-unpin &optional fiber)
(bliss-fiber:fiber-can-yield-p &optional fiber) -> boolean
(bliss-fiber:with-fiber-pinned ((&optional fiber)) body*)
bliss-fiber:*pinned-blocking-action*
```

Pinning is a balanced counter and prevents carrier migration. Explicit yield
while pinned signals `PROGRAM-ERROR`. The blocking policy is `:WARN` (default),
`:ERROR`, or `NIL` for silent OS-thread blocking. `WITH-FIBER-PINNED` balances
the counter on all exits.

### Introspection

| Function | Result |
| --- | --- |
| `(bliss-fiber:current-fiber)` | Current fiber or `NIL` on a plain native thread |
| `(bliss-fiber:list-all-fibers)` | Snapshot of non-reclaimed fibers |
| `(bliss-fiber:fiber-state fiber)` | `:CREATED`, `:RUNNABLE`, `:RUNNING`, `:SUSPENDED`, or `:DEAD` |
| `(bliss-fiber:fiber-name fiber)` | Name string or `NIL` |
| `(bliss-fiber:fiber-result fiber)` | Result values as a list after death |
| `(bliss-fiber:fiber-error-p fiber)` | Whether an unhandled condition caused death |
| `(bliss-fiber:fiber-alive-p fiber)` | Whether state is not `:DEAD` |
| `(bliss-fiber:fiber-carrier-thread fiber)` | Current or last carrier thread, or `NIL` |
| `(bliss-fiber:print-fiber-backtrace fiber &key stream count)` | Print its saved stack |

A running fiber must first reach a safepoint for stack inspection; otherwise
`FIBER-STILL-RUNNING` is signaled. Creation/submission errors signal
`FIBER-ERROR` with reasons such as `:LIMIT-EXCEEDED`, `:ALREADY-SUBMITTED`,
`:CLOSED-GROUP`, or `:OOM`.

## Timers

The `BLISS-EXT` timer API is **Specified**:

```lisp
(bliss-ext:make-timer function &key name thread) -> timer
(bliss-ext:schedule-timer timer time
  &key repeat-interval absolute-p catch-up) -> timer
(bliss-ext:unschedule-timer timer)
(bliss-ext:timer-scheduled-p timer &key delta) -> boolean
(bliss-ext:list-all-timers) -> list-of-timers
```

Relative `TIME` is in real seconds. With `ABSOLUTE-P`, it is universal time and
is converted to a monotonic deadline when scheduled. `REPEAT-INTERVAL` creates
a repeating timer; `CATCH-UP T` replays missed iterations. If `THREAD` is
supplied the callback is delivered there via `INTERRUPT-THREAD`; otherwise it
runs on the timer thread. `UNSCHEDULE-TIMER` waits for an executing callback and
is idempotent.

## Compiler and runtime introspection

### Additional tiering controls — **Specified**

```lisp
(bliss-ext:reset-deopt-log function)
(bliss-ext:set-runtime-option option value)
(bliss-ext:function-ic-stats function) -> property-list
(bliss-ext:dump-megamorphic-ics &key (stream *trace-output*))
```

`RESET-DEOPT-LOG` clears per-function deoptimization history and blacklisting.
`SET-RUNTIME-OPTION` updates supported compiler/runtime thresholds.
`FUNCTION-IC-STATS` reports inline-cache site state, hits, misses, and miss rate;
`DUMP-MEGAMORPHIC-ICS` reports unstable sites.

The `BLISS-PROFILER` threshold specials are:

- `*T0-T1-THRESHOLD*`, `*T1-T2-INVOKE-THRESHOLD*`,
  `*T1-T2-BACKEDGE-THRESHOLD*`, and `*OSR-BACKEDGE-THRESHOLD*`;
- `*DEOPT-PENALTY-MULTIPLIER*`, `*DEOPT-BLACKLIST-LIMIT*`, and
  `*SCHEDULER-POLL-MS*`;
- `*PERF-JITDUMP*`, `*PERF-MAP*`, `*DTRACE*`, and
  `*PROFILING-DISABLED*`.

Runtime threshold changes affect newly compiled functions; installed native
code retains the threshold with which it was compiled.

### Function and cross-reference introspection — **Specified**

```lisp
(bliss-ext:function-lambda-expression function)
  -> lambda-expression, closure-p, name
(bliss-ext:function-type function) -> type-or-nil
(bliss-ext:function-arglist function) -> lambda-list-or-:not-available
(bliss-ext:who-calls function-name &key package) -> locations
(bliss-ext:who-binds variable-name) -> locations
(bliss-ext:who-references variable-name) -> locations
(bliss-ext:who-sets variable-name) -> locations
(bliss-ext:who-macroexpands macro-name) -> locations
```

Source and cross-reference data require compilation with debug quality at
least one.

### Compiler hints — **Specified**

```lisp
(bliss-ext:truly-the type form)
(declaim (bliss-ext:always-bound *variable*))
(declaim (bliss-ext:freeze-type type-name))
(declaim (bliss-ext:muffle-conditions condition-type))
(declaim (bliss-ext:unmuffle-conditions condition-type))
```

`TRULY-THE` suppresses all checks; a false assertion has undefined behavior.
`ALWAYS-BOUND` removes unbound checks. `FREEZE-TYPE` permits stamp-based type
and slot optimizations and makes later redefinition exceptional. The muffling
declarations control compiler warnings/notes. `SB-EXT` aliases are specified
for all declaration identifiers.

### CLtL2 environment protocol — **Specified**

```lisp
(bliss-cltl2:variable-information symbol &optional environment)
  -> kind, local-p, declarations
(bliss-cltl2:function-information name &optional environment)
  -> kind, local-p, declarations
(bliss-cltl2:declaration-information name &optional environment) -> value
(bliss-cltl2:augment-environment environment
  &key variable symbol-macro function macro declare) -> new-environment
(bliss-cltl2:parse-macro name lambda-list body environment) -> expander
(bliss-cltl2:enclose lambda-expression &optional environment) -> function
```

Variable kinds are `:LEXICAL`, `:SPECIAL`, `:SYMBOL-MACRO`, `:CONSTANT`, or
`NIL`; function kinds are `:FUNCTION`, `:MACRO`, `:SPECIAL-FORM`, or `NIL`.
The protocol is intended for portable macro analyzers and compiler macros.
`PARSE-MACRO`'s environment is optional; the returned expander accepts
`(form environment)`. `ENCLOSE` closes a lambda expression over the supplied
compile-time environment.

## Metaobject protocol

The `BLISS-MOP` surface is **Specified** and intended to be re-exported from
`SB-MOP`. The bootstrap has CLOS internals but does not expose this complete
package contract yet.

### Class introspection and metaclasses

| Function | Result |
| --- | --- |
| `class-name`, `(setf class-name)` | Class name access/mutation |
| `class-direct-superclasses`, `class-direct-subclasses` | Direct class relations |
| `class-precedence-list` | Finalized precedence list |
| `class-direct-slots`, `class-slots` | Direct/effective slot definitions |
| `class-default-initargs`, `class-direct-default-initargs` | Default initarg triples |
| `class-finalized-p` | Finalization state |
| `class-prototype` | Lazily allocated prototype instance |

The metaclass/slot-definition classes are `FUNCALLABLE-STANDARD-CLASS`,
`FORWARD-REFERENCED-CLASS`, `STANDARD-DIRECT-SLOT-DEFINITION`, and
`STANDARD-EFFECTIVE-SLOT-DEFINITION`.

### Slot-definition accessors

All take one slot-definition object:

```text
slot-definition-name              slot-definition-initform
slot-definition-initfunction      slot-definition-type
slot-definition-allocation        slot-definition-initargs
slot-definition-readers           slot-definition-writers
slot-definition-location
```

### Generic functions and methods

```text
generic-function-name
generic-function-methods
generic-function-lambda-list
generic-function-method-combination
generic-function-method-class
generic-function-argument-precedence-order
method-function
method-generic-function
method-lambda-list
method-specializers
method-qualifiers
accessor-method-slot-definition
```

Each takes its corresponding generic-function or method object and returns the
named property. Reading a generic function's method list acquires its lock.

### Dispatch protocol

| Function | Signature/result |
| --- | --- |
| `compute-applicable-methods-using-classes` | `(gf classes) -> methods, definitive-p` |
| `compute-applicable-methods` | `(gf arguments) -> methods` |
| `compute-effective-method` | `(gf method-combination methods) -> form` |
| `make-method-lambda` | `(gf method lambda environment) -> lambda, initargs` |
| `compute-discriminating-function` | `(gf) -> function` |
| `slot-value-using-class` | `(class instance slot-definition) -> value` |
| `(setf slot-value-using-class)` | `(new class instance slot-definition) -> new` |
| `slot-boundp-using-class` | `(class instance slot-definition) -> boolean` |
| `slot-makunbound-using-class` | `(class instance slot-definition) -> instance` |
| `intern-eql-specializer` | `(object) -> eql-specializer` |
| `eql-specializer-object` | `(specializer) -> object` |
| `specializer-direct-methods` | `(specializer) -> methods` |
| `specializer-direct-generic-functions` | `(specializer) -> generic-functions` |
| `add-direct-method`, `remove-direct-method` | `(specializer method) -> unspecified` |

### Finalization and mutation

```text
compute-class-precedence-list (class)
compute-slots (class)
compute-effective-slot-definition (class name direct-slots)
compute-default-initargs (class)
finalize-inheritance (class)
ensure-class (name &rest initargs)
ensure-class-using-class (class name &rest initargs)
validate-superclass (class superclass)
allocate-instance (class &rest initargs)
ensure-generic-function (name &rest initargs)
ensure-generic-function-using-class (gf name &rest initargs)
add-method (gf method)
remove-method (gf method)
set-funcallable-instance-function (instance function)
```

Class finalization calls the four `COMPUTE-*` customization points in order.
Adding/removing a method also invalidates relevant Bliss inline caches.

## Gray streams

The `BLISS-GRAY-STREAMS` protocol is **Partial**. Built-in stream operations
contain Gray-dispatch paths, but the complete documented class hierarchy and
package are not yet installed by the bootstrap.

### Classes

`FUNDAMENTAL-STREAM` is the root. The mixin hierarchy adds
`FUNDAMENTAL-INPUT-STREAM`, `FUNDAMENTAL-OUTPUT-STREAM`,
`FUNDAMENTAL-CHARACTER-STREAM`, `FUNDAMENTAL-BINARY-STREAM`, and their four
character/binary input/output combinations.

### Generic functions

| Input | Output | Query/lifecycle |
| --- | --- | --- |
| `stream-read-char` | `stream-write-char` | `stream-element-type` |
| `stream-unread-char` | `stream-line-column` | `open-stream-p` |
| `stream-read-char-no-hang` | `stream-start-line-p` | `close` |
| `stream-peek-char` | `stream-write-string` | `interactive-stream-p` |
| `stream-listen` | `stream-terpri` | `stream-external-format` |
| `stream-read-line` | `stream-fresh-line` | `stream-file-position` |
| `stream-clear-input` | `stream-finish-output` | `stream-file-length` |
| `stream-read-byte` | `stream-force-output` |  |
| `stream-read-sequence` | `stream-clear-output` |  |
|  | `stream-advance-to-column` |  |
|  | `stream-write-byte` |  |
|  | `stream-write-sequence` |  |

The standard CL stream functions dispatch to these generics for fundamental
stream subclasses. A subclass must implement the elemental operation for its
direction; bulk and convenience operations have protocol defaults.

External formats are extensible through `CODEC-ENCODE`, `CODEC-DECODE`,
`CODEC-NAME`, and `CODEC-REPLACEMENT-CHARACTER` generic functions. Their
defining contract currently does not assign a separate public codec package.

## Debugger and profiling API

These developer-tool interfaces are **Specified** unless noted. The ordinary
ANSI operators `TRACE`, `UNTRACE`, `DESCRIBE`, `INSPECT`, `ROOM`, `TIME`, and
`DISASSEMBLE` are not Bliss-specific, but their Bliss-only options and support
interfaces are listed here.

### Debugger

```lisp
(bliss-debug:break-on-entry function &key when) -> breakpoint
(bliss-debug:break-at file line &key when) -> breakpoint
(bliss-debug:watch variable &key test thread) -> watchpoint
(bliss-debug:unwatch watchpoint-or-variable)
(bliss-debug:encapsulate function-name wrapper &key type)
(bliss-debug:unencapsulate function-name &key type)
(bliss-debug:inspect-object-parts object) -> vector
```

Lexical watchpoints require `(OPTIMIZE (DEBUG 3))`; special-variable
watchpoints work at every debug level. `TRACE` adds Bliss/SBCL-compatible
`:BREAK`, `:CONDITION`, `:REPORT`, and `:METHODS` options. `DISASSEMBLE` adds
`:TIER :T0`, `:T1`, or `:T2`.

The extension specials are `BLISS-EXT:*REPL-PROMPT-FUNCTION*` and
`BLISS-EXT:*ASDF-OUTPUT-TRANSLATIONS*`.

### Profiling and tool counters

The planned high-level profiler entry points are:

```lisp
(bliss-profiler:with-profiling (options*) body*) -> values, report
(bliss-profiler:report-profile report &key stream format sort-by)
```

Sampling, deterministic instrumentation, and allocation profiles are specified.
The supporting counter API used by `TIME` is:

```text
bliss-gc:gc-count                 bliss-gc:total-gc-time-ns
bliss-gc:bytes-allocated         bliss-sys:page-faults
bliss-sys:monotonic-ns           bliss-sys:cpu-user-ns
bliss-sys:cpu-system-ns
```

These support functions do not yet have stable detailed lambda lists beyond
their zero-argument uses in `TIME`.

## Foreign-function interface

The FFI is **Partial**: typed marshalling, foreign-call support, sandbox checks,
and callback structures exist in Rust, but the Lisp surface is not installed in
the bootstrap package initializer yet. The public contract is:

```lisp
(bliss-ffi:load-foreign-library path &key search system) -> foreign-library
(bliss-ffi:close-foreign-library foreign-library) -> nil
(bliss-ffi:foreign-symbol foreign-library name) -> foreign-pointer

(bliss-ffi:define-foreign-function lisp-name
    (foreign-name &key library calling-convention)
    return-type
    argument-types
  &key documentation sandbox-capability)
  -> lisp-name

(bliss-ffi:foreign-funcall foreign-pointer return-type argument-types &rest args)
  -> value

(bliss-ffi:make-callback function return-type argument-types
  &key calling-convention keepalive-name) -> foreign-pointer

(bliss-ffi:free-callback foreign-pointer) -> nil
```

`BLISS-FFI:FFI-ERROR` is the public error condition. Foreign addresses must be
wrapped as validated `BLISS-EXT:FOREIGN-POINTER` objects; user code never
receives a raw address. `RETURN-TYPE` and each element of `ARGUMENT-TYPES` are
foreign type designators from the FFI type table (`:VOID`, signed and unsigned
integer widths, pointer, C string, and callback pointer). Foreign operations
require the sandbox `:FFI` capability; a missing capability signals
`BLISS-EXT:SANDBOX-VIOLATION`.

## Deferred extensible sequences

`SB-SEQUENCE:DEFINE-SEQUENCE-CLASS` is **Deferred** to v2. In v1 the specified
macro signals `BLISS-EXT:NOT-YET-IMPLEMENTED` with a message identifying that
deferral.

## Configuration used by the extension APIs

The most user-relevant environment variables are:

| Variable | Purpose |
| --- | --- |
| `BLISS_WORKERS` | Default scheduler-group carrier count |
| `BLISS_STACK_SIZE` | Per-fiber CL stack size |
| `BLISS_MAX_FIBERS` | Fiber limit; legacy `BLISS_MAX_THREADS` is accepted |
| `BLISS_TIME_SLICE_US` | Fiber cooperative-preemption interval |
| `BLISS_PINNED_BLOCKING_ACTION` | `WARN`, `ERROR`, or `NIL`/`NATIVE` policy |
| `BLISS_T0_T1_THRESHOLD` | T0 to T1 invocation threshold |
| `BLISS_T1_T2_INVOKE_THRESHOLD` | T1 to T2 invocation threshold |
| `BLISS_T1_T2_BACKEDGE_THRESHOLD` | T1 to T2 loop threshold |
| `BLISS_OSR_BACKEDGE_THRESHOLD` | Mid-loop OSR threshold |
| `BLISS_DEOPT_BLACKLIST_THRESHOLD` | Repeated-deoptimization cutoff |
| `BLISS_BAIL_TRACE` | Collect lowerer bail reasons for `BAIL-REPORT` |
| `BLISS_PERF_MAP` | Emit Linux `perf-<pid>.map` JIT symbols |
| `BLISS_PERF_JITDUMP` | Emit Linux perf jitdump metadata |
| `BLISS_INIT_FILE` | REPL initialization file; defaults to `~/.blissrc` |

Environment variables are read during bootstrap or code compilation; changing
the host environment after startup need not update already-installed code.

## Sources and status checks

The contracts summarized here come from these focused specifications:

- `spec/02-runtime-core.md` — execution, callbacks, startup, shutdown;
- `spec/04-02-macroexpand.md` — `BLISS-CLTL2`;
- `spec/04-06-osr.md`, `spec/04-08-inline-caches.md`, and
  `spec/04-09-profiling.md` — compiler/runtime introspection;
- `spec/05-01-packages-bootstrap.md` — packages and conduits;
- `spec/05-04-streams.md` — Gray streams;
- `spec/06-devtools.md` — debugger and profiling APIs;
- `spec/08-security-robustness.md` — sandbox and safe FFI;
- `spec/09-extensions.md` — the main extension contract;
- `spec/13-concurrency.md` — memory ordering, fibers, and synchronization.

To audit current bootstrap availability, search exact extension dispatch arms:

```sh
rg '"BLISS-EXT:[A-Z-]+" =>' crates/bliss/src/cli.rs
```

An undefined function for an entry marked **Specified** is an implementation
status result, not a package-qualification problem.
