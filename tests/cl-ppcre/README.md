# CL-PPCRE upstream suite

Run the pinned core CL-PPCRE suite with fresh and cached EGCL loads:

```sh
EGCL_TIMEOUT=1800 EGCL_BIN="$PWD/target/x86_64-unknown-linux-musl/release/egcl" \
SBCL_BIN="$(command -v sbcl)" bash scripts/test-cl-ppcre.sh
```

The runner installs immutable ocicl dependencies into a private directory under
`target/`, isolates ASDF caches, and calls `cl-ppcre-test:run-all-tests` directly.
This executes the Perl corpus, optimized character-test functions, and simple
API tests. It preserves upstream's own intentional Perl differences; it adds no
exclusions. The optional CL-PPCRE-UNICODE extension is a separate upstream system.
All installer and Lisp processes use the memory-limiting wrapper. Failures do not
prevent the cached phase or optional SBCL baseline from running. Logs and sources
remain in the printed artifact directory.

The pinned upstream suite passes on SBCL. EGCL's lost lexical captures after
lazy compilation are fixed in `dffb211b`; binary stream element-type aliases
and parameterized type expansion are fixed in `cb007f0c`; Gray-stream prefix
reparsing and repeated read-time evaluation are fixed in `3932dd29`.
Compiler-macro and compiler-registered ordinary macro callbacks now reclaim
their dynamic environment handles (`bliss-x3eov`, `bliss-p409f`). The latter
leak still retained 345,640 environments after fixing compiler macros alone.
A complete passing EGCL run is still pending.
