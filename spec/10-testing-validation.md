# §10  Testing & Validation

**Scope:** Strategy for verifying that Bliss meets its ANSI compliance,
performance, correctness, and robustness goals. Covers test suites,
benchmarking, continuous integration, fuzzing, property-based testing,
differential testing, sanitizer integration, performance regression
detection, and test infrastructure.

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

### 10.4.1  Fuzz Targets

**R10.23** The following dedicated fuzz targets MUST be maintained
under `fuzz/fuzz_targets/`:

| Target | Entry point | Input format |
|--------|------------|--------------|
| `fuzz_reader` | `bliss_compiler::reader::read_from_string` | Raw bytes (UTF-8 lossy) |
| `fuzz_macroexpand` | reader → macroexpand | Raw bytes → parsed form |
| `fuzz_compile` | reader → macroexpand → IR → codegen | Raw bytes → compiled fn |
| `fuzz_eval` | Full read-eval pipeline | Raw bytes |
| `fuzz_format` | `bliss_stdlib::format::format_to_string` | Structured: control string + args |
| `fuzz_ffi` | `bliss_rt::ffi::call_foreign` | Structured: type-tag + value bytes |
| `fuzz_image_load` | `bliss_rt::image::load_image` | Raw bytes (corrupt image) |

### 10.4.2  Corpus Management

**R10.24** Each fuzz target MUST maintain a persistent seed corpus in
`fuzz/corpus/<target_name>/`. Seed corpora MUST be checked into the
repository.

**R10.25** Seed corpus construction:

| Target | Seed sources | Minimum seeds |
|--------|-------------|---------------|
| `fuzz_reader` | Quicklisp top-50 systems' `.lisp` files, ANSI test suite inputs | 500 |
| `fuzz_compile` | All reader seeds + known edge-case forms (circular refs, deep nesting) | 200 |
| `fuzz_eval` | Compile seeds + `ansi-test` expressions | 300 |
| `fuzz_format` | FORMAT directive corpus: every `~` directive with boundary args | 100 |
| `fuzz_ffi` | Type-tag × value matrix (all 8 tag types × boundary values) | 64 |
| `fuzz_image_load` | Valid `.bimg` images + header-mutated variants | 50 |

**R10.26** Corpus minimisation MUST be run weekly via
`cargo fuzz cmin <target>`. The minimised corpus replaces the previous
checked-in corpus. Corpus growth beyond 10,000 entries per target
triggers mandatory minimisation.

**R10.27** A merge corpus (`fuzz/corpus-merge/`) SHOULD be maintained
for cross-target pollination — inputs that exercised interesting paths
in one target are injected as seeds for related targets (e.g.,
`fuzz_reader` findings feed `fuzz_compile`).

### 10.4.3  Coverage-Guided Feedback

**R10.28** All fuzz targets MUST be compiled with LLVM source-based
coverage (`-C instrument-coverage`) to enable coverage-guided mutation.
`cargo-fuzz` provides this by default via `libFuzzer`.

**R10.29** Weekly fuzz coverage reports MUST be generated using
`llvm-cov` and archived. A coverage dashboard SHOULD track:
- Total edge coverage per target (absolute and delta from previous week)
- Uncovered functions in fuzzed modules (triage: unreachable vs. gap)
- New edges discovered in the past 7 days (zero new edges for 2
  consecutive weeks triggers corpus review)

**R10.30** Custom mutators SHOULD be implemented for structured targets:

```rust
// Example: custom mutator for fuzz_format
// Preserves FORMAT control-string structure while mutating directives
fn custom_mutate(data: &mut [u8], max_size: usize) -> usize {
    // Parse as control-string + args separator
    // Mutate: swap directives, insert/remove ~X, flip case
    // Preserve: matching ~{ ~} nesting, valid UTF-8
    ...
}
```

**R10.31** For `fuzz_eval`, a comparison function MUST be provided that
ignores non-deterministic output (e.g., hash-table print order, object
addresses) when checking for crashes vs. differential output.

### 10.4.4  Crash Triage Workflow

**R10.32** Fuzz crash triage MUST follow this workflow:

```text
 1. DISCOVER — libFuzzer writes crashing input to fuzz/artifacts/<target>/
 2. REPRODUCE — `cargo fuzz run <target> fuzz/artifacts/<target>/crash-<hash>`
 3. MINIMISE — `cargo fuzz tmin <target> <crash-input> -o <minimised>`
 4. CLASSIFY — Assign severity:
      P0-critical: memory safety violation (UB, use-after-free, buffer overflow)
      P1-high:     panic in unsafe block or infinite loop
      P2-medium:   unexpected panic in safe Rust (unwinding)
      P3-low:      incorrect output without safety impact
 5. FILE — Create GitHub issue with labels `fuzz`, `P<N>`, `<target>`
 6. FIX — Implement fix; add minimised input to regression corpus
 7. VERIFY — Re-run fuzz target on the minimised input → no crash
```

**R10.33** P0 and P1 crashes MUST block the next release. P2 crashes
SHOULD be fixed within 2 sprints. P3 crashes are tracked but MAY be
deferred.

**R10.34** All minimised crash inputs MUST be added to
`fuzz/regression/<target>/` and MUST be run as part of the standard
`cargo test` suite (not just fuzz runs) to prevent regressions.

**R10.35** A crash deduplication heuristic MUST be applied before
filing: stack-trace signature (top 5 frames, demangled, ignoring
address offsets). Duplicate signatures are appended to the existing
issue.

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

## 10.7  Property-Based Testing

**R10.36** Bliss MUST employ property-based testing (QuickCheck-style)
using the `proptest` crate for Rust components and a custom CL-side
property test harness for self-hosted components.

### 10.7.1  Generator Strategy

**R10.37** The following generators MUST be implemented and maintained
in `tests/proptest/generators/`:

| Generator | Output type | Strategy |
|-----------|------------|----------|
| `arb_sexp` | Random s-expression | Recursive: atoms (fixnum, float, char, string, symbol) + compound (cons, list of depth ≤ 8, vector, nested quote/backquote) |
| `arb_type_specifier` | CL type specifier | Grammar-based: atomic types, `(AND ...)`, `(OR ...)`, `(NOT ...)`, `(MEMBER ...)`, `(SATISFIES ...)`, `(INTEGER low high)`, `(ARRAY el-type dims)`, `(FUNCTION arg-types result-type)` |
| `arb_class_hierarchy` | Class hierarchy DAG | Generate 3–20 classes with random superclass edges (acyclic), slot definitions, method definitions; ensure metaclass consistency |
| `arb_format_string` | FORMAT control string | Grammar-based: literal chars + `~` directives with random params and nesting of `~{~}`, `~[~]`, `~<~>` |
| `arb_lambda` | Lambda expression | Random params (required, &optional, &rest, &key with defaults) + body from `arb_sexp` |
| `arb_package_ops` | Package operation sequence | Random interleave of `MAKE-PACKAGE`, `USE-PACKAGE`, `INTERN`, `EXPORT`, `UNINTERN`, `DELETE-PACKAGE` |

**R10.38** Recursive generators (e.g., `arb_sexp`) MUST use size-bounded
recursion to prevent unbounded growth. Default max depth = 8, max
breadth = 16. These limits MUST be configurable per-test.

### 10.7.2  Properties to Verify

**R10.39** The following properties MUST be tested:

| Property | Generator | Invariant |
|----------|-----------|-----------|
| Reader round-trip | `arb_sexp` | `(equal x (read-from-string (write-to-string x)))` for printable-readable types |
| Type lattice consistency | `arb_type_specifier` | `(subtypep A B)` ∧ `(subtypep B C)` → `(subtypep A C)` (transitivity) |
| Type disjointness | `arb_type_specifier` | `(subtypep A (not B))` → `(not (typep x A))` ∨ `(not (typep x B))` for random `x` |
| CLOS linearization | `arb_class_hierarchy` | `COMPUTE-CLASS-PRECEDENCE-LIST` succeeds or signals `PROGRAM-ERROR` for inconsistent hierarchies — never panics |
| FORMAT ↔ parse | `arb_format_string` | Parse and re-emit control string; re-parse must produce identical directive tree |
| Package isolation | `arb_package_ops` | After any operation sequence, internal symbol tables MUST be consistent (no dangling symbol refs, no orphaned home-package pointers) |
| GC liveness | `arb_sexp` | Allocate random object graph, trigger GC, verify all reachable objects survived and unreachable were collected |
| Hash-table consistency | Random key-value pairs | After random insert/delete/rehash sequence, all `GETHASH` lookups agree with shadow map |

**R10.40** Property tests MUST run as part of `cargo test` with a
default iteration count of 256. CI nightly runs SHOULD use 10,000
iterations.

### 10.7.3  Shrinking

**R10.41** All generators MUST support automatic shrinking. For
`arb_sexp`, shrinking MUST reduce: depth first, then breadth, then
atom complexity (e.g., large fixnum → 0, long string → `""`).

**R10.42** Failing seeds MUST be persisted to
`tests/proptest/regressions/<test_name>.txt` and re-run on every
`cargo test` invocation to prevent regressions.

## 10.8  Differential Testing Against SBCL

**R10.43** Bliss MUST support differential testing against SBCL as
the reference implementation. The differential test harness lives in
`tests/differential/`.

### 10.8.1  Harness Architecture

```text
┌──────────────────────┐     ┌─────────────┐     ┌─────────────┐
│  Test Generator      │────▶│  SBCL runner │────▶│  Reference  │
│  (arb_sexp / corpus) │     │  (subprocess)│     │  output     │
│                      │     └─────────────┘     └──────┬──────┘
│                      │                                 │ compare
│                      │     ┌─────────────┐     ┌──────▼──────┐
│                      │────▶│ Bliss runner │────▶│  Test       │
│                      │     │ (subprocess) │     │  output     │
│                      │     └─────────────┘     └─────────────┘
└──────────────────────┘
```

**R10.44** The differential harness MUST:
1. Accept input as a CL expression (text).
2. Evaluate it under both SBCL and Bliss in sandboxed subprocesses
   with a 10-second timeout and 512 MB memory limit.
3. Capture: printed output, return values (printed readably), condition
   type (if signalled), and exit status.
4. Compare outputs using the canonicaliser (§10.8.2).
5. Report: PASS (identical), KNOWN-DIFF (in exceptions list), or FAIL.

### 10.8.2  Output Canonicalisation

**R10.45** Before comparison, outputs MUST be canonicalised:

| Non-determinism source | Canonicalisation |
|------------------------|-----------------|
| Object addresses (`#<FUNCTION {1234}>`) | Replace `{[0-9A-F]+}` with `{ADDR}` |
| Hash-table print order | Sort entries by printed key |
| Gensym counters (`#:G123`) | Normalise to `#:G<N>` in encounter order |
| Floating-point formatting | Round to 6 significant digits |
| Implementation-specific type names | Strip package prefix for known aliases |
| Pathname host/device | Normalise to logical pathname where possible |

### 10.8.3  Known-Difference Exceptions

**R10.46** A file `tests/differential/known-diffs.toml` MUST track
legitimate behavioural differences:

```toml
[[exception]]
pattern = "(type-of 42)"
reason = "SBCL returns FIXNUM; Bliss returns (INTEGER 0 4611686018427387903)"
category = "type-specifier-detail"

[[exception]]
pattern = "(documentation 'car 'function)"
reason = "Implementation-specific documentation strings"
category = "documentation"
```

**R10.47** Each exception MUST reference the ANSI section that permits
the divergence or be tagged `bliss-extension` if it stems from §9.

### 10.8.4  Differential Test Corpus

**R10.48** The differential corpus MUST include:

| Source | Count | Description |
|--------|-------|-------------|
| `ansi-test` expressions | ~18,000 | Extracted from test suite |
| Property-generated | 1,000/run | From `arb_sexp` + `arb_lambda` |
| Edge cases | 200+ | Hand-curated: numeric boundaries, reader macros, circular structures, `EVAL-WHEN` variants, `THE`/`DECLARE` interactions |
| Quicklisp top-20 load forms | — | Top-level forms from popular libraries |

**R10.49** Differential tests MUST run nightly in CI. New FAIL results
MUST be filed as issues with label `differential`.

## 10.9  Sanitizer Integration

**R10.50** Bliss MUST be regularly tested under LLVM sanitizers to
detect memory safety, threading, and uninitialised-memory bugs in
Rust `unsafe` blocks, inline assembly, and FFI code.

### 10.9.1  Sanitizer Configurations

**R10.51** The following sanitizer configurations MUST be maintained:

| Sanitizer | Cargo flag | Scope | Frequency |
|-----------|-----------|-------|-----------|
| AddressSanitizer (ASan) | `-Z sanitizer=address` | All crates | Nightly CI |
| ThreadSanitizer (TSan) | `-Z sanitizer=thread` | `bliss-rt` (thread, gc, scheduler) | Nightly CI |
| MemorySanitizer (MSan) | `-Z sanitizer=memory` | `bliss-rt` (ffi, image loader) | Weekly CI |
| LeakSanitizer (LSan) | `ASAN_OPTIONS=detect_leaks=1` | All crates (Linux only) | Nightly CI (with ASan) |

**R10.52** Sanitizer builds MUST use the nightly Rust toolchain:
```bash
RUSTFLAGS="-Z sanitizer=address" \
  cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
  --tests -- --test-threads=1
```

### 10.9.2  Suppression Files

**R10.53** Suppression files MUST be maintained at
`tests/sanitizers/<sanitizer>.supp` for known false positives:

```text
# tests/sanitizers/tsan.supp
# False positive: GC safepoint handshake uses acquire/release
# fencing that TSan cannot model (§3 write barrier)
race:bliss_rt::safepoint::poll
race:bliss_rt::gc::write_barrier_slow

# False positive: symbol value cell uses SeqCst atomics but
# TSan sees the non-atomic fast-path read
race:bliss_rt::object::SymbolCell::value_relaxed
```

```text
# tests/sanitizers/asan.supp
# Intentional: stack-scanning reads past logical stack top
# to find conservative GC roots (§3)
interceptor_via_fun:bliss_rt::gc::scan_stack_conservative
```

**R10.54** Every suppression entry MUST include:
- The spec section it relates to (e.g., `§3`, `§2.4`)
- A one-line justification
- The date added and a tracking issue number

**R10.55** Suppression files MUST be audited quarterly. Suppressions
whose underlying code has changed MUST be re-validated or removed.

### 10.9.3  Sanitizer-Specific Build Adjustments

**R10.56** The build system MUST support the following
sanitizer-related adjustments:

| Adjustment | Reason |
|-----------|--------|
| Disable custom allocator under ASan | ASan replaces malloc; custom TLAB allocator must fall back to system alloc |
| Reduce nursery size to 256 KB under ASan | Prevent ASan shadow memory exhaustion |
| Disable green-thread stack switching under TSan | TSan cannot track `setjmp`/`longjmp` across green-thread stacks |
| Annotate `__attribute__((no_sanitize("memory")))` on JIT code entry | JIT-emitted code is opaque to MSan; entry stubs must be annotated |

**R10.57** A compile-time feature flag `cfg(sanitizer)` MUST gate
these adjustments. The flag MUST be automatically set when any
`-Z sanitizer=*` flag is detected.

## 10.10  Performance Regression Detection

**R10.58** Bliss MUST employ a statistically rigorous methodology for
detecting performance regressions in CI, avoiding false alerts from
measurement noise.

### 10.10.1  Measurement Protocol

**R10.59** Each benchmark MUST be run a minimum of **N = 5** times
(after 3 warm-up iterations that are discarded). The reported metrics
are:
- Median wall-clock time
- Interquartile range (IQR)
- Peak RSS (resident set size)
- GC pause count and P99 pause duration

**R10.60** Benchmarks MUST run on dedicated CI runners with:
- CPU frequency scaling disabled (`performance` governor)
- Turbo boost disabled
- No concurrent workloads (isolated runner)
- `taskset` to pin to specific cores
- `nice -20` for highest scheduling priority

### 10.10.2  Statistical Comparison

**R10.61** Performance regression detection MUST use the
**Mann-Whitney U test** (non-parametric, no normality assumption)
to compare the current commit's sample against the baseline:

```text
Algorithm: Mann-Whitney U Test for Regression Detection
─────────────────────────────────────────────────────────
Inputs:
  baseline[N]  — timing samples from the last known-good commit
  current[N]   — timing samples from the commit under test

Steps:
  1. Compute U statistic and p-value (two-tailed)
  2. Compute effect size r = Z / √(N₁ + N₂)
  3. Compute relative change = (median_current - median_baseline) / median_baseline

Decision matrix:
  ┌──────────────┬───────────┬───────────────────────────────────┐
  │ p-value      │ Effect    │ Action                            │
  ├──────────────┼───────────┼───────────────────────────────────┤
  │ p < 0.01     │ r > 0.5   │ ALERT — block merge if Δ > 5%    │
  │ p < 0.01     │ r ≤ 0.5   │ WARN — log, do not block         │
  │ p < 0.05     │ any       │ NOTE — log for trend tracking     │
  │ p ≥ 0.05     │ any       │ PASS — within noise               │
  └──────────────┴───────────┴───────────────────────────────────┘
```

**R10.62** The alerting thresholds MUST be configurable per-benchmark
in `tests/benchmarks/thresholds.toml`:

```toml
[benchmarks.tak]
min_samples = 7
alert_pct = 5.0       # relative change threshold for ALERT
warn_pct = 3.0        # relative change threshold for WARN
p_threshold = 0.01

[benchmarks.gcbench]
min_samples = 10       # GC benchmarks are noisier
alert_pct = 8.0
warn_pct = 5.0
p_threshold = 0.01

[benchmarks.startup]
min_samples = 15       # startup is very noisy
alert_pct = 15.0
warn_pct = 10.0
p_threshold = 0.005
```

### 10.10.3  Baseline Management

**R10.63** The performance baseline MUST be updated only on `main`
after a merge. Baselines are stored in
`tests/benchmarks/baselines/<platform>/<commit-sha>.json`.

**R10.64** Baseline rolling window: the comparison uses the most
recent 5 baselines' pooled samples (to smooth out commit-to-commit
noise). If the pooled comparison still alerts, the regression is real.

**R10.65** A quarterly performance audit MUST review trend data and
update thresholds if the benchmark's coefficient of variation has
changed (e.g., due to hardware changes on CI runners).

### 10.10.4  Reporting

**R10.66** Benchmark results MUST be published as CI artifacts in
JSON format and rendered as trend charts in a project dashboard
(e.g., GitHub Pages or Bencher.dev).

**R10.67** Each benchmark result MUST include:

```json
{
  "benchmark": "tak",
  "commit": "abc1234",
  "platform": "linux-x86_64",
  "samples_ns": [1234567, 1234890, ...],
  "median_ns": 1234700,
  "iqr_ns": 323,
  "peak_rss_kb": 8192,
  "gc_pauses": 3,
  "gc_p99_pause_ns": 450000,
  "vs_baseline": {
    "p_value": 0.42,
    "effect_size_r": 0.12,
    "relative_change_pct": 0.3,
    "verdict": "PASS"
  }
}
```

## 10.11  Test Infrastructure

### 10.11.1  Test Harness Architecture

**R10.68** Bliss MUST provide a unified test harness (`bliss-test`)
that drives all CL-level tests. Architecture:

```text
┌────────────────────────────────────────────────────────┐
│                    bliss-test harness                   │
├──────────┬──────────┬──────────┬──────────┬────────────┤
│  ANSI    │  Unit    │  Integ   │  Prop    │  Diff      │
│  runner  │  runner  │  runner  │  runner  │  runner    │
├──────────┴──────────┴──────────┴──────────┴────────────┤
│              Test Discovery & Scheduling               │
├────────────────────────────────────────────────────────┤
│         Result Collector & Report Generator            │
└────────────────────────────────────────────────────────┘
```

**R10.69** `bliss-test` MUST support:
- Parallel test execution across multiple Bliss subprocess workers
- Timeout per test (default 30 s, configurable)
- Retry on flaky detection (max 2 retries; if a test passes on
  retry, it is marked `flaky` and tracked)
- JUnit XML and JSON output for CI integration
- TAP (Test Anything Protocol) output for human consumption

### 10.11.2  Expected-Output Format

**R10.70** CL integration tests MUST use the following annotation
format in `.lisp` test files:

```lisp
;;; TEST: reader-basic-list
;;; TAGS: reader, smoke
;;; TIMEOUT: 5

;; --- test body ---
(print (read-from-string "(1 2 3)"))

;;; EXPECT-OUTPUT:
;;; (1 2 3)

;;; EXPECT-VALUES:
;;; (1 2 3)
;;; 5
```

**R10.71** Annotation directives:

| Directive | Required? | Description |
|-----------|-----------|-------------|
| `TEST:` | Yes | Unique test name (kebab-case) |
| `TAGS:` | No | Comma-separated tags for filtering |
| `TIMEOUT:` | No | Per-test timeout in seconds (default 30) |
| `EXPECT-OUTPUT:` | No | Expected stdout (exact match after canonicalisation) |
| `EXPECT-VALUES:` | No | Expected return values, one per line (printed readably) |
| `EXPECT-CONDITION:` | No | Expected condition type (e.g., `TYPE-ERROR`) |
| `EXPECT-ERROR:` | No | Alias for `EXPECT-CONDITION:` |
| `SKIP-WHEN:` | No | Predicate: `platform=macos`, `feature=no-threads`, etc. |
| `KNOWN-FAILURE:` | No | Bug tracker reference; test runs but failure is expected |

### 10.11.3  Test Tagging & Filtering

**R10.72** Tests MUST be taggable with one or more tags from a
controlled vocabulary:

| Tag | Meaning |
|-----|---------|
| `smoke` | Fast, critical-path test; runs on every commit |
| `reader` | Tests the CL reader (§4.1) |
| `compiler` | Tests compilation pipeline (§4) |
| `gc` | Tests garbage collector (§3) |
| `clos` | Tests CLOS (§5.2) |
| `conditions` | Tests condition system (§5.3) |
| `threads` | Tests thread safety (§2) |
| `ffi` | Tests foreign function interface (§2) |
| `format` | Tests FORMAT (§5.6) |
| `perf` | Performance-sensitive test |
| `slow` | Takes > 10 s; excluded from default `cargo test` |
| `nightly` | Only runs in nightly CI |
| `flaky` | Known non-deterministic; tracked for stabilisation |

**R10.73** Test filtering MUST be supported via command-line:

```bash
# Run only smoke tests
bliss-test --tags smoke

# Run everything except slow and nightly
bliss-test --exclude-tags slow,nightly

# Run tests matching a name pattern
bliss-test --filter 'reader-*'

# Run tests for a specific spec section
bliss-test --tags reader,compiler
```

### 10.11.4  Test Organisation on Disk

**R10.74** Test files MUST be organised as follows:

```text
tests/
├── ansi-test/                  # Git submodule → Paul Dietz's suite
│   ├── expected-failures.txt   # Known failures with rationale
│   └── ...
├── integration/                # CL integration tests
│   ├── reader/                 # Grouped by subsystem
│   │   ├── basic-atoms.lisp
│   │   ├── backquote.lisp
│   │   └── ...
│   ├── compiler/
│   ├── clos/
│   ├── conditions/
│   ├── gc/
│   ├── threads/
│   └── format/
├── differential/               # Differential test harness (§10.8)
│   ├── harness.rs
│   ├── known-diffs.toml
│   └── corpus/
├── proptest/                   # Property-based tests (§10.7)
│   ├── generators/
│   └── regressions/
├── benchmarks/                 # Performance benchmarks (§10.10)
│   ├── thresholds.toml
│   └── baselines/
├── sanitizers/                 # Suppression files (§10.9)
│   ├── asan.supp
│   ├── tsan.supp
│   └── msan.supp
fuzz/                               # Fuzz targets and corpora (§10.4)
├── fuzz_targets/
├── corpus/
└── regression/
```

### 10.11.5  CI Integration Matrix

**R10.75** The full CI test matrix:

| Trigger | Test suite | Platform | Sanitizers | Timeout |
|---------|-----------|----------|------------|---------|
| Every push | `smoke` + `cargo test` | linux-x86_64, macos-aarch64 | None | 15 min |
| Every PR | All except `slow`, `nightly` | linux-x86_64, macos-aarch64 | None | 30 min |
| Nightly | All + fuzz (60s) + differential | linux-x86_64 | ASan, TSan | 120 min |
| Weekly | All + fuzz (3600s) + coverage | linux-x86_64 | ASan, TSan, MSan | 480 min |
| Release | All + benchmarks + full ansi-test | linux-x86_64, macos-aarch64 | All | 600 min |

## 10.12  Requirement Traceability

The repository already treats requirement identifiers as first-class
artifacts via `scripts/spec-coverage.py`. This section makes that
contract normative so later implementation and review work can rely on
it consistently.

**R10.76** Every automated test that exists primarily to verify a
normative requirement MUST cite at least one requirement ID (`R*.*`) in
source comments or test names so that traceability tooling can detect
the link.

**R10.77** The repository MUST maintain a machine-generated coverage
report from `scripts/spec-coverage.py` that distinguishes `MUST`,
`SHOULD`, and uncategorised requirements by chapter and by ID.

**R10.78** Release builds MUST gate on traceability quality: every
`MUST` requirement in chapters §2–§10 that is marked implemented in code
or documentation MUST either have at least one automated test reference
or be listed in an explicit waiver file with rationale and owner.

### 10.12.1  Traceability Sources

The traceability report aggregates evidence from the following sources:

| Source | Evidence type | Notes |
|--------|---------------|-------|
| Rust unit/integration tests | `R*.*` identifiers in comments, module docs, or test names | Primary implementation-level evidence |
| CL integration tests | `;; R*.*` annotations in `.lisp` files | Used for reader/compiler/stdlib behaviour |
| Fuzz regressions | Requirement IDs in regression test wrappers | Counts only when replayed under `cargo test` |
| Benchmark harness metadata | Requirement IDs for performance constraints | Used for R4/R7/R10/R11 performance gates |
| Waiver registry | Explicit uncovered requirement with justification | Release-management exception path only |

### 10.12.2  Waiver Registry

`spec/traceability-waivers.toml` SHOULD be used for temporary gaps that
are understood but not yet automated. Each waiver entry MUST contain:

- `requirement_id`
- `scope` (`unit`, `integration`, `fuzz`, `benchmark`, or `manual`)
- `justification`
- `owner`
- `expires_on`

Expired waivers MUST fail the release traceability check.

### 10.12.3  Review Workflow

When a change adds or materially revises a normative requirement, the
same change series SHOULD include one of:

- a new automated test referencing the requirement ID,
- an update to an existing test to reference the requirement ID more
  precisely, or
- a time-bounded waiver entry.

This keeps specification growth incremental rather than allowing large
untracked testing debt to accumulate.

## 10.13  Stage-Gated Acceptance

The staged build policy in `spec/stages.json`, §0.3, and §11 requires
acceptance coverage that matches the current vertical slice rather than
pretending the entire language is already complete.

**R10.79** The primary acceptance gate for active development MUST be
stage-aware: it MUST execute the current stage's gate scenario plus a
no-regression pass of all earlier stages through the real `bliss`
binary.

**R10.80** Advancing `spec/stages.json` `current_stage` MUST require
simultaneous evidence of:
- the new stage gate passing through the real CLI/REPL entrypoint,
- all earlier stage gates still passing, and
- traceability coverage for all in-scope `MUST` requirements as defined
  by `scripts/spec-coverage.py --gate`.

**R10.81** Acceptance tests for stage gates MUST assert a user-visible
result: printed value, printed diagnostic, exit status, produced file,
or persisted runtime artifact. Parser-only, type-only, or mocked-seam
tests MUST NOT be used as sole evidence that a gate passes.

### 10.13.1  Real Entrypoint Contract

**R10.82** Stage-gate acceptance scenarios MUST drive the same surface a
user would drive in that stage: `bliss --eval`, `bliss --load`,
positional script execution, or the interactive REPL. Calling internal
library functions directly MAY supplement acceptance coverage, but MUST
NOT replace the real-entrypoint gate.

**R10.83** A stage gate that claims file loading or library loading MUST
exercise an actual filesystem path and actual evaluator/loader work.
Recognising a specific path, basename, or hard-coded form to skip the
real load/eval path is prohibited.

### 10.13.2  Evidence Quality Rules

**R10.84** Acceptance artifacts and test code MUST make seam-faking
detectable during review. Where a gate concerns source loading,
evaluation, compilation, or image loading, the test SHOULD include a
payload whose observable result depends on the real subsystem under
test rather than on a canned success path.

**R10.85** When a stage is too large for one delivery step, interim work
SHOULD add observable progress tests that measure how far a real payload
gets through the real binary, while keeping the formal gate unchanged
until the full stage contract is met.
