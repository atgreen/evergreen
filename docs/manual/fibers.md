# Fibers and scheduler groups

Fibers are managed executions multiplexed over native carrier threads. A fiber
can suspend while waiting and let another fiber run on that carrier. Native
threads created by `torcl-thread:make-thread` remain dedicated OS threads.

## Availability { #availability }

The implemented API is the Rust `torcl_rt` runtime interface, documented in
[Runtime fiber API](contributing/reference/fibers.md), including a runnable
example. Stack-switching fibers are implemented for x86-64 Unix and Windows.
Windows validation currently includes Wine; that is not native Windows testing.
See [Platform support](user/reference/platforms.md) for the wider target matrix.

The `TORCL-FIBER` **Lisp package is specified but is not installed by the current
bootstrap**. The dictionary below describes that proposed interface. Its
forms are not runnable Lisp examples today. In particular, the proposal's
multiple-value results, join timeouts, and idempotent group completion must not
be inferred from the current Rust interface, which has different contracts.

For currently callable Lisp concurrency, use
[Threads and synchronization](concurrency.md).

## Proposed Lisp lifecycle { #lifecycle }

**Functions — specified, not installed**

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

The intended lifecycle separates creation from scheduling. `make-fiber` creates
an unscheduled fiber; its first submission assigns it to a group. `start-fibers`
starts carriers and submits the supplied fibers. `run-fibers` is the convenience
operation that starts and finishes a group. The proposed `carrier-count`
defaults to the available processor count and must be at least one.

The proposed `finish-fibers` closes submission, waits for the submitted work,
returns results in submission order, and joins the carriers. The proposal calls
for idempotent completion. These are design requirements, not guarantees of the
current Rust `SchedulerGroup::finish` implementation.

## Proposed Lisp waiting { #waiting }

**Functions — specified, not installed**

```lisp
(torcl-fiber:fiber-yield)
(torcl-fiber:fiber-sleep seconds)
(torcl-fiber:fiber-park predicate &key timeout) -> predicate-won-p
(torcl-fiber:fiber-join fiber &key timeout) -> entry-values
```

These operations are intended to suspend an unpinned fiber without occupying
its carrier. Sleep durations and timeouts are expressed in seconds. A yield is
a scheduling opportunity, not a synchronization protocol. Shared state still
requires a mutex, condition variable, semaphore, or another explicit protocol.

## Proposed Lisp pinning { #pinning }

**Functions, macro, and variable — specified, not installed**

```lisp
(torcl-fiber:fiber-pin &optional fiber)
(torcl-fiber:fiber-unpin &optional fiber)
(torcl-fiber:fiber-can-yield-p &optional fiber) -> boolean
(torcl-fiber:with-fiber-pinned ((&optional fiber)) body*)
torcl-fiber:*pinned-blocking-action*
```

Pinning is a balanced counter that prevents carrier migration. The proposed
macro balances the counter across ordinary and nonlocal exits. Explicit yield
while pinned signals `program-error`. The proposed blocking policy is `:warn`
by default, `:error` to reject blocking, or `nil` for silent native blocking.
The runtime currently exposes this policy through Rust and an environment
variable; binding the proposed Lisp variable does not configure it.

## Proposed Lisp observations { #observations }

**Functions — specified, not installed**

| Function | Intended result |
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

Observations are snapshots. A carrier can change after a yield, and observing
completion does not consume a result. The proposed Lisp conditions include
`torcl-fiber:fiber-error` and `torcl-fiber:fiber-still-running`; these conditions
are also not installed. Consult the current Rust reference for the actual
state enum and error returns.

Design sources: [Lisp API inventory](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/docs/torcl-lisp-api.md#fibers)
and [concurrency specification](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/spec/13-concurrency.md).
