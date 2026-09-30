# Runtime fiber API

This page documents the implemented Rust interfaces in `egcl_rt`. The
[public Lisp interface](../../fibers.md) is installed by the bootstrap and uses these runtime fibers.
The runtime API is under development; treat IDs as process-local handles.

## Run two fibers { #example }

This example uses native Rust entry functions on x86-64 Unix or Windows. The
unsafe conversion is valid only for a function with the runtime entry ABI
`fn() -> EgclVal`; it is not a general conversion of a Lisp closure or foreign
function pointer. An embedding that runs Lisp callables installs its entry
runner through `thread::set_thread_entry_runner`.

```rust
use egcl_rt::{SchedulerConfig, SchedulerGroup, EgclVal};
use egcl_rt::thread::{current_thread_id, fiber_yield, make_fiber};

fn answer() -> EgclVal {
    fiber_yield().unwrap();
    EgclVal::from_fixnum(42)
}

fn main() -> Result<(), egcl_rt::EgclError> {
    current_thread_id();
    egcl_rt::gc::ensure_heap_initialized();
    // SAFETY: answer has the runtime's native entry signature.
    let entry = unsafe {
        EgclVal::from_function_ptr(answer as *const () as *mut u8)
    };
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 })?;
    group.submit(make_fiber(entry)?)?;
    group.submit(make_fiber(entry)?)?;
    let mut results = group.finish()?;
    egcl_rt::rooted_ref!(_results = &mut results);
    assert_eq!(results.iter().map(|v| v.as_fixnum()).collect::<Vec<_>>(), [42, 42]);
    println!("two fibers completed on one carrier");
    Ok(())
}
```

## Creation and group ownership { #groups }

`thread::make_fiber(entry: EgclVal) -> Result<FiberId, EgclError>` allocates a
fiber in `FiberState::Created`. It does not enqueue it. Keep entry values rooted
across allocating runtime calls as described in [GC safety](../how-to/gc-safety.md).

| API | Contract |
| --- | --- |
| `SchedulerConfig { num_workers: usize }` | Explicit carrier count; group initialization rejects zero |
| `SchedulerGroup::init(&SchedulerConfig) -> Result<Self, EgclError>` | Create a pool and its visible native carriers |
| `group.submit(FiberId) -> Result<(), EgclError>` | Submit work and record it for group completion; reject a closed group or an invalid submission |
| `scheduler::run_fibers(&[FiberId], &SchedulerConfig) -> Result<Vec<EgclVal>, EgclError>` | Initialize, submit, and finish a group |
| `group.finish() -> Result<Vec<EgclVal>, EgclError>` | Close submission, join recorded fibers in submission order, then shut down and join carriers |
| `group.shutdown() -> Result<(), EgclError>` | Close submission and shut down only if no submitted fiber is active; otherwise return an error |
| `group.active_fiber_count() -> usize` | Snapshot of submitted fibers not yet dead or removed |
| `group.carrier_thread_ids() -> &[NativeThreadId]` | Native carrier IDs belonging to this group |
| `group.requested_carrier_count() -> usize` | Requested pool size |

Use one initial `submit` per fiber. Do not submit a fiber to another live group.
For manual park/resume, use `unpark`, which does not append another completion
record. Coordinate submission and finishing externally; do not race producers
against `finish` or `shutdown`.

Successful `finish` returns one primary `EgclVal` per submitted fiber, not a
list of Lisp multiple values. Root the returned vector before later allocating
calls. **Completion is consuming:** do not join a group's fibers individually
and then ask the group to finish them, or finish a group twice. Early-error
draining is still incomplete: `finish` stops its result loop at the first join
error. Do not rely on it as a cancellation or complete error-draining API.
Finish or shut down groups explicitly from an external caller, not one of their
own carriers. Dropping the handle is not the completion protocol.

`thread::submit_fiber(FiberId)` uses the runtime's default worker pool. Prefer
an explicit group when the application needs carrier ownership and completion.
`Scheduler` is a compatibility alias for `SchedulerGroup`, and
`active_thread_count` is a compatibility spelling for `active_fiber_count`.

## Joining, yielding, and manual parking { #waiting }

| API | Contract |
| --- | --- |
| `thread::join_fiber(FiberId) -> Result<EgclVal, EgclError>` | Wait for completion, consume the primary result or error, and remove the fiber from the registry |
| `thread::fiber_yield() -> Result<(), EgclError>` | Suspend and requeue the current unpinned fiber; reject calls outside a fiber or while pinned |
| `sync::fiber_sleep(Duration) -> Result<(), EgclError>` | Park an unpinned fiber until its timer fires; use native sleep outside a fiber or under the pinned fallback policy |
| `thread::park_current_fiber() -> Result<(), EgclError>` | Park the current unpinned fiber until a matching wake |
| `group.unpark(FiberId) -> Result<(), EgclError>` | Resume a parked fiber belonging to the pool; reject a non-parked or unknown fiber |
| `group.request_yield(FiberId)` | Request a cooperative yield at a later poll; no immediate suspension guarantee |

Joining oneself is an error. Joining an unsubmitted fiber cannot complete until
someone submits it. A successful join is single-consumer; repeated joins are
errors. The current Rust join has no timeout parameter. An unpinned managed
caller parks while joining, allowing other fibers on the same carrier to run.

Manual parking requires coordination: a wake sent before the target has entered
its parked state is not a stored permit. Prefer the synchronization primitives
below for producer/consumer protocols. Zero-duration sleep yields; it does not
wait for a timer. Arbitrary blocking Rust/foreign calls do not automatically
become cooperative merely because they run in a fiber.

## Pinning and native blocking { #pinning }

`thread::current_fiber() -> Option<&'static Fiber>` returns the mounted fiber,
or `None` on a plain native thread. Use the returned reference only while that
fiber is current; its signature does not make a cached reference safe after
completion and reclamation. Prefer `current_fiber_id()` for identity.

`Fiber::pin()` increments a counter, `unpin() -> Result<(), EgclError>`
decrements it, and `can_yield() -> bool` tests for zero. Balance every pin on all
exit paths. Underflow and an explicit yield or park while pinned are errors.
Holding a native ordered runtime mutex/RwLock guard also pins the owning fiber.

`sync::set_pinned_blocking_action(PinnedBlockingAction)` sets the process-wide
policy: `Warn` logs then blocks natively, `Error` rejects the operation, and
`Native` blocks without a warning. The initial environment setting
`EGCL_PINNED_BLOCKING_ACTION` accepts `WARN` (default), `ERROR`, or
`NIL`/`SILENT`/`NATIVE` for silent native blocking. This is not a Lisp dynamic
binding. `sync::blocking_mode(&str)` applies that policy and returns `Fiber` or
`Native`; it does not itself make the OS call GC-safe.

A native blocking path that owns only Rust data must explicitly use
`safepoint::NativeBlockingScope` under its safety contract. While that scope is
active, do not access Lisp values or root slots: a moving collection can rewrite
them. Do not hold a native mutex across an operation that needs to yield.

## Synchronization and I/O { #synchronization }

The `egcl_rt::sync` types preserve execution ownership across migration.
Timeouts use `Option<Duration>`: `None` means no deadline. Blocking policy still
applies to a pinned caller.

| API | Result and behavior |
| --- | --- |
| `EgclMutex::new(Option<String>, recursive: bool)` | Construct a mutex |
| `mutex.grab(wait: bool, Option<Duration>) -> Result<bool, EgclError>` | True when acquired; false for unsuccessful nonblocking/timed acquisition |
| `mutex.release() -> Result<(), EgclError>` | Release by the owning execution |
| `EgclCondVar::new(Option<String>)` | Construct a condition variable |
| `condvar.wait(&EgclMutex, Option<Duration>) -> Result<bool, EgclError>` | Release the held mutex, wait, and reacquire it; true for notification, false for timeout; always recheck the predicate |
| `condvar.notify(usize) -> usize`, `broadcast() -> usize` | Notify up to the requested number, or all waiters; return the number selected |
| `EgclSemaphore::new(Option<String>, count: i64) -> Result<Self, EgclError>` | Construct a semaphore with a non-negative count |
| `semaphore.wait(Option<Duration>) -> Result<bool, EgclError>` | Consume one permit; false on timeout |
| `semaphore.try_wait(permits: i64) -> Result<bool, EgclError>` | Consume a positive number of permits without waiting if available |
| `semaphore.signal(permits: i64) -> Result<(), EgclError>` | Add a positive number of permits and wake eligible waiters; reject overflow |
| `semaphore.count() -> i64` | Snapshot of available permits |
| `sync::wait_fd(fd, IoInterest, Option<Duration>) -> Result<bool, EgclError>` | Unix descriptor readiness; true for a ready event, false for timeout |
| `sync::wait_socket(&TcpStream, IoInterest, Option<Duration>) -> Result<bool, EgclError>` | TCP readiness on Unix and Windows; retains the caller's synchronous interface while parking an unpinned fiber |

`IoInterest` is `Read`, `Write`, or `ReadWrite`. Readiness is a hint to retry I/O,
not a transfer or ownership change. Keep the descriptor alive until the wait
ends. Linux/Android managed waits use epoll; supported BSD/macOS targets have
kqueue paths. Windows `wait_fd` is not a supported handle-readiness API: a Windows socket
or pipe HANDLE cannot be passed as a Unix descriptor.

Use `wait_socket` for TCP sockets on either platform. Windows uses a shared
WSAPoll service whose snapshots own duplicate sockets until polling completes;
admitting new registrations can take up to one 10 ms poll slice. Unix uses the
descriptor service above. A caller performing I/O must use non-blocking sockets
and retry after readiness. The stdlib's established TCP streams do this for
reads, writes, flushing, and input readiness. Connect, accept, and DNS resolution
remain blocking. Stream operation ownership is retained across a parked read
or write; explicit input readiness waits release it and keep a duplicate handle.

The stdlib's owned child-pipe streams have their own cooperative I/O and
readiness path, including Windows. This does not make arbitrary Win32 handle
waits available through `wait_fd`.

## Observation and configuration { #observations }

`thread::all_fiber_ids() -> Vec<FiberId>` snapshots the registry.
`fiber_state(FiberId) -> Option<FiberState>` returns `None` after removal or for
an unknown ID. `fiber_carrier_thread(FiberId) -> Option<NativeThreadId>` reports
the carrier association; it is not a stable affinity guarantee.

The current Rust states are `Created`, `Runnable`, `Running`, `Suspended`,
`Blocked`, `Native`, `Waiting`, and `Dead`. Polling state is useful for diagnostics
but does not replace synchronization. Joining removes the fiber's registry entry.

`EGCL_STACK_SIZE` selects stack bytes (also accepting `k`, `m`, or `g` suffixes);
the default is 512 KiB. `EGCL_TIME_SLICE_US` controls cooperative preemption:
carriers observe yield requests at runtime polls, not at arbitrary instructions.
The explicit group API uses `SchedulerConfig::num_workers` for its carrier count.

Sources: [group lifecycle](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl-rt/src/scheduler.rs),
[fiber lifecycle](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl-rt/src/thread.rs),
[synchronization](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl-rt/src/sync/mod.rs).
