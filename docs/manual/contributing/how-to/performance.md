# Measure performance

Use this procedure to compare changes to EGCL's runtime or generated code.

## Fix the build and workload

Use release binaries and record the source revision, Cargo command, target,
feature flags, and environment variables. Do not infer performance from binary
size. A debug artifact at the same pathname may have been produced by a different
build command or profile.

Run a representative workload. To measure tiered steady state, use at least one
million loop iterations. The default OSR threshold is 100,000 back edges; a
100,000-iteration workload mixes interpretation and compilation near the
transition and can produce misleadingly variable measurements.

## Check the execution regime

Compare time per iteration over increasing input sizes. Do not use
`disassemble` or `function-tier` alone as proof that a running loop did or did
not enter native code: they report the installed function tier, not live OSR.
For the distinction, see [How Lisp runs](../../user/explanation/execution.md).

Separate startup, library load, compilation warmup, and steady-state timings.
Repeat measurements and preserve the command and raw output so another run can
reproduce them.

## Measure the right heap

`room` reports Lisp heap behavior. Rust-side strings, vectors, hash maps, and
other host allocations can dominate CPU time without appearing as Lisp consing.
Use the allocation-count instrumentation when investigating host allocation:

```sh
cargo build --release -p egcl --features alloc-count
EGCL_PROBE_ALLOC_COUNT=1 scripts/egcl-limited.sh \
  target/x86_64-unknown-linux-musl/release/egcl --no-init --load workload.lisp
```

Use the same memory cap and timeout in both comparisons. If the cap kills the
process, that is a failed run, not a valid performance sample. Detailed case
studies are in the repository's
[measurement notes](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/docs/design/measuring-performance.md).
