# Public packed-double kernel benchmark

This benchmark prepares reproducible evidence for the closed raw-double pass.
It does not yet report an EGCL result: the public-loop release artifact is
pending. Do not claim a speedup, allocation-free kernel, or raw OSR support from
source inspection or a successful checksum alone.

The portable [workload](packed-kernel-workload.lisp) uses only public
`MAKE-ARRAY :ELEMENT-TYPE 'DOUBLE-FLOAT`, `AREF`, `*`, `+`, and `(SETF AREF)`.
The kernel takes `(src dst n scale)` and returns `dst`; its body is:

```lisp
(dotimes (i n)
  (let ((x (aref src i)))
    (setf (aref dst i) (+ (* x scale) x))))
```

Keep parameter type declarations absent for the initial EGCL admission path.
Runtime guards establish packed rank-one array layout, index bounds, and a
boxed double scalar (`2d0`, not fixnum `2`). The loaded value, product, and sum
remain within this graph. No raw value is returned or passed to another Lisp
function. `NOTINLINE` preserves the measured kernel entry.

Source elements repeat exactly representable doubles 1 through 32; each output
must equal three times its corresponding source. Validation checks **every
output**, then checks the exact integer checksum:

```text
3 * (528 * floor(n/32) + (n mod32) * ((n mod32)+1) /2)
```

For n=4096 the checksum is 202752. Arrays, initialization, repeated full-entry
warmup, listings, checksum validation, allocation probes, and printing are
outside timed samples. A sample performs a fixed number of complete kernel
invocations; its denominator is `elements * passes`. Report that fixed outer
invocation/harness overhead rather than treating the value as instruction cost.

## Runner and correctness smoke

```sh
python3 tests/benchmarks/test_packed_kernel.py
python3 tests/benchmarks/packed_kernel.py --sbcl-only --sbcl-mode typed --cpu 4 \
  --elements 65536 --passes 256 --samples 3 --trials 1 \
  --output /tmp/packed-sbcl-typed-smoke
```

The SBCL smoke validates tooling and public Lisp behavior. It is not a quiet
performance run. The runner retains the SBCL version, executable **and core**
hashes, timer units, CPU affinity/governor, loads, exact generated source,
listings, checksums, individual samples, and stdout/stderr. EGCL uses its public
nanosecond clock; SBCL uses `GET-INTERNAL-REAL-TIME` scaled by its reported units.
Increase passes if samples approach clock resolution; a zero-time sample is
rejected. Separate process trials alternate implementation order.

Once the verified public-loop CI artifact is available:

```sh
python3 tests/benchmarks/packed_kernel.py \
  --egcl-root /tmp/packed-ci/extracted --egcl-run RUN --egcl-artifact ARTIFACT \
  --cpu 4 --elements 262144 --passes 16 --samples 9 --trials 5 \
  --output /tmp/packed-comparison
```

No build is performed. The runner requires an x86-64 musl release artifact and
verifies the executable against its copied CI manifest. Preserve the downloaded
archive, its hash, and build logs alongside the results. An EGCL process is
accepted only when the **named kernel** has all of:

- Stable pre/post mapped T2 listings containing native `MULSD` and `ADDSD`.
- A named installation log with `raw_f64_values >=4`, followed by actual mapped
  installation/publication and `osr_entries=0`.
- No named deoptimization, invalidation, or recompilation in that process log.

Repeated full invocations warm the ordinary mapped function entry. These checks
make **no raw OSR claim**; representation-aware OSR import is separate work.
Inspect the actual loop, guards, loads/stores, and slow branches before claiming
an allocation-free fast path. Listings and logging cannot by themselves count
fallbacks in every timed invocation.

## Real allocation counter and limits

Allocation probes use the implementations' existing public `(TIME (kernel ...))`
report, redirected through `*TRACE-OUTPUT*`, **separately from timed samples**.
The same warmed function and already allocated arrays are called at 0, n, and
2n elements. The parser requires the reported `bytes consed` field; missing
counters are errors, never invented zeroes. It preserves all full-call counts
and the n/2n-minus-zero differences, including negatives.

In EGCL, `cli.rs`'s `TIME` reads `egcl_rt::heap_stats().bytes_allocated` around
form evaluation. Heap double boxing goes through `alloc_double_float` →
`alloc_typed` → `HeapAllocator`, which increments this Lisp allocation statistic.
Thus per-element boxing should scale these counts. The statistic is global and
includes Lisp allocation during dispatch/evaluation. Equal counts at 0/n/2n
provide evidence of no **size-dependent Lisp consing in these calls**, not a
proof that full calls allocate nothing. Nonzero fixed adapter costs must remain
visible. Repeat stable probes and pair them with actual native-loop inspection
before asserting allocation-free inner execution.

This counter does **not** measure Rust heap allocation. EGCL's optional
`alloc-count` feature and `EGCL_PROBE_ALLOC_COUNT` report process-wide Rust
allocations, including startup/compiler activity, rather than a per-kernel
counter; the runner does not pretend otherwise. No allocation-free claim is
made solely by subtracting a noisy baseline.

## SBCL comparison scope

The runner defaults to `--sbcl-mode typed`, an explicitly optimized SBCL
reference. It inserts declarations only into SBCL's copy of the **same kernel
operation**, preserving the shared initialization, checksum, and arithmetic:

```lisp
(declare (optimize (speed 3) (safety 1) (debug 0))
         (type (simple-array double-float (*)) src dst)
         (type (integer 0 20000000) n)
         (type double-float scale))
;; Within DOTIMES:
(declare (type fixnum i))
```

The index/count bound covers the runner's largest 2n allocation probe. These
settings, reference mode, complete generated source, and hashes are recorded.
The typed path must retain native `MOVSD`, `MULSD`, and `ADDSD` in its stable
pre/post listing. Its correctness smoke at n=65536 returned checksum3244032
for all three samples; real TIME probes at0/65536/131072 reported0/0/0Lispbytes.
The inspected 80-byte kernel uses double loads/multiply/add/store and retains
array bounds traps. This is allocation-counter and code evidence for the tested
reference, not a quiet throughput measurement. A smaller batch first failed the
zero elapsed-time guard, so the smoke batch was enlarged rather than accepting
an unmeasurable sample.

`--sbcl-mode same-source` remains a separately labeled undeclared-source control.
Its smoke observed48Lispbytes per element from generic array/arithmetic boxing.
Do not silently mix that control with the typed reference or establish a broad
“2× SBCL” goal against the generic comparator. EGCL's source remains undeclared
in **both modes** because the current admission pass uses runtime guards. Any
future speed comparison must state this asymmetric declaration policy and the
explicit SBCL safety/optimization settings.

Retain min/median/P95/max and every sample, not just a best time. Quiet repeated
measurements, EGCL release/path evidence, allocation observations, and explicit
baseline scope remain prerequisites for publishing conclusions. This task
currently supplies tooling and correctness smoke only.
