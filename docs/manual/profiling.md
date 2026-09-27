# Profiling and efficiency

A tiered implementation has several costs: startup, source loading, bytecode
compilation, native compilation, and steady-state execution. Decide which one
you are measuring before interpreting a timing.

Use release binaries for comparisons. A loop near an OSR threshold measures
warmup as well as execution. For steady-state loop measurements, use at least
one million iterations and inspect several input sizes.

## Engine observations

### `torcl-ext:profile-report` { #profile-report }

**Function** `(torcl-ext:profile-report)` → `nil`

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
(torcl-ext:profile function-designator...)
(torcl-ext:unprofile function-designator...)
(torcl-ext:profile-reset)
(torcl-ext:profile-report-calls)
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
(torcl-ext:events-start)     ; enable recording, returns T
(torcl-ext:events-stop)      ; disable recording, returns NIL
(torcl-ext:events-reset)     ; clear recorded events, returns NIL
(torcl-ext:events-count)     ; current recorded event count
(torcl-ext:events-report)    ; chronological text report
(torcl-ext:events-summary)   ; aggregated report
(torcl-ext:events-json)      ; JSON export
```

Reports write to `*trace-output*` and return `nil`. Recording exposes events
such as compilation, OSR, deoptimization, and collection without requiring an
application to infer them from elapsed time alone. Start recording before the
workload whose events you need to observe.

```lisp
(torcl-ext:events-reset)
(torcl-ext:events-start)
(defun measured-sum (n)
  (let ((sum 0))
    (dotimes (i n sum) (incf sum i))))
(measured-sum 1000000)
(torcl-ext:events-stop)
(torcl-ext:events-summary)
```

## Statistical sampling

### Sampling control { #sprof }

**Functions**

```lisp
(torcl-ext:sprof-start &optional (frequency 1000))
(torcl-ext:sprof-stop)
(torcl-ext:sprof-fold)
```

The sampler records Lisp call-chain information across execution tiers.
`sprof-start` takes a sampling frequency in Hz. `sprof-stop` stops sampling and
returns the collected sample count. `sprof-fold` writes folded-stack output to
`*trace-output*` and returns `nil`.

Sampling observes a running workload; short programs may end before meaningful
samples accumulate. Retain the workload and build configuration with any
profile you use to justify a performance change.

## Allocation and measurement discipline

`room` measures Lisp-heap activity. Rust strings, vectors, hash maps, and other
host allocations can dominate execution time independently. The contributor
[measurement guide](contributing/how-to/performance.md) describes host-allocation
instrumentation, reproducible builds, and the memory-limit wrapper.

Implementation reference: [Profiling entry points](https://cave.moxielogic.com/atgreen/bliss/src/branch/main/crates/torcl/src/cli.rs).
