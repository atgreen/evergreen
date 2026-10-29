# Embedding one Lisp world

`egcl::run` retains command-line process ownership. Applications that keep
running after Lisp returns use `egcl::run_embedded`; the iOS C adapter in
`egcl-ios` supplies a worker and a cohort supervisor around that root API.
This implementation, scaffold and regression fixtures are source-only and
have not been built, typechecked or executed.

## Ownership and exit

Embedded ownership must be selected before runtime process-signal installation.
It is permanent for the process lifetime, including in subsequently created
Lisp threads and fibers. `QUIT` and `SAVE-LISP-AND-DIE` use the existing
non-condition shutdown control transfer instead of calling process exit.
`UNWIND-PROTECT` cleanups can run; a genuine cleanup error remains an error.
The first **published explicit** Lisp exit code wins across the world. A host
stop request does not replace it with zero. The root-return API may precede a
later descendant exit request; the cohort supervisor reads the final explicit
code only after the root and active descendants stop.

Embedded installation leaves application SIGINT, SIGTERM and SIGALRM
handlers alone. Existing runtime fault, SIGPIPE I/O and safepoint handling
remains installed. Internal installation on a later worker inherits the chosen
profile. Defensive TERM/ALRM handler paths cannot arm a hard-exit alarm or
terminate an embedded process. This is not a native-code sandbox: arbitrary
foreign code, fatal native faults and aborting/OOM failures are not contained.

Only one embedded root may start in a process. Runtime registries and TLS do
not provide isolated generations, and completion is not full heap/metadata or
carrier-pool teardown. A second `run_embedded` call is rejected. The iOS C API
accepts one start attempt after configuration validation, even if startup later
fails. It never resets process ownership to command-line mode.

## Cooperative supervision

The root API alone does **not** imply its descendants have stopped. The iOS
supervisor retains only Rust-owned status and diagnostics; an `EgclError` is
rendered while the execution and precise roots remain alive. No Lisp heap
value or arbitrary panic payload is stored in the host status record.

Stop closes admission to native thread/fiber creation and initial fiber
submission. In-flight creation remains counted through registry/queue
publication. The supervisor repeatedly snapshots live Lisp identities and posts
shutdown once per execution, including late publication by an admitted creator.
It excludes itself and idle carriers. Native `Dead` and `Aborted` states are
terminal; unsubmitted `Created` fiber objects are not running activations.
Completion closes admission and rechecks both publication count and live
attachments before publishing a terminal state. The supervisor tracks its own
root worker by physical join and exact identity; descendant snapshots cover
explicit Lisp native threads and submitted fibers. Auto-registered collector/IO
helpers are not Lisp descendants. Foreign Lisp callbacks are not admitted by
the initial calls-only iOS adapter.

Delivery is also gated at consumption. Explicit exit marks its execution as
already unwinding; a queued or late shutdown bit is removed without delivering
a second error into cleanup. Publication/retirement clear the corresponding
pending count. No target spins waiting for a publisher which a collection
might have paused. This control state contains no Lisp values and adds no
saved-image or native-context-layout fields.

The control thread stays in a native blocking scope and never reads Lisp heap
contents or changes root slots. The UI never joins the worker. `ROOT_COMPLETED`
means the root returned but Lisp attachments remain active. `FINISHED` and
`FAILED` are published only after the root is joined and active descendants
are terminal. Descendant OS-thread/TLS retirement can follow the end of Lisp
execution, and idle carriers/process metadata can remain. The adapter retains
its Rust-owned control state for process life and never frees borrowed host
resources on the strength of an early root return. Numeric/error outcomes describe the root and explicit exit
policy; background thread error handling otherwise retains the runtime's
existing thread/join semantics.

Cancellation is cooperative. Existing opt-in fiber cancellation can wake
cancellable waits; arbitrary waits and foreign calls are not forcibly released.
Stop may therefore remain pending indefinitely. A supervisor panic reports
that completion is unknown instead of claiming that the world stopped. Rust
unwinds are contained with the default workspace profiles; native faults and
aborting panics are not recovered.

## iOS source application

See the [SwiftUI/C scaffold](../examples/ios/README.md) and
[C header](../crates/egcl-ios/include/egcl_ios.h). Configuration strings are
copied before `start` returns. Bundle and writable paths are separate, and
Lisp pathname defaults are scoped without changing process CWD or environment.
Source bootstrap is explicit; executable-image delivery is refused on iOS.

The shared process library rejects subprocess creation on iOS, and the direct
native-runtime builder refuses cargo/probe spawning there as well. Ordinary
thread creation remains supported. These are EGCL API restrictions, not a
restriction on arbitrary host or foreign native code.

SDK linking, simulator/device startup, UI responsiveness, native/GC regression
execution and source-free lifecycle evidence are all unrun. No working-device,
complete teardown, restartability or performance claim follows from this patch.
