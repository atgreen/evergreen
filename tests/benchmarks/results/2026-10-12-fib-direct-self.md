# EGCL T2 and SBCL: untyped Fibonacci

The new direct-self-call implementation takes **390 ms** for `fib(35)`, versus
**154 ms** for SBCL 2.6.8. SBCL is **2.53× faster** on this workload. EGCL's median
is **18.75% lower** than the published v0.0.7 baseline; this is progress, not parity.

| Implementation | Samples | Minimum | Median | Maximum |
|---|---:|---:|---:|---:|
| EGCL v0.0.7, T2 | 21 | 478 ms | 480 ms | 482 ms |
| EGCL direct self calls, T2 | 21 | 388 ms | 390 ms | 393 ms |
| SBCL 2.6.8, compiled | 21 | 152 ms | 154 ms | 155 ms |

Each row combines three fresh processes with seven samples per process. Their
individual medians were EGCL before **480/480/479 ms**, EGCL after
**389/389/391 ms**, and SBCL **154/154/154 ms**. These ranges describe this run,
not confidence intervals. EGCL's reported clock resolution was 1 ms.

## Workload and execution evidence

All implementations used the same [untyped source](../fib_self.lisp), with
`(optimize (speed 3) (safety 1) (debug 1))`. Every measured call returned
**9,227,465**. The [runner](../fib_self.py) rotated implementation order, pinned
execution to CPU 4, and excluded loading, compilation, warmup, disassembly and
printing from the timed region. EGCL loaded source and reached installed T2;
SBCL used `COMPILE-FILE` followed by loading its FASL. This comparison does not
measure startup or compilation speed.

All nine processes retained identical pre/post listings. Each EGCL after process
had two relative self calls to its ordinary prologue, skipping its arity check:
the first process calls at **+021e** and **+0477** target **+003e**. Its T2 log
reports four speculative arithmetic sites and one installed ordinary body;
there are no later FIB deoptimization or invalidation records. Each SBCL listing
contains two `CALL L0` self calls.

The full EGCL code allocation shrank from **3,534 to 2,806 bytes**, including
adapters and cold paths. SBCL's displayed body is **138 bytes** and calls shared
generic arithmetic routines, so these sizes are not equivalent accounting of
all reachable machine code. See the [before listing](2026-10-12-fib-direct-self-before.asm),
[annotated after listing](2026-10-12-fib-direct-self-after.asm), and
[SBCL listing](2026-10-12-fib-direct-self-sbcl.asm).

The self edges no longer look up the named descriptor or check its binding
revision. They retain execution ownership, pending-event, multiple-value,
native-depth and physical-stack admission checks, plus mapped argument homes
and ordinary frames. Static assembly explains what remains; this experiment
does not isolate the cost of each check or dynamically count cold fallbacks.

## Why SBCL is faster

The assembly shows two immediate sources of avoidable work:

1. **Hot multiple-value cleanup.** Every non-base EGCL invocation calls
   `c2i_t2_mv` at +0317 and +0569. Its zero-count path invokes `c2i_clear_mv`,
   which retrieves `NATIVE_ENV` through execution-local state before clearing
   the vector length and active flag. The [helper disassembly](2026-10-12-fib-direct-self-clear-mv.asm)
   shows register saves, current-fiber and cached-pointer lookup before those
   two stores. Its mutex/hash-map fallback is also present, but that does not
   imply the fallback runs on cache hits. This workload has 14,930,351 non-base
   invocations, hence 29,860,702 such clears on the normal path. SBCL's listing
   has no corresponding MV cleanup helper calls.
2. **Large self-call admission sequences.** Each edge executes 60 instructions
   from the control load through argument setup before CALL, including 15
   conditional admission branches. SBCL uses a short frame/argument setup and
   `CALL L0`. EGCL also uses a larger frame: 168 bytes below incoming RSP versus
   SBCL's 32-byte recursive frame reservation. This demonstrates extra work,
   not a measured cache-miss penalty.

Separate hardware-counter runs corroborate the instruction gap. Three fresh
processes per implementation each evaluated ten checked `fib(35)` calls.
User-space counters were enabled with acknowledged `perf` controls after
warmup/pre-disassembly and disabled before post-disassembly. The region includes
minor driver/printing/file-handshake overhead. All events ran at 100%, without
multiplexing. Medians below are normalized per `fib(35)`:

| Counter | EGCL T2 | SBCL |
|---|---:|---:|
| Retired instructions | 5.974 billion | 1.255 billion |
| CPU cycles | 776 million | 298 million |
| Branch instructions | 1.344 billion | 373 million |
| Branch misses | 26,275 | 56,859 |
| Instructions per cycle | 7.69 | 4.21 |

EGCL executes **4.76× as many instructions**. Its branch-miss rate is only
**0.002%**, compared with SBCL's **0.015%**: branch misprediction is not the
evident explanation. EGCL retires more instructions per cycle but has much more
work to retire. Both implementations recurse directly; SBCL still calls generic
arithmetic helpers, whereas EGCL inlines guarded fixnum arithmetic.

A separate 999 Hz user-cycle sample of the warmed EGCL workload collected
3,908 samples with none lost: **38.55%** in `c2i_clear_mv`, **16.50%** in
`cached_pointer`, **4.36%** in `c2i_t2_mv`, **3.33%** in `current_fiber`, and
**37.18%** in `T2:FIB`. This supports addressing redundant MV cleanup first
(`bliss-mgggv`), then simplifying self admission (`bliss-1qu3b`). These are sample
attributions, not a promise that removing a component yields its percentage as
an end-to-end speedup. GC, interruption and stack safety must remain correct.

The [counter data](2026-10-12-fib-direct-self-counters.json) and
[profile archive](2026-10-12-fib-direct-self-profile-evidence.tar.gz) preserve the
commands, controlled drivers, raw events, profile, symbol map and outputs.

## Provenance and limits

The run took place **2026-10-12 19:28:31–19:28:57 UTC**, on an Intel Core Ultra 7
255H, CPU 4, with the existing `powersave` governor and `intel_no_turbo=1`.
No compiler or test build ran during measurement. The desktop remained active;
this was not an isolated host. Initial one-minute load was 1.75.

Both EGCL executables used Rust **1.94.1**, default x86-64 musl release settings,
optimization level 3, thin LTO, one codegen unit and default features.

| Artifact | Revision | Executable SHA-256 |
|---|---|---|
| Published v0.0.7 static RPM | `0b29071ae0685ec863efb3b27b5768a247ec2212` | `846046466d1c56f7319e831095639014a1f27a0fed54b51215bf7c66fee53b69` |
| Local release, PR #307 | `35a2185ff398396afe3b630a780a4644cd5a1a62` | `1949e855ce9be61b9c47becd90dea5b25390f97ec619b7880e96f2bf41b7917b` |

The baseline hash matches the [release build metadata](https://github.com/atgreen/evergreen/releases/tag/v0.0.7).
The after build used `cargo build --release -p egcl` under the repository memory
limiter. The baseline came from the release builder; the after executable was
built locally. The after revision also includes main's allocation, rational,
image and profiler fixes from PRs #302–306, so the before/after result compares
these complete revisions rather than isolating one changed instruction.

The [JSON evidence](2026-10-12-fib-direct-self.json) retains every sample, command,
hash, process summary and host setting. The [evidence archive](2026-10-12-fib-direct-self-evidence.tar.gz)
retains raw output, T2 logs, drivers, build metadata and validation logs, including
the earlier failing CLI run and the subsequent passing regression run.
