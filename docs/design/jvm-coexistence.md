# JVM coexistence contract and feasibility findings

Status: coexistence design and historical diagnostic findings. An experimental
native API now lives in [egcl-jvm](../../lib/egcl-jvm/README.md), tracked by
`bliss-b4f28`. It implements checked calls, interface callbacks, explicit reference
ownership and JVM lifecycle; it does not merge collectors or CLOS metaclasses.
Tracked by `bliss-brbpc`. Scope is native x86-64 Linux on the developer's laptop.
The Android comparison below is an architectural boundary only; the user asked
that execution testing stay on the laptop. No phone app was installed or launched.

## Decision

Provide a Lisp object interface to Java using two independent heaps and an
explicit native boundary. Do not merge collectors, object layouts, stack walkers,
or JIT code ownership. JNI is the supported execution boundary. Class integration
and compiler call-site optimization come after coexistence is demonstrated.

The initial feasibility result at `1fa4fd9` was mixed: native Lisp-first embedding passed
useful workloads, but JVM-first embedding crashed because EGCL overwrote
HotSpot's fault handlers. The same installer was reachable from a safepoint timeout
fallback, so startup ordering alone could not constitute a supported solution.
The crash was tracked in `bliss-95g0b` (now fixed as described below).

## Implementation update

The native package adds per-thread alternate stacks, preserves an enabled host
stack, and prevents safepoint fallback from reinstalling process handlers.
GNU/c-ffi builds use interposable `sigaction`. JVM-first initialization requires
preloaded JDK `libjsig.so`; detected JVM-first startup without it reports a
condition before EGCL changes dispositions. Image saving is inhibited before
bridge loading, and native entry rejects switched fiber stacks.

The package tests run on the laptop with OpenJDK 26.0.2.1 and checked JNI.
They include Java-created-thread callbacks, callback revocation, nested calls,
explicit weak/strong handles and shutdown refusal with live resources. The guest
test verifies that detaching EGCL leaves the host JVM usable, then lets the host
destroy it. These results supersede the original signal/alternate-stack failures
below; the original table remains a record of the investigation.

## Reproduction and evidence

The [probe](../../tools/jvm-probe/README.md) builds a small Java workload and C
shim, then loads the shim into the actual dynamic EGCL executable. It runs both
initialization orders in separate, resource-limited processes. The shim does not
repair or intercept EGCL's signal installation. It checks results and requires
a completion marker; failures remain nonzero exits.

Initial measurements on 2026-09-27:

- Runtime source: `1fa4fd9`, built with
  `cargo build --target x86_64-unknown-linux-gnu --features egcl-rt/c-ffi -p egcl`.
- JVM: local Homebrew OpenJDK 26.0.2.1, x86-64 HotSpot; checked JNI, 128 MiB Java
  heap, `-Xrs`. This is a tested combination, not a compatibility matrix.
- The initial installed-RPM smoke test was exploratory; conclusions below use
  the executable rebuilt from this checkout.

| Workload | Lisp first | JVM first |
|---|---|---|
| Start/attach and cached primitive call | 42 | 42 |
| Java GC request | 19 | 19 |
| Java null exception caught in Java | 29 | 29 |
| Java stack overflow caught in Java | 23 | process exits 139 |
| HotSpot SIGSEGV entry remains installed | yes | no |
| Java → Lisp → Java recursion, GC in callbacks | 15 | not reached |
| Callback from Java-created thread | 11 | not reached |
| Overlapping Java/Lisp collection requests | 11 | not reached |
| Create/use/delete 10,000 JNI global references | 10,000 | not reached |
| Lisp GC completes while 3-second Java call remains active | yes | not reached |
| Native-thread join after Java call | 31, completed | not reached |
| Lisp error containment on original and Java-created threads | contained; foreign-thread diagnostic consumed | not reached |
| Explicit bridge cleanup and `DestroyJavaVM` | 0 | not reached |

Lisp-first also completed the callback/collection/reference/long-call workload
with `EGCL_GC_STRESS=1 EGCL_GC_POISON=1 EGCL_GC_VERIFY=1`,
including the Lisp-exception and explicit JVM-shutdown checks. These are bounded examples, not a
proof that all JNI, collector, or scheduler interactions are safe. Overlapping
requests do not establish simultaneous collector phases. Calling `System.gc`
requests collection; it does not provide a general cross-heap liveness oracle.

Stack observations: the original Lisp thread and its nested callbacks ran on
its pthread stack with an enabled alternate signal stack (context bits 3).
A Java-created callback thread ran on its pthread stack with the alternate
signal stack disabled (bits 1). These probes do not execute Java on a switched
EGCL fiber stack.

The full stress/poison/verification run completed without JNI warnings or GC
verification failures. It is a bounded diagnostic, not a broad library or
platform qualification.

The cached-call microbenchmark runs one million calls, after 20,000 warmup calls,
inside a single attached native frame. Initial checked-JNI samples were roughly
116–152 ns/call. They include exception checks and exclude Lisp argument
marshalling and thread attachment. Treat them as a diagnostic baseline only:
there is no controlled A/B comparison, production latency claim, or evidence
that MethodHandles would improve them.

## Boundary contract

### Process and signal ownership

Desktop embedding creates one JVM on a dedicated pthread and records ownership.
Guest embedding accepts an existing VM and must never destroy a VM it does not
own. Initialization is serialized and has explicit not-started, starting, ready,
stopping, stopped, and failed states. Restart in the same process is unsupported.

EGCL needs a guest signal policy, not another unconditional call to its current
installer. Install process handlers once; initialize thread-local facilities
separately. Preserve prior dispositions, masks, relevant flags, and default/ignore
semantics. A dispatcher may handle a fault only if the fault address, instruction
ownership, and execution context establish that it belongs to that runtime.
Neither a low address nor a registered thread alone proves fault ownership.
Unknown faults must remain visible to the host VM or the normal fatal path.

HotSpot can chain preinstalled handlers. `libjsig` interposes libc `sigaction`
for later installation, but EGCL's x86-64 raw `rt_sigaction` syscall bypasses
that interception. The current wrapper also discards the old action.
`SIGINT`, `SIGTERM`, `SIGHUP`, and `SIGQUIT` need an explicit process-control
policy; HotSpot documents that these cannot be chained and suggests `-Xrs` when
the application owns them. `-Xrs` does not solve synchronous fault coexistence.

Alternate signal stacks are per-thread resources. Save an existing thread's
configuration before changing it, define whether to reuse it, and restore it
when detaching where safe. Never register the same backing buffer for concurrent
threads. The current process-global `SIGNAL_ALT_STACK` allocation and installer
require a separate ownership audit, tracked in `bliss-53j07`. Signal paths must not allocate, lock runtime
mutexes, invoke JNI, or call Lisp.

Source anchors: `crates/egcl-rt/src/runtime.rs` (`install_signal_handlers`,
`classify_sigsegv_address`, `sigsegv_handler`), `syscall.rs` (`rt_sigaction`),
`safepoint.rs` (timeout fallback calls the installer).

### Native threads, fibers, roots, and reentrancy

Store `JavaVM*` at process scope. Obtain `JNIEnv*` for the current OS thread on
each entry or through thread-local attachment state. Never retain it in a
migratable fiber or share it with another thread. Scope JNI local references
with local frames in long-running attached threads. Promote only references
that actually outlive the call.

Attach native threads according to an explicit daemon policy. Detach only
attachments owned by the bridge, after the outermost Java frame has returned.
Java-created threads are already JVM-attached; attach their Lisp state before
callback execution and retire bridge-owned Lisp state at the appropriate exit.
Virtual-thread identity is not an OS-thread TLS identity; no persistent Lisp
execution context may be keyed only by a carrier thread.

Use the existing `ForeignStateScope` as the starting point, not a second
independent state machine. Publish Lisp roots before entering Java, retire the
TLAB as required, and let GC stop waiting for that mutator. Reenter through the
GC admission protocol before touching Lisp objects. Nested
Lisp → Java → Lisp → Java transitions must restore their previous state.
No runtime lock needed by GC, callbacks, class initialization, or library loading
may be held while calling the other runtime. Callbacks can happen synchronously
before an outbound call returns.

Initial supported entries must execute on ordinary attached pthread stacks.
Pinning a fiber to a carrier alone does not prove that its switched stack meets
HotSpot stack bounds and guard assumptions. Before exposing Java calls to fibers,
choose and validate either a supported carrier-stack transition or a request
mechanism with explicit callback/reentrancy semantics. A single bridge worker
that blocks awaiting a callback on its own queue can deadlock. Until that work
is complete, reject unsupported fiber-stack entry before invoking JNI.

### Object ownership and cross-heap cycles

A Lisp Java proxy owns a strong global JNI reference or an explicitly weak one.
A Java Lisp proxy owns a generation-checked stable handle whose table entry is a
GC-visible Lisp root. Java never receives a raw moving `EgclVal` address.
Temporary call arguments use scoped roots; persistent callbacks use retained
roots, with revocation only after foreign publication and active calls end.

Define release as an idempotent operation on a handle, with stale generation
checks and use-after-release errors. Finalization only enqueues reclamation;
a JVM-attached drain performs JNI deletion outside Lisp collector locks.
Java-side cleanup similarly releases Lisp handles without arbitrary evaluation.
Do not wait for Java collection while holding a Lisp GC lock, or vice versa.

Strong edges in both directions can create permanently retained cross-heap
cycles. V1 requires scoped owners or explicit close to break them. Weak edges
are opt-in and can disappear while the proxy itself remains live. Promote a weak
JNI reference to a strong local reference before use and handle a cleared
referent. Neither finalizers nor repeated collection requests solve cycles.
The probe verifies explicit JNI release operations, not a distributed collector.

### Exceptions and Java-facing callbacks

No Lisp nonlocal exit, Rust panic, or native exception may unwind across JVM
frames. Check pending JNI exceptions before proceeding with ordinary JNI work;
retain the Throwable where needed, clear the pending state, and raise a Lisp
Java-exception condition after reentry.

For Java-facing callbacks, convert a contained Lisp failure to a Java exception
before returning from the registered native method. Preserve useful diagnostics
without retaining arbitrary cross-heap graphs. The current generic FFI callback
mechanism returns zero and records an error; that is containment, not yet a
Java Throwable adapter. Java code must not accidentally interpret that zero as
a successful callback result. The probe makes this distinction explicit.

### Class identity, method caches, and conversions

Key Java class identity by defining loader and class identity, not just a name.
A global `jclass` reference prevents unloading; decide whether caches intentionally
retain loaders or use weak keys and eviction. Method/field IDs are valid only
for the lifetime of the defining class. JNI reference addresses are not Java
object identity; use `IsSameObject` and define whether proxy interning is needed
for Lisp `eq`. Separate that from Java `equals`.

Callbacks from native-created threads need a captured application class loader;
blind `FindClass` does not reliably select the intended application loader.
Class initialization can execute arbitrary Java and callbacks. Respect module
access rules rather than treating reflection as unlimited access.

Start with explicit signatures and cached `jmethodID` calls. Later overload caches
must include argument conversion shape/range, not just receiver class. Define
boxing, overflow, null/boolean distinction, UTF-16 versus Lisp characters, and
array-copy semantics. Views over Lisp storage require an explicit lifetime and
synchronization protocol; JNI critical regions cannot enclose arbitrary Lisp
execution or blocking work.

MethodHandles require a Java-side adapter or `invokeWithArguments`; directly
calling signature-polymorphic `invoke`/`invokeExact` through JNI is unsupported.
Neither MethodHandles nor `invokedynamic` remove the native boundary for EGCL
functions. A JVM bytecode backend has a different object-representation and
runtime-semantics problem and is outside this contract.

### Images, shutdown, process creation, and budgets

Saved Lisp images cannot preserve live VM pointers, JNI references, cached IDs,
attached-thread state, or native callback addresses. V1 should reject saving
live Java bridge resources with a named diagnostic. An eventual reconstruction
protocol may serialize declarative classpaths and application recipes, then
resolve fresh resources after restart. It must never silently restore stale
addresses or imply that arbitrary Java objects were serialized.

Shutdown first rejects new bridge work, drains active calls, revokes callbacks,
releases both kinds of handles, and detaches owned threads. Only then may the
owner call `DestroyJavaVM`. It can wait for non-daemon Java/attached native
threads, so shutdown needs an observable timeout policy. Do not unload HotSpot
or JNI helper code while references, callbacks, or VM threads remain. Never
call `DestroyJavaVM` for a guest VM. `System.exit`/fatal VM failures affect the
whole process; this integration is not a sandbox.

After JVM startup, do not run Lisp in a forked child. Use a supported spawn/exec
path with only permitted child-side operations. Audit ELF symbol visibility and
native-library ownership instead of assuming Java and Lisp have separate copies
of process-global library state. Both heaps, JIT code caches, direct/native
buffers, stacks, and metadata count against the same memory budget. Do not use
fixed mappings that overwrite another runtime's reservations. Profilers and
signal-based sampling need the same ownership review as runtime fault handlers.

### Android comparison (not part of this execution test)

An Android application already has ART. Obtain its `JavaVM*` from the platform
entry (`ANativeActivity.vm` or `JNI_OnLoad`), attach the Lisp worker, and obtain
that worker's own `JNIEnv*`. Never move `ANativeActivity.env` from the UI thread
to the Lisp worker. Capture the correct app loader for application classes.
Do not create another VM, use HotSpot-specific signal assumptions, or destroy ART
at Activity teardown. Activity references have Activity lifetimes; the VM has
process lifetime. This requires a separate explicitly requested Android probe.

## Implementation gates remaining after the feasibility study

The implementation is tracked separately in `bliss-b4f28`, dependent on
`bliss-95g0b` and `bliss-53j07`.

The startup-order fault is a demonstrated blocker to general guest embedding.
Supporting native Linux integration also requires a reviewed per-thread stack
and signal policy, stable handle lifecycle, Java Throwable adapters, class-loader
cache ownership, image rejection/reconstruction behavior, and a supported fiber
entry protocol. Passing this diagnostic is necessary evidence, not sufficient
qualification for arbitrary Java libraries. The diagnostic itself must continue
to expose the JVM-first failure until the actual runtime contract is implemented.

## Primary references

- [JNI Invocation API](https://docs.oracle.com/en/java/javase/25/docs/specs/jni/invocation.html)
- [HotSpot signal chaining](https://docs.oracle.com/en/java/javase/25/vm/signal-chaining.html)
- [JNI functions and reference/critical-region contracts](https://docs.oracle.com/en/java/javase/24/docs/specs/jni/functions.html)
- [MethodHandle invocation restrictions](https://docs.oracle.com/en/java/javase/25/docs/api/java.base/java/lang/invoke/MethodHandle.html)
- [Android JNI guidance](https://developer.android.com/ndk/guides/jni-tips)
