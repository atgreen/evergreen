# EGCL support for swank

Use the [atgreen fork and ocicl Git pin](../README.md) — swank lives in the slime
tree, on a `egcl` branch:

```sh
ocicl install git+https://github.com/atgreen/slime@a1d235181efa139eb0f51feffbe74f784842c1b3
```

Based on the `v2.32` tag, which is the revision ocicl shipped.

| File | What it is |
|---|---|
| `swank/egcl.lisp` | The backend: sockets, process, streams, compilation, `gray-package-name` |
| `swank.asd` | Builds it `:if-feature :egcl` |
| `swank-loader.lisp` | Names it in `*sysdep-files*`, lists `:egcl` in `*implementation-features*`, reads its version |

The backend is adapted from this repo's own `lib/slynk/backend/egcl.lisp` —
slynk is a fork of swank, so the interface is the same shape with different
names (`swank-compile-string` for `slynk-compile-string`, and so on). It runs at
communication style NIL. EGCL compiles as it evaluates and tiers up on its own,
so the compilation entry points evaluate and report no compiler notes rather than
inventing them, and `MAKE-FD-STREAM` says it is not implemented instead of
answering a stream that is not connected to the descriptor.

## What is declined rather than faked

EGCL exports the pretty-printer dispatch symbols but defines neither
`SET-PPRINT-DISPATCH` nor `PPRINT-DISPATCH` (`bliss-kp8ix`), so
`*BACKTRACE-PPRINT-DISPATCH-TABLE*` is the plain table and backtrace strings
print with the standard escaping instead of swank's. The fork says so in a
comment at that form.

## The EGCL fix it took

Reading `SAVE-IMAGE` bare handed back CL-USER's symbol whatever `*PACKAGE*` was,
so `swank/backend`'s own `DEFINTERFACE` — which ends with
`(export ',name :swank/backend)` — reported that the symbol was not accessible
there. The four image-control spellings are now package-local, taking the legacy
identity only when read from CL-USER or CL. This repo's bundled `lib/slynk` has
the same `definterface`, so it was hitting the same wall.
