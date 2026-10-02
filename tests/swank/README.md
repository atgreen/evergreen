# Swank EGCL backend checks

```sh
EGCL_BIN=/usr/bin/egcl SBCL_BIN="$(command -v sbcl)" scripts/test-swank.sh
```

The runner installs the pinned `atgreen/slime` EGCL fork in an isolated
directory, then tests fresh and cached ASDF loads. It checks implementation
selection, the Gray-stream package, redirected character output, loopback
socket creation and cleanup, and actual file compilation with and without
loading the result. `SBCL_BIN` optionally runs the same portable checks on SBCL.
Every process runs under `scripts/egcl-limited.sh`; logs remain in the printed
artifact directory.

The backend uses communication style NIL. These checks do not establish a
complete Emacs debugging session. Compiler source-location mapping and Swank
compiler-note conversion are not implemented, and custom backtrace pretty
printing awaits EGCL's pretty-printer dispatch support (`bliss-kp8ix`).

The original TorCL backend has been renamed to EGCL. Update an existing project
with `ocicl install git+https://github.com/atgreen/slime@egcl`, commit the resulting
`ocicl.csv`, and restart Lisp before loading Swank.
