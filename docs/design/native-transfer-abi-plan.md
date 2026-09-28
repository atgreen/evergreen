# Native transfers without checks after successful native returns

Status: proposed implementation plan, not implemented.
Tracking: **bliss-shih7**, with executable steps in its child Beads.
Baseline: `2c84d2e1` (the counted pending-error fast path, `bliss-5fzra`).
That completed mitigation retained return checks; this work replaces the protocol
on top of it. Do not remove checks until the new boundary contract is verified.

## Objective and decision

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

* `crates/torcl/src/cli/bytecode.rs`: `c2i_call_args` and other helpers store an
  escaping `TorclError` and return a placeholder. `native_loop_should_exit`
  checks the slot and claims signals. `run_native` and `run_native_osr` restore
  enclosing evaluator state and deliver the error.
* `crates/torcl-compiler/src/t2/emit.rs`: `emit_call` emits a transfer check even
  after a direct recursive call. `emit_transfer_check` saves an **unrooted**
  primary result and calls a helper that must not collect, yield or invoke Lisp.
* `crates/torcl-rt/src/stack.rs`: the host stack and `TorclStack` are distinct.
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

`crates/torcl-rt/src/native_transfer.rs` owns the machine boundary contract:

```rust
#[repr(u64)]
enum NativeExit { Returned = 0, Transfer = 1, Deopt = 2 }

#[repr(C)]
struct NativeOutcome {
    value: TorclVal,
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
previous segment, execution owner, host stack/register save area, TorclStack
watermark, exceptional landing continuation, and rooted transfer state. Machine
save areas are backend-specific, with compile-time offset assertions. Landing
stubs must also satisfy enabled control-flow hardening: Win64 unwind/CFG rules,
x86 shadow-stack and indirect-branch protections, and AArch64 return signing/
branch-target rules where applicable. A plain stack-pointer reset is not a
valid shadow-stack unwind. Gate activation on a tested platform-supported
transition; retain the legacy ABI when that transition is unavailable. An owning
Rust guard registers and unregisters the segment and restores enclosing state.
Do not introduce a mandatory lookup or new reserved register on each successful
native return; exceptional/helper adapters can obtain the active anchor through
execution-owned state. Fiber migration must preserve that ownership.

`crates/torcl/src/cli/bytecode/native_transfer.rs` owns the evaluator-side
transfer packet and preparation/dispatch:

```rust
fn prepare_native_transfer(
    segment: &mut NativeSegment,
    site: &NativeUnwindSite,
    registers: &NativeRegisterSnapshot,
) -> Result<PreparedNativeTransfer, TorclError>;

fn resume_native_transfer(
    transfer: PreparedNativeTransfer,
    env: &mut Env,
) -> Result<TorclVal, TorclError>;
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
   `TorclError` as an unconditional throw.
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
   `torcl_stdlib::acquire_preallocated_storage_condition()` for the condition;
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
| 10 | bliss-shih7.10 | AArch64/s390x adapters after host stabilization; PPC work remains deferred. |

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
  `TORCL_GC_REGION_LOG=1` as supporting evidence; a clean stress run alone does
  not prove movement. Test zero, one and several values across cleanup.
* Run workspace gates and record unrelated baseline failures explicitly.

This plan does not remove the interpreter, replace Rust's unwinder, promise
zero overhead for signal polling, or change the Fibonacci algorithm.

## Segment boundary implementation checkpoint

The runtime provides x86-64 Linux SysV and Windows Win64 segment adapters in
`crates/torcl-rt/src/native_transfer.rs` and its `win64` module. The Rust wrapper pins the anchor,
records execution ownership and TorclStack watermarks, and restores the previous
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
native ABI. Separate probes cause real null-page and TorclStack guard faults,
then use the existing signal recovery targets to return through a Rust helper
and the segment landing. They verify fault classification, destructor execution,
unchanged TorclStack watermarks and restoration of enclosing recovery targets.

This is boundary infrastructure, not activation: generated Lisp calls
still use the existing ABI and successful-return checks. Transfer payload
rooting, cleanup/root retirement, native dispatch, actual JIT integration of the
fault-recovery/unwind gates, fiber migration and native-Windows execution gates remain
required before rollout. The
adapter records TorclStack watermarks but does not restore them itself.

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
cargo test -p torcl-compiler --test native_helper_veneer -- --include-ignored --nocapture
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
cargo test -p torcl-compiler --test native_capture_sysv -- --include-ignored
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
cargo test -p torcl-compiler --test native_invoke_emit -- --include-ignored
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
also run with `TORCL_GC_STRESS=1 TORCL_GC_POISON=1`:

```text
cargo test -p torcl --lib native_v2_bridge -- --include-ignored
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
pending error, and exactly-once calls and callee cleanup. They are explicitly
enabled with `--include-ignored` and also run under GC stress plus poison.
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
maps. If they transfer, fallback adopts the already-rooted continuations and
unwinds them at their recorded handler depths, without replaying the failed call.
Legacy emission and transfer emission without explicit cleanup helpers still
refuse these operations.

Source-level execution tests cover nested normal cleanup, all multiple-value
shapes, forced moving GC during cleanup, and a replacement THROW crossing two
running cleanup continuations before executing the outer cleanup once. The
helper calls use the normal call-frame, clobber and shadow-root conventions;
they add no generated caller status test. These helpers do not execute Lisp,
yield, signal or collect. Copying multiple values still uses Rust allocation;
the emergency allocation-failure contract remains an ABI installation gate.
A real-fiber gate also runs native cleanup on one and four carrier threads,
observes at least two cleanup continuations suspended together, and collects
from outside their stacks. On resumption, distinct per-fiber answers, multiple
values, replacement THROW tokens, stack watermarks and native nesting depth must
remain correct. It additionally forces collection within each resumed cleanup
and verifies actual address relocation. This gate passes repeated stress/poison
runs; it does not assert carrier migration and does not replace the wider
fiber/foreign-boundary gates.

Native **exceptional** cleanup destinations and ordinary tier installation
remain outstanding: exceptional entry into an outer cleanup in these tests still
uses the bytecode fallback.

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
cargo test -p torcl-rt --target x86_64-pc-windows-msvc --lib native_transfer -- --include-ignored --nocapture
cargo test -p torcl-rt --target x86_64-pc-windows-msvc --test native_segment_windows -- --include-ignored --nocapture
```

Report machine execution, refusal behavior, ignored tests and metadata checks
separately. Never disable mitigations to make a gate pass. Native Windows CI and
the remaining SEH/mitigation gates are tracked in `bliss-shih7.14` and gate final
activation. The boundary follows Microsoft's [x64 prologue/epilogue rules](https://learn.microsoft.com/en-us/cpp/build/prolog-and-epilog)
and [process mitigation query contract](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocessmitigationpolicy).

## Baseline contract oracles and boundary inventory

`crates/torcl/tests/native_transfer_cli.rs` verifies real T1/T2 entries, direct
calls, OSR, escaping side effects, multiple values, fiber yields and native
reentry through resumable restarts and replacing cleanup transfers. The shared
`crates/torcl/tests/fixtures/native-transfer-osr.lisp` is also consumed by
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

PPC deferral is scheduling per user direction, not a claim that its existing
backend lacks native compilation. Foreign callbacks and all architecture-specific
entry stubs remain in the final portability audit.

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
