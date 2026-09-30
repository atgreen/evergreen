# EGCL compatibility forks

Maintain Common Lisp library adaptations in GitHub forks under `atgreen`, using
`#+egcl` / `#-egcl` or ASDF `:if-feature :egcl` where implementation selection
is needed. Preserve the other implementations' behavior. Do not advertise a
different Lisp implementation or replace required semantics with silent stubs.

**Do not submit patches, pull requests, or issues upstream yet.** Publishing to
the `atgreen` forks is authorized; upstream submissions require a new instruction.

Use ocicl Git sources instead of editing downloaded registry copies. Pin full
commit SHAs and commit `ocicl.csv`, not the fetched source trees. The Git-source
workflow is documented in `~/git/ocicl/README.md` under “Installing Systems from
Git”. The reproducible [library scenario](../tests/library-forks/README.md)
exercises these forks:

| Library | Fork | Tested revision |
|---|---|---|
| trivial-features | https://github.com/atgreen/trivial-features | `26e8d168deb7d70d762d0d7e41b2919eba4321aa` |
| trivial-gray-streams | https://github.com/atgreen/trivial-gray-streams | `e55ad7e91aa5aa92408fe082b2e307f8986be080` |
| usocket (binary TCP client subset) | https://github.com/atgreen/usocket | `5f8ba3596b4be3b3962a26957ff5508d4d02cbcc` |
| trivial-cltl2 | https://github.com/atgreen/trivial-cltl2 | `116e6d4ccbc5e4dbe67c9eda1441f73a0c5c7691` |
| trivial-garbage | https://github.com/atgreen/trivial-garbage | `0bb7ebd89ec5c8245516c5117f4bc598ebd56e84` |
| bordeaux-threads | https://github.com/atgreen/bordeaux-threads | `0251844d5e9482eb5fc91fa4686a05aacd24d9b4` |
| precise-time | https://github.com/atgreen/precise-time | `deadbdeb95ee98cd743c538e649880181e8416f7` |
| fset | https://github.com/atgreen/fset | `60a28fe91abfddfc8c2a3f6971933a0c2c5b20aa` |
| cffi (and cffi-grovel, cffi-toolchain, cffi-libffi, cffi-uffi-compat, uffi) | https://github.com/atgreen/cffi | `8fc4b2b439525e87795efdfb848a1761869adc2a` |
| iolib (and iolib.base, iolib.conf) | https://github.com/atgreen/iolib | `81ac1fdc376fbcdb491fe758a3bf957ec0a9c175` |
| swank (in the slime tree, branch `egcl`) | https://github.com/atgreen/slime | `a1d235181efa139eb0f51feffbe74f784842c1b3` |

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
ocicl install git+https://github.com/atgreen/trivial-features@26e8d168deb7d70d762d0d7e41b2919eba4321aa
ocicl install git+https://github.com/atgreen/trivial-gray-streams@e55ad7e91aa5aa92408fe082b2e307f8986be080
ocicl install git+https://github.com/atgreen/trivial-cltl2@116e6d4ccbc5e4dbe67c9eda1441f73a0c5c7691
ocicl install git+https://github.com/atgreen/trivial-garbage@0bb7ebd89ec5c8245516c5117f4bc598ebd56e84
ocicl install git+https://github.com/atgreen/usocket@5f8ba3596b4be3b3962a26957ff5508d4d02cbcc
ocicl install git+https://github.com/atgreen/bordeaux-threads@0251844d5e9482eb5fc91fa4686a05aacd24d9b4
ocicl install git+https://github.com/atgreen/precise-time@deadbdeb95ee98cd743c538e649880181e8416f7
ocicl install git+https://github.com/atgreen/cffi@8fc4b2b439525e87795efdfb848a1761869adc2a
ocicl install git+https://github.com/atgreen/iolib@81ac1fdc376fbcdb491fe758a3bf957ec0a9c175
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
