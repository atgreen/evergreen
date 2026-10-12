# Fibonacci: compact ordinary call admission and frames

The combined change reduces measured EGCL T2 `fib(35)` median time from
**212 ms to 204 ms**, a **3.8% reduction**. SBCL 2.6.8 measures **154 ms**;
EGCL remains **1.32× slower**, so performance parity is still open.

| Implementation | Samples | Minimum | Median | Maximum |
|---|---:|---:|---:|---:|
| EGCL before, merged PR #308 | 21 | 206 ms | 212 ms | 216 ms |
| EGCL after, PR #309 | 21 | 201 ms | 204 ms | 212 ms |
| SBCL 2.6.8 | 21 | 150 ms | 154 ms | 156 ms |

Three fresh processes per implementation each measured seven calls, rotating
implementation order on CPU 4. Per-process medians were 213/211/211 ms before,
208/203/203 ms after, and 150/154.001/154 ms for SBCL. These are observed ranges,
not confidence intervals. Every call returned 9,227,465; every process retained
identical pre/post disassembly. The EGCL listings prove installed ordinary T2
code with two direct self calls. Timing excludes compilation, loading, warmup,
printing and disassembly. The unchanged [untyped source](../fib_self.lisp) uses
`(optimize (speed 3) (safety 1) (debug 1))`.

## Assembly changes and remaining work

The [annotated installed listing](2026-10-12-fib-admission-after.asm) shows:

- Pending flags are compared directly in memory, preserving byte versus qword
  widths and every event observation. Depth, stack and execution checks remain.
- An MV-active admission load is omitted only where existing per-instruction
  dataflow proves the tuple empty. Unknown tuples still take the checked path.
- Frame homes are densely renumbered without changing their alias relationships.
  This removes unused allocator slots; it does not coalesce live values.

Fibonacci's frame now occupies **136 bytes below entry RSP**, versus 168 before.
The complete installed allocation shrinks from **2,818 to 2,657 bytes**, including
cold code. Both self calls still enter a full prologue, including base cases.
Carry-based result normalization from PR #308 remains unchanged.

SBCL's captured listing is 138 bytes, excluding the generic arithmetic helpers
it calls. Its recursive edges have much less explicit admission work. Byte counts
are not timing attribution: this experiment measures the combined changes and
does not isolate their individual contributions. A separate probe of EGCL's
existing full register pool measured the same 209 ms median as the reduced pool;
the default therefore stays unchanged. That probe likewise does not isolate
the cost of extra register saves from its smaller frame.

The next candidate, tracked in `bliss-hf23v`, is a general, noncollecting early
return prefix that can bypass frame setup. It is not part of this result.

## Provenance and validation

Measurement ran on 2026-10-12, 21:25:06–21:25:22 UTC, Intel Core Ultra 7 255H,
CPU 4, `powersave`, `intel_no_turbo=1`, initial one-minute load 2.20.
The desktop remained active; no compiler or test build overlapped timing.
Both binaries use Rust 1.94.1, default x86-64 musl release settings, optimization
level 3, thin LTO, one codegen unit and default features.

| Artifact | Production revision | Executable SHA-256 |
|---|---|---|
| Before | `591d3ada5b0d13fdeccb55df2804c0d41db46c99` | `f16cf04a07310594925a83266967cf740fa6062928bd138f5fb47d0f32ab7d0d` |
| After | `7d433e628b23b898db1c7e8a53fd4bae42fcc6e1` | `5207520f9bde29884e32777f5e46640022c6368e5717ba240a650ed74bbe3f84` |

The before binary has the same production source as merged base `eb976393`,
recorded in the manifest. The after binary was built from a clean tree using
the memory-limited release build. Later report/changelog additions do not change
compiled code. The [manifest](2026-10-12-fib-admission.json) records commands,
samples, checksums, hashes and host state. The
[evidence archive](2026-10-12-fib-admission-evidence.tar.gz) preserves raw outputs,
T2 logs, drivers, runner, build/test logs and the register-pool probe.

- Compiler library: **229 passed**; frame maps: **9**; ordinary functions: **15**;
  MV admission: **2**; ordinary tail: **1**; transfer IR: **18**.
- Runtime native tests, including ignored native cases: **229 passed**.
- Targeted GC stress and poison: **4 passed**, including two tests with actual
  native-root relocation assertions.
- [Implementation CI](https://github.com/atgreen/evergreen/actions/runs/38308889956):
  musl build/regression, strict Clippy, root lint and validation contracts passed.
  Spec coverage passed its existing ratchet with 37 uncovered requirements and
  no new gaps. Windows execution was skipped by workflow policy.

New structural tests failed before implementation. The combined compiler log
retains an initial tail-fixture failure: it supplied active MV before ordinary
entry, violating the ABI precondition. The fixture now uses pending yield;
its separate final run passes. The new executable two-call regression tests
active MV produced after valid entry, so rejection of unknown tuples remains
covered. No production code changed for that fixture correction.

Tracked work: `bliss-1qu3b`, `bliss-h017d`; parity objective: `bliss-mmcwn`.
