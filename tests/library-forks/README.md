# ocicl Git-source library scenario

This scenario imports the `atgreen` trivial-features and trivial-gray-streams
forks by immutable Git commit, alongside registry dependencies pinned by digest.
It never patches downloaded files or uses the developer's ASDF source registry.

From the TorCL repository:

```sh
OCICL_BIN="$HOME/git/ocicl/ocicl" \
OCICL_RUNTIME="$HOME/git/ocicl/runtime/ocicl-runtime.lisp" \
TORCL_BIN="$PWD/target/torcl" \
TORCL_PORT_STRESS=1 SBCL_BIN="$(command -v sbcl)" \
  bash scripts/test-library-forks.sh
```

The defaults are `ocicl` on PATH, its installed runtime under
`${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/`, and `target/torcl`. The ocicl binary
must support `git+` sources. No ocicl checkout changes are needed.

Each run creates and retains a private temporary project with the committed
`ocicl.csv`. `ocicl install` fetches its pins. TorCL then runs twice in fresh
processes with `--no-init` and a private FASL cache:

- Cold: compile/load Babel and Flexi Streams, encode Hello, check platform
  features and native Gray generic identities, and read ASCII through Flexi
  Streams' UTF-8 decoder.
- Cached: repeat the assertions and reject source recompilation.
- Optional `TORCL_PORT_STRESS=1`: repeat with GC stress stride 20,000, poisoning
  and heap verification.
- Optional `SBCL_BIN`: run the same checks using SBCL and a separate cache.

Commands are memory/time capped by `scripts/torcl-limited.sh`; configure
`TORCL_MEM_MAX` and `TORCL_TIMEOUT` as needed. Logs and downloaded sources stay in
the printed artifact directory, including on failure. Runtime auto-downloads
are disabled, so missing manifest dependencies fail instead of silently using a
different version. This is a compatibility scenario, not a full CL-Unicode build
or a claim that either library's complete upstream suite passes on TorCL.

## Known multibyte regression

`check-utf8.lisp` additionally reads valid two-byte and three-byte UTF-8
characters. It currently fails on TorCL with an “overlong” sequence error
(`bliss-bsjw`), independently of the Gray symbol import fix. Keep this regression
visible; the passing ASCII scenario does not establish complete UTF-8 support.
To reproduce it, enter a retained artifact directory and run:

```sh
TORCL_PORT_RUNTIME="$HOME/git/ocicl/runtime/ocicl-runtime.lisp" \
TORCL_PORT_CACHE="$PWD/cache/" \
  /path/to/torcl/scripts/torcl-limited.sh /path/to/torcl/target/torcl \
    --no-init --load check-utf8.lisp
```

Refresh pins deliberately with `ocicl install git+URL@SHA` in an isolated
project, rerun this scenario, and update the committed CSV and port documentation.
Do not submit upstream patches or PRs without new authorization.
