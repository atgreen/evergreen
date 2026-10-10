# Universal native calling ABI and exceptional transfers

Status: in progress. The user approved the universal calling contract below on
2026-10-09. Existing segment compilation, native cleanup and precise deoptimization
are implementation components; enabling their rollout flag alone is not completion.
Tracking: **bliss-shih7**, with executable steps in its child Beads.
Baseline: `2c84d2e1` (the counted pending-error fast path, `bliss-5fzra`).
That completed mitigation retained return checks; this work replaces the protocol
on top of it. Do not remove checks until the new boundary contract is verified.

## v0.0.4 scope

The release defaults to the native calling ABI on x86-64 Linux. Eligible T1
and T2 bodies compile to mapped native entries. Unsupported body shapes use
the published interpreter adapter; a declined T2 compilation retains an
existing mapped T1 body. Legacy checked-ABI code is not installed on Linux.
Current-frame OSR remains disabled there until mapped OSR is implemented, so
cold functions containing long loops and unsupported bodies can run slower.

`EGCL_NATIVE_TRANSFER=0` disables only the opportunistic segment cache on
Linux; it does not restore checked-ABI installation or disable published
mapped entries. Other platforms retain their existing rollout policy and do
not gate this release. Full compiled-shape coverage and mapped current-frame
OSR remain tracked by the open universal ABI epic and `bliss-shih7.17.4`.

## Objective and decision

Every callable exposes the native calling ABI by default. A caller may bypass it
only when both the caller and the currently selected target are known to be
interpreted. Being difficult to compile does not change a function's public
calling convention: its entry points to an interpreter adapter instead.

Use the existing stable named-call ordinals and executable `CallCell` slots.
Publish a compatible entry for compiled code, interpreted code, builtins,
closures, generic functions and undefined functions. Keep the existing small
register-argument and large argument-slice entry shapes while integrating the
transfer contract. Tiering and redefinition update entry publication with the
existing revision and retained-code lifetime rules. The ordinary caller loads
the entry and calls it; it does not choose an ABI, resolve a symbol name, or
check transfer status after a successful native return. `FUNCALL` and `APPLY`
must obey the same contract. Temporary legacy adapters translate at the boundary;
they are not permission to preserve a second caller-visible default ABI.

Platform compatibility is an entry-publication and execution-boundary invariant.
Worker startup and fiber mounting must prevent unsupported native continuations
from resuming. A scheduler migration issue is not a reason to add checks after
every Lisp call. Independent GC, preemption and asynchronous-signal polls remain.

Conditions and restarts have two distinct phases:

* **Live signaling:** call matching `HANDLER-BIND` handlers while the signaling
  computation, dynamic bindings and restarts remain available. Returning from a
  handler declines; it does not replace the failed operation's result.
* **Selected transfer:** only a chosen nonlocal exit begins unwinding.
  `HANDLER-CASE` and `RESTART-CASE` clause bodies execute after the appropriate
  unwind. A `RESTART-BIND` function runs in the invocation's dynamic environment
  and may return normally. Lexical captures do not replace that dynamic context.

In particular, a Rust helper must not destroy live restart context by returning
an unsignaled error outward and only then searching handlers. Signal while the
required state is live (or precisely reified and rooted); propagate an already
selected escaping transfer through ordinary Rust returns before native unwinding
continues. Preserve Rust destructors, exact cleanup order, replacement transfers,
multiple values, moving roots and the executing definition's identity. Neither
deoptimization nor transfer fallback may replay completed effects.

The expanded implementation is tracked by `bliss-shih7.15` (contract),
`bliss-shih7.16` (entry publication), `bliss-shih7.17` (emitted calls and interpreter
boundaries), `bliss-shih7.18` (live conditions and restarts), and the existing
exceptional CFG, native scope, boundary, platform
and performance tasks. This extends the endpoint below; it does not replace it
with a Linux-only subset or a default-on configuration change.

A successful native-to-native call returns its Lisp value directly, without
calling `c2i_transfer_pending` or testing a pending-error flag afterward.
An escaping error or nonlocal exit takes a cold transfer path. GC, preemption,
signals and sandbox deadlines have explicit, root-safe polling points.

Use **native segments**: a segment is the generated-code portion of execution
between one Rust-to-native entry and its return. Every reentry from Rust creates
a new segment, including interpreter calls, OSR and foreign callbacks. A segment
may contain many native calls, but an exceptional jump cannot cross its Rust
caller. This obtains the useful property of SBCL-style native transfer without
attempting to jump through arbitrary Rust or foreign frames.

The required endpoint dispatches transfers through **native handler and cleanup
landing pads**. Selected transfers within a fully supported native segment stay
native. Bytecode reconstruction is an intermediate milestone and a fallback for
unavailable or invalidated native continuations, not the normal unwind strategy.
It must never restart the failed function or replay the failed call.

Known local `RETURN-FROM` and `GO` become direct branches with required cleanup
and binding restoration when legal. General nonlocal transfers use the native
unwinder. Condition signaling and restart search retain the live dynamic context;
only a selected escaping transfer begins unwinding.

## Evidence and existing integration points

* `crates/egcl/src/cli/bytecode.rs`: `c2i_call_args` and other helpers store an
  escaping `EgclError` and return a placeholder. `native_loop_should_exit`
  checks the slot and claims signals. `run_native` and `run_native_osr` restore
  enclosing evaluator state and deliver the error.
* `crates/egcl-compiler/src/t2/emit.rs`: `emit_call` emits a transfer check even
  after a direct recursive call. `emit_transfer_check` saves an **unrooted**
  primary result and calls a helper that must not collect, yield or invoke Lisp.
* `crates/egcl-rt/src/stack.rs`: the host stack and `EgclStack` are distinct.
  Both must be restored consistently; changing only the host stack is invalid.
* `NativeCode`, `ActiveNativeCode`, `NativeDepthGuard`, environment restoration,
  dynamic scope guards and fault-recovery state must remain valid on each exit.
* [T2 frame layout](t2-frame-layout.md),
  [FrameState specification](../../spec/04-10-t2-frame-state.md),
  [OSR specification](../../spec/04-06-osr.md), and
  [concurrency specification](../../spec/13-concurrency.md) constrain the design.

The existing Fibonacci benchmark is not tail-recursive. SBCL's disassembly has
both recursive calls and no equivalent return-poll helper. TCO is a separate
feature (`bliss-ifws8`), not a substitute for this work.

## Proposed interfaces and module responsibilities

These names and signatures specify new interfaces; they are not existing APIs.

`crates/egcl-rt/src/native_transfer.rs` owns the machine boundary contract:

```rust
#[repr(u64)]
enum NativeExit { Returned = 0, Transfer = 1, Deopt = 2 }

#[repr(C)]
struct NativeOutcome {
    value: EgclVal,
    exit: NativeExit,
}

unsafe extern "C" fn invoke_native_segment(
    entry: *const u8,
    slots: *mut u64,
    stack: *const u8,
    segment: *mut NativeSegment,
    out: *mut NativeOutcome,
);
```

Use an explicit out parameter at the Rust/assembly boundary so Win64 and SysV
need not agree on aggregate return classification. This is paid at a boundary,
not at every recursive native call. Secondary values stay in rooted execution
state, with their count preserved separately from the primary result.

`NativeSegment` has a stable address for its active lifetime and records its
previous segment, execution owner, host stack/register save area, EgclStack
watermark, exceptional landing continuation, and rooted transfer state. Machine
save areas are backend-specific, with compile-time offset assertions. Landing
stubs must also satisfy enabled control-flow hardening: Win64 unwind/CFG rules,
x86 shadow-stack and indirect-branch protections, and AArch64 return signing/
branch-target rules where applicable. A plain stack-pointer reset is not a
valid shadow-stack unwind. Gate machine transitions on tested platform support;
when one is unavailable, publish a compatible adapter instead of exposing a
different caller ABI. Existing checked entries are temporary bridge inputs. An owning
Rust guard registers and unregisters the segment and restores enclosing state.
Do not introduce a mandatory lookup or new reserved register on each successful
native return; exceptional/helper adapters can obtain the active anchor through
execution-owned state. Fiber migration must preserve that ownership.

`crates/egcl/src/cli/bytecode/native_transfer.rs` owns the evaluator-side
transfer packet and preparation/dispatch:

```rust
fn prepare_native_transfer(
    segment: &mut NativeSegment,
    site: &NativeUnwindSite,
    registers: &NativeRegisterSnapshot,
) -> Result<PreparedNativeTransfer, EgclError>;

fn resume_native_transfer(
    transfer: PreparedNativeTransfer,
    env: &mut Env,
) -> Result<EgclVal, EgclError>;
```

The interfaces above describe the bytecode fallback. Preparation roots the
payload and reconstructs the exact logical continuation.
It must finish while the physical frame information is still available. A
preparation failure must itself produce a valid transfer, including on OOM or
stack exhaustion; reserve an emergency path before enabling the ABI. The
prepared packet owns every reference needed after physical frame retirement.
Resumption enters bytecode **transfer propagation**, not normal instruction
execution at the failed call. Pending transfers suspended during cleanup are
rooted, nestable execution-owned objects.

Compiler metadata supplies `NativeUnwindSite`: return/transfer PC, physical
frame/save recipe, root locations, logical/inlined scopes, continuation BCP,
dynamic actions, and retained definition identity. The compiler must preserve
exception-live values as well as deopt-live values. Code installation rejects
incomplete metadata rather than guessing a reconstruction.

The native dispatcher uses a rooted, execution-owned `NativeUnwindCursor`:
segment identity, retained code version, current frame/site, selected destination,
next cleanup action, binding watermark and pending transfer payload. Runtime
preparation returns a `NativeUnwindAction` (`RunCleanup`, `EnterTarget`,
`LeaveSegment`, or `MaterializeFallback`) to assembly. Rust returns normally
before assembly adjusts generated frames. A cleanup landing pad resumes the
cursor after normal completion; a new escaping transfer replaces it according
to Lisp semantics. Frames needed by cleanup remain live until that cleanup has
finished. The cursor is rooted before the first action is returned: even the
first `RunCleanup` may allocate. Reentry and suspended cleanup cursors are rooted
and nestable.

The compiler represents normal and exceptional successors explicitly through
lowering, T2 IR, optimization, liveness and emission, with equivalent T1 metadata.
Exception-live values and dynamic scope boundaries constrain motion, elimination,
inlining and register allocation. Native landing pads consume verified maps;
missing native destinations select the explicit fallback, never a guessed PC.

Static scope descriptions belong in code metadata. Runtime handler/restart/catch
records carry only information that is dynamic or needed for dynamic visibility.
Do not erase records merely because a scope did not throw during profiling.
Measure normal scope entry/exit instructions and allocations as well as throws.

Architecture emitters implement entry, helper veneers, capture and landing
stubs. Helper v2 adapters publish `NativeOutcome` only after their ordinary Rust
call returns. Each veneer handles exceptional status locally and branches to
the cold transfer stub. Direct native calls return only on normal completion;
they have no corresponding status test.

## Control flow and invariants

Normal path:

```
Rust -> segment entry -> native A -> native B -> return value to A -> Rust
```

Escaping transfer from a runtime helper:

```
native B -> Rust helper -> ordinary Rust return to assembly veneer
         -> capture/root selected transfer and consult native scope maps
         -> run required native cleanup, restoring bindings at each boundary
         -> enter native target with preserved values

If the destination lies outside the segment:
         -> finish required cleanup in this segment
         -> segment landing -> ordinary return to Rust -> propagate outward

If native continuation is unavailable or invalidated:
         -> materialize precise bytecode transfer state before retiring frames
         -> segment landing -> ordinary return to Rust -> bytecode propagation
```

1. **Never skip a live Rust or foreign frame.** No `longjmp` through Rust,
   Rust panic across `extern "C"`, or reuse of a signal-recovery epilogue as an
   arbitrary unwinder. A Rust helper may call more Lisp; that creates an inner
   segment which returns to the helper before the outer transfer proceeds.
2. **Signaling is not unwinding.** `HANDLER-BIND`, `SIGNAL`, `CERROR` and restarts
   can inspect or resume live dynamic state. Run handler search/signaling while
   the required context is live (or equivalently reified and rooted). Only a
   selected escaping transfer may retire frames. Do not reinterpret every
   `EgclError` as an unconditional throw.
3. **Cleanup executes exactly once and in order.** Preserve active handlers,
   catch/block/tag targets, special-binding depths and `UNWIND-PROTECT` actions.
   Handlers live at segment entry may have been established by the interpreter
   before OSR. Record that ownership and preserve their state; a segment cannot
   elide their scope transitions based on ordinary function-entry assumptions.
   Cleanup may allocate, yield, call native code, or supersede the original
   transfer. Restore bindings at their defined unwind points, not in one bulk
   reset before cleanup. A running cleanup's saved continuation also has a
   dynamic extent: retire it when an exit crosses its enclosing handler boundary,
   while preserving outer continuations and exits contained inside the cleanup.
   The bytecode fallback records this handler depth (`bliss-8m8ac`); omitting it
   replayed an outer cleanup suffix even when final return values were correct.
4. **GC sees all live state throughout the transition.** Capture current
   register roots and publish call-site maps before any preparation allocation
   or safepoint. Keep old maps valid until the reconstructed frames/packet are
   roots. Retire native root/frame links only afterward. Never keep an unrooted
   primary or secondary value across a collector/yield call.
5. **Preserve definitions and progress.** Reification uses the code version that
   was executing, including inlined callers, even after redefinition. Reuse
   deopt frame reconstruction, but distinguish transfer propagation from deopt
   resumption. Already-completed side effects and the throwing call do not run
   again.
6. **Handle resource exhaustion.** Stack guards need enough reserved space to
   publish the failure and reach the landing continuation. OOM must not require
   another successful allocation to begin unwinding. Reuse
   `egcl_stdlib::acquire_preallocated_storage_condition()` for the condition;
   reserve cursor/capture storage separately, since a preallocated condition
   alone does not supply unwind metadata storage. Test failed preparation,
   not only successful reconstruction.
7. **Cross-version calls are explicit.** Tag native code with its transfer ABI
   and target architecture/calling convention.
   Direct-call installation, OSR and cache invalidation verify both. Reject
   incompatible artifacts before executable installation; no backend may install
   another architecture's bytes even if an unrelated compilation guard passes.
   Legacy code stays behind a bridge segment until converted. No old helper can
   silently return a placeholder into unchecked new code.

The current implementation carries both `transfer_abi_version` and a target
architecture identifier on every installed `NativeCode` object. Direct native
calls are admitted only when both match the running backend. This is a
compatibility fence, not a completed old-to-new bridge: artifacts with an older
version or another architecture are conservatively kept on the checked/fallback
path until an explicit bridge is implemented.

The segment ABI can now be exercised for ordinary native invocations with
`EGCL_NATIVE_TRANSFER=1`. Eligible bytecode bodies are compiled into a
execution-owned transfer-code cache and entered through `invoke_native_segment`;
unsupported bodies, platforms, or hardening states fall back to the legacy
checked entry. This rollout switch remains opt-in while native Windows gates and
the full saved-image bridge are unfinished. Recursive bodies may now form the
outer segment, but a call made while a segment is active is refused by the
segment cache and uses the bounded legacy/native bridge. This prevents
pathological nested segment chains while preserving the existing stack-depth
guard; `bliss-49gcf` tracks a fully frame-aware recursive segment entry.

Every segment-cache result, including a declined compilation, retains a weak
reference to its bytecode definition. This reserves the Arc allocation used as
the cache key until the entry is dropped, preventing a replacement definition
from inheriting an old decline through address reuse. Rejected bytecode contents
and their Lisp constants are not retained; successful code keeps its existing
definition and GC-root ownership.

The process-level switch is deliberately inert until Lisp bootstrap has
finished. Bootstrap itself exercises ordinary bytecode helpers while evaluator
registries and image state are still being established; entering the segment
cache there can select a body whose source-loading assumptions are not yet
valid. The CLI regression gate covers this startup boundary. The benchmark
harness also clears the switch while loading each workload's definitions and
re-enables it immediately before validation, training, warmup, and timing, so
the report measures admitted native work rather than source-loading helpers.

The segment emitter's exceptional edges and landing pads are exercised by the
native POWER-MOD and caught-condition benchmarks. Each segment now performs a
root-safe safepoint and pending-signal poll before generated entry. Native loop
headers also use the helper-v2 veneer to poll for GC and asynchronous signals;
a slow poll transfers through the same capture and cleanup landing path as an
exceptional helper result, without a successful-call status check. Loop-header
roots are derived from split allocation ranges and synchronized into the
activation shadow area before the poll, then restored after relocation. Loops
whose root maps cannot be proven, unsupported platforms, and hardening failures
conservatively use the legacy checked entry. Long straight-line regions also
receive bounded polls at fixed source-instruction intervals with the same
precise shadow synchronization. `bliss-shih7.6` remains open for the full
fiber/foreign callback matrix and platform-specific signal/deadline gates.

## Polling independently of exceptions

Remove the signal/GC responsibility from ordinary return checks only after new
poll sites are proven. Return safepoint polls are not inherently forbidden:
[HotSpot also supports return polling](https://github.com/openjdk/jdk/blob/master/src/hotspot/share/runtime/sharedRuntime.cpp#L621).
The prohibited cost is a mandatory transfer helper/status test after every
successful native call. Start with a cheap inline poll-word test and a cold slow
path at root-safe function entries, loop back-edges and bounded straight-line
intervals. Entry coverage must include every recursive cycle, including
Lisp-to-Python-to-Lisp recursion through `PY:EXPORT` callback segment entries. A slow poll may
collect/yield only after live references are published; a delivered interrupt
uses the new transfer path.

Keep the existing process signal epoch, per-execution ownership and deadline
semantics. Preserve blocked-foreign-call protocols. Lisp poll sites cannot
interrupt Python while no Lisp code runs: `bliss-ziuwp` requires a foreign-runtime
interruption protocol, not merely additional Lisp polls. Verify signal response and
GC rendezvous deadlines from the existing tests on recursive, looping and
straight-line workloads. The known SIGINT regression (`bliss-mhuai`) is a gate
prerequisite, not an excuse to waive signal tests.

A protected-page read remains a possible implementation of the fast poll.
The current global GC page is not itself a complete per-execution interrupt and
error protocol; page ownership, fault-PC maps and reason clearing must be
specified and measured before replacing the poll word. No polling change may
reuse today's unrooted return-value save area as a GC safepoint.

## Implementation sequence and gates

The Beads contain task descriptions and acceptance criteria; the order is:

| Step | Bead | Deliverable and gate |
|---|---|---|
| 1 | bliss-shih7.1 | ABI/semantic contract and baseline transfer oracles. |
| 2 | bliss-shih7.2 | Verified root and exceptional-continuation metadata; real relocation tests. |
| 3 | bliss-shih7.3 | SysV/Win64 segment trampolines; register, destructor and fault-recovery probes. |
| 4 | bliss-shih7.4 | Versioned runtime helper outcomes; complete helper inventory checks. |
| 5 | bliss-shih7.5 | Bytecode fallback milestone; cleanup, handlers, restarts and no-replay tests. |
| 6 | bliss-shih7.6 | Root-safe independent polling; signal, deadline, preemption and GC liveness gates. |
| 7 | bliss-shih7.7 | Fiber migration and nested Rust/foreign callback ownership tests. |
| 8 | bliss-shih7.8 | Enable mapped new-ABI T1/T2 code and remove checks after successful native calls. |
| 9 | bliss-shih7.9 | Controlled release comparisons, instruction counts, regression gates and documentation. |
| 10 | bliss-shih7.10 | AArch64/s390x adapters after host stabilization; PPC follows as the next ELFv2 adapter milestone. |

Required additions to the sequence (Beads dependencies are authoritative):

| Bead | Deliverable and dependencies |
|---|---|
| bliss-shih7.11 | Explicit exceptional compiler edges; follows step 1 and precedes step 2. |
| bliss-shih7.12 | Native handler/cleanup dispatch; follows fallback step 5 and gates fiber integration step 7. |
| bliss-shih7.13 | Direct local exits and cheap protected-scope setup; follows native dispatch and compiler edges, gates activation step 8. |

Steps 2 and 3 follow the contract; step 4 needs both. The fallback milestone
unblocks independent polling work, but native dispatch and local-exit/scope work
remain required for the final endpoint. A limited activation milestone may
remove successful-return checks using verified bytecode fallback after steps
1–6 plus boundary/fiber coverage from step 7. It must be labeled intermediate
and report fallback costs. Step 8 final activation still requires native dispatch,
step 7 and local-exit/scope work. Steps 9 and 10 follow final activation.
Keep unsupported platforms on the old ABI until their own gates pass.
QEMU proves functional behavior only; native hardware is required for platform
performance claims. x86-64 Linux and Windows are the first delivery milestone.
The portability gate now builds AArch64 and s390x with the cross-image-compatible
Rust 1.93.0 override and runs them under QEMU. Both pass `portable_os`,
interpreter/T0/T2, GC-stress and image round-trip probes; s390x also passes its
native T1/T2, OSR, moving-GC, signal and JIT smoke probes. These results validate
the legacy ABI boundary, not native transfer activation.

The full Lisp segment entry is compiled and activated only on x86-64 Linux.
PPC64LE now has an opt-in, deliberately narrow entry for allocation-free,
scope-free, deopt-free bodies; calls, speculative guards, loops requiring polls,
handlers, cleanup and other unsupported shapes decline to the checked ABI. The
x86-64 Windows build
deliberately routes ordinary invocations through the checked legacy ABI while
the native Windows mitigation and SEH gates remain unproven. These are rollout
boundaries, not claims of full native transfer coverage; each platform's
release gate must be green before broadening its segment cache.

The rollout gate requires:

* Disassembly proves no transfer helper call or status branch after successful
  native self calls in warmed Fibonacci; required entry/loop polls remain.
* Same source and inputs, verified T2 before/after every sample, correct
  checksums and recorded deopts. Archive paired timings, retired instructions,
  code size and binary/source hashes in the existing HTML report.
* Supported compiled transfer workloads remain native: zero transfer-triggered
  deopts, checked separately from deliberate fallback tests. Disassembly proves
  direct branches for eligible local exits. Forced fallback preserves semantics.
* Compare protected-scope entry/exit without throwing, instructions, allocations
  and code/metadata size as well as transfer cost. Publish SBCL comparisons for
  normal and exceptional paths; Fibonacci alone cannot establish ABI parity.
* Measure nonthrowing, throwing, handler-heavy, cleanup-heavy, fiber and callback
  workloads. There must be lower instruction cost on the normal path and no
  unexplained regression in the other paths. An SBCL win is a measurement, not
  an assumption or acceptance shortcut.
* Forced moving GC/poison, nested cleanup transfers, multiple values, migration,
  OSR, invalidation, legacy bridges and Windows ABI tests pass. Verify actual
  relocation: retain a raw copy of a rooted heap value before the allocation
  under test, require the rooted value to change address, then check its content
  and aliases. Never dereference the stale copy. Run with GC poison and use
  `EGCL_GC_REGION_LOG=1` as supporting evidence; a clean stress run alone does
  not prove movement. Test zero, one and several values across cleanup.
* Run workspace gates and record unrelated baseline failures explicitly.

The checked-in [native-transfer benchmark report](../../benchmarks/sample-report/index.html)
contains the current five-sample T2/instruction measurements, SBCL comparison,
and a same-protocol EGCL comparison against baseline commit `737be001`.
`2c84d2e1` remains the counted pending-error fast-path baseline for the ABI
design, while `737be001` is the executable baseline recorded by the report.
The recorded baseline/current binary hashes and raw JSON are part of the report
provenance. The comparison shows the current native path improving Fibonacci
while POWER-MOD and caught conditions remain slower; those regressions are
tracked as performance gaps rather than hidden by the Fibonacci result.

This plan does not remove the interpreter, replace Rust's unwinder, promise
zero overhead for signal polling, or change the Fibonacci algorithm.

## Segment boundary implementation checkpoint

The runtime provides x86-64 Linux SysV and Windows Win64 segment adapters in
`crates/egcl-rt/src/native_transfer.rs` and its `win64` module. The Rust wrapper pins the anchor,
records execution ownership and EgclStack watermarks, and restores the previous
anchor with a Rust guard. The private assembly entry uses the explicit outcome
out parameter above; the Rust wrapper returns `Result<NativeOutcome,
SegmentUnavailable>`. Normal return, transfer and deopt exits restore the six
nonvolatile integer registers, MXCSR and x87 control word. Cold exits reset the
host stack only after Rust helpers have returned. Nested entries keep their
enclosing Rust frames alive.

Activation currently requires a successful Linux shadow-stack status query
reporting no enabled features. Unknown status and enabled shadow stacks refuse
entry; no mitigation is disabled. Indirect assembly entries and the landing
continuation have `ENDBR64`. Win64 entry additionally preserves RDI, RSI and
XMM6–XMM15, supplies the callee's 32-byte home area, and carries assembler-generated
SEH unwind metadata. It requires successful CFG and user-shadow-stack policy
queries with no enabled flags. Query output starts with an invalid sentinel;
success without a written output does not grant permission. Other targets refuse
this adapter.

Assembly probes cover register/control-word restoration, stack alignment,
anchor invalidation, ownership/watermarks, nested entries and Rust destructor
counts. The SysV entry has DWARF frame descriptions for every prologue and
epilogue stack adjustment. A backtrace probe crosses it from a Rust helper
through an assembly fixture that clobbers all six nonvolatile integer registers.
This proves stack walking through the adapter; emitted JIT frames still need
their own metadata, and it does not authorize Rust panic unwinding across the
native ABI. Separate probes cause real null-page and EgclStack guard faults,
then use the existing signal recovery targets to return through a Rust helper
and the segment landing. They verify fault classification, destructor execution,
unchanged EgclStack watermarks and restoration of enclosing recovery targets.

This is boundary infrastructure, not activation: generated Lisp calls
still use the existing ABI and successful-return checks. Transfer payload
rooting, cleanup/root retirement, native dispatch, actual JIT integration of the
fault-recovery/unwind gates, fiber migration and native-Windows execution gates remain
required before rollout. The
adapter records EgclStack watermarks but does not restore them itself.

### Generated helper outcome adapter

`t2::native_transfer::emit_helper_veneer` emits an x86-64 Linux SysV adapter for
a typed helper-v2 function `(request, outcome_out)`. The Rust helper writes an
explicit NativeOutcome and returns normally. The adapter tests only the exit
field: successful values, including zero and NIL, return in RAX. Transfer and
Deopt tail-jump to a generated cold entry with `(request, value, exit)`, after
removing only the adapter's temporary frame. The generated caller needs no
post-call transfer check. Caller frames remain available for capture.

An executable fixture runs generated caller → generated adapter → Rust helper
→ generated cold route → segment landing, checking primary values, exit kinds,
Rust destructor execution, and whether the caller's success continuation ran.
Run the capability-dependent execution gate explicitly:

```text
cargo test -p egcl-compiler --test native_helper_veneer -- --include-ignored --nocapture
```

This is an adapter primitive, not production installation or a Lisp unwind
implementation. Its test uses immediate values and has no Lisp cleanup/root
retirement obligations. Production installation still needs typed request
layouts, retained targets, precise call-site capture maps, rooted payloads,
native dispatch/fallback, generated-frame unwind metadata, and converted helper
classes. The temporary outcome slot is not a GC root; the helper must root its
inputs across allocation, and the cold route must publish roots before its first
allocation. There are no calls or safepoints between reading the outcome and
entering the cold route. Win64 and other architecture veneers remain separate
work; the ordinary pipeline continues to reject Invoke emission.

The SysV `emit_capture_stub` now supplies the helper veneer's cold entry. It
saves RBX/RBP/R12–R15, the caller's pre-CALL stack pointer, return PC and helper
outcome before calling Rust preparation. Preparation returns normally; the stub
reloads any updated preserved registers and outcome, removes only its own
temporary frame, and tail-dispatches. Caller-saved GPRs and XMM registers are
already clobbered by the helper, so exception-live values require preserved
registers or spill homes. The capture image itself does not register GC roots.

`native_capture_sysv` executes a generated caller and both adapters, copies an
actual register root and caller-stack root into TransferSnapshot, roots the
transfer payload, forces relocation during reconstruction, writes the relocated
homes back, and reaches the segment landing. It checks all six preserved
registers, exact return PC, an unboxed double spill, updated native register/stack
roots, payload identity and both Rust destructor counts. It covers Transfer and
Deopt outcomes, including GC stress/poison, with the explicit gate:

```text
cargo test -p egcl-compiler --test native_capture_sysv -- --include-ignored
```

The fixture uses checked `SysvCaptureLocation` recipes for preserved registers
and bounded spill slots, including temporary call-stack adjustments. It tests
both an unchanged body SP and 16 bytes of temporary call space. Snapshot
writeback preserves raw float/integer representations while updating relocated
tagged references. Volatile registers and invalid stack offsets are rejected.
The fixture still supplies its known frame layout and preparation callback.
Production emission still needs installed recipes, retained code/PC lookup,
payload/cursor preparation, cleanup/target dispatch and unwind metadata. This
gate does not activate ordinary Lisp Invoke emission or remove its return checks.

`transfer_sites::SysvTransferTable` now binds emitted return offsets to checked
logical maps and physical capture recipes. Lookup is allocation-free and matches
the exact PC within the owning code range; it never substitutes a nearby call.
Construction rejects duplicate/out-of-range offsets, inconsistent logical frame
dimensions/origins, unavailable registers, invalid stack slots, and missing roots.
Execution-owned snapshots borrow their checked site and refuse capture/writeback
at another return PC. A rejected recapture invalidates the previous snapshot.
The executable fixture makes a normal call followed by an exceptional call, with
no caller status check between them, and selects the second site's map using
the captured return PC. Transfer and Deopt both preserve moved roots and raw
spills with and without temporary stack adjustment. This verifies the lookup
boundary; production code ownership, complete emitter site coverage, ABI checks,
and dispatcher integration are still required before activation.

Capture also distinguishes a native home from its canonical GC shadow. A helper
can collect and then transfer without executing the caller's normal shadow-root
restoration, leaving saved registers/spills with stale addresses. Checked sites
therefore carry bounded activation shadow slots for synchronized tagged roots.
`capture_from_activation` reads those updated slots, reads raw words from their
native homes, and rejects missing activation storage. Writeback repairs the
native homes. The activation must remain rooted until its frame is retired.
The execution gate forces GC inside the helper, proves that the shadow moved
while the native register stayed stale, then collects again during reconstruction
and checks repaired registers, stack roots and payload identity. It does not
assume that an object already moved/tenured by the helper must move a second time.
Production emission must derive shadow mappings from the same ordered root list
used by its pre-call synchronization; omitted mappings must never be guessed.

The opt-in x86 Linux `emit_framed_transfers` entry now emits Invoke through the
rich emitter, using its actual allocation, final frame homes, root synchronization,
result storage and normal-edge moves. A 32-byte `TransferCallRequest` passes the
symbol, arity, rooted argument slice and owning activation to a helper-v2 veneer.
The emitter records the exact post-CALL offset and stack adjustment, derives
canonical shadow mappings from its synchronization list, and rejects an unmapped
potentially moving root or an unconsumed capture map. Invoke arguments may spill;
requiring all arguments in registers reproduced a regalloc2 panic at eight args.
Legacy emitter entry points still reject Invoke.

`native_invoke_emit` executes bytecode-built functions through the generated code,
not a hand-written caller. Its explicit capability gate covers zero, one, four and
eight arguments, normal return/Transfer/Deopt, collection inside both helpers,
rooted argument slices, preserved logical locals/stack, exact second-call recovery,
normal results distinct from the original argument, and Rust destructor counts:

```text
cargo test -p egcl-compiler --test native_invoke_emit -- --include-ignored
```

Actual Lisp-source lowering also reaches this emitter in the CLI unit gate.
This entry remains an integration path rather than production activation: it
refuses unboxed values, OSR, guards and other helper classes until their contracts
are connected; helper argument slices currently contain tagged values only.
The fixture supplies rooted activation storage, per-execution snapshot reservation
and cold dispatch. Full runtime helper coverage, code/definition retention,
installation ABI checks, native cleanup/handlers and polling remain required.

The CLI now supplies `c2i_call_legacy_v2`, an explicit compatibility bridge to
the existing Lisp dispatcher. Both ABIs share Result-based dispatch; the bridge
publishes Returned or Transfer only after callee Rust frames finish. Pending
errors and THROW multiple values remain rooted in the execution-owned legacy
storage until cold preparation takes ownership. An already-pending error prevents
another call from executing side effects. Nested legacy deoptimization completes
inside the callee and returns its final value, rather than requesting a deopt of
the new caller.

The CLI `native_transfer_tests` exercise real Lisp callees with zero and multiple
values, eight arguments, errors, THROW and first-error preservation. The explicit
capability gate compiles a caller from Lisp source, emits both Invoke sites,
executes those sites through the real bridge, and captures the second call on
error. It checks relocated locals/operands, multiple values, normal Rust drops,
and side effects/UNWIND-PROTECT cleanup occurring exactly once. All three tests
also run with `EGCL_GC_STRESS=1 EGCL_GC_POISON=1`:

```text
cargo test -p egcl --lib native_v2_bridge -- --include-ignored
```

This is still an opt-in integration test. The callee cleanup runs through the
existing runtime; it does not prove native cleanup landing pads. The test cold
dispatcher leaves the segment and inspects the pending transfer, rather than
resuming a reconstructed caller in bytecode. Ordinary native installation and
its successful-return checks remain unchanged.

`native_transfer_entry::TransferCode` adds an opt-in runtime-owned entry for this
emitter. It retains the original bytecode and all linked executable buffers,
reserves and roots snapshots before invocation, and uses an execution-local
capture context with nested save/restore. Cold preparation copies the failed
site's values without allocation before assembly leaves the generated frame.
The entry then reconstructs the logical activation and calls `initiate_unwind`
before any bytecode execution. The failed call's PC is an origin, never a resume
instruction. Exact scope maps rebuild admitted local, non-escaping BLOCK and
TAGBODY records and pending UNWIND-PROTECT cleanups; missing dynamic identity or inherited state is refused at
compilation. Enclosing pending errors are rooted while saved, and lexical scopes,
native depth, environment pointer and fault-recovery settings are restored.

The `native_v2_fallback` execution gates run real Lisp success/error/THROW cases,
including redefining the caller inside its callee. They check retained-definition
recovery, multiple values, moving-GC relocation, unchanged enclosing stack and
pending error, and exactly-once calls and callee cleanup. The local-exit and
fallback cleanup/replacement cases are regular tests on the supported host and
also run under GC stress plus poison; the broader handler/fiber matrix remains
explicitly enabled with `--include-ignored` until its platform gates are closed.
This entry is not installed by ordinary tiering. General protected caller scopes, OSR,
closures, other helper classes, native cleanup/handler destinations, and emergency
reconstruction failure handling remain required. Output reconstruction still
allocates; allocation-free cold capture alone does not satisfy the OOM gate.

The transfer builder now admits UNWIND-PROTECT regions whose normal path ends
in a known non-returning call. Calls within that region retain their exact
pending cleanup scopes and cleanup-only locals. Cold cleanup bodies remain in
the retained bytecode; the runtime reconstructs their handlers and enters them
through the unwind driver. A real Lisp native caller with nested protected
regions verifies inner-before-outer cleanup exactly once, replacement THROW
with multiple values, and moving-GC preservation of a local used only by cleanup.
This proves exceptional bytecode cleanup fallback, not native cleanup execution.
The ordinary builder still refuses protected code. GO/RETURN-FROM crossing a
cleanup are explicitly refused rather
than silently branching past it; source-level tests cover both refusal cases.

Normal cleanup now has explicit transfer-SSA operations: `CleanupSave` preserves
the protected primary and complete runtime multiple-value state, while
`CleanupRestore` restores that tuple and produces the primary on the normal
resume edge. Both are effectful runtime boundaries with FrameState metadata;
their continuation identity names the cleanup and normal resume bytecodes.
The builder connects normal cleanup entry/return edges, including nested and
branched forms. V13 verifies matching ordered continuation stacks, consistent
joins, no abandoned values on normal return, and matching running-cleanup scopes
on exceptional exits. Source tests cover cleanup inside cleanup, dead-code
elimination preserving save/restore, and malformed identities and joins.
The opt-in SysV emitter now lowers these operations to normal-returning runtime
helpers. The owning invocation reserves and roots its continuation stack before
machine entry. Save retains the primary, zero/one/many-value state, and exact
handler depth; restore republishes the saved values after native cleanup code.
Calls made during cleanup retain their running-cleanup identities in the transfer
maps. Selected throws now enter native exceptional cleanup as described below;
fallback adopts the already-rooted continuations for unsupported outcomes and
unwinds them at their recorded handler depths, without replaying the failed call.
Legacy emission and transfer emission without explicit cleanup helpers still
refuse these operations.

Source-level execution tests cover nested normal cleanup, all multiple-value
shapes, forced moving GC during cleanup, and a replacement THROW crossing two
running cleanup continuations before executing the outer cleanup once. The
helper calls use the normal call-frame, clobber and shadow-root conventions;
they add no generated caller status test. These helpers do not execute Lisp,
yield, signal or collect. Multiple-value snapshots use a fallible reserve-and-
copy and turn reservation failure into a resumable propagation continuation;
the remaining dynamic handler/restart record allocations are still an ABI
installation gate.
Before native entry, the transfer path now reserves the execution-local
control-value map for the statically possible primary, multiple-value and
restart-argument records. Payload extraction finds existing secondary and
restart keys by borrowed lookup, without formatting temporary Rust strings. The
cold capture path also reuses a pre-reserved frame-chain scratch vector, so
validating the native cluster stack after a transfer does not grow a Rust
container. Native catch and handler registration now uses fallible string,
token, and vector construction; restart names and captured exit stacks are
prepared before mutating the live restart stack, so a failed reservation cannot
leave a partial dynamic scope. Static restart clause bytecode is rooted in
per-entry templates and dynamic scopes share handles to those templates rather
than cloning a bytecode function after native entry.
The reserve failure is reported as `STORAGE-CONDITION`/`Oom` before generated
code runs. This removes two avoidable cold-path allocations; arbitrary cleanup
code can still allocate, and the full preallocated emergency storage path is
still required before ordinary ABI activation.
A real-fiber gate also runs native cleanup on one and four carrier threads,
observes at least two cleanup continuations suspended together, and collects
from outside their stacks. On resumption, distinct per-fiber answers, multiple
values, replacement THROW tokens, stack watermarks and native nesting depth must
remain correct. It additionally forces collection within each resumed cleanup
and verifies actual address relocation. This gate passes repeated stress/poison
runs; it does not assert carrier migration and does not replace the wider
fiber/foreign-boundary gates.

The fiber gate now also suspends exceptionally entered native cleanup, with a
pending THROW rooted across suspension, collection and a replacement THROW.
Ordinary tier installation and broader handler/foreign/migration gates remain
outstanding.

### Native-frame landing adapter

`emit_native_landing_stub` consumes an execution-owned `SysvNativeLanding`
packet after the capture stub has returned from Rust preparation and restored
its updated nonvolatile registers. It selects the destination frame's normal
body SP, places the primary in RAX, and tail-jumps to the native landing pad.
The original native frame remains live. The packet is a machine interface,
not a target-admission check: dispatch must validate retained code,
frame recipes, live-value homes, roots and segment ownership before selecting it.

An executable fixture proves normal Rust destructor completion before landing,
all six updated nonvolatile registers, stack-slot preservation, aligned calls
from the landing pad, and return through the original native frame and segment.
It exercises 0/16/32/64-byte temporary call areas, forces moving GC in the helper
and preparation, and dereferences relocated register/stack pointers after landing.
Run this capability gate explicitly with `cargo test -p egcl-compiler --test
native_landing_sysv -- --include-ignored`.

The opt-in Lisp transfer entry now selects this adapter for supported throws.
`SysvTransferTable::with_cleanup_landings` binds a compiler-selected
cold edge to an exact call site, checks the innermost local UNWIND-PROTECT identity,
and requires an in-bounds ENDBR64 entry in the supplied code bytes. Duplicate or
unknown call sites, inherited cleanup ownership and unaligned body-SP recipes
are rejected. Unlisted sites retain fallback. The checked site can construct a
same-frame landing packet without allocation, using its temporary call-area size;
it rejects the wrong captured PC, exit kind, stack alignment and address overflow.
This is metadata validation, not proof of runtime ownership: the dispatcher still
must retain that exact code, establish current-segment/frame ownership, root the
pending continuation and repair native homes before selecting the packet.

`build_from_bytecode_for_native_cleanups` constructs exceptional cleanup
predecessors **before** SSA sealing and phi simplification. Calls split into normal
and cold blocks; the cold edge truncates the operand stack to the selected
UNWIND-PROTECT's saved depth. Locals merge with the normal cleanup entry, so an
assignment after a throwing call cannot replace the exceptional pre-call value.
`CleanupLanding` retains the entry FrameState and continuation identity.
Cleanup completion is an Invoke with `CleanupContinuation` metadata: only its
normal edge pops/restores the saved answer, while its cold edge resumes the
pending transfer. NlxTransfer may name a verified cleanup successor or leave the
native CFG for fallback. V13 verifies landing identity, stack depth, retained
running continuations and the distinct completion routes.

Source tests cover nested normal/exceptional cleanup, ERROR-only protected paths,
zero/multiple values, enclosing operand-stack prefixes and loop back edges,
including DCE. Phi replacement also visits synthetic call-edge blocks; otherwise
removing a trivial phi left dangling arguments on those new edges. Intrinsic
expansion is conservatively deferred in this builder so structural throwing
predecessors cannot disappear while SSA is being built. Ordinary tiering keeps
its existing builder; the opt-in transfer runtime consumes this new CFG.

`emit_framed_native_cleanups` emits ENDBR64 cold entries followed by parallel
phi-home moves into the selected cleanup. The machine layer separates the cold
FrameState use from its operand-free branch, as required by regalloc2 when the
cleanup has multiple predecessors. CleanupLanding itself is a liveness marker.
Admission currently requires each source Invoke's cold block to contain only
NlxTransfer, with no incoming block arguments. A transform that adds work before
that terminator is rejected until landing maps describe execution of the full
edge; otherwise a native jump could silently skip that work.
Cleanup completion uses a typed 32-byte helper-v2 request: normal completion
restores the saved primary and multiple values, while exceptional completion
retains its rooted continuation until cold capture consumes the pre-op map.
Direct THROW is also an Invoke, with rooted tag/primary arguments and a reserved
request discriminator outside the u32 symbol-index domain. Its helper preserves
multiple values and returns through Rust before dispatch. SETQ's ClearMv uses
the existing nonallocating reset helper; no successful-return check is added.

The runtime retains code, maps and adapters for the invocation, captures exact
source homes, and checks the current Lisp frame and active segment. For a selected
throw to a live outer CATCH, it repairs native homes from canonical rooted shadows,
parks the pending continuation before dispatch, and enters the cold edge. Running
cleanup continuations are retired only at crossed handler depths. Completion
continues through outer native cleanups or reconstructs bytecode for the remaining
unwind; it never repeats the failed call or the completed cleanup. Raw errors
retain fallback so signaling and restart search still happen in the live context.
Native CATCH and HANDLER-CASE destinations are admitted by the opt-in entry;
HANDLER-BIND and RESTART-CASE now have activation-owned registration records and
exact bytecode fallback, but remain fallback barriers rather than native landing
destinations. Inherited OSR scope state is still rejected at this entry.

The compiler execution fixture verifies normal/exceptional phi values after
actual moving GC, and both completion outcomes without any bytecode evaluator.
Lisp gates count native cleanup entries for nested throws, direct THROW,
replacement transfers and fiber suspension. A replacement to a different catch
also checks retirement of the superseded token's payload roots.

Paused cleanup continuations now privately own their control payloads
(`bliss-shih7.12.4`). Catch/block tokens name destinations, so they cannot also
identify individual pending transfers: a second throw to the same catch may be
redirected inside cleanup while the original throw must still complete. Native,
bytecode and tree-walker cleanup move primary/secondary values out of the shared
token map, root them while paused, and restore them only when resuming that
transfer. Dropping a superseded continuation cannot erase a newer transfer's
values. Selected HANDLER-CASE conditions and restart arguments receive the same
ownership; local bytecode RETURN-FROM
retains its full multiple-value state. Tree-walker cleanup also roots the original
error datum while running allocating cleanup.

Regression gates cover reentrant throws with zero/one/many values, conditions, restart
arguments, local-return multiple values, and actual relocation during native
cleanup and error propagation. These changes do not activate the ABI for ordinary
tiers. Emergency allocation guarantees (`bliss-shih7.12.3`) still apply to Rust
key construction, map restoration and multiple-value vector snapshots, alongside
the remaining ABI activation gates.

### Selecting the next native unwind action

`egcl-compiler::native_unwind` selects the next logical action from one retained
activation's ordered scope map (`bliss-shih7.12.5`). The runtime resolves a live
destination first; the selector neither signals conditions nor searches Lisp
names. A local target is identified by its establishing bytecode PC within that
exact activation, not its resume address. It selects an intervening cleanup before
the target, never an outer cleanup beyond the selected destination. Existing
native cleanup dispatch uses the selector for transfers to an outer activation.

Inherited OSR records and unsupported dynamic restoration select fallback.
Running cleanup continuations do not count as installed handlers. A local tagbody
alone is insufficient evidence that its dynamic registration can be discarded;
the scope map does not encode whether NamedTag exposed it to a closure. EnterTarget
is only a logical action: native registration, destination-specific CFG/phi edges,
landing validation and state restoration must still exist before a jump is legal.
Native CATCH execution is tracked in `bliss-shih7.12.6`; the selector does not claim
that capability or enable ordinary ABI installation.

Catch bindings retain the actual tagged Lisp object as a GC root, and THROW
compares tags by object identity across bytecode, tree-walking and native helper
paths (`bliss-shih7.12.6.2`). Printed tag text is only diagnostic: distinct lists,
strings or uninterned symbols may print alike without naming the same catch.
This representation is also used by native catch registration;
moving GC must repair the saved tag before dynamic destination search.

The opt-in entry executes CATCH registration and normal retirement through
helper-v2 (`bliss-shih7.12.6.1`). Catch requests carry an establishing BCP in a
reserved request class outside the symbol-index domain; both preserve multiple
values. The activation retains the generated control token, while Env roots the
dynamic tag. A scope guard retires this invocation's registrations on exit without
discarding enclosing catches. Bytecode fallback reconstructs handlers with the
same tokens, so it delivers the already-selected throw without replaying its call.
The native selector runs only cleanups inside the selected catch boundary.

The next opt-in extension adds native catch delivery (`bliss-shih7.12.6.3`).
Catch destinations enter the exceptional CFG before SSA sealing, with a separate
landing block for each source site and selected catch. Its noncollecting helper
produces the catch's primary value, and the resulting edge joins ordinary
completion with the correct locals and enclosing operand-stack prefix. Emission
binds each landing to an exact return PC, establishing BCP, resume BCP and checked
machine entry; it cannot jump past an intervening cleanup.

Runtime preparation plans explicit retirement of crossed local catches, validates
the destination and live registrations before changing them, repairs native homes,
and moves the selected values into a rooted payload. Rust returns before assembly
enters the landing, whose helper consumes that payload and restores multiple
values. Missing destinations retain the original registrations for bytecode
fallback. Test counters distinguish actual catch landings from fallback, so a
correct result alone cannot disguise interpreter execution.

The opt-in tests exercise same-tag shadowing, crossed catches, cleanup ordering
and replacement throws, modified and branch-merged locals, enclosing operand-stack
values, zero/one/many returned values, and actual moving-GC relocation. They require
native catch execution and zero fallback for supported local destinations.
A test-only unavailable-destination seam also withholds one selected catch landing
while retaining its exact source map and live registrations. The regression
requires one bytecode fallback, no native catch entry, no replay of the throwing
call, correct cleanup order, preserved locals/multiple values and an unchanged
enclosing catch, with actual tag relocation under GC stress. It covers both
immediate fallback and fallback after an intervening native cleanup.
This extension does not enable ordinary ABI installation. The compiler still
declines direct exits requiring catch unregistration, and the opt-in transfer
entry does not yet start from an OSR continuation. The existing T0-to-native OSR
path does preserve interpreter-owned handlers: `run_native_osr` keeps the live
activation stacks, establishes a frame-scoped fault-recovery window, and hands
pending signals back to the interpreter before restoring the outer native state.
Host allocation failure during token construction and payload restoration remains
part of `bliss-shih7.12.3`.

### Opt-in native HANDLER-CASE delivery

The SysV transfer entry also registers live HANDLER-CASE clusters with original
activation-owned clause tokens and rooted condition-cluster frames. Raw errors
are signaled while that dynamic context is still installed; selecting a clause
then begins unwinding. The builder creates per-source, per-clause exceptional
edges before sealing SSA. A checked HandlerLanding defines the condition local
at the clause's bytecode destination, with the enclosing operand stack restored.
The transfer table binds that destination to the exact source scope, clause,
return PC and native stack recipe.

Native preparation validates the owned cluster-frame chain and all registrations
before retirement. It roots the selected condition across intervening native
cleanup, retires crossed handler clusters in order, and delivers the condition
only after Rust returns to the assembly dispatcher. A missing native destination
retains the original cluster frames and tokens for bytecode transfer propagation;
the failed call is not replayed. Normal completion unregisters the same records.

The handler regressions distinguish native clause entry from fallback, check
nested and declined clauses, replacement errors, collecting cleanup, multiple
values and actual condition-datum relocation. Fiber tests suspend several pending
conditions together, collect while suspended, and check each fiber's result and
restored stack/segment state. A test-only missing-destination path exercises both
immediate fallback and fallback after native cleanup.

Already-signaled errors and transfers to a validated live enclosing restart can
also run checked local native cleanup before materializing the outer boundary.
The live-signaling regression requires a returning handler to run exactly once,
before cleanup, and preserves the condition or restart arguments through moving
GC. The outer handler/restart registration remains installed for its owner to
consume. HANDLER-BIND and RESTART-CASE setup/retirement now uses helper-v2
requests with a single ordered cluster-frame guard, and fallback reconstructs
their interpreter records without replaying the failed call. They do not yet
provide native local restart landing.
The consumer test invokes the retained restart body after removing its bindings,
checks that cleanup has finished, and preserves multiple values through another
collection. A replacement-error regression also verifies that raw errors raised
by a handler are signaled while its cluster is hidden: only older clusters see
the new error. Shared signaling roots the condition, copied entries and hidden
cluster tail throughout callbacks and nested signaling.

This remains opt-in infrastructure. Inherited OSR scope admission, complete
emergency allocation and production ABI installation still have separate gates;
ordinary installed native functions retain their checks.

### Direct local exits

When a `RETURN-FROM` or `GO` crosses only locally established lexical block or
tagbody metadata, the native builder lowers it to the ordinary SSA branch. The
branch restores the destination operand-stack depth and carries the returned
value through the same merge as normal control flow; it does not enter the
generic transfer helper. A regular test with direct block and tagbody exits
exercises the installed native path under moving-GC stress. Companion fallback
tests cover replacement cleanup, error propagation and no replay of the original
definition. The older rejection rule remains: an exit crossing `UNWIND-PROTECT`,
a catch, or another dynamic registration is not converted to a branch, because
it must run cleanup or preserve the live registration for fallback.

OSR scope maps mark interpreter-established records as `Inherited`. The native
unwind selector refuses to retire or branch across inherited records, even when
the lexical target itself is a block or tagbody. Such an exit remains a bytecode
fallback until the activation can prove ownership of every crossed record.

### Windows validation and Wine limits

Wine remains a fast regression environment for Windows functionality. It does
not establish Windows performance or hardware-mitigation behavior. Wine 11.0
(Staging) here returns success from `GetProcessMitigationPolicy` for CFG and
user shadow stacks without changing the output DWORD. The sentinel check
therefore refuses segment execution. This does not invalidate independently
exercised pathname, I/O or unwind-table tests.
The behavior matches the [Wine 11.0 source stub](https://github.com/wine-mirror/wine/blob/wine-11.0/dlls/kernelbase/process.c#L828-L836),
which returns success without writing the buffer. Repairing that query requires
accurate policy reporting or an unsupported-query error; arbitrary zero output
would conceal the missing information.

The Win64 machine probe compiles the same instructions on Linux with Rust's
`win64` calling convention and checks host shadow-stack compatibility before
execution. It covers all eight integer nonvolatiles, all 128 bits of XMM6–XMM15,
FP controls, home space, alignment, Rust helper destructors and all three exit
kinds. Windows `RtlVirtualUnwind` separately checks the actual COFF metadata at
the body and landing continuation under Wine using a synthetic saved frame.
Neither substitutes for running the public entry on native Windows.

Capability-dependent execution tests are explicitly ignored by default. Selecting
them with `--include-ignored` makes unknown or incompatible mitigation state a
failure, not a successful no-op. On native Windows, the release gate must run:

```text
cargo test -p egcl-rt --target x86_64-pc-windows-msvc --lib native_transfer -- --include-ignored --nocapture
cargo test -p egcl-rt --target x86_64-pc-windows-msvc --test native_segment_windows -- --include-ignored --nocapture
```

Report machine execution, refusal behavior, ignored tests and metadata checks
separately. Never disable mitigations to make a gate pass. Native Windows CI and
the remaining SEH/mitigation gates are tracked in `bliss-shih7.14` and gate final
activation. The boundary follows Microsoft's [x64 prologue/epilogue rules](https://learn.microsoft.com/en-us/cpp/build/prolog-and-epilog)
and [process mitigation query contract](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocessmitigationpolicy).
The checked-in `windows-native-transfer` job in `.github/workflows/ci.yml` runs
the native Windows execution test with `--include-ignored`; the Linux/Wine job
continues to provide only cross-build and regression coverage.

The cross-target `native_segment_windows` test was also run under Wine outside
the sandbox. Its refusal test passed, while the execution test failed with
"mitigation state unavailable or incompatible". That is the intended refusal
under Wine and confirms that the sentinel policy does not silently execute the
segment without verified mitigations.

The full `native_transfer_cli` integration target also passes 8/8 under the
Windows GNU target with Wine. This exercises T2 transfers, OSR, fibers,
multiple values, and cleanup replacement in the cross-built runtime; it remains
separate from native Windows SEH and mitigation evidence.

## Baseline contract oracles and boundary inventory

`crates/egcl/tests/native_transfer_cli.rs` verifies real T1/T2 entries, direct
calls, OSR, escaping side effects, multiple values, fiber yields and native
reentry through resumable restarts and replacing cleanup transfers. The shared
`crates/egcl/tests/fixtures/native-transfer-osr.lisp` is also consumed by
`scripts/s390x-jit-smoke.py`: expired catch, unwinding GO, and outer GO run with
OSR traps and GC stress. Extend these oracles rather than duplicating them.

The following existing sites must be covered when installing the segment ABI:

| Boundary/state | Current implementation and required treatment |
|---|---|
| Full native invocation | `run_native`: parameter/root frame, lexical scope guard, depth guard, native environment, pending error save/restore, fault recovery IPs, active code and MV context. Anchor before machine entry; restore each exactly once. |
| T0 OSR | `run_native_osr`: reuses the interpreter frame and its live handlers. Anchor records inherited scope ownership and frame watermark. |
| T1-to-T2 OSR | `c2i_t1_backedge`: Rust helper directly enters a T2 alternate entry; this is a nested segment, not a native-only jump across the helper. Preserve deopt operand slots. |
| Calls into Lisp/runtime | `c2i_call`, `c2i_call_slice`, `c2i_call_builtin`, `c2i_call_builtin_regs`, `c2i_call_args`; reentrant calls may establish inner segments. Helpers return normally before their veneer transfers. |
| Environment/allocation helpers | `c2i_load_global`, `c2i_load_function`, environment load/store/define, host evaluation/closure/cons creation and MV helpers. Inventory their allocation, signaling and transfer effects before classifying a helper as nonthrowing. |
| Existing transfer state | `NATIVE_ERROR`, `native_error_pending`, `c2i_transfer_pending`, `native_loop_should_exit`; replace checked legacy calls only behind versioned bridges. Preserve first-error behavior and nested save/restore during migration. |
| Deopt | `c2i_deopt`, `c2i_deopt_state`, `c2i_deopt_t2`, `NATIVE_DEOPT_RESUME`; keep transfer propagation distinct from ordinary resumption and preserve original code identity. |
| Fault recovery | `c2i_set_native_sigsegv_recovery`, architecture recovery stubs and recovery guards; only generated-frame faults may use generated-frame recovery. No generic unwind through Rust helpers. |
| Installation/lifetime | `NativeCode`, `OsrCode`, `ActiveNativeCode`, `publish_native`, `install_t2_completion`, `compile_t2_artifact` and retained direct callees; verify architecture plus ABI at every entry/cache/installation boundary. |

The ppc64le backend already has native T1/T2 compilation and ELFv2 foreign-call
support. The first ELFv2 segment slice is implemented in
`crates/egcl-rt/src/native_transfer/ppc64le.S`: its QEMU probe covers normal
return, direct transfer landing, anchor cleanup, and ELFv2 nonvolatile state.
The CLI also has a narrow opt-in PPC64LE entry in
`crates/egcl/src/cli/bytecode/native_transfer_entry_ppc64le.rs`; it enters the
real segment for deopt-free bodies with no calls or protected scopes and falls
back to the checked ABI otherwise. Full PPC deoptimization and fault-recovery
metadata, native handlers,
cleanup capture, loop polling and local-exit admission remain open under
`bliss-1rt.2`. Foreign callbacks and all architecture-specific entry stubs
remain in the final portability audit.

The AArch64 runtime now has the corresponding AAPCS64 segment enter/leave
boundary in `crates/egcl-rt/src/native_transfer/aarch64.rs`, with a QEMU probe
covering normal return, direct transfer, anchor cleanup and host-stack
restoration. The CLI has a narrow `EGCL_NATIVE_TRANSFER=1` entry for constant
and identity T2 bodies: it supplies the nonallocating multiple-values adapter,
publishes the active environment, and falls back before any Lisp call,
deoptimization, handler, cleanup, or loop-polling operation. This does not
claim general AArch64 activation; generated entry admission, deoptimization
metadata, fault recovery, handlers and cleanup capture remain target-specific
gates.

The s390x runtime now has the corresponding ELF64 segment enter/leave boundary
in `crates/egcl-rt/src/native_transfer/s390x.S`, with a QEMU probe covering
normal return, direct transfer, anchor cleanup and host-stack restoration. The
CLI now has the same narrow `EGCL_NATIVE_TRANSFER=1` entry for constant and
identity T2 bodies, using the nonallocating multiple-values adapter and
falling back before calls, deoptimization, handlers, cleanup, or loop polling.
General s390x activation remains gated on generated entry admission,
deoptimization metadata, fault recovery, handlers, cleanup capture and precise
native polling.
The separate s390x foreign-call adapter is also still pending: the System Z
scalar ABI independently allocates integer arguments in `r2`–`r6` and even
floating-point registers, with 32-bit overflow values using their ABI-specific
save-area offsets. The generic legacy eighteen-shape dispatcher cannot safely
stand in for that contract, so `bliss-0tazp` retains it until a generated adapter
and QEMU foreign-call tests exist.

Compiler work decomposition under `bliss-shih7.11`:

* `bliss-shih7.11.1`: Specify exceptional CFG and liveness invariants.
* `bliss-shih7.11.2`: Preserve exceptional state through T2 optimization and allocation.
* `bliss-shih7.11.3`: Implement the exceptional scope contract in T1 emission.

The CFG contract follows the baseline contract. T2 pass integration and T1
emission follow the CFG contract; the parent completes only after all three gates.

### Exceptional call IR foundation

`Opcode::Invoke` now describes distinct normal and exceptional CFG edges. Its
results are available only as arguments to its own normal edge; all downstream
uses name normal block parameters. `Function::make_call_exceptional` converts a
bytecode-built Call by splitting its block and rewriting downstream value/frame
state references. The verifier checks call effects, edge shape, pre-call frame
state and result availability. DCE keeps exceptional-only operands; inlining
maps result definitions before successor arguments.

This is compiler infrastructure, not activation of the native transfer ABI.
The ordinary installation path still emits Call. A separate
`build_from_bytecode_for_transfers` entry constructs Invoke edges automatically
for remaining calls after intrinsic expansion. Each exceptional edge reaches a
distinct NlxTransfer continuation with the pre-call FrameState and a TransferSite
snapshot of the source control scopes. That continuation starts propagation;
it does not authorize replay of the throwing bytecode. Verification requires
matching origin/capture metadata, transfer effects, and no normal successor.
Exception-only state remains live through DCE and reaches both the call and cold
machine instruction's root liveness. Machine lowering pairs an annotated INVOKE
with an operand-free INVOKE_ROUTES CFG marker. The call retains its arguments,
result definitions, pre-call FrameState and call clobbers; the marker records
normal/exceptional successors without introducing a successful-return status
test. Result arguments belong only to the normal edge. Register allocation owns
edge moves; lowering does not eagerly copy a normal result onto both paths.
Tests check allocation and stack-map locations for exception-only locals, their
survival across caller-saved clobbers, and a call lowered from actual Lisp source.

`transfer_map::lower_transfer_maps` connects each Invoke to its TransferSite,
ordered control scopes, and resolved FrameState slot descriptors. It reads the
call's own operand allocations rather than a function-wide register summary or
the later cold continuation's locations. Required tagged roots include inputs
inside reconstruction recipes; raw unboxed words are excluded. Missing call
allocations, missing/mismatched root maps, duplicate calls and uncomposed inlined
scope stacks are compilation errors. This reuses deopt descriptor lowering but
the recorded BCP is an unwind origin, never permission to replay the failed call.
These are pre-emission maps: machine instruction indices still need native PCs,
allocator locations still need physical save recipes, and logical function names
still need retained executing definitions before they form installable unwind
sites. Tests cover actual Lisp lowering as well as malformed-map rejection.

The rich x86 emitter can assign a permanent spill home to a value whose
allocator ranges move between locations. Its `x64_frame::select_frame_homes`
policy is now shared with transfer metadata rather than duplicated.
`lower_framed_transfer_maps` resolves descriptors and roots against those final
homes, materializes omitted immediate constants, and rejects moving heap
literals. Tests cover split ranges whose final home differs from the allocator
location and constants with no physical home. This is the map variant required
by rich emission; raw allocator maps alone do not describe its physical frame.

`transfer_capture::TransferSnapshot` reserves storage before native entry and
copies located words without Lisp allocation while the source frame is still
available. It scans only tagged saved words; raw integers/floats are preserved
as raw bits. Reconstruction roots the saved inputs and uses the shared deopt
slot evaluator, whose in-progress and completed output frames are now also
rooted. An explicit moving-GC regression failed before this fix with an earlier
local retaining its old heap address. It now checks changed input/result
addresses, completed frames surviving later boxing, and a raw word equal to an
old heap address remaining unchanged. This proves relocation across this capture
and reconstruction boundary, not yet across the complete emitted unwind path.

The caller must root returned frames and any pending transfer payload before
another allocation. The snapshot neither runs cleanup nor retires native root
links; those responsibilities remain with the dispatcher. Physical register/save
recipes still supply its capture reader. Snapshot reservation occurs before
entry, but output reconstruction still allocates and does not yet implement the
required emergency preparation-failure/OOM path.

This path covers bytecodes already modelled by the SSA builder and ordinary
function entry. Protected-bytecode SSA, inlined/OSR scope composition, pass-wide
integration, machine landing pads and unwind maps remain required. Existing
legacy emission entry points still refuse Invoke; the compact machine emitter explicitly rejects the new
call/route opcodes before applying ordinary-call allocation assumptions. This
prevents accidental emission using the old ABI.


### Ordered control-scope analysis

`control_scope::ScopeMap` tracks active BLOCK, TAGBODY and CATCH records at
bytecode positions. Joins require the same ordered records; lexical exits must
select an active target. GO retains its TAGBODY and reports intervening records
in unwind order; RETURN-FROM also removes its target BLOCK. T2 validates this
state before erasing handler instructions and uses it to resolve RETURN-FROM.

OSR analysis marks records already live at entry as inherited, and subsequent
pushes as local. Inherited BLOCK/CATCH exceptional resumes remain reachable even
though their establishing PUSH is outside the segment. Ownership disagreements
at joins conservatively refuse compilation. This metadata does not authorize
eliding inherited runtime records or change the existing OSR fallback policy.

UNWIND-PROTECT analysis distinguishes its installed handler, the normal-path
POP/EnterCleanupNormal handoff, and a running saved continuation. Cleanup entry
removes the installed handler. CleanupReturn reaches registered normal resumes;
resumed unwinds retain their dynamic target selection. A lexical exit's removed
records distinguish cleanups still to execute from running continuations that
must be superseded. Nested cleanups and OSR inside a running cleanup preserve
outer scope ownership. Malformed cleanup handoffs and orphan returns are refused.

The analysis is conservative: cold target edges establish possible scope state,
not proof that a cleanup will complete normally. Actual native control flow must
run required cleanups and respect a cleanup's replacement transfer before
reaching an exit target. T2 still declines cleanup bytecodes until that lowering
exists. Full-function analysis reads condition/restart tables: HANDLER-CASE
clause entry removes the selected cluster, HANDLER-BIND remains active while
signaling can return, and a delivered restart result resumes outside its
cluster. Inherited OSR entry preserves those exceptional destinations and outer
owners. The instruction-only API refuses table-dependent scopes instead of
guessing their destinations. Table indices, clause targets, condition slots,
and matching cluster pops are checked.

Special bindings and captured lexical environments are also ordered logical
records. Normal LET* teardown may remove bindings and child environments from
their separate runtime stacks; lexical exits report both in restoration order.
Cleanups retain any surrounding bindings until unwinding crosses them.
Extending Invoke construction to protected forms and attaching scope maps to
installed native code remain required before native dispatch; this analysis
does not enable emission of currently unsupported protected forms.

### Current validation boundary (2026-09-29)

The supported-host compiler and transfer integration gates currently pass:

```text
cargo test --locked -p egcl-compiler --lib                 153 passed
cargo test --locked -p egcl-compiler --test native_poll_abi 2 passed
cargo test --locked -p egcl --test native_transfer_cli     9 passed
cargo test --locked -p egcl --lib native_v2_ -- --ignored 18 passed
```

The native segment emitter passes a zero `c2i_transfer_pending` address. Its
eligible native-to-native returns therefore have no successful-return status
call or branch; independent loop and straight-line poll veneers remain in
place. The fallback and handler tests cover multiple values, inherited OSR
handlers, replacing cleanups, fiber yields, and moving-GC roots on x86-64
Linux. Narrow constant/identity segment entries also pass the AArch64 and
s390x QEMU portability gates.

T2 OSR segment entry now includes a regression for a call-free loop whose
back-edge executes the poll veneer. The emitter treats the injected poll as a
real Rust call when selecting its prologue, so the veneer enters Rust with the
required SysV stack alignment; the test passes normally and under
`EGCL_GC_STRESS=1 EGCL_GC_POISON=1`. The earlier crash in
`revalidate_current_segment` was an unaligned call frame, not a reason to keep
all OSR bodies on the checked ABI.

The process-level opt-in smoke also passes with forced moving GC and poison:
`EGCL_GC_STRESS=1 EGCL_GC_POISON=1 EGCL_NATIVE_TRANSFER=1` on a basic
`--no-init --eval` form. This specifically covers the bootstrap activation gate
that prevents the segment cache from running before `BOOT_COMPLETE`.

The legacy checked ABI remains a required compatibility path. It is still used
for unsupported bodies and platforms, nested native-to-Lisp calls, allocation
and signaling helpers, deoptimization, fault recovery, incomplete protected
scope lowering, foreign callbacks, and image/Windows boundaries. A full
GC-stress run of the largest T2 transfer fixture is currently dominated by
compilation under every-allocation collection and needs a bounded stress
fixture before it can serve as a completion gate. The remaining delivery work
is tracked in Beads (`bliss-shih7.6`, `bliss-shih7.14`, `bliss-0tazp`, and
`bliss-x7qyn`); these are deliberately not represented as successful native
coverage.

The Windows GNU cross-build also completes with `scripts/windows-port.sh build`,
and the resulting release executable runs a basic `--no-init --eval` smoke under
Wine. Wine is recorded only as a regression environment: the native Windows
SEH, CFG, and shadow-stack execution gates still require the checked-in
`windows-native-transfer` job on an actual Windows runner.

### Recursive segment calls

The opt-in x86-64 Linux segment entry can call its own retained definition
directly for fixed-arity, tagged bodies without dynamic control scopes or
captured environments. Each call owns a distinct precise `EgclStack` frame and
a linked retirement record on the native stack. A root-safe entry poll checks
for pending transfers and revalidates the carrier after migration. The existing
definition-generation guard keeps unchanged calls free of symbol lookup;
after invalidation, both the function cell and retained bytecode identity must
still match before a direct call is allowed. Redefinition, unbinding, and
function-cell aliases use ordinary named-call dispatch.

Normal returns retire exactly one activation without allocating or polling.
An escaping transfer retires the recursive chain after Rust helpers have
returned, preserving the pending condition or control payload. The successful
generated-to-generated return has no `c2i_transfer_pending` check. Depth and
native-stack bounds select the existing flat-interpreter fallback before a
child frame is published; a successful fallback returns its value to the
suspended native caller.

### Optimized segment guards

Scope-free tagged bodies run numeric speculation and the existing mid-end
before residual calls become `Invoke`. Supported fixnum arithmetic executes
in native code. Each guard serializes its precise `FrameState` through a
helper-v2 deoptimization veneer, retaining the exact original bytecode body
and selected speculation metadata independently of the legacy native registry.

The interpreter resumes the current activation at the guarded instruction.
Earlier effects are not replayed. It owns that precise frame during resumption;
on completion or a caught Rust panic, a nonallocating guard restores the full
original frame extent at the same address. Successful results return through
the generated epilogue to the suspended native caller. Failed continuations
use a distinct exact-PC-checked retirement path after Rust returns; they never
recapture the discarded pre-deopt SSA state. This avoids duplicate logical
backtrace frames and preserves recursive retirement records.

Repeated guard failures retire only the exact cached segment version. Resumed
bytecode samples the original body, so the next cache lookup recompiles from
the changed profile. Unsupported numeric types retain generic calls.

This remains an integration increment. Default activation, optimized guards
inside dynamic scopes, cross-function segment calls, and the full T1/OSR and
platform gates still require the corresponding continuation and ownership work.

### Mapped calls through existing linkage cells (bliss-shih7.16.1)

SysV mapped `CallTarget` sites now retain the existing execution-owned
`CallCell` and a generated adapter that loads its current slice entry on every
call. Cold resolution, warmed native/builtin dispatch, replacement, and
unbinding therefore use the same linkage state as legacy callers. The caller's
existing return-PC map and precise activation roots remain unchanged.

This is an explicit migration boundary. The adapter enables legacy fault
recovery for the cell invocation, then disables it before inspecting the
nonallocating legacy error predicate. It preserves the primary and multiple
values. An escaping error removes the adapter's temporary stack and tail-enters
the original mapped capture only after every legacy and Rust frame has returned.
Same-definition mapped recursion retains its separate precise-frame path.

This does not publish universal native entries or enable the segment ABI by
default. Arbitrary mapped native-to-native activations and their retirement
remain part of bliss-shih7.16; the legacy predicate belongs only to this temporary
boundary and must disappear when the cell itself exposes the universal ABI.

### Distinct mapped callees within a segment (2026-10-09)

The named-call veneer can now prepare a distinct mapped activation for the exact
fully promoted T2 target retained by its existing call cell. Preparation returns
through Rust before machine code calls the child. Cold/T0/T1 and unsupported
bodies keep the cell's existing boundary path and promotion accounting. Initial
admission covers fixed positional arguments, tagged values and scope-free
bodies; outer optimization and direct-self-recursion admission are unchanged.

Each child owns retained code, immutable site recipes, reserved snapshots and a
precise Lisp frame. An outer-segment root container retains stable child owners
across allocation and reentry. The call record saves machine continuation state;
its register copies are not GC roots. Canonical activation shadows remain the
source of relocated values on return and capture.

Normal retirement and escaping-child retirement both run from a caller-owned
veneer, so dropping the last child code owner cannot unmap a live return PC.
Cold retirement restores the actual preceding Lisp frame, including a caller's
recursive activation or handler cluster, then captures the original caller
Invoke. The existing caller handler/cleanup machinery processes the selected
transfer. A metadata failure still retires crossed recursive frames and retains
the original failure; it does not abandon their stack/depth accounting.

Regression coverage includes preparation success/decline/error, moving roots,
multiple values, caller handlers/restarts, mutual recursion and depth fallback,
active redefinition/unbinding, and injected child capture failure below recursive
callers. The initial slice left protected children, child deoptimization and
fiber migration to `bliss-shih7.16.2`. Universal entry publication and default ABI
acceptance remain separate outstanding gates.


### Protected mapped baseline callees (2026-10-09)

With the opt-in segment ABI enabled after bootstrap, ordinary T1 promotion can
install a tagged mapped baseline when the legacy emitter declines a protected
body. Installed code retains its real representation and transfer ABI version;
legacy direct-call gates reject that version. Admission currently requires fixed,
unconstrained positional parameters in local slots and no captured
environment. DISASSEMBLE identifies the tagged baseline explicitly.

A warmed call cell can enter this installed baseline in its caller's segment.
Outer and child activations share reserved scope storage and precise roots. If a
child cannot land its selected transfer natively, the caller-owned cold veneer
reconstructs its exact bytecode continuation. Frame and scope ownership move to
that rooted continuation before Lisp runs, and the native capture pointer is
masked during recovery. The child can finish locally and return values, or
propagate a selected transfer after its cleanups. Retirement releases child code
only while executing caller-owned storage. Scope-free children retain direct
native escape without unnecessary reconstruction.

Loop-header visits contribute sampled heat to the exact installed definition
and request background T2 compilation at the existing threshold. Header visits
include initial entries; they do not count every block in a cyclic region.
Redefinition prevents an old activation from heating its replacement. This
preserves future-call promotion; it does not add mapped OSR.

Dedicated tests cover local catch and handler landing, missing-destination
recovery followed by allocation and Lisp reentry, crossed handler/restart
clusters, local restarts, replacement transfers from cleanup, multiple values,
and observed root relocation. The parent remains open for child deoptimization
and fiber validation, general callable coverage, and universal default entry
publication.

### Suspended protected child continuations (2026-10-09)

The Linux x86-64 child-fiber tests exercise warmed installed mapped children
inside their caller's segment while suspending in the body, exceptional cleanup,
live handler/restart, cold bytecode recovery, and normal cleanup. They check the
exact child code owner (or masked capture during recovery), segment ownership on
resume, multiple values, exactly-once execution, and restored frames, depth and
dynamic scopes. One- and four-carrier cohorts distinguish ordinary suspension
from observed migration. Root relocation is reported by the owning fiber after
an external collection; the driver never reads another fiber's stack slots.

The off-by-default `native-transfer-test-hooks` feature exposes the existing
runtime refusal injection to these cross-crate tests. Once all children pause,
one selected child allocates a fresh rooted probe and arms its own next carrier
change. The scheduler refuses that destination, collects before requeue, and
resumes the child on its previously validated carrier. Tests require an actual
refusal, collection, relocation and resumption; unrelated fibers and production
platform capability detection are unaffected.

Run the platform-gated child tests on a supported host with:

```sh
scripts/egcl-limited.sh cargo test -p egcl --lib \
  --features native-transfer-test-hooks native_v2_protected_children \
  -- --ignored --test-threads=1
```

These tests cover protected baseline child suspension. The next slice extends
this cohort with suspension during optimized child deoptimization, described
below. Broader callable coverage and universal default publication remain
separate acceptance gates.

### Optimized mapped child deoptimization (2026-10-09)

Eligible distinct T2 children now compile speculative guards using their retained
body and deoptimization metadata. A failed guard resumes T0 on the child's
existing Lisp frame while masking its abandoned native capture. The saved frame
header is independently rooted until restoration, which completes before the
capture is reinstalled. Successful recovery returns to the native caller;
a selected transfer retires the child through the caller-owned cold veneer.
Neither path replays completed child effects or crosses a live Rust frame.

Resumed loop heat follows the callable saved in the original frame. Legacy
symbol-only headers may resolve a callable only while the retained body still
matches the current definition. Guard feedback likewise belongs to the exact
current segment-cache, installed-code, or warm call-cell version. Failed installed
versions are rebuilt after the existing threshold; publication and body/callable
identity are rechecked after compilation, and suspended old code stays retained.

Regression tests cover actual guard execution, numeric-phase rebuilding,
non-replayed effects, multiple values, parent cleanup and live restarts,
redefinition before a guard, retired-version feedback isolation, and restored
frame/depth state. The saved-header root test uses a movable sentinel because
interpreted callable objects are pinned; it requires observed relocation.
The fiber cohort adds guard recovery that suspends with native capture masked,
then migrates or rejects an incompatible carrier while preserving moving roots.
This remains part of the opt-in Linux x86-64 rollout, not universal ABI completion.

### Mapped child reentry through Rust (2026-10-09)

The Linux x86-64 acceptance test now crosses the real `EVAL` implementation
from a warmed mapped child into a separately installed mapped inner function.
It verifies the exact inner, child and caller code owners and requires the inner
entry to establish a distinct native segment. Normal return and `THROW` preserve
multiple values and restore the original segment, capture, frame, stack extent
and native depth.

An execution-local, test-only guard lives in the actual Rust `EVAL` frame. Its
fresh precise root must relocate and retain its contents. The ordered trace must
show inner cleanup, the Rust guard's destructor, child cleanup, then caller
cleanup, each exactly once. A negative control with the inner function left
interpreted fails the distinct-segment assertion. Warmup always returns normally:
a throwing cold call cannot publish a warmed call-cell target.

This completes the distinct mapped-child ownership/continuation acceptance
tracked by `bliss-shih7.16.2`, together with protected continuation recovery,
optimized guard recovery and fiber migration tests. Universal publication,
broader callable adapters and remaining platform gates stay open under the
parent tasks; these tests do not claim a default native ABI.

### Published named native entries (2026-10-09)

Linux x86-64 CallCells now expose process-lifetime native register/slice entries.
Mapped named callers marshal an explicit continuation context and tail-jump
through the current slot. Target selection, mapped-child preparation, checked
compatibility adaptation and retirement belong to the shared published entry.
The caller performs no target-ABI selection or successful-return error check.

The register shape carries cell, count, three arguments and a context pointer;
the slice shape carries cell, count, rooted arguments and a context pointer.
The context names the caller's request and capture continuation. Live register
arguments are copied into the caller's writable scanned activation storage before
preparation can allocate. Slice entries use the supplied scanned slice. Normal
Invoke requests now reserve 64 bytes, including the invocation view added below; recursive requests keep
their existing 64-byte layout. Capture maps record the actual stack adjustment.

The permanent shared entry now owns a 128-byte machine record in a 136-byte
aligned stack reservation, including the forwarding fields described below. Helpers return before
native entry or capture; child retirement may release its final code owner because
its continuation runs in permanent code. Checked fallback reloads the cell after
preparation, restores legacy recovery while executing the checked target, and
converts its result only after all checked frames return. Rust reentry establishes
its own segment and does not forward the original caller's continuation context.

During migration, explicitly named checked slots retain the old caller contract.
There is one target, revision and publication path: checked-slot readiness controls
root retention and invalidation, while native adapter addresses remain permanent.
The existing T1/T2 emitters still use that checked view. Callable-object coverage,
full caller migration, default-mode acceptance and other platform gates remain
open; this publication increment does not complete `bliss-shih7.16` or its parent.


### Callable publication and native FUNCALL (2026-10-09)

Interpreted function metadata publishes a process-lifetime register/slice
callable dispatcher pair. It reuses the old raw native-body pointer word, so the
heap layout and saved-image format do not grow. The accessor repairs pre-install
and restored metadata against the trusted process descriptor without dereferencing
saved address bits. Execution-local registries still own actual compiled versions.

Callable entries receive the address of a scanned callable slot. They reload it
after polling and root the value while selecting code. Only the exact current
registered definition without captured bindings can reuse mapped named code.
Saved definitions, closures, builtin wrappers, generic functions and funcallable
instances use the exact-object adapter; invalid designators signal there.

The native FUNCALL entry advances the invocation argument view past the callable
and tail-forwards only after its Rust preparation frame has returned. The original
request and return PC remain unchanged for capture. The context now holds request,
capture, argument pointer and argument count; the permanent machine record holds
an explicit forwarding descriptor separately from child ownership. Register and
slice forwarding use the same scanned argument storage.

Tests cover measured callable-wrapper and argument relocation, multiple values,
normal and throwing mapped callees, old definitions after redefinition/unbinding,
closure captures, funcallable replacement, and invalid designators. The EVAL
reentry fixture also runs through FUNCALL and checks exact code owners, distinct
Rust reentry segments, root relocation, destructor return and cleanup order.

This implements `bliss-shih7.16.4.1`. APPLY still needs an expanded argument owner
that survives Rust preparation, under `bliss-shih7.16.4`. Default emitted caller
migration, full interpreter routing and other platform gates remain open.

The fresh-process image test checks restored callable invocation and native caller
compilation. Retention of source-free definitions across replacement is described
below; it is required by the full callable rollout.

### Native APPLY argument ownership (2026-10-09)

APPLY now forwards through the same permanent callable entries as FUNCALL. Its
expanded argument vector belongs to the active segment and is scanned alongside
child activations. Pending owners are keyed by the stable caller context address;
the context and capture ABI sizes stay unchanged. Expansion uses the interpreter's
existing list conversion, so the separately tracked improper-tail gap (`bliss-0qbf`)
is not claimed fixed.

Callable preparation retains the buffer if mapped entry declines. Successful
binding copies arguments into a scanned child frame, then takes and drops the
buffer before entering native code. The exact-object adapter instead takes it
into a rooted Rust local until `apply_function` returns. Both an early poll
transfer and a preparation error retire the pending owner before assembly captures
the original caller continuation. Taking an owner clears its obsolete invocation
pointers; no Rust frame is skipped and no buffer waits for segment destruction.

Tests exercise empty, register and slice invocations, explicit prefixes, measured
wrapper and argument relocation, normal and throwing mapped targets, depth
fallback, arity/type/invalid-callable errors, and an injected callable-entry poll
transfer. A marker verifies zero pending owners while the parent remains live,
including repeated calls. The exact-code EVAL reentry fixture now includes APPLY;
fresh-process tests cover closure captures and replacement of a warmed APPLY
binding. This is `bliss-shih7.16.4.2`; full callable routing and default migration
remain under the parent tasks.

### Retained callable definitions (2026-10-09)

Each interpreted function object owns an immutable private definition identity,
separate from its public name. Compiled bodies and captured lexical environments
follow that identity across DEFUN replacement, FASL reload and unbinding. Native
entry and bytecode fallback carry the selected function object as well as the
dispatch key, preserving captures, tiering and OSR context. Public publication
checks the captured owner rather than attaching old code to a newer function cell.

Ordinary FASL loads allocate new function objects. Image restoration explicitly
reconnects existing objects, and private compiled bodies retain their diagnostic
names without reinstating old public bindings. Image format 7 records the new
definition field; older images must be rebuilt. Conditional GC tracing retains
compiled constants and captures while their function remains reachable and
releases them after it dies. Tests require actual constant relocation, correct
saved results across replacement, and eventual release of the retained body.

This addresses `bliss-dm7tb`. Coherent concurrent selection of a named body and its
owner remains separately tracked by `bliss-s751o`; retaining retired definitions
does not by itself make that dispatch snapshot atomic.

### Native root aliases and mapped-adapter crossings

The SysV value maps now retain writable native stack and preserved-register
homes alongside activation shadows. Tagged volatile registers still use their
activation copies; raw poll spills retain their separate ordering. No shadow
synchronization is removed by this increment.

During debug GC marking, the published walker checks that every described copy
of a heap-referencing SSA value agrees before relocation starts. Comparing during
relocation would be invalid because another scanner may already have updated one
copy. Tests deliberately corrupt a native copy to verify that the check fails,
then require actual movement and agreement of native and activation copies after
collection.

Permanent named and callable adapters retain their exact child-call return PCs.
When a child frame unwinds into one of these adapters, the walker recovers the
retained parent code owner and replaces all six preserved-register locations with
the adapter's saved words. Cold landing addresses are not ordinary return PCs.
Scanner lookups use already-published descriptors without initializing or waiting
on a registry while mutators are stopped. Protected-child tests exercise this
crossing; omitting it leaves stale native copies and fails the alias check.

This advances `bliss-shih7.2.7.3.3` and `bliss-shih7.2.7.3.2.1`. Activation
validation also follows the publishing execution's actual managed-frame links:
in-range bytes resembling a header are insufficient. Each uninterrupted native
walk caches its last matched managed frame so outward traversal does not rescan
the whole chain for every frame or alias (`bliss-shih7.2.7.3.2.3.1`).

In test builds, every unexpected walker bailout fails immediately across the
native corpus. The deliberately malformed-publication fixture allows only its
specific reason during one collection and requires exactly that head's two
failures, from marking and relocation (`bliss-shih7.2.7.3.4`). These checks retain
every shadow. Removing redundant shadows still requires a common partition for
synchronization, restoration and maps; tagged volatile registers continue to need
shadows to preserve their values. The universal native ABI is not enabled by
default by these changes.
