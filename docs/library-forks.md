# EGCL compatibility forks

Maintain Common Lisp library adaptations in GitHub forks under `atgreen`, using
`#+egcl` / `#-egcl` or ASDF `:if-feature :egcl` where implementation selection
is needed. Preserve the other implementations' behavior. Do not advertise a
different Lisp implementation or replace required semantics with silent stubs.

**Do not submit patches, pull requests, or issues upstream yet.** Publishing to
the `atgreen` forks is authorized; upstream submissions require a new instruction.

The forks are the ONLY copy. This repository no longer carries a `ports/`
directory of patch sources or a vendored `lib/slynk/`: each adaptation lives in
its fork, which is what the pinned `ocicl.csv` entries fetch. Edit the fork, push
it, re-pin the SHA here and in the scenario's `ocicl.csv`.

Use ocicl Git sources instead of editing downloaded registry copies. Pin full
commit SHAs and commit `ocicl.csv`, not the fetched source trees. The Git-source
workflow is documented in `~/git/ocicl/README.md` under “Installing Systems from
Git”. The reproducible [library scenario](../tests/library-forks/README.md)
exercises these forks:

| Library | Fork | Tested revision |
|---|---|---|
| trivial-features | https://github.com/atgreen/trivial-features | `651e8ea90db0b143d39b9a414ee382ec62efad2f` |
| trivial-gray-streams | https://github.com/atgreen/trivial-gray-streams | `0554d306864d252985c23923ef872a1b177894a6` |
| usocket (binary TCP client subset) | https://github.com/atgreen/usocket | `9c88be9854200183d7e8e5d3f52fd314743c3ee4` |
| trivial-cltl2 | https://github.com/atgreen/trivial-cltl2 | `cf3253050711277e847a9dc445a45fa7170dc9cb` |
| trivial-garbage | https://github.com/atgreen/trivial-garbage | `3c4f9c86d4d3454dcd4f8d19113b4101c3add032` |
| bordeaux-threads | https://github.com/atgreen/bordeaux-threads | `2f736ed7ef61d1856f2c6a5aefde9a5ee7b3b66f` |
| precise-time | https://github.com/atgreen/precise-time | `863446b80546db43dec2ca2c077103ecbc4969c0` |
| fset | https://github.com/atgreen/fset | `553d6a75f6520ef4435748f16b0f619e71988628` |
| cffi (and cffi-grovel, cffi-toolchain, cffi-libffi, cffi-uffi-compat, uffi) | https://github.com/atgreen/cffi | `ee7e4ea5238efce6ce4be7d6f0f29699884ad791` |
| iolib (and iolib.base, iolib.conf) | https://github.com/atgreen/iolib | `57bc68250f498d48a6d0a3d07ccd2b36b8a561ea` |
| swank (in the slime tree, branch `egcl`) | https://github.com/atgreen/slime | `a1d235181efa139eb0f51feffbe74f784842c1b3` |
| slynk (in the sly tree, branch `egcl`) | https://github.com/atgreen/sly | `e81f332e` — the EGCL backend, moved out of `lib/slynk/` |

The separate [native usocket scenario](../tests/usocket-fork/README.md) checks
incremental loopback I/O through cold and cached loads. It requires the new
EGCL client primitives and is not a claim of HTTPS support.

The [Completions scenario](../tests/completions/README.md) is the end-to-end
client: it drives Dexador and Completions through a real HTTP request/response
cycle against an Ollama-shaped local server that echoes the prompt back, so a
malformed request or a mis-decoded reply fails an assertion. It imports all of
the forks above (bordeaux-threads, precise-time, trivial-cltl2,
trivial-features, trivial-garbage, trivial-gray-streams and usocket) by
immutable commit. The endpoint is plain HTTP, so it is not a claim of HTTPS
support.

The trivial-cltl2 fork adds only `#+egcl #:egcl-cltl2` to its `:use` list.
EGCL provides that package — its counterpart of SB-CLTL2 — exporting the part
of the CLtL2 environment API the implementation can answer truthfully:
`DEFINE-DECLARATION` and `DECLARATION-INFORMATION` (user declarations, the
`DECLARATION` key, and the `OPTIMIZE` policy merged from defaults,
proclamations and lexical declarations). The remaining CLtL2 names are
deliberately left unbound rather than stubbed, so a caller's own `FBOUNDP`
guard — Serapeum's `macro-tools` uses one — sees the truth. This is what lets
Trivia load.

For an existing ocicl project, run there:

```sh
ocicl install git+https://github.com/atgreen/trivial-features@651e8ea90db0b143d39b9a414ee382ec62efad2f
ocicl install git+https://github.com/atgreen/trivial-gray-streams@0554d306864d252985c23923ef872a1b177894a6
ocicl install git+https://github.com/atgreen/trivial-cltl2@cf3253050711277e847a9dc445a45fa7170dc9cb
ocicl install git+https://github.com/atgreen/trivial-garbage@3c4f9c86d4d3454dcd4f8d19113b4101c3add032
ocicl install git+https://github.com/atgreen/usocket@9c88be9854200183d7e8e5d3f52fd314743c3ee4
ocicl install git+https://github.com/atgreen/bordeaux-threads@2f736ed7ef61d1856f2c6a5aefde9a5ee7b3b66f
ocicl install git+https://github.com/atgreen/precise-time@863446b80546db43dec2ca2c077103ecbc4969c0
ocicl install git+https://github.com/atgreen/cffi@ee7e4ea5238efce6ce4be7d6f0f29699884ad791
ocicl install git+https://github.com/atgreen/iolib@57bc68250f498d48a6d0a3d07ccd2b36b8a561ea
ocicl install git+https://github.com/atgreen/slime@a1d235181efa139eb0f51feffbe74f784842c1b3
```

The CFFI fork adds `src/cffi-egcl.lisp` (the CFFI-SYS backend over `EGCL-FFI`)
and `src/cffi-egcl-fsbv.lisp`, which passes and returns structures by value
through `EGCL-FFI:FOREIGN-CALL-BUFFERED` — so **no libffi and no C compiler are
needed for structures by value**, and `cffi-tests` does not depend on
`cffi-libffi` on EGCL. The runtime lays a structure out from a description of
its fields, so that description is checked against the layout CFFI computed
(size, alignment and every field offset) and a structure it cannot describe is
reported rather than called with invented padding, which would change the
structure's ABI classification. CFFI's own suite passes 342 of 344 tests on
x86-64 Linux; the fork's `EGCL.md` names the two failures and the seven EGCL
conformance bugs the port turned up (bliss-nj6id, bliss-cb3c7, bliss-msyk,
bliss-hfn71, bliss-jre1u, bliss-06l4z, bliss-bpjw6). `cffi-grovel`,
`cffi-toolchain`, `cffi-libffi`, `cffi-uffi-compat`, `uffi` and `cffi-examples`
load unchanged, and the examples run.

The iolib fork adds ONE line — `#+egcl :egcl-gray-streams` in
`DEFINE-GRAY-STREAMS-PACKAGE`, plus `egcl` in its `#-(or …)` guard. No stubs
were needed: all 28 Gray-stream symbols iolib imports are already exported from
`EGCL-GRAY-STREAMS`. Everything else iolib needed was EGCL's own, and four
fixes landed here for it — a DEFCONSTANT compiled in a file is now marked as a
constant, `(setf (compiler-macro-function …))` works, the initialization protocol
receives the initargs the caller supplied, and a LOOP `sum`/`count` accumulator
starts at its identity. The last two were silent wrong answers in core CL. All of
iolib loads: syscalls, multiplex, streams, zstreams, sockets, pathnames, over the
CFFI backend above, with groveling and libfixposix.

The swank fork lives on the slime tree's `egcl` branch and adds
`swank/egcl.lisp` — the backend interface over EGCL's sockets, process and Gray
streams at communication style NIL — plus its registration in `swank.asd` and
`swank-loader.lisp`. One capability is declined rather than faked: EGCL exports
the pretty-printer dispatch symbols but defines neither `SET-PPRINT-DISPATCH` nor
`PPRINT-DISPATCH` (`bliss-kp8ix`), so swank's backtrace dispatch table is the
plain one and backtrace strings print with standard escaping, said in a comment
where it happens. EGCL's own bundled `lib/slynk` shares swank's `DEFINTERFACE`,
so the fix that unblocked it — keeping the image-control builtin names
package-local — mattered to both.

The trivial-garbage fork uses EGCL's deferred Lisp finalizers and its native
weak hash tables (`:key`, `:value`, `:key-and-value`, reported through
`EGCL-EXT:HASH-TABLE-WEAKNESS`). Its upstream suite passes 9 of 11 tests on
EGCL with no unexpected failures; weak POINTERS and `:key-or-value` tables are
reported missing rather than substituted with strong references — the latter
needs an ephemeron fixpoint the collector does not have.

Restart EGCL after switching implementations of a protocol library. The fork
source directories differ from registry directories, so ASDF uses distinct cache
paths. Record new pins only after testing the new revision.

The older patch files are retained as historical references. The Bordeaux
Threads port still requires an audit (`bliss-59qu`): its documented no-op
synchronization is not a general-purpose threading implementation. Do not copy
that workaround into a published compatibility fork as though it were one.
