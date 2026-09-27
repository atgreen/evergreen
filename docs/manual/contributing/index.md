# Runtime contributor notes

Start with the [runtime architecture](explanation/architecture.md) and
[repository map](reference/repository.md). Then follow
[Change the runtime](how-to/change-runtime.md) for a small, validated change.

Two practices matter especially in this codebase:

- [Keep values safe across GC](how-to/gc-safety.md). Allocating code can move
  Lisp objects; an ordinary Rust local is not automatically a root.
- [Measure performance](how-to/performance.md) in the execution regime you mean
  to measure. Warmup and steady-state execution are different workloads.

For manual changes, use the [documentation guidelines](../meta/documentation-guidelines.md).
The full repository policy is in
[AGENTS.md](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/AGENTS.md).
