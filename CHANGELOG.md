# Changelog

Notable changes to Evergreen Common Lisp (EGCL) are recorded here, grouped by
version with the newest changes first.

## Unreleased

## 0.0.1 - 2026-10-01

Initial experimental development version. See the [manual](docs/manual/index.md)
for supported interfaces and platform limitations.

### Added

- Common Lisp reader, evaluator, macros, CLOS, conditions, restarts, and ASDF
  integration.
- Bytecode execution, baseline and optimizing native compilation, on-stack
  replacement, and deoptimization for supported operations.
- Precise generational garbage collection, native threads, and stackful fibers
  with cooperative I/O on established TCP streams.
- Saved images, standalone executables, and application delivery with tree
  shaking.
- C interoperability, JVM integration, and optional CPython integration.
- Runtime ports for x86-64, AArch64, PPC64LE, and s390x, including Windows and
  Android support on selected targets.
- Android application tooling, a user and contributor manual, and native,
  cross-platform, and GC-stress CI coverage.

### Changed

- Batch independent acceptance expressions per backend to reduce repeated
  startup, while retaining isolated tests for process-sensitive cases.
- Allow CI runs to finish when newer commits are pushed; queue documentation
  deployments without replacing pending runs.

### Fixed

- Preserve carrier-local state lookup across fiber migration and avoid native
  function-pointer alignment assumptions in cross-platform test fixtures.
- Synchronize shared state and retry backtrace snapshots in fiber API tests.
- Link the FFI math fixture against `libm` when using the system dynamic loader.
