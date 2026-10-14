# Typed Fibonacci with checked primary results

This compares clean main `36aee49543d4d41f86162fd4969571a2471aac0f`
with the checked-result implementation accompanying this report. Both were
built serially with the same release profile, one Cargo job, incremental
compilation disabled, and a 3 GiB/no-swap cap. The candidate runtime was
unchanged between its integration tests and these measurements.

## Matched results

The unchanged source and driver from the
[pure-recursion report](2026-10-13-fib-pure-recursion.md) warm with 6000 calls
to `fib(8)`, require installed T2, verify inputs 0–40 against an iterative
oracle, and time seven `fib(35)` calls. Three processes per revision ran
serially on CPU 0, ordered before/after, after/before, before/after. No build
ran concurrently. All six processes exited successfully with empty stderr;
all 42 timed answers were 9227465.

| Revision | Median | Mean | Minimum–maximum | Native bytes, including cold paths |
|---|---:|---:|---:|---:|
| Main | 30 ms | 30.33 ms | 29–31 ms | 2356 |
| Checked results | 31 ms | 31.10 ms | 31–32 ms | 2700 |

This is a small measured slowdown: 3.3% by median, 2.5% by mean. Timing has
millisecond resolution; this batch does not establish a general performance
effect. It does **not** justify claiming no regression. The candidate checks
the retained primary-result contract even at safety zero. Both listings retain
ordinary T2 and two direct recursive machine calls. The candidate retains the
sum overflow check and additional result validation. No fresh SBCL comparison
was run in this batch.

Raw samples, milliseconds, one row per process:

```text
Main
29 30 31 31 31 31 30
29 30 30 30 30 30 30
30 31 31 31 30 31 31

Checked results
31 31 31 31 31 31 31
31 31 31 32 31 31 31
31 31 31 31 32 31 31
```

## Reproduction and provenance

Both executables ran with `--no-init --load
/tmp/pure-recursion-matched-egcl.lisp`, under `taskset -c 0` and a shared
1 GiB/no-swap cap. The runner unsets `EGCL_BOOT_FILE`, `EGCL_FORCE_TIER`,
`EGCL_GC_STRESS`, `EGCL_GC_STRESS_AFTER_INIT`, `EGCL_GC_POISON`, and
`EGCL_TCO_DISABLE`. Keep `NOTINLINE` local to the benchmark caller, not global
before lazy compilation.

The host runner is `/tmp/dpuvm-fib-compare.sh`; raw timings and full annotated
disassembly are `/tmp/dpuvm-fib-{before,after}-{0,1,2}.stdout`, with matching
`.stderr` files. These temporary files are local evidence, not repository
artifacts; the samples and binary fingerprints are retained here.

SHA-256:

```text
Main executable
0e2c59e2ced8c264913241074ca70f1b2b8ffa601406e21365a2995cdb2cca1c
Candidate executable
d1ca4c303bac1cd1d8037cc0d0c0bedb92c4a38479b16d8efab04f194d95e83f
EGCL driver
01f4a388ca365e25ec760f51cee0b0943fd6c93e78f3415b5b0e87e2b72e3037
Shared timing driver
282694a6911116c77dc1aff3eaf04c33be052cd3a07504af35ae61700d0a8f1c
Bounded Fibonacci source
29ce13e8a4c0bd9820016af942ac704e404c3f8ef78a6522f263fb817a3a58c5
```

This milestone does not complete `bliss-dpuvm`. General checked result-fact
propagation across calls, nested-deoptimization recursive inlining, and the
private unboxed local-call ABI remain unfinished.
