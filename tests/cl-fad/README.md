# CL-FAD environment lookup

Run from the repository root with an EGCL binary and an ocicl installation:

```sh
EGCL_BIN=target/x86_64-unknown-linux-musl/release/egcl \
SBCL_BIN=/home/linuxbrew/.linuxbrew/bin/sbcl scripts/test-cl-fad.sh
```

The scenario fetches the immutable fork revision in `ocicl.csv`, loads CL-FAD
with cold and cached FASLs, and checks its temporary-directory selection.
Environment tests cover set, empty, and unset variables on the native path and
on the optional UIOP fallback. A separate process hides UIOP before reading the
GETENV definition and checks that unsupported implementations report an error.
Setting `SBCL_BIN` repeats the environment checks on SBCL.

The fallback tests clear `*FEATURES*` to select the unsupported-implementation
branch. No downloaded dependency sources are edited.

This scenario covers environment lookup and loading, not CL-FAD's full
filesystem API. Directory listing and recursive deletion still need an EGCL
port (bliss-3wh8b).
