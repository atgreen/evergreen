# Native TCP client and server through the pinned usocket fork

This scenario installs `atgreen/usocket` at
`39c189d2c51317fbdfa6d6023a3369fdb329e61e` using ocicl's Git source support.
It uses an isolated source registry and FASL cache, with runtime downloads
disabled. No installed libraries or user configuration are modified.

```sh
OCICL_BIN="$(command -v ocicl)" \
OCICL_RUNTIME="$HOME/git/ocicl/runtime/ocicl-runtime.lisp" \
EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/debug/egcl" \
SBCL_BIN="$(command -v sbcl)" \
  bash scripts/test-usocket-fork.sh
```

Use a newly built EGCL providing `%SOCKET-CONNECT`, `%SOCKET-READ-TIMEOUT`,
the FASL `IN-PACKAGE` fix, and shared native listeners (PR #60), including the
four-argument `%SOCKET-LISTEN`, and `EGCL-EXT:IO-TIMEOUT` (PR #72);
an older installed image is insufficient.
The script retains logs in its printed temporary directory and runs:

- Cold and cached loads, with no source recompilation allowed in the latter.
- A loopback byte exchange: the server waits for an acknowledgement of the
  first byte before sending the remainder. Buffering until EOF cannot pass.
- Receive-timeout configuration, octet-stream metadata, EOF, close, and explicit
  rejection of unsupported EGCL connection options.
- Inherited `:timeout`, independent connect/read overrides, explicit `NIL`,
  actual read deadline expiry, mapped `USOCKET:TIMEOUT-ERROR` socket identity,
  and successful reads after a timeout.
- Literal octet types and one-hop/two-hop DEFTYPE aliases, each with its own
  full duplex connection; character and non-octet integer widths stay rejected.
- EGCL server listen/accept/close and local-port queries, including cross-thread
  ownership, inherited and overridden octet aliases, and wildcard/auto-port defaults.
- Linux address-reuse checks using active close and TIME_WAIT, including the
  deprecated `:reuseaddress` spelling and precedence of `:reuse-address`.
- Optional GC stress at stride 1,000 with poisoning and heap verification.
  Its child timeout is 330 seconds to allow stressed ASDF startup.
- Optional SBCL client control using the same fixture and a separate FASL cache.
  The server and read-timeout regressions are EGCL-specific and are not counted
  as SBCL coverage.

The wrapper applies `EGCL_MEM_MAX` (default 4G) and `EGCL_TIMEOUT` (default
600 seconds) to each phase. All socket traffic is loopback, without credentials.

This checks a binary TCP client/server subset, not TLS, a complete usocket port,
Dexador, or `completions` streaming. Character sockets, UDP, Unix-domain sockets,
USOCKET readiness/address queries, connected-socket port queries, explicit client
local binding, deadlines, and disabling TCP_NODELAY remain unsupported. DNS
resolution precedes the connection timeout.

## Known GC-stress load failure

Adding `EGCL_PORT_STRESS=1` currently reproduces `bliss-7hh0`: a callable-type
error during ASDF setup, before usocket loads. With load tracing enabled it can
instead crash in `ASDF:INITIALIZE-OUTPUT-TRANSLATIONS`. The script reports this
as a failure; it does not suppress it or treat a timeout as success. A separate
progress-instrumented probe passed, but its changed allocation sequence does
not establish GC safety for the unmodified scenario.
