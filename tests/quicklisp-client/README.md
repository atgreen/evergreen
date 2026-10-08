# Quicklisp client scenario

The committed `ocicl.csv` fetches `atgreen/quicklisp-client` at
`2115f1d96963bf48d6a4c5c7111189bd326bfbed`. No downloaded sources are patched.
The client fork contains the conditional native EGCL networking and directory
adapters; all other implementation branches are preserved.

From the repository root, with ocicl's Git-source support, Python 3, and a
post-PR-90 EGCL build on x86-64 Linux:

```sh
EGCL_BIN=/absolute/path/to/egcl scripts/test-quicklisp-client.sh
```

The scenario uses `scripts/egcl-limited.sh` and keeps its temporary artifacts.
It runs the fixtures from the pinned client:

- Binary TCP octets, hostname resolution, EOF counts, refused connections, and
  connection closure when a callback fails.
- Exact directory enumeration, including hidden and extensionless files,
  subdirectories, empty directories, and no recursive leakage.
- Fresh distribution installation from a local HTTP server, index and archive
  downloads, compilation and loading of two dependent systems, and result 43.
- A new process loading that installation after the server stops, proving the
  cached dependency chain does not need network access.

Setup uses the client's normal ASDF discovery; no extra ASDF preload, missing
variable bindings, or local-project bypasses are supplied.

Set `SBCL_BIN=/absolute/path/to/sbcl` to run the same three fixtures as an
implementation control. Set `QUICKLISP_PUBLIC_SMOKE=1` to additionally install
the official `2026-01-01` distribution and download/quickload Alexandria:

```sh
EGCL_BIN=/absolute/path/to/egcl \
SBCL_BIN=/absolute/path/to/sbcl \
QUICKLISP_PUBLIC_SMOKE=1 scripts/test-quicklisp-client.sh
```

The public test requires external HTTP access. It clears inherited ASDF search
paths, verifies the downloaded archive and source pathname under the fresh
Quicklisp home, runs an `ALEXANDRIA:IOTA` assertion, then repeats in a new
process. Unlike the local fixture's offline phase, the public cached phase
does not disable networking; do not interpret it as an offline-network test.

No existing user installation, initialization file, or source registry is
modified. Tests download releases but do not claim HTTPS support or universal
Common Lisp library compatibility. User setup instructions are in
[the ASDF guide](../../docs/manual/user/how-to/asdf.md#quicklisp).
