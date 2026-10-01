# Clingon upstream suite

Run Clingon 0.5.0's complete Rove suite with immutable ocicl pins:

```sh
EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/debug/egcl" \
SBCL_BIN="$(command -v sbcl)" bash scripts/test-clingon.sh
```

The runner installs into a private project under `target/`, isolates ASDF's
source registry and FASL cache, and checks the suite's boolean result. It runs
EGCL twice (cold compilation and cached FASL loading); `SBCL_BIN` adds a
comparison run. Logs and dependency sources remain in the printed artifact
directory. All commands use `scripts/egcl-limited.sh`.

Clingon itself is unmodified. Bordeaux Threads, trivial-features and
trivial-gray-streams use the pinned EGCL compatibility forks. The upstream
suite does not test real SIGINT delivery; `with-user-abort`'s EGCL interrupt
mapping is tracked separately in bliss-oeloq.
