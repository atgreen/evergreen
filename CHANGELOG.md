# Changelog

## 0.0.1 - 2026-10-01

The initial experimental release of Evergreen Common Lisp (EGCL). This version
establishes the starting point for a Common Lisp implementation with a tiered
JIT runtime and standalone application delivery.

- A reader and evaluator, macros, CLOS, conditions and restarts, and ASDF
  integration.
- Bytecode execution, baseline and optimizing native compilation, on-stack
  replacement, and deoptimization for supported operations.
- Precise generational garbage collection, native threads, and stackful fibers.
- Saved images and standalone executables, with application tree shaking.
- C interoperability and optional JVM and CPython integration.
- Runtime ports for x86-64, AArch64, PPC64LE, and s390x, including Windows and
  Android support on selected targets.
- Fedora RPM packaging, Android application tooling, and a user and contributor
  manual.

EGCL is under active development. See the [manual](docs/manual/index.md) for
supported interfaces and platform limitations.
