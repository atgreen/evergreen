# Babel upstream suite

Run the pinned Babel encoding suite in fresh and cached EGCL processes:

```sh
EGCL_TIMEOUT=1800 EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/release/egcl" \
SBCL_BIN="$(command -v sbcl)" bash scripts/test-babel.sh
```

The runner installs immutable ocicl dependencies into a new directory under
`target/`, isolates ASDF caches, and retains every log. It calls Babel's test
runner directly and requires both a successful result and a nonzero assertion
count. Failures remain failures; no upstream cases are suppressed. The optional
SBCL baseline runs even when EGCL fails. All Lisp and installer processes use
`scripts/egcl-limited.sh`.

The pinned upstream suite currently has two errors on SBCL in `RW-EQUIV.1` and
`ENCODER/DECODER-RETVALS` (bliss-tutll). The KSC-5601 roundtrip attempts to turn
NIL into a character. These baseline errors must be distinguished from EGCL
failures when comparing results.

EGCL is not yet passing: the full optimized run currently reaches an internal
symbol-allocation failure (bliss-t4z7n). Cached suite registration (bliss-0jx70),
incorrect decoding multiple values (bliss-p3lxe), and quadratic string traversal
(bliss-tivfl) are fixed. Rebuild existing FASL caches to pick up compiler changes.
Use `EGCL_TIMEOUT` to allow a longer full run; the runner rejects an empty suite
even if its framework reports success.
