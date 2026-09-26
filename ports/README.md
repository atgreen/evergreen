# TorCL compatibility forks

Maintain Common Lisp library adaptations in GitHub forks under `atgreen`, using
`#+torcl` / `#-torcl` or ASDF `:if-feature :torcl` where implementation selection
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

The separate [native usocket scenario](../tests/usocket-fork/README.md) checks
incremental loopback I/O through cold and cached loads. It requires the new
TorCL client primitives and is not a claim of HTTPS support.

The [Completions scenario](../tests/completions/README.md) is the end-to-end
client: it drives Dexador and Completions through a real HTTP request/response
cycle against an Ollama-shaped local server that echoes the prompt back, so a
malformed request or a mis-decoded reply fails an assertion. It imports all of
the forks above (bordeaux-threads, precise-time, trivial-cltl2,
trivial-features, trivial-garbage, trivial-gray-streams and usocket) by
immutable commit. The endpoint is plain HTTP, so it is not a claim of HTTPS
support.

The trivial-cltl2 fork adds only `#+torcl #:torcl-cltl2` to its `:use` list.
TorCL provides that package — its counterpart of SB-CLTL2 — exporting the part
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
```

The trivial-garbage fork uses TorCL's deferred Lisp finalizers and its native
weak hash tables (`:key`, `:value`, `:key-and-value`, reported through
`TORCL-EXT:HASH-TABLE-WEAKNESS`). Its upstream suite passes 9 of 11 tests on
TorCL with no unexpected failures; weak POINTERS and `:key-or-value` tables are
reported missing rather than substituted with strong references — the latter
needs an ephemeron fixpoint the collector does not have.

Restart TorCL after switching implementations of a protocol library. The fork
source directories differ from registry directories, so ASDF uses distinct cache
paths. Record new pins only after testing the new revision.

The older patch files are retained as historical references. The Bordeaux
Threads port still requires an audit (`bliss-59qu`): its documented no-op
synchronization is not a general-purpose threading implementation. Do not copy
that workaround into a published compatibility fork as though it were one.
