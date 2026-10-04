# trivial-backtrace on EGCL

This scenario fetches the `atgreen/trivial-backtrace` fork at the immutable
revision in `ocicl.csv`, using ocicl Git sources. No downloaded files are patched.
The adapter uses `EGCL-DEBUG:LIST-BACKTRACE` and `EGCL-DEBUG:PRINT-BACKTRACE`.

```sh
EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/release/egcl" \
  scripts/test-trivial-backtrace.sh
```

Requires ocicl with Git-source support and its installed runtime. Override
`OCICL_BIN` and `OCICL_RUNTIME` when needed. Every process runs through
`scripts/egcl-limited.sh`; `EGCL_MEM_MAX` and `EGCL_TIMEOUT` control its limits.
Artifacts and logs remain in the printed temporary directory under `target/`.

The checks assert frame names and order, actual installed tiers, argument identity
and retention where available, string/stream/file output, append behavior, and
capture inside a live condition handler. Cold and cached ASDF loads must pass;
the cached load must not recompile. Pure interpretation, T0, T1, and T2 run the
same checks. Native frames currently expose names but no argument locations, so
the test explicitly checks that their variable lists are empty. Source locations
and additional lexical locals are not provided by this adapter.

Set `SBCL_BIN` to an SBCL executable to run the same contracts against the
unchanged SBCL adapter. `EGCL_PORT_STRESS=1` additionally requests a full stress,
poison, and heap-verifier run; this is expensive, and a timeout is a failed check.
The focused Rust snapshot test separately requires actual argument relocation.

A condition object alone cannot recover a stack that has already unwound. Capture
from a live handler (`HANDLER-BIND`) when the signalling frames matter. Snapshot
arguments are references to the original objects, not deep copies.
