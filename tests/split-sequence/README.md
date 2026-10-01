# Split-sequence upstream suite

Run the complete split-sequence 2.0.1 FiveAM suite using immutable ocicl pins:

```sh
EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/release/egcl" \
SBCL_BIN="$(command -v sbcl)" bash scripts/test-split-sequence.sh
```

The upstream suite includes one million randomized list/vector comparisons.
The runner preserves that count and allows four hours by default; override
`EGCL_TIMEOUT` if needed. Use a current optimized EGCL build. `SBCL_BIN` adds
an independent comparison (including SBCL's extra sequence-extension tests).

Dependencies, isolated ASDF caches, and logs remain in the printed directory
under `target/`. A failing FiveAM result makes the process fail. All Lisp and
installer processes run through `scripts/egcl-limited.sh`.

The full suite currently exposes EGCL heap-region exhaustion during fuzzing
(bliss-bvmnd). SBCL completes all 141 checks; EGCL's full run is not yet passing.
