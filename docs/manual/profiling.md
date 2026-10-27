# Profiling and efficiency

A tiered implementation has several costs: startup, source loading, bytecode
compilation, native compilation, and steady-state execution. Decide which one
you are measuring before interpreting a timing.

Use release binaries for comparisons. A loop near an OSR threshold measures
warmup as well as execution. For steady-state loop measurements, use at least
one million iterations and inspect several input sizes.

## Engine observations

### `egcl-ext:profile-report` { #profile-report }

**Function** `(egcl-ext:profile-report)` → `nil`

Writes an engine report to `*trace-output*`, including GC activity,
deoptimizations, and native OSR entries. It is an implementation report, not a
portable Common Lisp interface.

Use [compiler counters](compiler.md#counters) for a particular function.
`function-tier` reports the installed entry tier; `function-osr-count` gives
separate evidence of loop entry through OSR.

## Deterministic call counts

### Profiling selected functions { #profile }

**Functions**

```lisp
(egcl-ext:profile function-designator...)
(egcl-ext:unprofile function-designator...)
(egcl-ext:profile-reset)
(egcl-ext:profile-report-calls)
```

`profile` marks recognized functions and establishes count baselines.
`unprofile` removes selected functions; with no arguments it clears the selected
set. `profile-reset` resets count baselines. `profile-report-calls` prints the
selected functions' counts to `*trace-output*`.

For exact counting, this instrumentation pins profiled functions to T0. It
therefore changes the execution regime: do not use its timings as an estimate
of uninstrumented native execution.

## Event recording

### Event stream { #events }

**Functions**

```lisp
(egcl-ext:events-start)     ; enable recording, returns T
(egcl-ext:events-stop)      ; disable recording, returns NIL
(egcl-ext:events-reset)     ; clear recorded events, returns NIL
(egcl-ext:events-count)     ; current recorded event count
(egcl-ext:events-report)    ; chronological text report
(egcl-ext:events-summary)   ; aggregated report
(egcl-ext:events-json)      ; JSON export
```

Reports write to `*trace-output*` and return `nil`. Recording exposes events
such as compilation, OSR, deoptimization, and collection without requiring an
application to infer them from elapsed time alone. Start recording before the
workload whose events you need to observe.

```lisp
(egcl-ext:events-reset)
(egcl-ext:events-start)
(defun measured-sum (n)
  (let ((sum 0))
    (dotimes (i n sum) (incf sum i))))
(measured-sum 1000000)
(egcl-ext:events-stop)
(egcl-ext:events-summary)
```

## Statistical sampling

### Sampling control { #sprof }

**Functions**

```lisp
(egcl-ext:sprof-start &optional (frequency 1000))
(egcl-ext:sprof-stop)
(egcl-ext:sprof-fold)
```

The sampler records Lisp call-chain information across execution tiers.
`sprof-start` takes a sampling frequency in Hz. `sprof-stop` stops sampling and
returns the collected sample count. `sprof-fold` writes folded-stack output to
`*trace-output*` and returns `nil`.

Sampling observes a running workload; short programs may end before meaningful
samples accumulate. Retain the workload and build configuration with any
profile you use to justify a performance change.

## Minor-GC phase timing { #minor-gc-phases }

Set `EGCL_GC_PHASE_LOG` before starting EGCL to write phase timings to stderr.
The flag is cached on first use and tests presence: `=0` also enables it.
Leave it unset for ordinary timing comparisons.

```sh
EGCL_GC_PHASE_LOG=1 scripts/egcl-limited.sh \
  target/x86_64-unknown-linux-musl/release/egcl --no-init --load workload.lisp \
  2>gc-phases.log
env -u EGCL_GC_PHASE_LOG scripts/egcl-limited.sh \
  target/x86_64-unknown-linux-musl/release/egcl --no-init --load workload.lisp
```

Each successful minor body emits one `[gc-phases] minor` line with eight
consecutive intervals in nanoseconds:

| Field | Work included |
| --- | --- |
| `setup_index_ns` | Heap setup and initial nursery indexing |
| `roots_mark_ns` | Stack, symbol, external, and remembered roots |
| `old_scan_ns` | Older-generation scanning, including mandatory off-heap scans |
| `young_mark_ns` | Pinned-region roots and transitive young marking |
| `evacuate_ns` | Capacity checks, copies, and pinned-region promotion |
| `heap_relocate_ns` | Remembered slots and heap-object fields |
| `roots_relocate_ns` | Stack, symbol, and external root relocation |
| `reclaim_rearm_ns` | Weak references, finalizers, verification, reset, soft-dirty rearming, and final bookkeeping |

Lazy nursery indexing is charged to whichever interval forces it.
`phase_sum_ns` equals `body_total_ns`; that body total excludes formatting and
writing its own line. `recorded_pause_us` retains the existing GC-statistics
boundary, which ends before final bookkeeping. Timings include the diagnostic
clock reads and any other enabled GC diagnostics.

The separate `[gc-phases] minor_stw` line reports `enter_ns`,
`body_with_log_ns`, and `exit_ns`, whose sum is `total_ns`. Entry covers the
stop-the-world rendezvous and TLAB retirement; exit covers resumption. The body
interval includes its diagnostic output. These intervals exclude the collector
admission lock acquired by callers before the wrapper; subtracting body time
therefore does not measure that lock's admission cost.

A minor drained inside a major collection emits only the body line. A failed
minor body emits no complete phase line; its wrapper reports `ok=0`, with the
three interval fields absent if the body was never entered. Logging adds
measurement overhead and makes no hard real-time guarantee. On Linux,
soft-dirty tracking can cause first-write page faults during later execution;
that system-time cost falls outside the recorded collection pause.

## Root traversal comparison { #root-visit-sharing }

Bytecode root inventory walks share visitation state for repeated code bodies
and captured environment frames by default. Public and owner-specific walks
start fresh contexts; this changes no global GC epoch or phase boundary.

For a diagnostic comparison, start a fresh process with
`EGCL_DISABLE_ROOT_VISIT_SHARING=1` to restore repeated traversal. This flag is
cached on first use and checks presence, so `=0` also disables sharing. Leave
it unset for the default behavior, and compare the same binary and workload
in separate processes. The flag changes traversal work, not the required roots.

## Allocation and measurement discipline

`room` measures Lisp-heap activity. Rust strings, vectors, hash maps, and other
host allocations can dominate execution time independently. The contributor
[measurement guide](contributing/how-to/performance.md) describes host-allocation
instrumentation, reproducible builds, and the memory-limit wrapper.

Implementation reference: [Profiling entry points](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/egcl/src/cli.rs).
