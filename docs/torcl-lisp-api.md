# TorCL-specific Lisp API

> Historical extension inventory: implementation-status labels below reflect
> an earlier development stage. The [new manual](manual/index.md) contains
> current task guides and a focused reference; check source/tests before
> relying on status labels in this inventory.

This manual is the user-facing reference for Lisp interfaces that are specific
to TorCL. It covers the callable bootstrap API and the extension packages in
the staged TorCL specification. ANSI Common Lisp is outside its scope.

The implementation is still staged. An API appearing in the specification does
not necessarily mean that the bootstrap evaluator can call it today. Every
section therefore carries one of these labels:

| Label | Meaning |
| --- | --- |
| **Available** | Callable through the current `torcl` evaluator. |
| **Partial** | Some underlying behavior exists, but the documented public contract is incomplete. |
| **Specified** | The public contract is defined by the spec, but no Lisp entry point is currently wired. |
| **Deferred** | Deliberately outside the v1 implementation target. |

The status in this document reflects Stage 5 (`spec/stages.json`) on
2026-08-20. The technical specification remains normative for future behavior;
the availability labels below have not been comprehensively re-audited against
the current bootstrap.

## Package map

| Package | Purpose | Current status | Compatibility |
| --- | --- | --- | --- |
| `TORCL-EXT` | General runtime, compiler, process, package, image, atomic, and memory-barrier extensions | Package and selected functions available | Planned `SB-EXT` re-exports |
| `TORCL` | Deprecated nickname for `TORCL-EXT` | Specified compatibility alias; not created today | Older spelling only |
| `TORCL-THREAD` | OS-backed native threads and synchronization objects | Specified | Planned `SB-THREAD` re-exports |
| `TORCL-THREADS` | Deprecated nickname for `TORCL-THREAD` | Specified compatibility alias; not created today | Older spelling only |
| `TORCL-FIBER` | Lightweight M:N fibers and scheduler groups | Specified | Deliberately distinct from native threads |
| `TORCL-MOP` | Metaobject protocol | Partial internals; public package specified | Planned `SB-MOP` re-exports |
| `TORCL-CLTL2` | Compile-time environment inspection | Specified | Planned `SB-CLTL2` alias |
| `TORCL-GRAY-STREAMS` | Gray stream classes and generic functions | Partial internal dispatch | Intended to be re-exported from `COMMON-LISP` |
| `TORCL-FFI` | Typed foreign calls and callbacks | Partial Rust runtime; Lisp package not yet installed | CFFI/SB-ALIEN migration surface |
| `TORCL-PYTHON` (nickname `PY`) | Embedded CPython as a second object system | Available in a build with the `python` feature | TorCL-specific; deliberately not CFFI-shaped |
| `TORCL-DEBUG` | Breakpoints, watchpoints, and debugger plumbing | Partial Rust library; Lisp API specified | TorCL-specific |
| `TORCL-PROFILER` | Runtime profiling controls and reports | Specified; tier counters have separate available accessors | TorCL-specific |
| `TORCL-GC` | GC counters used by developer tools | Specified | TorCL-specific |
| `TORCL-SYS` | OS timing and page-fault counters | Specified | TorCL-specific |
| `TORCL-SBCL-COMPAT` | Opt-in SBCL migration re-exports | Specified | Transition aid, not a new semantic API |

`COMMON-LISP-USER` uses `COMMON-LISP` and `TORCL-EXT`, but portable code
should still qualify extension names. The canonical public names are
`TORCL-EXT`, `TORCL-THREAD`, `TORCL-FIBER`, and `TORCL-FFI`. Older `TORCL` and
`TORCL-THREADS` references are compatibility spellings only; new spec text must
not introduce fresh primary APIs under those package names. Compatibility
packages are not yet created by the bootstrap package initializer.

`TORCL-INTERNALS`/`BI` is an unstable implementation package and is
intentionally not a supported API.

## Currently available API

### Environment and working directory

#### `torcl-ext:getenv`

```lisp
(torcl-ext:getenv name) -> string-or-nil
```

Returns the process environment variable named by `name`, or `NIL` when it is
unset or cannot be represented by the host environment API.

```lisp
(or (torcl-ext:getenv "HOME") "no home directory")
```

The current bootstrap does not apply the specified sandbox `:ENV` capability
check to this function yet.

#### `torcl-ext:getcwd`

```lisp
(torcl-ext:getcwd) -> directory-namestring-or-nil
```

Returns the current working directory as a string. The bootstrap appends a
trailing slash, which lets ASDF treat the result as a directory namestring.
Returns `NIL` if the host query fails.

```lisp
(format t "Working in ~A~%" (torcl-ext:getcwd))
```

### Subprocesses

#### `torcl-ext:run-program`

```lisp
(torcl-ext:run-program command) -> exit-code, stdout, stderr
```

Runs a subprocess synchronously and captures both output streams.

- A string command is executed by `/bin/sh -c` on Unix or `COMSPEC` with
  `/D /S /C` on Windows.
- A list of strings is executed directly. Its first element is the program and
  the remaining elements are arguments.
- `exit-code` is `-1` if the process ended without a host exit code.
- Captured byte output is decoded with replacement for invalid UTF-8.
- Host launch failures signal `FILE-ERROR`.
- Sandbox mode denies the operation with a sandbox violation.
- Waiting permits garbage collection. Unpinned fibers park and release their
  carrier; pinned fibers follow the configured blocking policy. The error
  policy rejects capture before starting a child.

Prefer the list form when shell parsing is not required:

```lisp
(multiple-value-bind (code output error-output)
    (torcl-ext:run-program '("printf" "%s" "hello"))
  (list code output error-output))
;; => (0 "hello" "")
```

The string form intentionally permits shell syntax:

```lisp
(torcl-ext:run-program "printf 'one\\ntwo\\n' | wc -l")
```

### Command-line arguments

#### `torcl-ext:raw-command-line-arguments`

```lisp
(torcl-ext:raw-command-line-arguments) -> list-of-strings
```

Returns the host process argument vector, including the TorCL executable as the
first element. This is the low-level interface used by ASDF/UIOP.

#### `*command-line-args*`

```lisp
*command-line-args* -> list-of-strings
```

When running a script as

```sh
torcl script.lisp -- first second
```

the variable contains `("first" "second")`. It is `NIL` when no arguments
follow `--`. The current bootstrap name is unqualified
`*COMMAND-LINE-ARGS*`; the runtime spec's future spelling
`TORCL-EXT:*COMMAND-LINE-ARGUMENTS*` (also written
`TORCL:*COMMAND-LINE-ARGUMENTS*` in the migration guide) is not wired yet.

### Tiering and compiler diagnostics

These interfaces are diagnostic. Their results are observations, not controls,
and application correctness must not depend on when a function changes tier.
A function designator may be a function-name symbol or a function object.

| Function | Result |
| --- | --- |
| `(torcl-ext:function-tier function)` | `0`, `1`, or `2` for T0 bytecode/interpreter, T1 baseline native, or T2 optimized native; `NIL` if the designator is not a tiered function |
| `(torcl-ext:function-invoke-count function)` | Monotonic invocation count, or `NIL` for an unrecognized designator |
| `(torcl-ext:function-back-edge-count function)` | Aggregate loop back-edge count, or `NIL` for an unrecognized designator |
| `(torcl-ext:deopt-count)` | Process-wide number of speculative native deoptimizations |
| `(torcl-ext:bail-report)` | Prints bytecode-lowering bail reasons and counts, then returns the number of distinct reasons |

`FUNCTION-TIER` polls for completed background compilation before reading the
tier. `BAIL-REPORT` is useful when the process was started with
`TORCL_BAIL_TRACE=1`; without collected reasons it prints nothing and returns
zero.

```lisp
(defun sum-to (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum i)))))

(dotimes (i 20) (sum-to 1000))
(list (torcl-ext:function-tier 'sum-to)
      (torcl-ext:function-invoke-count 'sum-to)
      (torcl-ext:function-back-edge-count 'sum-to)
      (torcl-ext:deopt-count))
```

### Bootstrap image writer

#### `save-image` — **Partial**

```lisp
(save-image path) -> t
```

Writes restorable Lisp definitions for interpreted functions to `path`. The
current implementation is a minimal source-form snapshot, not the relocatable
heap image specified for `TORCL-EXT:SAVE-IMAGE`, and it does not exit the
process. A write failure signals `FILE-ERROR`.

Do not use it as a persistence boundary for arbitrary objects, compiled code,
dynamic state, or deoptimization data.

## Specified general extensions

Unless individually marked otherwise, the APIs below are **Specified**, not
yet callable through the bootstrap evaluator.

### Package-local nicknames

These CDR-10-style functions are exported from `TORCL-EXT`:

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
(torcl-ext:defconduit name
  (:use package*)
  (:extends source-package*))

(torcl-ext:refresh-conduit conduit-package)
```

`DEFCONDUIT` defines a package that imports and re-exports the external symbols
of every `:EXTENDS` package. `REFRESH-CONDUIT` synchronizes it after a source
package changes.

### Global definitions

```lisp
(torcl-ext:defglobal name value &optional documentation)
(torcl-ext:define-load-time-global name value &optional documentation)
```

`DEFGLOBAL` creates one process-global atomic value rather than a per-thread
special binding. `DEFINE-LOAD-TIME-GLOBAL` evaluates its initializer once and
does not re-evaluate it when the defining file is reloaded.

### Weak references and finalizers

```lisp
(torcl-ext:make-weak-pointer object) -> weak-pointer
(torcl-ext:weak-pointer-value weak-pointer) -> object, alive-p
(torcl-ext:finalize object function &key dont-save) -> unspecified
(torcl-ext:cancel-finalization object) -> unspecified
```

The collector may clear a weak pointer once its referent is otherwise
unreachable. Finalizers run on a dedicated thread, outside GC, and cancellation
is idempotent.

### Extended hash tables

```lisp
(make-hash-table &key test size rehash-size rehash-threshold
                      hash-function synchronized weakness)
(torcl-ext:with-locked-hash-table (table) body*)
(torcl-ext:hash-table-synchronized-p table) -> boolean
(torcl-ext:hash-table-weakness table) -> nil-or-keyword
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
| `(torcl-ext:save-image path &key executable compression purify)` | Save a full relocatable heap image and return its pathname; `:COMPRESSION` is `:NONE` or `:ZSTD`; deprecated alias `TORCL:SAVE-IMAGE` |
| `(sb-ext:save-lisp-and-die path &rest options)` | Compatibility alias that saves and exits |
| `(torcl-ext:exit &key (code 0) abort timeout)` | Orderly process exit; `:ABORT` skips cleanup and `:TIMEOUT` bounds joins |
| `(torcl-ext:quit &key (unix-status 0) recklessly-p)` | SBCL-compatible spelling of `EXIT` |
| `(torcl-ext:with-timeout (seconds timeout-form*) body*)` | Run `body`; on expiry evaluate timeout forms and signal `TIMEOUT` |
| `(torcl-ext:native-namestring pathname)` | Return the host-native pathname representation |
| `(torcl-ext:definition-source name)` | Return source location as `(:FILE ... :LINE ... :FORM ...)` |

### Extension metadata and conditions

`TORCL-EXT:*TORCL-EXT-VERSION*` is specified as a `(major minor)` list.
The extension condition/type surface comprises:

- `TORCL-EXT:IMAGE-ERROR`, `TORCL-EXT:CORRUPT-IMAGE-ERROR`,
  `TORCL-EXT:SIGNAL-ERROR`, and `TORCL-EXT:INTERNAL-ERROR`;
- `TORCL-EXT:STACK-OVERFLOW-ERROR`, `TORCL-EXT:TIMEOUT`, and
  `TORCL-EXT:TIMEOUT-CONDITION`;
- `TORCL-EXT:NOT-YET-IMPLEMENTED`;
- `TORCL-EXT:FOREIGN-POINTER`, an opaque validated foreign-address wrapper.

`TORCL-EXT:INTERNAL-ERROR` denotes a TorCL bug and is not a recoverable
application condition. `TORCL:IMAGE-ERROR` is a deprecated compatibility
spelling.

The deprecated `TORCL` compatibility package additionally specifies:

| API | Contract |
| --- | --- |
| `(torcl:gc &rest options)` | Older-package spelling of `TORCL-EXT:GC` |
| `torcl:*gc-run-time*` | Older-package spelling of `TORCL-EXT:*GC-RUN-TIME*` |
| `(torcl:defglobal name value &optional documentation)` | Older-package spelling of `TORCL-EXT:DEFGLOBAL` |
| `(torcl:make-weak-pointer object)` | Older-package spelling of `TORCL-EXT:MAKE-WEAK-POINTER` |
| `(torcl:exit &key code abort timeout)` | Older-package spelling of `TORCL-EXT:EXIT` |
| `(torcl:struct-slot-offset structure-type slot-name)` | Older-package spelling of `TORCL-EXT:STRUCT-SLOT-OFFSET` |
| `(torcl:deprecated-symbols)` | Return `(symbol replacement removal-version)` entries |

The public tuning specials are `TORCL-EXT:*DEFAULT-EXTERNAL-FORMAT*` (default
`:UTF-8`), `TORCL-EXT:*HASH-TABLE-DEFAULT-SIZE*` (16),
`TORCL-EXT:*HASH-TABLE-SYNCHRONIZED-DEFAULT*` (`NIL`),
`TORCL-EXT:*SORT-PARALLEL-THRESHOLD*` (10000), and
`TORCL-EXT:*READER-SOURCE-TRACKING*` (`T`). They are **Specified** and are not
installed by the bootstrap today.

### Sandboxing

```lisp
(torcl-ext:with-sandbox
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
does not yet expose `TORCL-THREAD` Lisp objects or functions.

### Native thread lifecycle

```lisp
(torcl-thread:make-thread function &key name arguments ephemeral) -> thread
(torcl-thread:join-thread thread &key default timeout) -> result, status
(torcl-thread:thread-alive-p thread) -> boolean
(torcl-thread:destroy-thread thread) -> unspecified
(torcl-thread:interrupt-thread thread function) -> unspecified
(torcl-thread:abort-thread &optional condition) -> does-not-return
(torcl-thread:all-threads) -> list-of-threads
torcl-thread:*current-thread*
```

`MAKE-THREAD` creates one dedicated OS thread. It never creates a fiber.
`ARGUMENTS` is applied to `FUNCTION`. `JOIN-THREAD` status is `:OK`,
`:TIMEOUT`, or `:ABORTED`; multiple joiners are permitted. Interrupt and
destroy requests take effect at safepoints.

### Non-recursive mutexes

```lisp
(torcl-thread:make-mutex &key name) -> mutex
(torcl-thread:grab-mutex mutex &key (waitp t) timeout) -> boolean
(torcl-thread:release-mutex mutex &key (if-not-owner :error))
(torcl-thread:with-mutex (mutex &key (waitp t) timeout) body*)
```

`WAITP NIL` performs a try-lock. `IF-NOT-OWNER` accepts `:WARN`, `:FORCE`, or
`:ERROR`. `WITH-MUTEX` releases with `UNWIND-PROTECT` semantics.

### Condition variables

```lisp
(torcl-thread:make-condition-variable &key name) -> condition-variable
(torcl-thread:condition-wait condition-variable mutex &key timeout) -> boolean
(torcl-thread:condition-notify condition-variable &optional (count 1)) -> count
(torcl-thread:condition-broadcast condition-variable) -> count
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
(torcl-thread:make-rw-lock &key name) -> rw-lock
(torcl-thread:grab-rw-lock-read rw-lock &key (waitp t) timeout) -> boolean
(torcl-thread:grab-rw-lock-write rw-lock &key (waitp t) timeout) -> boolean
(torcl-thread:release-rw-lock-read rw-lock &key (if-not-owner :error)) -> nil
(torcl-thread:release-rw-lock-write rw-lock &key (if-not-owner :error)) -> nil
(torcl-thread:with-rw-lock-read (rw-lock &key (waitp t) timeout) body*) -> values
(torcl-thread:with-rw-lock-write (rw-lock &key (waitp t) timeout) body*) -> values
```

RW-locks use writer preference to prevent writer starvation. `WAITP NIL`
performs a try-lock. `TIMEOUT` has the same meaning as for condition variables.
`IF-NOT-OWNER` accepts `:ERROR`, `:WARN`, or `:IGNORE`; write-lock ownership is
tracked per thread, while read-lock release only verifies that the current
thread holds at least one read acquisition.

### Semaphores

```lisp
(torcl-thread:make-semaphore &key name (count 0)) -> semaphore
(torcl-thread:signal-semaphore semaphore &optional (count 1))
(torcl-thread:wait-on-semaphore semaphore &key timeout notification) -> boolean
(torcl-thread:semaphore-count semaphore) -> non-negative-integer
(torcl-thread:try-semaphore semaphore &optional (count 1)) -> boolean
```

### Recursive locks

```lisp
(torcl-thread:make-lock &key name) -> recursive-lock
(torcl-thread:grab-lock lock &key (waitp t) timeout) -> boolean
(torcl-thread:release-lock lock &key (if-not-owner :error))
(torcl-thread:with-lock (lock &key (waitp t) timeout) body*)
```

Every successful recursive acquisition must be matched by a release.

### Atomics and barriers

```lisp
(torcl-ext:cas place old new) -> previous-value
(torcl-ext:atomic-incf place &optional (delta 1)) -> previous-value
(torcl-ext:atomic-decf place &optional (delta 1)) -> previous-value
(torcl-ext:memory-barrier &optional (kind :full)) -> nil
(torcl-ext:load-barrier) -> nil
(torcl-ext:store-barrier) -> nil
(torcl-ext:with-atomic () body*)
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
(torcl-thread:make-barrier count &key name) -> barrier
(torcl-thread:barrier-wait barrier &key timeout) -> index, status
(torcl-thread:barrier-count barrier) -> positive-integer
(torcl-thread:barrier-waiting-count barrier) -> non-negative-integer
(torcl-thread:reset-barrier barrier) -> nil
```

`COUNT` is the number of arrivals required to release a generation. `WAIT`
returns a zero-based arrival index and status `:OK`, or `NIL, :TIMEOUT` if the
timeout expires. `RESET-BARRIER` breaks the current generation and starts a new
one; waiters released by reset receive `NIL, :RESET`.

The fully qualified type names are `TORCL-THREAD:MUTEX`,
`TORCL-THREAD:RW-LOCK`, `TORCL-THREAD:CONDITION-VARIABLE`,
`TORCL-THREAD:SEMAPHORE`, and `TORCL-THREAD:BARRIER`. Lifecycle and
synchronization errors signal `TORCL-THREAD:THREAD-ERROR`. Older migration text
uses `TORCL-THREADS:MAKE-THREAD` and `TORCL-THREADS:MAKE-LOCK`; the current
contract keeps `TORCL-THREADS` only as a deprecated nickname for
`TORCL-THREAD`.

## Fibers

The `TORCL-FIBER` lifecycle, waiting, pinning, and observation APIs are installed
by the standard bootstrap on x86-64 Unix and Windows. See the
[fiber manual](manual/fibers.md) for result, timeout, stack-size, and error semantics.

Fibers are lightweight managed executions multiplexed over a scheduler group's
OS carrier threads. A fiber is not a thread, and `TORCL-THREAD` functions do
not accept fiber objects.

### Lifecycle and scheduler groups

```lisp
(torcl-fiber:make-fiber function
  &key name arguments stack-size initial-bindings) -> fiber

(torcl-fiber:run-fibers fibers &key carrier-count idle-hook) -> results
(torcl-fiber:start-fibers fibers &key carrier-count idle-hook) -> group
(torcl-fiber:submit-fiber group fiber) -> unspecified
(torcl-fiber:finish-fibers group) -> results
(torcl-fiber:fiber-group-done-p group) -> boolean
(torcl-fiber:scheduler-group-carriers group) -> list-of-threads
```

`MAKE-FIBER` leaves the object in `:CREATED`; it does not schedule it.
First submission permanently assigns a fiber to one group. `START-FIBERS`
starts the carrier threads, while `RUN-FIBERS` is the blocking convenience
equivalent of start followed by finish. `FINISH-FIBERS` closes submission,
waits for every submitted fiber, returns results in submission order, joins the
carriers, and is idempotent.

`CARRIER-COUNT` defaults to the available processor count and must be at least
one. Carriers are ordinary visible `TORCL-THREAD` objects.

```lisp
;; Intended API; not runnable in the current bootstrap.
(let* ((fibers (list (torcl-fiber:make-fiber (lambda () 20))
                     (torcl-fiber:make-fiber (lambda () 22))))
       (group (torcl-fiber:start-fibers fibers :carrier-count 2)))
  (torcl-fiber:finish-fibers group))
;; => (20 22)
```

### Yielding, sleeping, parking, and joining

```lisp
(torcl-fiber:fiber-yield)
(torcl-fiber:fiber-sleep seconds)
(torcl-fiber:fiber-park predicate &key timeout) -> predicate-won-p
(torcl-fiber:fiber-join fiber &key timeout) -> entry-values
```

These operations park an unpinned fiber without blocking its carrier. Timer
and wake generations prevent an expired earlier wait from waking a later one.

### Pinning

```lisp
(torcl-fiber:fiber-pin &optional fiber)
(torcl-fiber:fiber-unpin &optional fiber)
(torcl-fiber:fiber-can-yield-p &optional fiber) -> boolean
(torcl-fiber:with-fiber-pinned ((&optional fiber)) body*)
torcl-fiber:*pinned-blocking-action*
```

Pinning is a balanced counter and prevents carrier migration. Explicit yield
while pinned signals `PROGRAM-ERROR`. The blocking policy is `:WARN` (default),
`:ERROR`, or `NIL` for silent OS-thread blocking. `WITH-FIBER-PINNED` balances
the counter on all exits.

### Introspection

| Function | Result |
| --- | --- |
| `(torcl-fiber:current-fiber)` | Current fiber or `NIL` on a plain native thread |
| `(torcl-fiber:list-all-fibers)` | Snapshot of non-reclaimed fibers |
| `(torcl-fiber:fiber-state fiber)` | `:CREATED`, `:RUNNABLE`, `:RUNNING`, `:SUSPENDED`, or `:DEAD` |
| `(torcl-fiber:fiber-name fiber)` | Name string or `NIL` |
| `(torcl-fiber:fiber-result fiber)` | Result values as a list after death |
| `(torcl-fiber:fiber-error-p fiber)` | Whether an unhandled condition caused death |
| `(torcl-fiber:fiber-alive-p fiber)` | Whether state is not `:DEAD` |
| `(torcl-fiber:fiber-carrier-thread fiber)` | Current or last carrier thread, or `NIL` |
| `(torcl-fiber:print-fiber-backtrace fiber &key stream count)` | Print its saved stack |

A running fiber must first reach a safepoint for stack inspection; otherwise
`FIBER-STILL-RUNNING` is signaled. Creation/submission errors signal
`FIBER-ERROR` with reasons such as `:LIMIT-EXCEEDED`, `:ALREADY-SUBMITTED`,
`:CLOSED-GROUP`, or `:OOM`.

## Timers

The `TORCL-EXT` timer API is **Specified**:

```lisp
(torcl-ext:make-timer function &key name thread) -> timer
(torcl-ext:schedule-timer timer time
  &key repeat-interval absolute-p catch-up) -> timer
(torcl-ext:unschedule-timer timer)
(torcl-ext:timer-scheduled-p timer &key delta) -> boolean
(torcl-ext:list-all-timers) -> list-of-timers
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
(torcl-ext:reset-deopt-log function)
(torcl-ext:set-runtime-option option value)
(torcl-ext:function-ic-stats function) -> property-list
(torcl-ext:dump-megamorphic-ics &key (stream *trace-output*))
```

`RESET-DEOPT-LOG` clears per-function deoptimization history and blacklisting.
`SET-RUNTIME-OPTION` updates supported compiler/runtime thresholds.
`FUNCTION-IC-STATS` reports inline-cache site state, hits, misses, and miss rate;
`DUMP-MEGAMORPHIC-ICS` reports unstable sites.

The `TORCL-PROFILER` threshold specials are:

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
(torcl-ext:function-lambda-expression function)
  -> lambda-expression, closure-p, name
(torcl-ext:function-type function) -> type-or-nil
(torcl-ext:function-arglist function) -> lambda-list-or-:not-available
(torcl-ext:who-calls function-name &key package) -> locations
(torcl-ext:who-binds variable-name) -> locations
(torcl-ext:who-references variable-name) -> locations
(torcl-ext:who-sets variable-name) -> locations
(torcl-ext:who-macroexpands macro-name) -> locations
```

Source and cross-reference data require compilation with debug quality at
least one.

### Compiler hints — **Specified**

```lisp
(torcl-ext:truly-the type form)
(declaim (torcl-ext:always-bound *variable*))
(declaim (torcl-ext:freeze-type type-name))
(declaim (torcl-ext:muffle-conditions condition-type))
(declaim (torcl-ext:unmuffle-conditions condition-type))
```

`TRULY-THE` suppresses all checks; a false assertion has undefined behavior.
`ALWAYS-BOUND` removes unbound checks. `FREEZE-TYPE` permits stamp-based type
and slot optimizations and makes later redefinition exceptional. The muffling
declarations control compiler warnings/notes. `SB-EXT` aliases are specified
for all declaration identifiers.

### CLtL2 environment protocol — **Specified**

```lisp
(torcl-cltl2:variable-information symbol &optional environment)
  -> kind, local-p, declarations
(torcl-cltl2:function-information name &optional environment)
  -> kind, local-p, declarations
(torcl-cltl2:declaration-information name &optional environment) -> value
(torcl-cltl2:augment-environment environment
  &key variable symbol-macro function macro declare) -> new-environment
(torcl-cltl2:parse-macro name lambda-list body environment) -> expander
(torcl-cltl2:enclose lambda-expression &optional environment) -> function
```

Variable kinds are `:LEXICAL`, `:SPECIAL`, `:SYMBOL-MACRO`, `:CONSTANT`, or
`NIL`; function kinds are `:FUNCTION`, `:MACRO`, `:SPECIAL-FORM`, or `NIL`.
The protocol is intended for portable macro analyzers and compiler macros.
`PARSE-MACRO`'s environment is optional; the returned expander accepts
`(form environment)`. `ENCLOSE` closes a lambda expression over the supplied
compile-time environment.

## Metaobject protocol

The `TORCL-MOP` surface is **Specified** and intended to be re-exported from
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
Adding/removing a method also invalidates relevant TorCL inline caches.

## Gray streams

The `TORCL-GRAY-STREAMS` protocol is **Partial**. Built-in stream operations
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
`DISASSEMBLE` are not TorCL-specific, but their TorCL-only options and support
interfaces are listed here.

### Debugger

```lisp
(torcl-debug:break-on-entry function &key when) -> breakpoint
(torcl-debug:break-at file line &key when) -> breakpoint
(torcl-debug:watch variable &key test thread) -> watchpoint
(torcl-debug:unwatch watchpoint-or-variable)
(torcl-debug:encapsulate function-name wrapper &key type)
(torcl-debug:unencapsulate function-name &key type)
(torcl-debug:inspect-object-parts object) -> vector
```

Lexical watchpoints require `(OPTIMIZE (DEBUG 3))`; special-variable
watchpoints work at every debug level. `TRACE` adds TorCL/SBCL-compatible
`:BREAK`, `:CONDITION`, `:REPORT`, and `:METHODS` options. `DISASSEMBLE` adds
`:TIER :T0`, `:T1`, or `:T2`.

The extension specials are `TORCL-EXT:*REPL-PROMPT-FUNCTION*` and
`TORCL-EXT:*ASDF-OUTPUT-TRANSLATIONS*`.

### Profiling and tool counters

The planned high-level profiler entry points are:

```lisp
(torcl-profiler:with-profiling (options*) body*) -> values, report
(torcl-profiler:report-profile report &key stream format sort-by)
```

Sampling, deterministic instrumentation, and allocation profiles are specified.
The supporting counter API used by `TIME` is:

```text
torcl-gc:gc-count                 torcl-gc:total-gc-time-ns
torcl-gc:bytes-allocated         torcl-sys:page-faults
torcl-sys:monotonic-ns           torcl-sys:cpu-user-ns
torcl-sys:cpu-system-ns
```

These support functions do not yet have stable detailed lambda lists beyond
their zero-argument uses in `TIME`.

## Foreign-function interface

The FFI is **Partial**: typed marshalling, foreign-call support, sandbox checks,
and callback structures exist in Rust, but the Lisp surface is not installed in
the bootstrap package initializer yet. The public contract is:

```lisp
(torcl-ffi:load-foreign-library path &key search system) -> foreign-library
(torcl-ffi:close-foreign-library foreign-library) -> nil
(torcl-ffi:foreign-symbol foreign-library name) -> foreign-pointer

(torcl-ffi:define-foreign-function lisp-name
    (foreign-name &key library calling-convention)
    return-type
    argument-types
  &key documentation sandbox-capability)
  -> lisp-name

(torcl-ffi:foreign-funcall foreign-pointer return-type argument-types &rest args)
  -> value

(torcl-ffi:make-callback function return-type argument-types
  &key calling-convention keepalive-name) -> foreign-pointer

(torcl-ffi:free-callback foreign-pointer) -> nil
```

`TORCL-FFI:FFI-ERROR` is the public error condition. Foreign addresses must be
wrapped as validated `TORCL-EXT:FOREIGN-POINTER` objects; user code never
receives a raw address. `RETURN-TYPE` and each element of `ARGUMENT-TYPES` are
foreign type designators from the FFI type table (`:VOID`, signed and unsigned
integer widths, pointer, C string, and callback pointer). Foreign operations
require the sandbox `:FFI` capability; a missing capability signals
`TORCL-EXT:SANDBOX-VIOLATION`.

## Embedded Python

`TORCL-PYTHON`, nicknamed `PY`, calls CPython as a second object system rather
than as a foreign library. It is **available** in a build configured with the
`python` Cargo feature, which needs a target whose loader can open `libpython` —
glibc, not the default static musl. Without the feature every operation signals
an error saying so.

```lisp
(py:import "numpy")                      ; -> the module
(py:resolve "numpy.mean")                ; -> any dotted name: module, attribute, builtin
(py:call "numpy.mean" a)                 ; the callable may be named or in hand
(py:call-method x "reshape" 10 20)
(py:getattr x "shape")                   ; settable: (setf (py:getattr x "n") 5)
(py:type-of x)                           ; the Python TYPE, as an object
(py:typep x "numpy.ndarray")
(py:str x) (py:repr x)
(py:exec "print('hello')")               ; a statement, for its effect
(py:objectp x)                           ; also the type (typep x 'py:object)
(py:start) (py:stop)                     ; an interpreter starts on first use
```

The package does not use `COMMON-LISP`, which is what lets `IMPORT`, `TYPE-OF`,
`TYPEP` and `CALL-METHOD` keep the names Python gives them. Always write them
package-qualified.

### What crosses, and how

One policy, applied in both directions with no exceptions:

| Lisp | Python | |
| --- | --- | --- |
| integer, single/double float | `int`, `float` | by value |
| string, character | `str` | **by copy** |
| `NIL` | `None` | and `None`/`False` come back as `NIL` |
| `T` | `True` | |
| anything else from Python | `PY:OBJECT` | a proxy owning one reference |

Strings are copied because Python's Unicode representation and TorCL's are not
worth sharing: the conversion is O(n) either way, and sharing would buy a
lifetime problem for nothing. Zero copy is for numeric arrays, through the
buffer protocol, which is separate work.

Two consequences are worth knowing rather than discovering:

- **An integer too large for a fixnum stays a `PY:OBJECT`.** Python's integers
  are unbounded; a fixnum holds 61 bits. Such a value is left visible and exact
  rather than truncated or widened to a float — `(py:resolve "sys.maxsize")` is a
  proxy.
- **`True` is not `1`.** Python's `bool` is a subclass of `int`, so booleans are
  classified first; `T` and `NIL` come back, never the integers.

Every operation accepts any Lisp value as its receiver, not only a proxy, so
`(py:str 5)` is `"5"` and `(py:call-method "hello" "upper")` is `"HELLO"`. A value
with no Python equivalent — a list, say — signals an error naming what it was.

### Lifetime

A `PY:OBJECT` owns one Python reference. When the proxy becomes unreachable the
collector queues the release, and it is performed the next time any thread crosses
into Python. The indirection is required: a `Py_DECREF` can run `__del__`, and
arbitrary Python must not run inside a Lisp collection.

This means a released Python object is destroyed slightly later than a `del` in
Python would destroy it — after a collection and the next crossing. Code that
depends on a `__del__` running at a particular moment should call it explicitly.

### Errors

A Python raise is a first-class Lisp condition, `PY:EXCEPTION`, a subtype of
`ERROR`:

```lisp
(handler-case (py:call "numpy.mean" x)
  (py:exception (e)
    (py:exception-kind e)     ; "ValueError"  -- the exception class's name
    (py:exception-text e)     ; str(exception) -- Python's own message
    (py:exception-frames e)   ; ((file line function) ...), outermost first
    (py:exception-object e)   ; the exception itself, as a PY:OBJECT
    (py:backtrace e)))        ; ((:python function file line) ...), innermost first
```

The exception object is carried, not just its message, because its attributes are
usually the useful part -- an `HTTPError`'s status, a `KeyError`'s key. The message
and the frames are rendered when the condition is printed, caught or not:

```text
ValueError: invalid literal for int() with base 10: 'bad'
  Python  inner at <string>:2
  Python  outer at <string>:4
```

`PY:BACKTRACE` returns the Python half of a mixed-language backtrace as data rather
than printing it, so a debugger or log formatter can interleave it with the Lisp
frames however it presents them:

```text
0: Lisp    PROCESS-DATA
1: Lisp    PY:CALL
2: Python  fit at sklearn/base.py:1389
3: Python  asarray at numpy/_core/numeric.py:330
```

It is named `PY:EXCEPTION` rather than `PY:ERROR` for a reason worth knowing if you
define conditions of your own: TorCL's class registry is currently keyed by a
class's *bare* name, so a class named `PY:ERROR` replaces `CL:ERROR` image-wide
(bliss-kliz4). Until that is fixed, do not give a class a bare name that a standard
class already uses.

The inverse direction -- a Lisp condition escaping into Python becoming a Python
exception rather than unwinding through CPython frames -- needs Python-to-Lisp
calls, which do not exist yet.

### Signals and faults

TorCL stays the process's signal authority. The interpreter is started with
`Py_InitializeEx(0)`, so CPython installs no handlers at all and TorCL's remain in
place; from inside Python, `signal.getsignal(signal.SIGINT)` reports `None` — a
handler Python did not install — which is what proves both halves.

One consequence to know: **Ctrl-C cannot interrupt a running Python call.** TorCL's
handler sets a flag that only Lisp code examines, and no Lisp code runs until Python
returns, so an endless Python loop has to be killed (bliss-ziuwp).

A **fault inside CPython kills the process**, as it would in Python itself, rather
than being reported as a Lisp error. That is deliberate and it is not automatic:
TorCL redirects a faulting instruction to a recovery epilogue that unwinds a JIT
frame, and it decides to from the fault address alone — any address below one page
is "a null guard", with nothing looking at where the fault happened. A crossing
therefore disarms that recovery for its duration, so a null dereference in CPython,
a C extension or libffi cannot be rewritten into an unwind of a Lisp frame that is
not on top, which would leave CPython mid-operation with its reference counts wrong.
The crash message still says `null guard SIGSEGV` for any such fault, which is
misleading rather than wrong (bliss-p4mey).

Embedded Python requires the same sandbox permission as the FFI, and a sandboxed
image refuses to start an interpreter: `exec` is arbitrary code execution and
Python's own library reaches the whole filesystem, so a sandbox that allowed this
would not be one.

## Deferred extensible sequences

`SB-SEQUENCE:DEFINE-SEQUENCE-CLASS` is **Deferred** to v2. In v1 the specified
macro signals `TORCL-EXT:NOT-YET-IMPLEMENTED` with a message identifying that
deferral.

## Configuration used by the extension APIs

The most user-relevant environment variables are:

| Variable | Purpose |
| --- | --- |
| `TORCL_WORKERS` | Default scheduler-group carrier count |
| `TORCL_STACK_SIZE` | Per-fiber CL stack size |
| `TORCL_MAX_FIBERS` | Fiber limit; legacy `TORCL_MAX_THREADS` is accepted |
| `TORCL_TIME_SLICE_US` | Fiber cooperative-preemption interval |
| `TORCL_PINNED_BLOCKING_ACTION` | `WARN`, `ERROR`, or `NIL`/`NATIVE` policy |
| `TORCL_T0_T1_THRESHOLD` | T0 to T1 invocation threshold |
| `TORCL_T1_T2_INVOKE_THRESHOLD` | T1 to T2 invocation threshold |
| `TORCL_T1_T2_BACKEDGE_THRESHOLD` | T1 to T2 loop threshold |
| `TORCL_OSR_BACKEDGE_THRESHOLD` | Mid-loop OSR threshold |
| `TORCL_DEOPT_BLACKLIST_THRESHOLD` | Repeated-deoptimization cutoff |
| `TORCL_BAIL_TRACE` | Collect lowerer bail reasons for `BAIL-REPORT` |
| `TORCL_PERF_MAP` | Emit Linux `perf-<pid>.map` JIT symbols |
| `TORCL_PERF_JITDUMP` | Emit Linux perf jitdump metadata |
| `TORCL_INIT_FILE` | REPL initialization file; defaults to `~/.torclrc` |

Environment variables are read during bootstrap or code compilation; changing
the host environment after startup need not update already-installed code.

## Sources and status checks

The contracts summarized here come from these focused specifications:

- `spec/02-runtime-core.md` — execution, callbacks, startup, shutdown;
- `spec/04-02-macroexpand.md` — `TORCL-CLTL2`;
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
rg '"TORCL-EXT:[A-Z-]+" =>' crates/torcl/src/cli.rs
```

An undefined function for an entry marked **Specified** is an implementation
status result, not a package-qualification problem.
