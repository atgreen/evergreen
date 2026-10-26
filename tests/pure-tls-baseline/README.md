# Pinned pure-tls baseline

This harness observes the unmodified `pure-tls/test:run-tests` entrypoint at
atgreen/pure-tls commit `1fce0aab061bbb82777895f5fd8dab3272337265`.
The committed `ocicl.csv` freezes the complete original dependency collection,
including its unrelated systems, for reproducibility. Do not edit downloaded
sources. Compatibility changes belong in the pinned atgreen fork.
Tracking: `bliss-gxv10`.

This is a **failure baseline**, not a claim that pure-tls passes on EGCL.
The initial full run exposed handshake, cancellation, and OpenSSL-related
failures; retain the actual final logs and summaries when comparing revisions.
The measured result is recorded below. A failed suite or load
must remain a failed process exit; a timeout or memory-cap termination is not a
passing test. Do not compare failed-run timings as passing performance results.

## Prepare and run

Use an ocicl version supporting pinned `git+URL@SHA` dependencies. From this
directory, fetch the committed collection before starting the measurement:

```sh
ocicl install
mkdir -p runtime
cp "${OCICL_RUNTIME:-${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/ocicl-runtime.lisp}" runtime/
```

If your runtime needs its bundled ASDF upgrade, copy the adjacent `asdf.lisp`
into `runtime/` too. The runner records the runtime file's hash.
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
BoringSSL **pattern** tests are included. The separate ACME suites, external
BoringSSL tooling/corpus, and the network-dependent entrypoint are not run.
The main runner itself includes one GitHub connection probe, local sockets,
threads, and OpenSSL subprocesses. Its wall time includes fixed sleeps and
network effects. Individual skips remain visible in the FiveAM summaries.

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


## Recorded result: 2026-10-26

The single provisional full run completed all 13 suites with **499 checks:
482 passed, 17 failed, 0 FiveAM skips**. It exited 1. This is a failed-run
reference, not a passing performance result or a median. Filtered OpenSSL
configuration scenarios are not included in the FiveAM skip count.

Setup took 90.710 s; tests took 2713.980 s; process wall time was 46:52.49
with peak RSS 1,330,340 KiB. Setup used a mixed cache populated by earlier
failed load probes and forced closer-mop recompilation. Tests used natural JIT
tiering and include compilation, sleeps, sockets, network and subprocess costs.

| Suite | Seconds | Checks | Pass | Fail |
| --- | ---: | ---: | ---: | ---: |
| CRYPTO-TESTS | 17.330 | 50 | 50 | 0 |
| ML-DSA-TESTS | 265.002 | 69 | 69 | 0 |
| RECORD-TESTS | 60.766 | 48 | 48 | 0 |
| HANDSHAKE-TESTS | 37.301 | 90 | 88 | 2 |
| CERTIFICATE-TESTS | 5.082 | 58 | 58 | 0 |
| CANCEL-TESTS | 0.196 | 6 | 4 | 2 |
| CANCEL-INTEGRATION-TESTS | 10.513 | 5 | 5 | 0 |
| OPENSSL-TESTS | 2187.608 | 37 | 28 | 9 |
| BORINGSSL-TESTS | 0.441 | 23 | 23 | 0 |
| X509TEST-TESTS | 0.732 | 9 | 9 | 0 |
| SECURITY-REGRESSION-TESTS | 128.239 | 93 | 91 | 2 |
| TRUST-STORE-TESTS | 0.344 | 9 | 9 | 0 |
| RESUMPTION-INTEROP-TESTS | 0.369 | 2 | 0 | 2 |

[Machine-readable results](baseline-2026-10-26.json) preserve revisions, hashes,
failure names and reasons, OpenSSL adapter summaries, and the external raw log's
location and SHA256. [Suite timings](baseline-timings.csv) contain the same
13 measured rows. The raw log is a session-external artifact, not bundled here.
The measured runtime is `3cb5e8b236f3b9dfd64f85c84d92f4aa46b4b166`;
the pure-tls revision is the pinned `1fce0aab061bbb82777895f5fd8dab3272337265`.
The original measurement harness hash is preserved separately from hashes of
the portable committed harness: parameterizing its paths did not make the files
byte-identical, although both observe the original runner entrypoint.

Failure follow-ups are `bliss-v38sp` (fractional timeout), `bliss-2k83w`
(handshake/OpenSSL assertions), `bliss-30p86` (Gray input-stream recognition),
and `bliss-moehk` (UIOP subprocess launch). The cached closer-mop load workaround
is tracked by `bliss-uq2oi`. Separate SBCL case analysis remains in progress under
`bliss-aug7d`; this full-run record contains no SBCL comparison.
