# Pinned pure-tls baseline

This harness observes the unmodified `pure-tls/test:run-tests` entrypoint at
atgreen/pure-tls commit `1fce0aab061bbb82777895f5fd8dab3272337265`.
The committed `ocicl.csv` freezes the complete original dependency collection,
including its unrelated systems, for reproducibility. Do not edit downloaded
sources. Compatibility changes belong in the pinned atgreen fork.
Tracking: `bliss-gxv10`.

This is a **failure baseline**, not a claim that pure-tls passes on EGCL.
The initial full run exposed failures in four main suites and OpenSSL-related
coverage; retain the actual final logs and summaries when comparing revisions.
No final counts or timings are asserted by this harness. A failed suite or load
must remain a failed process exit; a timeout or memory-cap termination is not a
passing test. Do not compare failed-run timings as passing performance results.

## Prepare and run

Use an ocicl version supporting pinned `git+URL@SHA` dependencies. From this
directory, fetch the committed collection before starting the measurement:

```sh
ocicl install
```

The installation must provide `ocicl/` and `runtime/ocicl-runtime.lisp` alongside
the unchanged lock. Installation can use the network; the test process disables
ocicl downloads and enables local-only resolution. No library source override is
loaded. An alternative installation directory is accepted by `--work-dir`, but
its lock must byte-match the committed file.

Build EGCL in release mode using the repository's normal build instructions,
then run from the repository root:

```sh
EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/release/egcl" \
  python3 tests/pure-tls-baseline/run-baseline.py baseline-1 \
  --runtime-revision "$(git rev-parse HEAD)"
```

Supply the **actual binary's source revision**, including an explicit dirty
patch identifier if applicable, rather than the harness revision when they
differ. The original baseline used runtime `3cb5e8b2`; this harness can be
committed on a newer documentation/test-only revision without rebuilding that
runtime. `EGCL_BIN` (or `--egcl`) must name the intended release artifact.
The JSON records its SHA256, the declared runtime revision, harness repository
revision, runner/Lisp/manifest hashes, command, environment, limits, load averages,
and wall time. Preserve build-command/source-patch provenance with these files.

The runner uses `scripts/egcl-limited.sh`, defaulting to 8 GiB memory, zero swap,
and 3600 seconds. Override via `--memory`/`--timeout` or `EGCL_MEM_MAX`/
`EGCL_TIMEOUT`. Logs, GNU time resource data, and JSON markers go to
`logs/LABEL.{log,time,json}`; labels cannot overwrite existing results. Cache and
temporary files are local to the work directory. Loading and tests have separate
timers; each FiveAM suite records its own elapsed time after the original runner
returns. Inspect the full log for individual failure diagnostics and skipped
checks; timing markers alone are insufficient.

## Scope and load workaround

The main entrypoint runs 13 suites: crypto, ML-DSA, record, handshake, certificate,
cancellation, cancellation integration, OpenSSL adaptations, BoringSSL patterns,
X509test validation, security regression, trust store, and resumption interop.
BoringSSL **pattern** tests are included. External BoringSSL tooling/corpus,
external ACME integration, and the separate network-dependent entrypoint are not
run. Individual main-suite skips remain visible in the FiveAM summaries.

The default load phase forces `closer-mop` recompilation before loading
`pure-tls/test`. A cold compile/load worked, but fresh cached loading failed with
`DEFPACKAGE: there is no package named CLOSER-COMMON-LISP to import from`.
This setup workaround is reported explicitly and counted in load time, outside
test time. It does not alter suite assertions or substitute a different test
entrypoint. Related historical top-level MACROLET work: `bliss-21uzs` (#527).

For a separately labelled optional cached-load investigation, pass
`--closer-package-source PATH` naming the pinned installation's
`closer-mop-packages.lisp`. This preloads that one source file instead of forcing
recompilation. It is not the default baseline and must not be silently combined
with default load-time comparisons.
