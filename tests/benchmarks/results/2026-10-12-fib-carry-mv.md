# Fibonacci: consume the multiple-value return carry

Using the return carry flag to skip redundant multiple-value cleanup reduces
EGCL T2's median `fib(35)` time from **390 ms to 211 ms**, a **45.9% reduction**
(1.85× speedup). SBCL 2.6.8 takes **154 ms**, leaving EGCL **1.37× slower**.
This is progress toward parity, not parity.

| Implementation | Samples | Minimum | Median | Maximum |
|---|---:|---:|---:|---:|
| EGCL direct self calls, before this change | 21 | 387 ms | 390 ms | 406 ms |
| EGCL carry-based cleanup, T2 | 21 | 204 ms | 211 ms | 218 ms |
| SBCL 2.6.8 | 21 | 152.999 ms | 154 ms | 155.001 ms |

Three fresh processes per implementation each measured seven calls, with
implementation order rotated and execution pinned to CPU 4. Per-process medians
were **390/405/389 ms** before, **210/216/209 ms** after, and
**154/154.001/153 ms** for SBCL. These are observed ranges, not confidence
intervals; EGCL's clock resolution was 1 ms. Every measured call returned
**9,227,465**. The host remained an active desktop, so some variation is expected.

## What changed

The ordinary x86-64 Lisp ABI already promises that a return with **CF=0** has
one result in RAX and an empty, inactive multiple-value buffer. The caller now
consumes that carry descriptor before any flag-clobbering instruction. When
dataflow proves the secondary results are discarded, **JNC** skips the reset
helper for a single result. A discarded **CF=1** zero/many tuple still invokes
the existing noncollecting reset. The primary result has a home that survives
that helper. Subsequent redundant `ClearMv` operations are omitted.

The [annotated disassembly](2026-10-12-fib-carry-mv-after.asm) contains the actual
installed T2 listing. Fibonacci's ordinary single-value recursion skips both
cleanup helpers dynamically; the fallback code remains in the allocation.
The complete allocation is **2,818 bytes**, versus **2,806** before. Code size
alone therefore does not predict this improvement. Admission checks and mapped
ordinary frames remain; simplifying the self-call admission sequence is tracked
separately in `bliss-1qu3b`.

Full-tuple forwarding calls retain CF/RAX/RDX/RCX unchanged. This change does
**not** remove the Env-owned tuple backing for zero/many returns specified in
[the current ABI](../../../spec/04-07-x86-64-lisp-abi.md).
SBCL's native convention instead uses result registers and the control stack;
its scalar unknown-value receiver can conditionally restore RSP using carry.
See SBCL 2.6.8's [receiver code](https://github.com/sbcl/sbcl/blob/sbcl-2.6.8/src/compiler/x86-64/call.lisp#L328)
and [register definitions](https://github.com/sbcl/sbcl/blob/sbcl-2.6.8/src/compiler/x86-64/vm.lisp#L172).
Changing EGCL's full-tuple storage convention would require a separate ABI and
runtime change.

## Workload and provenance

The unchanged [source](../fib_self.lisp) uses untyped arguments and
`(optimize (speed 3) (safety 1) (debug 1))`. The [runner](../fib_self.py) excludes
loading, compilation, warmup, disassembly and printing from timing. EGCL loads
source with `EGCL_FORCE_TIER=t2`; SBCL compiles and loads a FASL. Each process
retained identical pre/post disassembly, and EGCL's listings demonstrate the
installed ordinary ABI and direct recursive calls. This measures steady-state
execution, not startup or compilation. No profiler or hardware-counter run was
added for this patch.

Measurement ran **2026-10-12 20:39:46–20:40:06 UTC**, on an Intel Core Ultra 7
255H, CPU 4, with `powersave` and `intel_no_turbo=1`. Initial one-minute load
was 1.24. No compiler or test build ran during the timing experiment.

Both EGCL binaries use Rust **1.94.1**, default x86-64 musl release settings,
optimization level 3, thin LTO, one codegen unit and default features.

| Artifact | Revision | Executable SHA-256 |
|---|---|---|
| Before, PR #307 | `35a2185ff398396afe3b630a780a4644cd5a1a62` | `1949e855ce9be61b9c47becd90dea5b25390f97ec619b7880e96f2bf41b7917b` |
| After, PR #308 | `591d3ada5b0d13fdeccb55df2804c0d41db46c99` | `f16cf04a07310594925a83266967cf740fa6062928bd138f5fb47d0f32ab7d0d` |

The before binary has the same production source as the merged base
`e19dceed0b70d3de3f5988d87816c6bf1904ac25`, which the manifest records as its
comparison revision. The after binary was built from a clean tree with
`scripts/egcl-limited.sh cargo build --release -p egcl --bin egcl` and a 3 GiB
memory limit. Later report/changelog additions do not alter compiled code.

The [manifest](2026-10-12-fib-carry-mv.json) records every command, sample,
checksum, executable/driver/source hash, exit status and host state. The
[evidence archive](2026-10-12-fib-carry-mv-evidence.tar.gz) preserves all raw
outputs, T2 logs, drivers, runner, build and validation logs.

## Validation

- Compiler library and native ordinary-function target: **229 + 13 passed**.
  The structural regression fails the former implementation if flags are
  clobbered before carry is consumed.
- Runtime `ordinary_` tests, including ignored native tests: **102 passed**.
  Fibonacci checks actual native execution and zero MV-clear helper calls;
  zero/one/many-result tests verify cleanup, primary preservation, arithmetic
  guard deoptimization and no replay of a producer's side effect.
- The two discarded-tuple regressions with `EGCL_GC_STRESS=1` and
  `EGCL_GC_POISON=1`: **2 passed**. This is stress evidence, not an independent
  measurement of object relocation by those fixtures.
- [CI on the implementation revision](https://github.com/atgreen/evergreen/actions/runs/38304778123):
  musl build/regression, strict Clippy, root lint and validation contracts
  passed. Spec coverage passed its existing debt ratchet with **37 uncovered
  requirements**, no new gaps. Windows native-transfer execution was skipped
  by workflow policy; this does not claim a Windows run.

Tracked implementation: `bliss-mgggv`; broader parity work: `bliss-mmcwn`.
