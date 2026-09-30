# Threads and synchronization

EGCL distinguishes native operating-system threads from the runtime's managed
fibers. The interface documented here is `EGCL-THREAD`; managed executions
use the separate [EGCL-FIBER API](fibers.md). The
[Runtime fiber API](contributing/reference/fibers.md) documents the Rust layer.

## Native thread lifecycle

### `egcl-thread:make-thread` { #make-thread }

**Function** `(egcl-thread:make-thread function &key name)` → thread handle

Starts a dedicated native thread that calls `function` with no arguments.
Accepts a callable function or a symbol naming a global function. `name` is a
string or `nil`. Treat the returned handle as opaque; its current representation
is not a portable application data format.

### `egcl-thread:join-thread` { #join-thread }

**Function** `(egcl-thread:join-thread thread &key timeout)` → value, completed-p

Waits for completion and returns the worker's primary result and `t`.
If the timeout expires, returns `nil, nil`. A worker that successfully returns
`nil` therefore yields `nil, t`, which is distinguishable from a timeout.

Timeouts are non-negative real seconds. Omitted or `nil` means no deadline.
An invalid timeout signals a type error. Do not depend on repeated joins
returning the same result; consume the worker's result once.

```lisp
(let ((worker (egcl-thread:make-thread (lambda () (+ 20 22))
                                        :name "answer")))
  (multiple-value-list (egcl-thread:join-thread worker :timeout 10)))
;; => (42 T)
```

### Thread observations { #thread-observations }

**Functions**

```lisp
(egcl-thread:current-thread)
(egcl-thread:thread-name thread)
(egcl-thread:thread-alive-p thread)
(egcl-thread:all-threads)
(egcl-thread:thread-yield)
```

`current-thread` returns the calling native thread's handle. `all-threads`
reports live registered threads. These observations are snapshots: another
thread can complete immediately afterward. Yielding is a scheduling hint, not
a substitute for a synchronization protocol.

## Mutexes

### Construction { #make-mutex }

**Function** `(egcl-thread:make-mutex &key name recursive)` → mutex

Creates a mutex, non-recursive by default. `egcl-thread:mutex-p` tests the
object and `egcl-thread:mutex` is its Lisp type. Use a recursive mutex only
when the locking protocol explicitly requires repeated acquisition by its owner.

### Acquisition and release { #grab-mutex }

**Functions**

```lisp
(egcl-thread:grab-mutex mutex &key (waitp t) timeout)
(egcl-thread:release-mutex mutex &key (if-not-owner :error))
```

Acquisition returns true on success and false when it cannot obtain the lock
under the requested waiting policy. `:waitp nil` requests nonblocking acquisition;
a timeout is measured in non-negative real seconds.

Release normally requires ownership. `:if-not-owner` accepts `:error`, `:warn`,
or `:ignore`; it does not grant a different execution the right to unlock the
owner's mutex. Release returns `nil`.

### `egcl-thread:with-mutex` { #with-mutex }

**Macro** `(egcl-thread:with-mutex (mutex &key (waitp t) timeout) body...)`

Runs the body only if the mutex was acquired. Releases it on ordinary return or
nonlocal exit, and preserves the body's values. Returns `nil` without running
the body if acquisition fails.

```lisp
(let ((lock (egcl-thread:make-mutex :name "state")))
  (egcl-thread:with-mutex (lock)
    (values 40 2)))
;; => 40, 2
```

Saved images do not recreate a usable operating-system mutex from an old live
handle. Create fresh synchronization objects for a restarted application's
runtime state rather than assuming locks survive an image boundary.

## Condition variables

### Construction and waiting { #condition-wait }

**Functions**

```lisp
(egcl-thread:make-condition-variable &key name)
(egcl-thread:condition-variable-p object)
(egcl-thread:condition-wait condition-variable mutex &key timeout)
```

Wait while holding the associated mutex. Waiting releases the mutex while the
execution sleeps and reacquires it before resuming. Always test the application
predicate in a loop: a notification is not the predicate, and a resumed waiter
must examine the shared state under the lock.

### Notification { #condition-notify }

**Functions**

```lisp
(egcl-thread:condition-notify condition-variable &optional (count 1))
(egcl-thread:condition-broadcast condition-variable)
```

`condition-notify` requests wakeup of up to `count` waiters; the count is a
non-negative integer. `condition-broadcast` wakes all waiters. Keep updates to
the predicate and the notification protocol coordinated with the mutex.

## Shared state

Use synchronization to protect shared mutable objects. A thread name, a liveness
query, or a scheduling yield is not a memory barrier for an application data
structure. Closure capture and dynamic bindings also do not turn a compound
read–modify–write operation into an atomic operation.

Backend and scheduler support varies by architecture. Check
[Platform support](user/reference/platforms.md) instead of extrapolating native
thread support into a claim about foreign callbacks or fiber context switching.

Implementation reference: [Thread and synchronization wrappers](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/lib/boot.lisp).
