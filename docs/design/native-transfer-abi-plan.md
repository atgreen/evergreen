# Native transfers without checks after successful native returns

Status: proposed implementation plan, not implemented.
Tracking: **bliss-shih7**, with executable steps in its child Beads.
Baseline: `2c84d2e1` (the counted pending-error fast path).

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

The initial complete design dispatches exceptional continuations through the
existing bytecode machinery. Native frames are materialized only on the cold
path. It does not restart the failed function or replay the failed call.
Native handler landing pads can later optimize exception-heavy workloads; they
are not required to remove successful-return checks.

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

Preparation roots the payload and reconstructs the exact logical continuation.
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
         -> capture/root/materialize exceptional continuation
         -> retire this segment's generated frames
         -> segment landing -> ordinary return to Rust
         -> resume bytecode transfer/cleanup dispatch
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
   Cleanup may allocate, yield, call native code, or supersede the original
   transfer. Restore bindings at their defined unwind points, not in one bulk
   reset before cleanup.
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
   another successful allocation to begin unwinding. Test failed preparation,
   not only successful reconstruction.
7. **Cross-version calls are explicit.** Tag native code with its transfer ABI.
   Direct-call installation, OSR and cache invalidation verify compatibility.
   Legacy code stays behind a bridge segment until converted. No old helper can
   silently return a placeholder into unchecked new code.

## Polling independently of exceptions

Remove the signal/GC responsibility from ordinary return checks only after new
poll sites are proven. Start with a cheap inline poll-word test and a cold slow
path at root-safe function entries, loop back-edges and bounded straight-line
intervals. Entry coverage must include every recursive cycle. A slow poll may
collect/yield only after live references are published; a delivered interrupt
uses the new transfer path.

Keep the existing process signal epoch, per-execution ownership and deadline
semantics. Preserve blocked-foreign-call protocols. Verify signal response and
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
| 5 | bliss-shih7.5 | Exception-only bytecode reification; cleanup, handlers, restarts and no-replay tests. |
| 6 | bliss-shih7.6 | Root-safe independent polling; signal, deadline, preemption and GC liveness gates. |
| 7 | bliss-shih7.7 | Fiber migration and nested Rust/foreign callback ownership tests. |
| 8 | bliss-shih7.8 | Enable mapped new-ABI T1/T2 code and remove checks after successful native calls. |
| 9 | bliss-shih7.9 | Controlled release comparisons, instruction counts, regression gates and documentation. |
| 10 | bliss-shih7.10 | AArch64/s390x adapters after host stabilization; PPC work remains deferred. |

Steps 2 and 3 both follow step 1. Step 4 needs both; subsequent host steps follow
in order. Keep unsupported platforms on the old ABI until their own gates pass.
QEMU proves functional behavior only; native hardware is required for platform
performance claims. x86-64 Linux and Windows are the first delivery milestone.

The rollout gate requires:

* Disassembly proves no transfer helper call or status branch after successful
  native self calls in warmed Fibonacci; required entry/loop polls remain.
* Same source and inputs, verified T2 before/after every sample, correct
  checksums and recorded deopts. Archive paired timings, retired instructions,
  code size and binary/source hashes in the existing HTML report.
* Measure nonthrowing, throwing, handler-heavy, cleanup-heavy, fiber and callback
  workloads. There must be lower instruction cost on the normal path and no
  unexplained regression in the other paths. An SBCL win is a measurement, not
  an assumption or acceptance shortcut.
* Forced moving GC/poison, nested cleanup transfers, multiple values, migration,
  OSR, invalidation, legacy bridges and Windows ABI tests pass. Verify actual
  relocation, not just a process exit under a stress environment variable.
* Run workspace gates and record unrelated baseline failures explicitly.

This plan does not remove the interpreter, replace Rust's unwinder, promise
zero overhead for signal polling, or change the Fibonacci algorithm.
