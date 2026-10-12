# Fibonacci: return before native frame setup

PR [#310](https://github.com/atgreen/evergreen/pull/310) reduces the measured
ordinary T2 `fib(35)` median from **204 ms to 190 ms**, a **6.9% reduction**.
SBCL 2.6.8 measures **154 ms**. EGCL remains **23% slower**; parity is not achieved.

| Implementation | Samples | Minimum | Median | Maximum |
|---|---:|---:|---:|---:|
| EGCL before, production code from PR #309 | 21 | 198 ms | 204 ms | 209 ms |
| EGCL after, `1b2cd236` | 21 | 185 ms | 190 ms | 195 ms |
| SBCL 2.6.8 | 21 | 152 ms | 154 ms | 156.001 ms |

Three fresh processes per implementation each measured seven calls, rotating
implementation order on CPU 4. Per-process medians were 202/203/206 ms before,
188/190/190 ms after, and 153.001/154/155.001 ms for SBCL. These are observed
ranges, not confidence intervals. All 63 results were 9,227,465. Pre/post
disassembly remained identical in each process and proved both direct self
calls in installed ordinary T2 code. Compilation, loading, warmup, printing
and disassembly are excluded from timing. The unchanged [source](../fib_self.lisp)
is untyped, with `(optimize (speed 3) (safety 1) (debug 1))`.

## What changed

The [annotated installed listing](2026-10-12-fib-early-return-after.asm) shows a
seven-instruction, 28-byte prefix at the common accepted entry. Base cases
return the incoming argument with carry clear before touching the stack.
Both direct self calls enter that prefix. Public arity checking remains first.

Selection is general: bounded noncollecting paths through supported guards,
constant comparisons and SSA branches can return a proven single argument or
constant. Failed prefix conditions preserve incoming ABI words and enter the
original framed body. Unsupported effects, safepoints and unproved multiple
value state prevent selection. This is not a Fibonacci-specific transformation.

Non-base cases retain the 136-byte frame and existing call admission. Complete
installed code grows from 2,657 to 2,685 bytes, including cold code. The experiment
measures the complete change; instruction counts do not establish timing
attribution. Remaining work includes reducing repeated pending-event and
execution checks (`bliss-si1hp`) and redundant adjacent reloads (`bliss-88cpt`).

## Provenance and validation

Timing ran on 2026-10-12, 21:54:40–21:54:55 UTC, Intel Core Ultra 7 255H,
CPU 4, `powersave`, `intel_no_turbo=1`, initial one-minute load 2.84. The desktop
remained active; no compiler or test build overlapped timing. Both performance
binaries use Rust 1.94.1, default features, x86-64 musl release optimization
level 3, thin LTO and one codegen unit.

| Binary | Production source | SHA-256 |
|---|---|---|
| Before | `7d433e62`, same production code as merged `d010a783` | `5207520f9bde29884e32777f5e46640022c6368e5717ba240a650ed74bbe3f84` |
| After | Clean `1b2cd2363cc799f29a07de8b0f3babe42ca80e27` | `086e4ecc59426120e7801563c954a7a0ed9e3c5101f0b67ef5bc0b2558bf7f71` |

The [manifest](2026-10-12-fib-early-return.json) records commands, samples,
hashes and host state. The [archive](2026-10-12-fib-early-return-evidence.tar.gz)
preserves raw measurement outputs, drivers, T2 logs, runner and validation logs.

- Compiler library: **233 passed**. Native integration: **48 passed**, covering
  early returns, ordinary functions, MV admission, tail calls, frame maps and
  transfer IR. The production comparison-flag correction was followed by
  another passing 233-unit, 2-early-return and 16-ordinary-function run.
- Benchmark evidence parser: **9 passed**. New executable regressions failed
  before implementation; they check the stack sentinel, recovery and self targets.
- [Implementation CI](https://github.com/atgreen/evergreen/actions/runs/38313043317):
  relevant gates passed; Windows was skipped. Its selected 613 tests included
  160 EGCL library cases, not the entire native suite.
- The CI library harness was subsequently reused locally for **229 native tests,
  all passed**, including ignored cases. CI merge `bab0f380` and implementation
  `1b2cd236` have the identical source tree
  `48c0f9d8f796a56361d02ed17ae94150a8ecc850`. The harness SHA-256 matches its
  archived manifest: `cc41eb87f465d6fa4fa301d6a52939afe154590453ee443af76383db73a444c5`.
  This harness enables debug assertions and overflow checks, disables LTO and
  uses 16 codegen units; it was **not** the performance executable.
- Targeted GC stress and poison: **4 passed**, including recursive recovery
  and generated-poll tests that assert actual native-root relocation.

An initial implementation passed hand-built IR tests but did not recognize the
production comparison's ordered-effect flag. Actual-worker validation caught
the missing prefix; that version measured 209 ms versus 208 ms before. A narrow
recognition correction and a regression using the production flags produced
the installed prefix measured above. The initial run is not evidence of speedup.

Bead: `bliss-hf23v`; parity objective remains open in `bliss-mmcwn`.
