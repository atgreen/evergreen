# Fibers and scheduler groups

Fibers are managed executions multiplexed over native carrier threads. A fiber
can suspend while waiting and let another fiber run on that carrier. Native
threads created by `torcl-thread:make-thread` remain dedicated OS threads.

## Availability { #availability }

The `TORCL-FIBER` Lisp package is loaded by the standard bootstrap. Stackful
fibers are supported on x86-64 Unix and Windows. Native Windows validation is
separate from testing under Wine. See [Platform support](user/reference/platforms.md).

The lower-level Rust interface is documented in
[Runtime fiber API](contributing/reference/fibers.md). Its consuming join and
completion operations have different contracts from the Lisp wrappers.

## Lisp lifecycle { #lifecycle }

**Functions**

```lisp
(torcl-fiber:make-fiber function
  &key name arguments stack-size initial-bindings) -> fiber
(torcl-fiber:start-fibers fibers &key carrier-count idle-hook) -> group
(torcl-fiber:run-fibers fibers &key carrier-count idle-hook) -> results
(torcl-fiber:submit-fiber group fiber) -> unspecified
(torcl-fiber:finish-fibers group) -> results
(torcl-fiber:fiber-group-done-p group) -> boolean
(torcl-fiber:scheduler-group-carriers group) -> list-of-threads
```

The lifecycle separates creation from scheduling. `make-fiber` creates
an unscheduled fiber; its first submission assigns it to a group. `start-fibers`
starts carriers and submits the supplied fibers. `run-fibers` is the convenience
operation that starts and finishes a group. `carrier-count`
defaults to the available processor count and must be at least one.

`finish-fibers` closes submission, waits for every submitted fiber, shuts down
and joins the carriers, and returns a list of primary results in submission
order. Repeated calls return the cached results. If an entry signals an
unhandled error, completion still drains the other fibers before signaling
`fiber-error`; subsequent joins and completion calls signal that saved failure.
A fiber cannot finish its own group.

`arguments` is the entry function's argument list. `initial-bindings` is an alist
of `(special-symbol . value)` pairs. Standard streams and `*package*` are inherited
from the creator; explicit bindings override them. Other special variables use
their global values until bound by the fiber. Package definitions are shared.

`stack-size` reserves the native stack in bytes, defaults to 524288 (512 KiB),
and accepts 512 KiB through 1 GiB. This is separate from the runtime's managed
Lisp frame stack. Deep interpreted code can need a larger native reservation.

An `idle-hook` receives the group on an idle carrier. With several carriers,
it can run concurrently; protect shared state as usual. Hook errors are saved
and reported by `finish-fibers` after work drains. Hooks should return promptly.

```lisp
(torcl-fiber:run-fibers
  (list (torcl-fiber:make-fiber
          (lambda () (torcl-fiber:fiber-yield) (values 41 :extra)))
        (torcl-fiber:make-fiber (lambda () 42)))
  :carrier-count 2)
;; => (41 42)
```

## Lisp waiting { #waiting }

**Functions**

```lisp
(torcl-fiber:fiber-yield)
(torcl-fiber:fiber-sleep seconds)
(torcl-fiber:fiber-park predicate &key timeout) -> predicate-won-p
(torcl-fiber:fiber-join fiber &key timeout) -> entry-values
```

These operations suspend an unpinned fiber without occupying
its carrier. Sleep durations and timeouts are expressed in seconds. A yield is
a scheduling opportunity, not a synchronization protocol. Shared state still
requires a mutex, condition variable, or another explicit protocol.

Subprocess pipe operations and process waits park unpinned fibers. Ordinary file,
socket-stream, and terminal I/O still use blocking OS calls and can occupy the
carrier; the runtime readiness service is not yet connected to all stream paths.
Foreign calls can also block a carrier. The pinned-blocking policy below applies
to operations that participate in the fiber-aware blocking protocol.

`fiber-join` preserves all entry values and can be called repeatedly. A timeout
returns two values, `nil nil`; use the state/result accessors when the entry's
own values could be ambiguous. `fiber-park` reevaluates its predicate between
cooperative waits and returns true when it succeeds, or false at the timeout.

## Lisp pinning { #pinning }

**Functions, macro, and variable**

```lisp
(torcl-fiber:fiber-pin &optional fiber)
(torcl-fiber:fiber-unpin &optional fiber)
(torcl-fiber:fiber-can-yield-p &optional fiber) -> boolean
(torcl-fiber:with-fiber-pinned ((&optional fiber)) body*)
torcl-fiber:*pinned-blocking-action*
```

Pinning is a balanced counter that prevents carrier migration. The
macro balances the counter across ordinary and nonlocal exits. Explicit yield
while pinned signals `program-error`. The blocking policy is `:warn`
by default, `:error` to reject blocking, or `nil` for silent native blocking.
Bind `torcl-fiber:*pinned-blocking-action*` dynamically to configure the current
execution. Rust callers retain the runtime policy/environment interface.

## Lisp observations { #observations }

**Functions**

| Function | Result |
| --- | --- |
| `(torcl-fiber:current-fiber)` | Current fiber, or `nil` on a plain native thread |
| `(torcl-fiber:list-all-fibers)` | Snapshot of non-reclaimed fibers |
| `(torcl-fiber:fiber-state fiber)` | Lisp lifecycle state keyword |
| `(torcl-fiber:fiber-name fiber)` | Name string or `nil` |
| `(torcl-fiber:fiber-result fiber)` | Result values as a list after completion |
| `(torcl-fiber:fiber-error-p fiber)` | Whether an unhandled condition ended execution |
| `(torcl-fiber:fiber-alive-p fiber)` | Whether the fiber has not completed |
| `(torcl-fiber:fiber-carrier-thread fiber)` | Current or last carrier, or `nil` |
| `(torcl-fiber:print-fiber-backtrace fiber &key stream count)` | Print the saved stack |

Observations are snapshots. States are `:created`, `:runnable`, `:running`,
`:suspended`, and `:dead`. A carrier can change after a yield. Joining transfers
the result into the Lisp object and removes the runtime registration;
`list-all-fibers` lists registrations that have not yet been reclaimed this way.
Created fibers must be submitted to run and become joinable.

`print-fiber-backtrace` defaults to `*standard-output*` and 20 frames. It snapshots
an unmounted continuation; inspecting a running fiber signals
`fiber-still-running`. Created fibers show their entry; completed fibers have no
active call frames. `fiber-error-fiber` and `fiber-error-cause` expose a failed
entry and its saved condition.

Runtime handles are not resumable through saved images. Using a fiber or group
from a previous process signals an error rather than reusing a stale identity.

Design sources: [Lisp API inventory](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/docs/torcl-lisp-api.md#fibers)
and [concurrency specification](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/spec/13-concurrency.md).
