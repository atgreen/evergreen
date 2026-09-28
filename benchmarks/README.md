# TorCL performance lab

Reproducible comparisons of the **same Common Lisp source** on TorCL and SBCL,
with a standalone HTML report, raw JSON, generated scripts, and process logs.
This showcase complements the regression suite in `tests/benchmarks/`.

[Open the checked-in sample HTML report](sample-report/index.html).
The current sample report is generated from native-transfer ABI milestone
`f2639868`. Five alternating samples measure Fibonacci at 32 ms on TorCL versus
57 ms on SBCL, and the Ironclad POWER-MOD kernel at 505 ms versus 9 ms. Every
TorCL sample passed the T2 and checksum gates; these are local measurements,
not a universal performance claim.

The sample report verifies T2 before and after every measured TorCL kernel
and batch driver. It reports whichever implementation is faster.

## Run

On Linux with Python 3.10+, SBCL, `taskset`, and a working systemd user session:

```sh
TORCL_MEM_MAX=8G TORCL_TIMEOUT=1200 scripts/torcl-limited.sh \
  cargo build --release -p torcl --bin torcl
python3 benchmarks/run.py --cpu 0
```

Open `benchmarks/results/index.html`. No server, JavaScript, external fonts, or
network access is needed to view the report. `results.json` contains all samples,
binary and workload hashes, versions, machine details, and the source revision.
Generated scripts and logs alongside it make the measurements inspectable.
Logs in the checked-in sample report are losslessly gzip-compressed; new runs
write plain text logs.
Output directories must be empty, preventing accidental overwrite of prior runs.

Use `--torcl PATH`, `--sbcl PATH`, `--samples 9`, `--output DIRECTORY`, or
`--case fibonacci` / `--case ironclad-power-mod` to select another run. The minimum
is five samples; the default is seven. `--cpu` defaults to the first allowed CPU.
Every child runs through `scripts/torcl-limited.sh` with a 4 GiB cap (override via
`TORCL_MEM_MAX`) and a 300-second process timeout. Missing tools, runtime failures,
failed assertions, wrong checksums, missing T2 evidence, and zero-length timings
fail the run; they
are never converted into speedups.

## What is measured

* **Recursive Fibonacci:** ten evaluations of F(30), with runtime input and a
  checked sum of 8,320,400. No type declarations or memoization.
* **Ironclad modular exponentiation:** 100,000 calls of `power-mod`, with bases
  2 through 100,001, exponent 65,537, modulus 104,729. Checked sum: 5,242,584,863.
  This deliberately measures small-integer arithmetic, **not RSA-sized integers
  or complete encryption throughput**. Both runtimes execute the same code.

Each independent process checks correctness, performs **10,000 short training
calls to the batch driver**, then three untimed full-workload warmups. Both
runtimes receive the same training work: Fibonacci uses F(10) once per training
batch, and modular exponentiation uses one input per training batch. The measured
inputs and iteration counts are restored before full warmup and timing.

Before starting its clock, TorCL must report `function-tier = 2` for **both the
kernel and `bench-workload`**. The harness polls for up to ten seconds to allow
asynchronous compilation to publish. It checks again after timing and rejects
the sample if either tier is missing or below T2. Raw JSON and logs record tiers,
invocation counts, back-edge counts, OSR entries, and deoptimization deltas during
timing. This checks installed function tiers directly; OSR counts alone cannot
establish T2. Installed T2 code may still call runtime helpers.

One sample is then measured using `get-internal-real-time`. The workloads
return an observable checksum, validated by the Python runner. Fibonacci has
small known-answer checks. Modular exponentiation has independently computed
known answers, including a zero exponent and a bignum exponent to cover the
retained sliding-window branch. The aggregate modular checksum was independently
computed using Python's three-argument `pow`.

TorCL and SBCL alternate order between samples and never run concurrently. Both
are pinned to the requested CPU. TorCL uses a release build and automatic tiering;
the runner clears inherited `TORCL_*` tuning except the memory cap. SBCL compiles
the identical workload to a temporary FASL and loads it. The shared policy is
`(optimize (speed 3) (safety 1) (debug 0))`; upstream function declarations remain
intact, including Ironclad's `safety 0` declarations.

The timing excludes startup, loading, compilation before execution, correctness
checks, training, tier polling, and warmup. GC and any ongoing JIT work **inside** the timed region count.
Numbers describe warmed execution, not launch latency. The report uses the median,
IQR, all samples, and the ratio of medians. A losing case names SBCL as the winner.

This is an exploratory workstation comparison, not the controlled CI regression
gate specified in §10.10. Frequency scaling, turbo, and background activity are
not controlled. No statistical-significance claim is made, and GC pause telemetry
is not collected. TorCL's timer has millisecond resolution on the measured build;
batches should be long enough to make clock quantization insignificant. Large
variance is visible in the sample table. Do not generalize a kernel win to all
Common Lisp or all of Ironclad.

## Ironclad provenance

`ironclad-power-mod.lisp` contains `power-mod-tab` and `power-mod` extracted
**without modifying their bodies or declarations** from
[Ironclad's `src/math.lisp` at f6519450b47a7648f837126e9f269857033e352a](https://github.com/sharplispers/ironclad/blob/f6519450b47a7648f837126e9f269857033e352a/src/math.lisp).
The original `in-package` form and unrelated definitions are omitted. The
benchmark adds only validation, inputs, and the timing workload around them.
The retained BSD license is in [LICENSE.ironclad](LICENSE.ironclad).
This is a standalone source excerpt, not an ASDF library port or dependency.

The initial exploration also checked Ironclad's `egcd`, `generate-small-primes`,
and `constant-time-equal`, plus iterative Fibonacci. Those exploratory probes
all favored SBCL on this machine, but did not enforce the final T2 gate.
They were not promoted into headline results:
the report focuses on two reproducible workloads with larger timed batches.
**No Ironclad workload faster than SBCL was established.** The suite makes future
compiler improvements measurable without changing the comparison to manufacture
a win.

## Validate and extend

```sh
python3 -m unittest discover -s benchmarks -v
```

Add a self-contained `.lisp` file defining `bench-validate`, `bench-train`, and
`bench-workload`, then add its metadata, `hot_functions`, and independently
checked integer checksum to `cases.json`. Training must heat every measured
function; do not infer tier installation merely from a fixed warmup count.
Use the same source and inputs for both implementations. Preserve third-party
licenses and revision links. Keep startup/throughput comparisons separate, and
report unsuccessful candidates as well as wins. Implementation-specific tier introspection belongs outside timing. Never add
implementation-name branches that make one runtime do less measured work.
