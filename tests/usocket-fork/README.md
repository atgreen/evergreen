# Native TCP client through the pinned usocket fork

This scenario installs `atgreen/usocket` at
`fa0df448d5582c0d637afe3799cf53b28e1586c9` using ocicl's Git source support.
It uses an isolated source registry and FASL cache, with runtime downloads
disabled. No installed libraries or user configuration are modified.

```sh
OCICL_BIN="$HOME/git/ocicl/ocicl" \
OCICL_RUNTIME="$HOME/git/ocicl/runtime/ocicl-runtime.lisp" \
EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/debug/egcl" \
SBCL_BIN="$(command -v sbcl)" \
  bash scripts/test-usocket-fork.sh
```

Use a newly built EGCL providing `%SOCKET-CONNECT`, `%SOCKET-READ-TIMEOUT`,
and the FASL `IN-PACKAGE` fix; an older installed image is insufficient.
The script retains logs in its printed temporary directory and runs:

- Cold and cached loads, with no source recompilation allowed in the latter.
- A loopback byte exchange: the server waits for an acknowledgement of the
  first byte before sending the remainder. Buffering until EOF cannot pass.
- Receive-timeout configuration, octet-stream metadata, EOF, close, and explicit
  rejection of unsupported EGCL connection options.
- Literal octet types and one-hop/two-hop DEFTYPE aliases, each with its own
  full duplex connection; character and non-octet integer widths stay rejected.
- Optional GC stress at stride 1,000 with poisoning and heap verification.
  Its child timeout is 330 seconds to allow stressed ASDF startup.
- Optional SBCL control using the same fixture and a separate FASL cache.

The wrapper applies `EGCL_MEM_MAX` (default 4G) and `EGCL_TIMEOUT` (default
600 seconds) to each phase. All socket traffic is loopback, without credentials.

This proves the binary TCP client subset, not TLS, a complete usocket port,
Dexador, or `completions` streaming. Character sockets, UDP, listeners, local
binding, deadlines, and disabling TCP_NODELAY are unsupported. DNS resolution
precedes the connection timeout.

## Known GC-stress load failure

Adding `EGCL_PORT_STRESS=1` currently reproduces `bliss-7hh0`: a callable-type
error during ASDF setup, before usocket loads. With load tracing enabled it can
instead crash in `ASDF:INITIALIZE-OUTPUT-TRANSLATIONS`. The script reports this
as a failure; it does not suppress it or treat a timeout as success. A separate
progress-instrumented probe passed, but its changed allocation sequence does
not establish GC safety for the unmodified scenario.
