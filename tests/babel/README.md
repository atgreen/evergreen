# Babel upstream suite

Run the pinned Babel encoding suite in fresh and cached EGCL processes:

```sh
EGCL_TIMEOUT=1800 EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/release/egcl" \
SBCL_BIN="$(command -v sbcl)" bash scripts/test-babel.sh
```

The runner installs immutable ocicl dependencies into a new directory under
`target/`, isolates ASDF caches, and retains every log. It calls Babel's test
runner directly and requires both a successful result and a nonzero assertion
count. Unexpected failures fail the run; recognized upstream errors are reported
separately as described below. The optional
SBCL baseline runs even when EGCL fails. All Lisp and installer processes use
`scripts/egcl-limited.sh`.

The pinned upstream suite has a known defect observed on SBCL and EGCL
(bliss-tutll). When a character has no KSC 5601 code, the `:KSC_5601` encoder
calls `HANDLE-ERROR`, a local macro that only `DEFINE-UNIBYTE-ENCODER`
defines, so `RW-EQUIV.1` and `ENCODER/DECODER-RETVALS` error. The encoder's
loop also declares that missing code `OF-TYPE CODE-POINT`, so SBCL signals a
`TYPE-ERROR` for the `NIL` first; EGCL, which does not check that declaration
there, reaches the call and signals `UNDEFINED-FUNCTION`. `check.lisp` accepts an undefined `BABEL-ENCODINGS::HANDLE-ERROR` or a
`TYPE-ERROR` whose datum is `NIL` when either test occurs in its context, and
prints it as `KNOWN-UPSTREAM-FAILURE`. It allows at most two recognized errors.
This recovered predicate does not inspect the expected type or require one
error from each distinct test; a matching unrelated error could be classified
as known. That exception matching remains a baseline limitation.

The original worktree reported fresh and cached suite passes. This recovered
integration baseline still needs combined runtime validation. Surrogate
characters (bliss-5fn6m) address the original EGCL failures: UTF-8B decodes invalid octets to the
characters `#\UDC80`-`#\UDCFF`, and the suite checks that `CODE-CHAR` returns a
character for every code below `CHAR-CODE-LIMIT`. Rebuild existing FASL caches
to pick up compiler changes. Use `EGCL_TIMEOUT` to allow a longer full run; the
runner rejects an empty suite even if its framework reports success.
