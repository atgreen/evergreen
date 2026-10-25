# Audited ocicl compatibility forks

Run from the repository root:

```sh
OCICL_BIN=/usr/bin/ocicl EGCL_BIN=/path/to/egcl scripts/test-audited-forks.py
```

`forks.json` identifies upstream sources, exact fork revisions, test file paths,
and coverage limits. `ocicl.csv` pins the forks and their dependency inventory.
The runner installs those pins in a retained temporary directory using ocicl's
Git-source support, rejects changed pins, and runs the tests committed in each
fork. Downloaded library sources are never patched. ASDF's inherited source
registry and runtime downloads are disabled; the pinned Named Readtables fork
has explicit precedence. FASLs and separate per-test logs stay in that directory. Each invocation keeps
a separate run-log directory and records the EGCL executable SHA-256. FASL
caches are separated by that digest, so rebuilding EGCL cannot reuse stale
macroexpansions from the prior runtime.

Each check uses a fresh EGCL process through `egcl-limited.sh`, sequentially,
with a default 2 GiB memory cap. Set `EGCL_TIMEOUT`, `EGCL_MEM_MAX`, or
`OCICL_RUNTIME` if needed. Use `--only NAME ...` for a focused subset and
`--work /path/to/retained-directory` to reuse downloaded sources and cached FASLs.
The runner reports success only after each selected test returns normally.

The checks exercise public behavior rather than load-only placeholders. They
are not full upstream suites: JSON fluid-object decoding, LIFT/Parachute timeout
enforcement, complete documentation rendering/navigation, remote HTTP/TLS
transport, and executable self-update are not claimed. The per-library manifest
states narrower limits. Exported ASDF systems beyond the tested base system may
also remain untested. Babel has separate encoding and Gray-stream checks.

Alexandria, Iterate, RT, and Parse Declarations were imported from their original
non-GitHub upstreams with source history preserved. The MGL-PAX fork uses the
audited `f7f9414` base; its newer ocicl README revision introduced a separate
autoload dependency outside that audited source snapshot.

Trivial Macroexpand All requires `EGCL-EXT:MACROEXPAND-ALL` in the selected
runtime ([runtime PR #641](https://github.com/atgreen/evergreen/pull/641)).
Unsupported atomic-update and macro-definition forms signal
`PROGRAM-ERROR`; compiler macro expansion is outside that interface.

A cold CL-Unicode data build on EGCL exceeded the default 600-second limit
while parsing `UnicodeData.txt` (tracked as `bliss-ks0tt`). To validate
CL-INTERPOL with prepared Unicode source data, use:

```sh
OCICL_BIN=/usr/bin/ocicl EGCL_BIN=/path/to/egcl \
  scripts/test-audited-forks.py --only cl-interpol --prepare-unicode
```

This optional mode runs the pinned `cl-unicode/build` generator with SBCL
(`SBCL_BIN` overrides its path), recording its log and hashes of `lists.lisp`,
`hash-tables.lisp`, and `methods.lisp`. SBCL uses a separate FASL cache. EGCL
then compiles those portable generated Lisp sources and runs the original
CL-INTERPOL semantic checks. This result is a check with **prepared Unicode
source data**, not a successful cold dependency build on EGCL. The default
mode retains the cold build behavior.
