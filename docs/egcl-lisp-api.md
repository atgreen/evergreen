# EGCL-specific Lisp API

> Historical extension inventory: implementation-status labels below reflect
> an earlier development stage. The [new manual](manual/index.md) contains
> current task guides and a focused reference; check source/tests before
> relying on status labels in this inventory.

This manual is the user-facing reference for Lisp interfaces that are specific
to EGCL. It covers the callable bootstrap API and the extension packages in
the staged EGCL specification. ANSI Common Lisp is outside its scope.

The implementation is still staged. An API appearing in the specification does
not necessarily mean that the bootstrap evaluator can call it today. Every
section therefore carries one of these labels:

| Label | Meaning |
| --- | --- |
| **Available** | Callable through the current `egcl` evaluator. |
| **Partial** | Some underlying behavior exists, but the documented public contract is incomplete. |
| **Specified** | The public contract is defined by the spec, but no Lisp entry point is currently wired. |
| **Deferred** | Deliberately outside the v1 implementation target. |

The status in this document reflects Stage 5 (`spec/stages.json`) on
2026-08-20. The technical specification remains normative for future behavior;
the availability labels below have not been comprehensively re-audited against
the current bootstrap. The atomics, memory-barrier, and Gray-stream sections
below have been refreshed for the current implementation.

## Package map

| Package | Purpose | Current status | Compatibility |
| --- | --- | --- | --- |
| `EGCL-EXT` | General runtime, compiler, process, package, image, atomic, and memory-barrier extensions | Package and selected functions available | Use the EGCL public names |
| `EGCL` | Bootstrap implementation entry points | Internal | Not a public nickname for `EGCL-EXT` |
| `EGCL-THREAD` | OS-backed native threads and synchronization objects | Native thread and mutex operations available; see the manual for the implemented subset | Use the EGCL public names |
| `EGCL-THREADS` | Nickname for `EGCL-THREAD` | Available | Prefer the canonical name |
| `EGCL-FIBER` | Lightweight M:N fibers and scheduler groups | Specified | Deliberately distinct from native threads |
| `EGCL-MOP` | Metaobject protocol | Specified public package; not installed by the bootstrap | Planned EGCL namespace |
| `EGCL-CLTL2` | Compile-time environment inspection | Package available; see the manual for the implemented subset | Use the EGCL public names |
| `EGCL-GRAY-STREAMS` | Gray stream classes and generic functions | Public classes and elemental protocol available; bulk protocol incomplete | Import from this package |
| `EGCL-FFI` | Typed foreign calls and callbacks | Partial Rust runtime; Lisp package not yet installed | CFFI/SB-ALIEN migration surface |
| `EGCL-PYTHON` (nickname `PY`) | Embedded CPython as a second object system | Available in a build with the `python` feature | EGCL-specific; deliberately not CFFI-shaped |
| `EGCL-DEBUG` | Breakpoints, watchpoints, and debugger plumbing | Partial Rust library; Lisp API specified | EGCL-specific |
| `EGCL-PROFILER` | Runtime profiling controls and reports | Specified; tier counters have separate available accessors | EGCL-specific |
| `EGCL-GC` | GC counters used by developer tools | Specified | EGCL-specific |
| `EGCL-SYS` | OS timing and page-fault counters | Specified | EGCL-specific |

`COMMON-LISP-USER` uses `COMMON-LISP` and `EGCL-EXT`, but portable code
should still qualify extension names. The canonical public names are
`EGCL-EXT`, `EGCL-THREAD`, `EGCL-FIBER`, and `EGCL-FFI`. `EGCL-THREADS` is a
nickname for `EGCL-THREAD`; `EGCL::%...` names are internal entry points, not
public extensions. EGCL does not provide `SB-*` package aliases or re-exports.
Portable library ports should select EGCL's packages explicitly.

`EGCL-INTERNALS`/`BI` is an unstable implementation package and is
intentionally not a supported API.

## Currently available API

### Stream I/O timeouts

`EGCL-EXT:IO-TIMEOUT` is a subtype of `STREAM-ERROR` and
`SIMPLE-CONDITION`. Native socket byte and byte-sequence reads signal it when
their configured receive timeout expires. `STREAM-ERROR-STREAM` returns the
affected stream; the simple-condition accessors provide the diagnostic text.
Other stream failures are not classified as timeouts. The socket remains open
after a timeout and can be read again or closed by the caller.

This is distinct from `EGCL-EXT:TIMEOUT-CONDITION`, which represents a sandbox
CPU deadline. It does not impose a total deadline on a multi-operation protocol.

### Environment and working directory

#### `egcl-ext:getenv`

```lisp
(egcl-ext:getenv name) -> string-or-nil
```

Returns the process environment variable named by `name`, or `NIL` when it is
unset or cannot be represented by the host environment API.

```lisp
(or (egcl-ext:getenv "HOME") "no home directory")
```

The current bootstrap does not apply the specified sandbox `:ENV` capability
check to this function yet.

#### `egcl-ext:getcwd`

```lisp
(egcl-ext:getcwd) -> directory-namestring-or-nil
```

Returns the current working directory as a string. The bootstrap appends a
trailing slash, which lets ASDF treat the result as a directory namestring.
Returns `NIL` if the host query fails.

```lisp
(format t "Working in ~A~%" (egcl-ext:getcwd))
```

### Subprocesses

#### `egcl-ext:run-program`

```lisp
(egcl-ext:run-program command) -> exit-code, stdout, stderr
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
    (egcl-ext:run-program '("printf" "%s" "hello"))
  (list code output error-output))
;; => (0 "hello" "")
```

The string form intentionally permits shell syntax:

```lisp
(egcl-ext:run-program "printf 'one\\ntwo\\n' | wc -l")
```

### Command-line arguments

#### `egcl-ext:raw-command-line-arguments`

```lisp
(egcl-ext:raw-command-line-arguments) -> list-of-strings
```

Returns the host process argument vector, including the EGCL executable as the
first element. This is the low-level interface used by ASDF/UIOP.

#### `*command-line-args*`

```lisp
*command-line-args* -> list-of-strings
```

When running a script as

```sh
egcl script.lisp -- first second
```

the variable contains `("first" "second")`. It is `NIL` when no arguments
follow `--`. The current bootstrap name is unqualified
`*COMMAND-LINE-ARGS*`; the runtime spec's future spelling
`EGCL-EXT:*COMMAND-LINE-ARGUMENTS*` (also written
`EGCL:*COMMAND-LINE-ARGUMENTS*` in the migration guide) is not wired yet.

### Tiering and compiler diagnostics

These interfaces are diagnostic. Their results are observations, not controls,
and application correctness must not depend on when a function changes tier.
A function designator may be a function-name symbol or a function object.

| Function | Result |
| --- | --- |
| `(egcl-ext:function-tier function)` | `0`, `1`, or `2` for T0 bytecode/interpreter, T1 baseline native, or T2 optimized native; `NIL` if the designator is not a tiered function |
| `(egcl-ext:function-invoke-count function)` | Monotonic invocation count, or `NIL` for an unrecognized designator |
| `(egcl-ext:function-back-edge-count function)` | Aggregate loop back-edge count, or `NIL` for an unrecognized designator |
| `(egcl-ext:deopt-count)` | Process-wide number of speculative native deoptimizations |
| `(egcl-ext:bail-report)` | Prints bytecode-lowering bail reasons and counts, then returns the number of distinct reasons |

`FUNCTION-TIER` polls for completed background compilation before reading the
tier. `BAIL-REPORT` is useful when the process was started with
`EGCL_BAIL_TRACE=1`; without collected reasons it prints nothing and returns
zero.

```lisp
(defun sum-to (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum i)))))

(dotimes (i 20) (sum-to 1000))
(list (egcl-ext:function-tier 'sum-to)
      (egcl-ext:function-invoke-count 'sum-to)
      (egcl-ext:function-back-edge-count 'sum-to)
      (egcl-ext:deopt-count))
```

### Bootstrap image writer

#### `save-image` — **Partial**

```lisp
(save-image path) -> t
```

Writes restorable Lisp definitions for interpreted functions to `path`. The
current implementation is a minimal source-form snapshot, not the relocatable
heap image specified for `EGCL-EXT:SAVE-IMAGE`, and it does not exit the
process. A write failure signals `FILE-ERROR`.

Do not use it as a persistence boundary for arbitrary objects, compiled code,
dynamic state, or deoptimization data.

## Specified general extensions

Unless individually marked otherwise, the APIs below are **Specified**, not
yet callable through the bootstrap evaluator.

### Package-local nicknames

These CDR-10-style functions are exported from `EGCL-EXT`:

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
(egcl-ext:defconduit name
  (:use package*)
  (:extends source-package*))

(egcl-ext:refresh-conduit conduit-package)
```

`DEFCONDUIT` defines a package that imports and re-exports the external symbols
of every `:EXTENDS` package. `REFRESH-CONDUIT` synchronizes it after a source
package changes.

### Global definitions

```lisp
(egcl-ext:defglobal name value &optional documentation)
(egcl-ext:define-load-time-global name value &optional documentation)
```

`DEFGLOBAL` creates one process-global atomic value rather than a per-thread
special binding. `DEFINE-LOAD-TIME-GLOBAL` evaluates its initializer once and
does not re-evaluate it when the defining file is reloaded.

### Weak references and finalizers

```lisp
(egcl-ext:make-weak-pointer object) -> weak-pointer
(egcl-ext:weak-pointer-value weak-pointer) -> object, alive-p
(egcl-ext:finalize object function &key dont-save) -> unspecified
(egcl-ext:cancel-finalization object) -> unspecified
```

The collector may clear a weak pointer once its referent is otherwise
unreachable. Finalizers run on a dedicated thread, outside GC, and cancellation
is idempotent.

### Extended hash tables

```lisp
(make-hash-table &key test size rehash-size rehash-threshold
                      hash-function synchronized weakness)
(egcl-ext:with-locked-hash-table (table) body*)
(egcl-ext:hash-table-synchronized-p table) -> boolean
(egcl-ext:hash-table-weakness table) -> nil-or-keyword
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
| `(egcl-ext:save-image path &key executable compression purify)` | Historical signature; see the [current image reference](manual/user/reference/images.md) for implemented options |
| `(egcl-ext:save-lisp-and-die path &rest options)` | Save and exit; see the [current image reference](manual/user/reference/images.md) |
| `(egcl-ext:exit &key (code 0) abort timeout)` | Orderly process exit; `:ABORT` skips cleanup and `:TIMEOUT` bounds joins |
| `(egcl-ext:quit &key (unix-status 0) recklessly-p)` | SBCL-compatible spelling of `EXIT` |
| `(egcl-ext:with-timeout (seconds timeout-form*) body*)` | Run `body`; on expiry evaluate timeout forms and signal `TIMEOUT` |
| `(egcl-ext:native-namestring pathname)` | Return the host-native pathname representation |
| `(egcl-ext:definition-source name)` | Return source location as `(:FILE ... :LINE ... :FORM ...)` |

### Extension metadata and conditions

`EGCL-EXT:*EGCL-EXT-VERSION*` is specified as a `(major minor)` list.
The extension condition/type surface comprises:

- `EGCL-EXT:IMAGE-ERROR`, `EGCL-EXT:CORRUPT-IMAGE-ERROR`,
  `EGCL-EXT:SIGNAL-ERROR`, and `EGCL-EXT:INTERNAL-ERROR`;
- `EGCL-EXT:STACK-OVERFLOW-ERROR`, `EGCL-EXT:TIMEOUT`, and
  `EGCL-EXT:TIMEOUT-CONDITION`;
- `EGCL-EXT:NOT-YET-IMPLEMENTED`;
- `EGCL-EXT:FOREIGN-POINTER`, an opaque validated foreign-address wrapper.

`EGCL-EXT:INTERNAL-ERROR` denotes an EGCL bug and is not a recoverable
application condition. Use the documented public `EGCL-EXT` names rather
than the older inventory's proposed `EGCL` aliases.

The public tuning specials are `EGCL-EXT:*DEFAULT-EXTERNAL-FORMAT*` (default
`:UTF-8`), `EGCL-EXT:*HASH-TABLE-DEFAULT-SIZE*` (16),
`EGCL-EXT:*HASH-TABLE-SYNCHRONIZED-DEFAULT*` (`NIL`),
`EGCL-EXT:*SORT-PARALLEL-THRESHOLD*` (10000), and
`EGCL-EXT:*READER-SOURCE-TRACKING*` (`T`). They are **Specified** and are not
installed by the bootstrap today.

### Sandboxing

```lisp
(egcl-ext:with-sandbox
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
does not yet expose `EGCL-THREAD` Lisp objects or functions.

### Native thread lifecycle

```lisp
(egcl-thread:make-thread function &key name arguments ephemeral) -> thread
(egcl-thread:join-thread thread &key default timeout) -> result, status
(egcl-thread:thread-alive-p thread) -> boolean
(egcl-thread:destroy-thread thread) -> unspecified
(egcl-thread:interrupt-thread thread function) -> unspecified
(egcl-thread:abort-thread &optional condition) -> does-not-return
(egcl-thread:all-threads) -> list-of-threads
egcl-thread:*current-thread*
```

`MAKE-THREAD` creates one dedicated OS thread. It never creates a fiber.
`ARGUMENTS` is applied to `FUNCTION`. `JOIN-THREAD` status is `:OK`,
`:TIMEOUT`, or `:ABORTED`; multiple joiners are permitted. Interrupt and
destroy requests take effect at safepoints.

### Non-recursive mutexes

```lisp
(egcl-thread:make-mutex &key name) -> mutex
(egcl-thread:grab-mutex mutex &key (waitp t) timeout) -> boolean
(egcl-thread:release-mutex mutex &key (if-not-owner :error))
(egcl-thread:with-mutex (mutex &key (waitp t) timeout) body*)
```

`WAITP NIL` performs a try-lock. `IF-NOT-OWNER` accepts `:WARN`, `:FORCE`, or
`:ERROR`. `WITH-MUTEX` releases with `UNWIND-PROTECT` semantics.

### Condition variables

```lisp
(egcl-thread:make-condition-variable &key name) -> condition-variable
(egcl-thread:condition-wait condition-variable mutex &key timeout) -> boolean
(egcl-thread:condition-notify condition-variable &optional (count 1)) -> count
(egcl-thread:condition-broadcast condition-variable) -> count
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
(egcl-thread:make-rw-lock &key name) -> rw-lock
(egcl-thread:grab-rw-lock-read rw-lock &key (waitp t) timeout) -> boolean
(egcl-thread:grab-rw-lock-write rw-lock &key (waitp t) timeout) -> boolean
(egcl-thread:release-rw-lock-read rw-lock &key (if-not-owner :error)) -> nil
(egcl-thread:release-rw-lock-write rw-lock &key (if-not-owner :error)) -> nil
(egcl-thread:with-rw-lock-read (rw-lock &key (waitp t) timeout) body*) -> values
(egcl-thread:with-rw-lock-write (rw-lock &key (waitp t) timeout) body*) -> values
```

RW-locks use writer preference to prevent writer starvation. `WAITP NIL`
performs a try-lock. `TIMEOUT` has the same meaning as for condition variables.
`IF-NOT-OWNER` accepts `:ERROR`, `:WARN`, or `:IGNORE`; write-lock ownership is
tracked per thread, while read-lock release only verifies that the current
thread holds at least one read acquisition.

### Semaphores

```lisp
(egcl-thread:make-semaphore &key name (count 0)) -> semaphore
(egcl-thread:signal-semaphore semaphore &optional (count 1))
(egcl-thread:wait-on-semaphore semaphore &key timeout notification) -> boolean
(egcl-thread:semaphore-count semaphore) -> non-negative-integer
(egcl-thread:try-semaphore semaphore &optional (count 1)) -> boolean
```

### Recursive locks

```lisp
(egcl-thread:make-lock &key name) -> recursive-lock
(egcl-thread:grab-lock lock &key (waitp t) timeout) -> boolean
(egcl-thread:release-lock lock &key (if-not-owner :error))
(egcl-thread:with-lock (lock &key (waitp t) timeout) body*)
```

Every successful recursive acquisition must be matched by a release.

### Atomics and barriers — **Available**

```lisp
(egcl-ext:cas place old new) -> previous-value
(egcl-ext:atomic-incf place &optional (delta 1)) -> previous-value
(egcl-ext:atomic-decf place &optional (delta 1)) -> previous-value
(egcl-ext:memory-barrier &optional (kind :full)) -> nil
(egcl-ext:load-barrier) -> nil
(egcl-ext:store-barrier) -> nil
```

`CAS`, `ATOMIC-INCF`, and `ATOMIC-DECF` are macros. `CAS` compares with `EQ`,
stores only on a match, and returns the actual previous value on success or
failure. Supported places are special-variable symbols, structure-slot
accessors, `SVREF`, `CAR`, `CDR`, `SYMBOL-PLIST`, `SYMBOL-VALUE`, and
instance-allocated `SLOT-VALUE` slots. Arbitrary SETF places and ordinary
lexical variables are not supported.

`ATOMIC-INCF` and `ATOMIC-DECF` support special-variable symbols,
structure-slot accessors, and `SVREF`. Both the current value and delta must
be fixnums (`TYPE-ERROR` otherwise); overflow signals `ARITHMETIC-ERROR`
without storing a replacement. They return the value before the update.
See the [user manual](manual/user/reference/extensions.md#atomic-updates) for
the supported-place table, evaluation rules, memory ordering, and examples.

The three barrier functions return `NIL`. Kinds are `:READ` (acquire),
`:WRITE` (release), `:FULL` (sequentially consistent), and
`:DATA-DEPENDENCY` (implemented as acquire). Unknown kinds signal
`PROGRAM-ERROR`. `LOAD-BARRIER` and `STORE-BARRIER` select `:READ` and
`:WRITE`. These fences are not rendezvous barriers and do not make ordinary
accesses atomic. See [memory ordering](manual/user/reference/extensions.md#memory-ordering).
On x86-64, literal kinds and the default/load/store wrappers lower to dedicated
T0 bytecode and native T1/T2 fences, also preserved in compiled files. Dynamic
kinds retain the checked runtime helper; other architectures keep the runtime
path. The API guarantees ordering, not a particular instruction sequence.

`WITH-ATOMIC` is not part of the implemented public API; do not rely on the
older planned scheduler-preemption interface.

### N-party barriers

```lisp
(egcl-thread:make-barrier count &key name) -> barrier
(egcl-thread:barrier-wait barrier &key timeout) -> index, status
(egcl-thread:barrier-count barrier) -> positive-integer
(egcl-thread:barrier-waiting-count barrier) -> non-negative-integer
(egcl-thread:reset-barrier barrier) -> nil
```

`COUNT` is the number of arrivals required to release a generation. `WAIT`
returns a zero-based arrival index and status `:OK`, or `NIL, :TIMEOUT` if the
timeout expires. `RESET-BARRIER` breaks the current generation and starts a new
one; waiters released by reset receive `NIL, :RESET`.

The fully qualified type names are `EGCL-THREAD:MUTEX`,
`EGCL-THREAD:RW-LOCK`, `EGCL-THREAD:CONDITION-VARIABLE`,
`EGCL-THREAD:SEMAPHORE`, and `EGCL-THREAD:BARRIER`. Lifecycle and
synchronization errors signal `EGCL-THREAD:THREAD-ERROR`. Older migration text
uses `EGCL-THREADS:MAKE-THREAD` and `EGCL-THREADS:MAKE-LOCK`; the current
contract keeps `EGCL-THREADS` only as a deprecated nickname for
`EGCL-THREAD`.

## Fibers

The `EGCL-FIBER` lifecycle, waiting, pinning, and observation APIs are installed
by the standard bootstrap on x86-64 Unix and Windows. See the
[fiber manual](manual/fibers.md) for result, timeout, stack-size, and error semantics.

Fibers are lightweight managed executions multiplexed over a scheduler group's
OS carrier threads. A fiber is not a thread, and `EGCL-THREAD` functions do
not accept fiber objects.

### Lifecycle and scheduler groups

```lisp
(egcl-fiber:make-fiber function
  &key name arguments stack-size initial-bindings) -> fiber

(egcl-fiber:run-fibers fibers &key carrier-count idle-hook) -> results
(egcl-fiber:start-fibers fibers &key carrier-count idle-hook) -> group
(egcl-fiber:submit-fiber group fiber) -> unspecified
(egcl-fiber:finish-fibers group) -> results
(egcl-fiber:fiber-group-done-p group) -> boolean
(egcl-fiber:scheduler-group-carriers group) -> list-of-threads
```

`MAKE-FIBER` leaves the object in `:CREATED`; it does not schedule it.
First submission permanently assigns a fiber to one group. `START-FIBERS`
starts the carrier threads, while `RUN-FIBERS` is the blocking convenience
equivalent of start followed by finish. `FINISH-FIBERS` closes submission,
waits for every submitted fiber, returns results in submission order, joins the
carriers, and is idempotent.

`CARRIER-COUNT` defaults to the available processor count and must be at least
one. Carriers are ordinary visible `EGCL-THREAD` objects.

```lisp
;; Intended API; not runnable in the current bootstrap.
(let* ((fibers (list (egcl-fiber:make-fiber (lambda () 20))
                     (egcl-fiber:make-fiber (lambda () 22))))
       (group (egcl-fiber:start-fibers fibers :carrier-count 2)))
  (egcl-fiber:finish-fibers group))
;; => (20 22)
```

### Yielding, sleeping, parking, and joining

```lisp
(egcl-fiber:fiber-yield)
(egcl-fiber:fiber-sleep seconds)
(egcl-fiber:fiber-park predicate &key timeout) -> predicate-won-p
(egcl-fiber:fiber-join fiber &key timeout) -> entry-values
```

These operations park an unpinned fiber without blocking its carrier. Timer
and wake generations prevent an expired earlier wait from waking a later one.

Established TCP stream reads, writes/flushes, and `%SOCKET-WAIT-FOR-INPUT`
also park on socket readiness. They use shared epoll/kqueue services on Unix
and one shared Winsock poller on Windows, preserving synchronous Lisp stream
calls and receive timeouts. On Unix, listener accept and readiness waits also
park unpinned fibers. Connect, DNS, Windows accept, regular file I/O, and terminal
I/O are not yet cooperative. See [socket I/O limitations](manual/fibers.md#socket-io).

The native server primitives used by library adapters are:

```lisp
(egcl::%socket-listen &optional (host "127.0.0.1") (port 0) (backlog 5) reuse-address)
(egcl::%socket-local-port listener) -> port-or-nil
(egcl::%socket-listener-ready-p listener timeout-ms) -> boolean
(egcl::%socket-accept listener) -> bidirectional-octet-stream
(egcl::%socket-close listener)
```

Listener IDs are process-wide: another native thread or fiber can query, accept,
or close a listener. Port zero selects an available local port. Ports must be in
0–65535 and backlog in 0–2147483647; the OS may cap the requested queue limit.
Explicit `reuse-address` controls `SO_REUSEADDR` before binding. Omitting it
preserves the previous platform default (true on Unix, false on Windows).
These internal interfaces are intended for adapters, not as a portable sockets API.

On Unix, negative readiness timeouts wait indefinitely, zero polls, and positive
values bound the wait in milliseconds. Closing a listener wakes pending Unix
accept/readiness waits with an error; it does not close already accepted streams.
Closed IDs are not reused, and `%SOCKET-LOCAL-PORT` returns `NIL` for a closed ID.
Windows listener readiness and cancellation do not yet provide these guarantees.

### Pinning

```lisp
(egcl-fiber:fiber-pin &optional fiber)
(egcl-fiber:fiber-unpin &optional fiber)
(egcl-fiber:fiber-can-yield-p &optional fiber) -> boolean
(egcl-fiber:with-fiber-pinned ((&optional fiber)) body*)
egcl-fiber:*pinned-blocking-action*
```

Pinning is a balanced counter and prevents carrier migration. Explicit yield
while pinned signals `PROGRAM-ERROR`. The blocking policy is `:WARN` (default),
`:ERROR`, or `NIL` for silent OS-thread blocking. `WITH-FIBER-PINNED` balances
the counter on all exits.

### Introspection

| Function | Result |
| --- | --- |
| `(egcl-fiber:current-fiber)` | Current fiber or `NIL` on a plain native thread |
| `(egcl-fiber:list-all-fibers)` | Snapshot of non-reclaimed fibers |
| `(egcl-fiber:fiber-state fiber)` | `:CREATED`, `:RUNNABLE`, `:RUNNING`, `:SUSPENDED`, or `:DEAD` |
| `(egcl-fiber:fiber-name fiber)` | Name string or `NIL` |
| `(egcl-fiber:fiber-result fiber)` | Result values as a list after death |
| `(egcl-fiber:fiber-error-p fiber)` | Whether an unhandled condition caused death |
| `(egcl-fiber:fiber-alive-p fiber)` | Whether state is not `:DEAD` |
| `(egcl-fiber:fiber-carrier-thread fiber)` | Current or last carrier thread, or `NIL` |
| `(egcl-fiber:print-fiber-backtrace fiber &key stream count)` | Print its saved stack |

A running fiber must first reach a safepoint for stack inspection; otherwise
`FIBER-STILL-RUNNING` is signaled. Creation/submission errors signal
`FIBER-ERROR` with reasons such as `:LIMIT-EXCEEDED`, `:ALREADY-SUBMITTED`,
`:CLOSED-GROUP`, or `:OOM`.

## Timers

The `EGCL-EXT` timer API is **Specified**:

```lisp
(egcl-ext:make-timer function &key name thread) -> timer
(egcl-ext:schedule-timer timer time
  &key repeat-interval absolute-p catch-up) -> timer
(egcl-ext:unschedule-timer timer)
(egcl-ext:timer-scheduled-p timer &key delta) -> boolean
(egcl-ext:list-all-timers) -> list-of-timers
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
(egcl-ext:reset-deopt-log function)
(egcl-ext:set-runtime-option option value)
(egcl-ext:function-ic-stats function) -> property-list
(egcl-ext:dump-megamorphic-ics &key (stream *trace-output*))
```

`RESET-DEOPT-LOG` clears per-function deoptimization history and blacklisting.
`SET-RUNTIME-OPTION` updates supported compiler/runtime thresholds.
`FUNCTION-IC-STATS` reports inline-cache site state, hits, misses, and miss rate;
`DUMP-MEGAMORPHIC-ICS` reports unstable sites.

The `EGCL-PROFILER` threshold specials are:

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
(egcl-ext:function-lambda-expression function)
  -> lambda-expression, closure-p, name
(egcl-ext:function-type function) -> type-or-nil
(egcl-ext:function-arglist function) -> lambda-list-or-:not-available
(egcl-ext:who-calls function-name &key package) -> locations
(egcl-ext:who-binds variable-name) -> locations
(egcl-ext:who-references variable-name) -> locations
(egcl-ext:who-sets variable-name) -> locations
(egcl-ext:who-macroexpands macro-name) -> locations
```

Source and cross-reference data require compilation with debug quality at
least one.

### Compiler hints — **Specified**

```lisp
(egcl-ext:truly-the type form)
(declaim (egcl-ext:always-bound *variable*))
(declaim (egcl-ext:freeze-type type-name))
(declaim (egcl-ext:muffle-conditions condition-type))
(declaim (egcl-ext:unmuffle-conditions condition-type))
```

`TRULY-THE` suppresses all checks; a false assertion has undefined behavior.
`ALWAYS-BOUND` removes unbound checks. `FREEZE-TYPE` permits stamp-based type
and slot optimizations and makes later redefinition exceptional. The muffling
declarations control compiler warnings/notes. Use the `EGCL-EXT` names; no
aliases in another implementation's extension package are provided.

### CLtL2 environment protocol — **Specified**

```lisp
(egcl-cltl2:variable-information symbol &optional environment)
  -> kind, local-p, declarations
(egcl-cltl2:function-information name &optional environment)
  -> kind, local-p, declarations
(egcl-cltl2:declaration-information name &optional environment) -> value
(egcl-cltl2:augment-environment environment
  &key variable symbol-macro function macro declare) -> new-environment
(egcl-cltl2:parse-macro name lambda-list body environment) -> expander
(egcl-cltl2:enclose lambda-expression &optional environment) -> function
```

Variable kinds are `:LEXICAL`, `:SPECIAL`, `:SYMBOL-MACRO`, `:CONSTANT`, or
`NIL`; function kinds are `:FUNCTION`, `:MACRO`, `:SPECIAL-FORM`, or `NIL`.
The protocol is intended for portable macro analyzers and compiler macros.
`PARSE-MACRO`'s environment is optional; the returned expander accepts
`(form environment)`. `ENCLOSE` closes a lambda expression over the supplied
compile-time environment.

## Metaobject protocol

The `EGCL-MOP` surface below is **Specified**. The bootstrap has CLOS
internals but does not install this public package. When exposed, the public
protocol belongs in EGCL's own package, not an alias in another
implementation's package. This inventory is not an availability guarantee.

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
Adding/removing a method also invalidates relevant EGCL inline caches.

## Gray streams

The `EGCL-GRAY-STREAMS` protocol is **Partial**. The bootstrap installs the
public package, the class hierarchy below, and the elemental input/output
generics. Import protocol symbols from `EGCL-GRAY-STREAMS`; query and lifecycle
functions such as `STREAM-ELEMENT-TYPE` and `CLOSE` retain their Common Lisp
names.

### Classes

`FUNDAMENTAL-STREAM` is the root. The mixin hierarchy adds
`FUNDAMENTAL-INPUT-STREAM`, `FUNDAMENTAL-OUTPUT-STREAM`,
`FUNDAMENTAL-CHARACTER-STREAM`, `FUNDAMENTAL-BINARY-STREAM`, and their four
character/binary input/output combinations.

### Available elemental generic functions

| Input | Output | Query/lifecycle |
| --- | --- | --- |
| `stream-read-char` | `stream-write-char` | `stream-element-type` |
| `stream-unread-char` | `stream-line-column` | `open-stream-p` |
| `stream-read-char-no-hang` | `stream-start-line-p` | `close` |
| `stream-peek-char` | `stream-write-string` | `input-stream-p` |
| `stream-listen` | `stream-terpri` | `output-stream-p` |
| `stream-read-line` | `stream-fresh-line` |  |
| `stream-clear-input` | `stream-finish-output` |  |
| `stream-read-byte` | `stream-force-output` |  |
|  | `stream-clear-output` |  |
|  | `stream-write-byte` |  |

Standard stream operations use the elemental methods for fundamental-stream
subclasses. `STREAMP` and `(TYPEP object 'STREAM)` recognize those instances;
direction predicates follow the input/output classes. The default lifecycle
starts open and `CLOSE` marks the stream closed; wrapper classes may specialize
these methods to delegate to an underlying stream.

Elemental readers return `:EOF` at end of input. `READ-BYTE` translates that
marker according to its `eof-error-p` and `eof-value` arguments.
`READ-SEQUENCE` uses character or byte methods according to
`STREAM-ELEMENT-TYPE`, honors `:START`/`:END`, and returns the first unread
index. Its binary-aware fallback is internal, not a new public bulk generic.

`STREAM-ADVANCE-TO-COLUMN` is exported but has no installed function.
Portable `STREAM-READ-SEQUENCE` / `STREAM-WRITE-SEQUENCE` method adapters and
the planned position, external-format, and codec extension protocols are not
covered by this implemented subset. Do not infer their availability from the
older inventory. See the [Drakma scenario](../tests/drakma/README.md) for the
pinned plain-HTTP regression using Chunga and Flexi Streams.

## Debugger and profiling API

These developer-tool interfaces are **Specified** unless noted. The ordinary
ANSI operators `TRACE`, `UNTRACE`, `DESCRIBE`, `INSPECT`, `ROOM`, `TIME`, and
`DISASSEMBLE` are not EGCL-specific, but their EGCL-only options and support
interfaces are listed here.

### Debugger

```lisp
(egcl-debug:break-on-entry function &key when) -> breakpoint
(egcl-debug:break-at file line &key when) -> breakpoint
(egcl-debug:watch variable &key test thread) -> watchpoint
(egcl-debug:unwatch watchpoint-or-variable)
(egcl-debug:encapsulate function-name wrapper &key type)
(egcl-debug:unencapsulate function-name &key type)
(egcl-debug:inspect-object-parts object) -> vector
```

Lexical watchpoints require `(OPTIMIZE (DEBUG 3))`; special-variable
watchpoints work at every debug level. `TRACE` adds EGCL/SBCL-compatible
`:BREAK`, `:CONDITION`, `:REPORT`, and `:METHODS` options. `DISASSEMBLE` adds
`:TIER :T0`, `:T1`, or `:T2`.

The extension specials are `EGCL-EXT:*REPL-PROMPT-FUNCTION*` and
`EGCL-EXT:*ASDF-OUTPUT-TRANSLATIONS*`.

### Profiling and tool counters

The planned high-level profiler entry points are:

```lisp
(egcl-profiler:with-profiling (options*) body*) -> values, report
(egcl-profiler:report-profile report &key stream format sort-by)
```

Sampling, deterministic instrumentation, and allocation profiles are specified.
The supporting counter API used by `TIME` is:

```text
egcl-gc:gc-count                 egcl-gc:total-gc-time-ns
egcl-gc:bytes-allocated         egcl-sys:page-faults
egcl-sys:monotonic-ns           egcl-sys:cpu-user-ns
egcl-sys:cpu-system-ns
```

These support functions do not yet have stable detailed lambda lists beyond
their zero-argument uses in `TIME`.

## Foreign-function interface

The FFI is **Partial**: typed marshalling, foreign-call support, sandbox checks,
and callback structures exist in Rust, but the Lisp surface is not installed in
the bootstrap package initializer yet. The public contract is:

```lisp
(egcl-ffi:load-foreign-library path &key search system) -> foreign-library
(egcl-ffi:close-foreign-library foreign-library) -> nil
(egcl-ffi:foreign-symbol foreign-library name) -> foreign-pointer

(egcl-ffi:define-foreign-function lisp-name
    (foreign-name &key library calling-convention)
    return-type
    argument-types
  &key documentation sandbox-capability)
  -> lisp-name

(egcl-ffi:foreign-funcall foreign-pointer return-type argument-types &rest args)
  -> value

(egcl-ffi:make-callback function return-type argument-types
  &key calling-convention keepalive-name) -> foreign-pointer

(egcl-ffi:free-callback foreign-pointer) -> nil
```

`EGCL-FFI:FFI-ERROR` is the public error condition. Foreign addresses must be
wrapped as validated `EGCL-EXT:FOREIGN-POINTER` objects; user code never
receives a raw address. `RETURN-TYPE` and each element of `ARGUMENT-TYPES` are
foreign type designators from the FFI type table (`:VOID`, signed and unsigned
integer widths, pointer, C string, and callback pointer). Foreign operations
require the sandbox `:FFI` capability; a missing capability signals
`EGCL-EXT:SANDBOX-VIOLATION`.

## Embedded Python

`EGCL-PYTHON`, nicknamed `PY`, calls CPython as a second object system rather
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

Strings are copied because Python's Unicode representation and EGCL's are not
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

### Calling Lisp from Python

`PY:EXPORT` binds a Python callable, in `__main__`, that calls back into Lisp:

```lisp
(py:export "add" (lambda (a b) (+ a b)))
(py:exec "print(add(2, 3))")            ; => 5
```

The callable is an ordinary Python value, so it can be passed around and called from
inside Python code, not just by name:

```lisp
(py:exec "def twice(f, x): return f(f(x))")
(py:export "inc" (lambda (n) (+ n 1)))
(py:call "__main__.twice" (py:resolve "__main__.inc") 5)   ; => 7
```

Arguments and results cross by the same value policy as everything else. A Lisp error
becomes a Python exception (a `RuntimeError` carrying the Lisp message) rather than
unwinding through CPython frames, which would leave their reference counts wrong.

**The Lisp function is reached through a stable handle, and is retained for the process
lifetime.** A Python callable can be stored anywhere on the Python side, so there is no
moment at which releasing it would be safe without reference-counting that side; the
handle also means the collector can move the function (and anything its closure
captures) without invalidating the callable.

Unlike the FFI's callbacks, this needs no generated trampoline: every export shares one
static C entry point and carries its Lisp function in the handle rather than in code.
So the only architecture-specific dependency is the foreign-to-managed thread
transition, which x86-64, AArch64 and ppc64le all have. **Verified on x86-64 only** —
the other two are expected to work by construction but have not been run.

Not yet available: exporting a whole Lisp package as a Python module
(`import lisp.statistics`), and `input()` reading `*standard-input*`.

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

It is named `PY:EXCEPTION` rather than `PY:ERROR` because that is the better word for
what it is. Originally the name was forced: the condition and class registries were
keyed by a class's *bare* name, so `PY:ERROR` replaced `CL:ERROR` image-wide
(bliss-kliz4, since fixed — a class or condition in another package now keeps its own
identity).

The inverse direction -- a Lisp condition escaping into Python becoming a Python
exception rather than unwinding through CPython frames -- needs Python-to-Lisp
calls, which do not exist yet.

### Streams

Python's `print` reaches `*standard-output*`, and `sys.stderr` reaches
`*error-output*`, so the two runtimes' output interleaves in program order and a
`WITH-OUTPUT-TO-STRING` or a rebinding captures both:

```lisp
(with-output-to-string (s)
  (let ((*standard-output* s))
    (princ "lisp ")
    (py:exec "print('python', end='')")))
;; => "lisp python"
```

`sys.stdout` and `sys.stderr` are redirected into in-memory buffers whose contents
are written to the Lisp streams at the end of each crossing. **Output therefore
appears when the call returns**, not as it is produced, so a long computation's
progress prints arrive together at the end. `PY:FLUSH` writes out what has
accumulated so far, for a loop that wants to report as it goes. Output printed
before a Python error also arrives.

Two consequences worth knowing. `sys.stdout` has no `fileno()`, so a library that
reaches for one will notice. And `input()` reads file descriptor 0 directly rather
than `*standard-input*`, because input is pulled rather than pushed and answering it
needs Python to call Lisp (bliss-m4h70's callbacks).

### Signals and faults

EGCL stays the process's signal authority. The interpreter is started with
`Py_InitializeEx(0)`, so CPython installs no handlers at all and EGCL's remain in
place; from inside Python, `signal.getsignal(signal.SIGINT)` reports `None` — a
handler Python did not install — which is what proves both halves.

One consequence to know: **Ctrl-C cannot interrupt a running Python call.** EGCL's
handler sets a flag that only Lisp code examines, and no Lisp code runs until Python
returns, so an endless Python loop has to be killed (bliss-ziuwp).

A **fault inside CPython kills the process**, as it would in Python itself, rather
than being reported as a Lisp error. That is deliberate and it is not automatic:
EGCL redirects a faulting instruction to a recovery epilogue that unwinds a JIT
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

User-defined sequence classes are **Deferred** to v2. No public class-definition
macro or placeholder protocol is installed for this extension in v1.

## Configuration used by the extension APIs

The most user-relevant environment variables are:

| Variable | Purpose |
| --- | --- |
| `EGCL_WORKERS` | Default scheduler-group carrier count |
| `EGCL_STACK_SIZE` | Per-fiber CL stack size |
| `EGCL_MAX_FIBERS` | Fiber limit; legacy `EGCL_MAX_THREADS` is accepted |
| `EGCL_TIME_SLICE_US` | Fiber cooperative-preemption interval |
| `EGCL_PINNED_BLOCKING_ACTION` | `WARN`, `ERROR`, or `NIL`/`NATIVE` policy |
| `EGCL_T0_T1_THRESHOLD` | T0 to T1 invocation threshold |
| `EGCL_T1_T2_INVOKE_THRESHOLD` | T1 to T2 invocation threshold |
| `EGCL_T1_T2_BACKEDGE_THRESHOLD` | T1 to T2 loop threshold |
| `EGCL_OSR_BACKEDGE_THRESHOLD` | Mid-loop OSR threshold |
| `EGCL_DEOPT_BLACKLIST_THRESHOLD` | Repeated-deoptimization cutoff |
| `EGCL_BAIL_TRACE` | Collect lowerer bail reasons for `BAIL-REPORT` |
| `EGCL_PERF_MAP` | Emit Linux `perf-<pid>.map` JIT symbols |
| `EGCL_PERF_JITDUMP` | Emit Linux perf jitdump metadata |
| `EGCL_INIT_FILE` | REPL initialization file; defaults to `~/.egclrc` |

Environment variables are read during bootstrap or code compilation; changing
the host environment after startup need not update already-installed code.

## Sources and status checks

The contracts summarized here come from these focused specifications:

- `spec/02-runtime-core.md` — execution, callbacks, startup, shutdown;
- `spec/04-02-macroexpand.md` — `EGCL-CLTL2`;
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
rg '"EGCL-EXT:[A-Z-]+" =>' crates/egcl/src/cli.rs
```

An undefined function for an entry marked **Specified** is an implementation
status result, not a package-qualification problem.
