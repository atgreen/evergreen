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

## Paired CI-artifact runner

`ordinary_bridge.py` consumes unpacked immutable `ci-tests-musl` archives. Retain
the downloaded archive, its SHA256, workflow run ID and artifact ID before the
two-day artifact retention expires. Extract each into a separate directory;
these are runtime artifacts, not shared Cargo build targets. Their included
`target/ci-tests/manifest.json` records the actual CI merge revision, source
digest, profile, target, features and executable hashes. A PR branch head is not
necessarily the compiled merge revision.

```sh
python3 tests/benchmarks/ordinary_bridge.py \
  --before-root /tmp/bridge-before --before-run RUN --before-artifact ARTIFACT \
  --after-root /tmp/bridge-after --after-run RUN --after-artifact ARTIFACT \
  --build-description 'matched release toolchain/configuration; retained CI log references' \
  --cpu 4 --iterations 1000000 --samples 9 --trials 5 \
  --output /tmp/bridge-results
```

The runner verifies executable hashes and matched release configuration, keeps
both manifests, and first runs the after artifact's exact four-argument warmed
ordinary-edge integration test. This is evidence for an equivalent bridge with
zero cold handoffs after warming, **not a fallback counter for every timed
process**. It then alternates binary order across trials and direct/forward
order within each process, recording every paired sample, checksum, host load,
CPU affinity/governor, T2 log and unchanged pre/post function listings.

The parser rejects missing/changed T2 code and missing after-side ordinary ABI
entries, but instruction-level guard/argument/return and outer dispatch review
remains necessary. The distributions include negative forward-minus-direct
differences; they do not discard noise or imply a speedup. Warmup, disassembly,
checksum validation and printing are outside each timed call. The timed call
still includes its common outer dispatch.

Runner validation uses `python3 tests/benchmarks/test_ordinary_bridge.py`.
The baseline CI release artifact has passed a one-pair, 1000-iteration smoke
with all four named T2 listings stable; integrated after-artifact smoke and
quiet-host measurements remain outstanding. No timing result is claimed here.
