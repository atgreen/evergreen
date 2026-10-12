# Measuring the initial ordinary call edge

`ordinary-bridge-direct` and `ordinary-bridge-forward` isolate an eligible
four-argument forwarding edge. Both execute the same counted accumulation loop,
pass `0`, `1`, `2`, and the loop index, and check the result `499999500000` for
one million iterations. Their call paths are:

```text
direct:  loop -> runtime entry -> identity leaf -> loop
forward: loop -> runtime entry -> forwarding bridge -> ordinary leaf -> loop
```

The identity leaf returns argument four. The bridge contains only a named call
and returns its result. These fit the first ordinary entry/call eligibility
rules: fixed arity zero through four, tagged values, no arithmetic leaf or loop
body. The existing `native-call4` benchmark's accumulation loop is outside that
subset; its loop-to-leaf timing alone cannot establish a new ordinary call edge.

Both variants load the same definitions, repeatedly warm both loops, and retain
one outer runtime call per iteration. `NOTINLINE` prevents either bridge or
leaf from being substituted into its caller. It does not prohibit another Lisp
implementation from tail-calling the leaf; inspect SBCL's code separately when
explaining cross-implementation differences.

Run correctness smoke checks on both implementations first. For timing, build
the current integrated release revision and use the existing runner:

```sh
tests/benchmarks/run.sh ordinary-bridge-direct ordinary-bridge-forward
```

Do not label the result an ordinary-ABI measurement until all of this evidence
has been retained from that binary:

- Per-function T2 publication for both loops, `ORDINARY-BRIDGE-FORWARD`, and
  `ORDINARY-BRIDGE-LEAF`. The runner's aggregate `T2=yes` is insufficient.
- Installed listings marking both bridge and leaf `Ordinary x86-64 Lisp ABI`.
  Inspect the distinct ordinary entries, rather than a legacy entry or adapter.
- The bridge's descriptor/revision/admission guards, argument homes in
  RDI/RSI/RDX with argument four on the stack, RCX count, RAX closure, stable
  R13 execution context, actual indirect `CALL` to the ordinary entry, and
  return-tuple/CF-preserving teardown. A managed child adapter is not this edge.
- Matching outer loop call/dispatch sequences: each loop enters its selected
  ordinary root through the same runtime path. A different tier, inlining,
  deoptimization, or extra outer adapter invalidates the matched comparison.
- Integration evidence that a warmed equivalent four-argument bridge takes
  zero ordinary cold handoffs with no pending poll work. Listing a fast path
  does not establish that the measured run uses it; retain any available
  dispatch/fallback diagnostics separately from timed runs.

Record revision, release profile, architecture, tier settings, disassemblies,
checksums, and host load. Measure on a quiet 64-bit host with at least five
repetitions, alternate pair order in a second run, and retain individual samples.
The runner reports minima from separate processes. Their difference estimates
incremental bridge overhead, including admission checks, argument movement,
stack/depth bookkeeping, and return; it is not the latency of a single `CALL`.
Reject differences comparable to timer resolution or run-to-run noise. The
checksum fits a fixnum on the intended 64-bit implementations; 32-bit boxing
would add a separate cost.

No timing or performance claim accompanies this workload. Current-release
execution and path verification are tracked by `bliss-t8a48.1`.
