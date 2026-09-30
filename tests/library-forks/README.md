# ocicl Git-source library scenario

This scenario imports the `atgreen` trivial-features and trivial-gray-streams
forks by immutable Git commit, alongside registry dependencies pinned by digest.
It never patches downloaded files or uses the developer's ASDF source registry.

From the EGCL repository:

```sh
OCICL_BIN="$HOME/git/ocicl/ocicl" \
OCICL_RUNTIME="$HOME/git/ocicl/runtime/ocicl-runtime.lisp" \
EGCL_BIN="$PWD/target/egcl" \
EGCL_PORT_STRESS=1 SBCL_BIN="$(command -v sbcl)" \
  bash scripts/test-library-forks.sh
```

The defaults are `ocicl` on PATH, its installed runtime under
`${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/`, and `target/egcl`. The ocicl binary
must support `git+` sources. No ocicl checkout changes are needed.

Each run creates and retains a private temporary project with the committed
`ocicl.csv`. `ocicl install` fetches its pins. EGCL then runs twice in fresh
processes with `--no-init` and a private FASL cache:

- Cold: compile/load Babel and Flexi Streams, encode Hello, check platform
  features and native Gray generic identities, and read ASCII through Flexi
  Streams' UTF-8 decoder.
- Cached: repeat the assertions and reject source recompilation.
- Optional `EGCL_PORT_STRESS=1`: repeat with GC stress stride 20,000, poisoning
  and heap verification.
- Optional `SBCL_BIN`: run the same checks using SBCL and a separate cache.

Commands are memory/time capped by `scripts/egcl-limited.sh`; configure
`EGCL_MEM_MAX` and `EGCL_TIMEOUT` as needed. Logs and downloaded sources stay in
the printed artifact directory, including on failure. Runtime auto-downloads
are disabled, so missing manifest dependencies fail instead of silently using a
different version. This is a compatibility scenario, not a full CL-Unicode build
or a claim that either library's complete upstream suite passes on EGCL.

## Multibyte UTF-8

`check-utf8.lisp` is a separate phase of the run: it loads the same scenario and
then decodes two-byte and three-byte characters through **both** decoders — a
Babel `string-to-octets`/`octets-to-string` round trip and a Flexi Streams read
off a binary stream. It is its own phase because the phases above are
deliberately ASCII-only, so they would keep passing while every non-ASCII
character was rejected — which is exactly what happened: both decoders were
broken for months behind a green ASCII scenario (`bliss-rnd7`, `bliss-bsjw`).
Their shared cause was in EGCL's LOOP, not in either library: an arithmetic
`for` variable was stepped from a private counter, so babel's decoder — `for i
fixnum from start below end` with an `(incf i)` for each continuation byte it
consumes — revisited every continuation byte and rejected it as a starter byte.

SBCL passes this file unchanged, and it runs under GC stress with
`EGCL_PORT_STRESS=1`. To run it by hand in a retained artifact directory:

```sh
EGCL_PORT_RUNTIME="$HOME/git/ocicl/runtime/ocicl-runtime.lisp" \
EGCL_PORT_CACHE="$PWD/cache/" \
  /path/to/egcl/scripts/egcl-limited.sh /path/to/egcl/target/egcl \
    --no-init --load check-utf8.lisp
```

This still is not a claim of complete UTF-8 or CL-Unicode support; it pins the
shapes listed above.

Refresh pins deliberately with `ocicl install git+URL@SHA` in an isolated
project, rerun this scenario, and update the committed CSV and port documentation.
Do not submit upstream patches or PRs without new authorization.
