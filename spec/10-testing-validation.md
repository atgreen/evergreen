# §10  Testing & Validation

**Scope:** Strategy for verifying that Bliss meets its ANSI compliance,
performance, correctness, and robustness goals. Covers test suites,
benchmarking, continuous integration, and fuzzing.

## 10.1  ANSI Compliance Testing

**R10.01** Bliss MUST pass Paul Dietz's `ansi-test` suite with zero
unexpected failures before any stable release. Known-failure lists MUST
be tracked in `tests/ansi-test/expected-failures.txt` with per-failure
rationale.

**R10.02** The `ansi-test` suite MUST run in CI on every commit to
`main`. Regressions (new failures not in the expected-failures list)
MUST block the merge.

**R10.03** Bliss SHOULD additionally pass the `cl-test-grid` portable
test suite for library compatibility validation.

## 10.2  Unit & Integration Tests

**R10.04** Every Rust crate (`bliss-rt`, `bliss-compiler`, `bliss-stdlib`,
`bliss-cli`) MUST maintain unit tests in `tests/` subdirectories,
exercising public API surfaces.

**R10.05** Unit test coverage SHOULD exceed 80% line coverage for Rust
code as measured by `cargo llvm-cov`.

**R10.06** Integration tests MUST exercise end-to-end CL evaluation:
read → macroexpand → compile → execute → print. These live in
`tests/integration/` as `.lisp` scripts with expected-output annotations.

**R10.07** GC-specific stress tests MUST verify:
- Nursery collection under allocation pressure
- Old-gen concurrent marking correctness (no dangling pointers)
- Finalizer ordering and execution
- Weak reference clearing semantics
- Large-object allocation and reclamation

**R10.08** Thread-safety tests MUST exercise:
- Concurrent package modifications (`INTERN` / `UNINTERN` under load)
- Parallel hash-table operations (both synchronized and unsynchronized)
- Concurrent compilation (multiple threads promoting functions)
- GC safepoint rendezvous under thread contention

## 10.3  Performance Benchmarking

**R10.09** Bliss MUST track performance on `cl-bench` (Gabriel
benchmarks + Boehm GC benchmarks) against SBCL as the reference
implementation. Target: within 2× of SBCL (goal G2).

**R10.10** A benchmark suite MUST run on every release candidate and
results MUST be archived for trend analysis. Minimum benchmarks:

| Category | Benchmarks | Metric |
|----------|------------|--------|
| Arithmetic | `tak`, `fib`, `nbody` | wall-clock time |
| GC pressure | `gcbench`, `binary-trees` | time, peak RSS |
| Compiler | `compiler-bench` (compile 10K-line file) | compile time |
| Startup | `hello-world`, image load | time-to-first-output |
| Sequences | `sort-bench`, `map-bench` | time |
| CLOS | `method-dispatch`, `slot-access` | time |
| I/O | `read-large-file`, `format-bench` | throughput |

**R10.11** GC pause times MUST be measured via `perf` / runtime
instrumentation. P99 pause target: < 10 ms for heaps ≤ 32 GB (§3).

**R10.12** Startup time MUST be measured cold (no OS page cache) and
warm. Target: < 50 ms warm for a minimal image (goal G3).

## 10.4  Fuzzing

**R10.13** The CL reader (§4.1) MUST be fuzzed with `cargo-fuzz`
using `libFuzzer`. Corpus seeded from Quicklisp source files.

**R10.14** The compiler pipeline (read → IR → codegen) MUST be fuzzed
end-to-end, verifying no panics, no UB, and no infinite loops (timeout
= 10 s per input).

**R10.15** FFI boundary functions MUST be fuzzed with random
type/value combinations to verify no memory safety violations.

**R10.16** Fuzz findings MUST be triaged within 48 hours and added to
regression tests once fixed.

## 10.5  Continuous Integration

**R10.17** CI pipeline (GitHub Actions) MUST include:

| Stage | What | Blocking? |
|-------|------|-----------|
| `cargo check` | Type-check all crates | Yes |
| `cargo test` | Unit + integration tests | Yes |
| `cargo clippy` | Lint (deny warnings) | Yes |
| `ansi-test` | ANSI compliance suite | Yes |
| `cargo-fuzz` (short) | 60-second fuzz run | No (nightly) |
| `cl-bench` | Performance regression | No (release) |
| `cargo llvm-cov` | Coverage report | No (weekly) |

**R10.18** CI MUST run on both Tier-1 platforms:
- Linux x86-64 (Ubuntu LTS)
- macOS aarch64 (latest stable)

**R10.19** Nightly CI SHOULD include:
- Address Sanitizer (`cargo +nightly test -Z sanitizer=address`)
- Thread Sanitizer (`-Z sanitizer=thread`)
- Miri for unsafe code validation where feasible

## 10.6  Regression Testing

**R10.20** Every bug fix MUST include a regression test that fails
before the fix and passes after.

**R10.21** Deoptimisation paths (§4.6) MUST have dedicated regression
tests verifying that deoptimised code produces identical results to
unoptimised execution.

**R10.22** Image save/restore (§7) MUST be tested round-trip: save
image, restore, verify all state (packages, symbols, compiled code,
closures) is intact.
