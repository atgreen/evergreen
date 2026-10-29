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
| trivial-gray-streams | https://github.com/atgreen/trivial-gray-streams | `e3c675548a9d935fc40860349ed984687c75abe0` |
| usocket (binary TCP client and server subset) | https://github.com/atgreen/usocket | `1318d92e817a8912e86233444af63038f37bf621` |
| trivial-cltl2 | https://github.com/atgreen/trivial-cltl2 | `cf3253050711277e847a9dc445a45fa7170dc9cb` |
| trivial-garbage | https://github.com/atgreen/trivial-garbage | `3c4f9c86d4d3454dcd4f8d19113b4101c3add032` |
| trivial-backtrace | https://github.com/atgreen/trivial-backtrace | `650da10abfe4b44c70b58a431aec6d9fe7330406` |
| bordeaux-threads | https://github.com/atgreen/bordeaux-threads | `2f736ed7ef61d1856f2c6a5aefde9a5ee7b3b66f` |
| atomics (CAS operations) | https://github.com/atgreen/atomics | `738ddccd7a6f3c44e83335ac5e3615e76972dfd2` |
| cl-fad (environment lookup) | https://github.com/atgreen/cl-fad | `6e415a049a2936e614f52108274d7cc016e55694` |
| precise-time | https://github.com/atgreen/precise-time | `045df7c4cddc84d775610cbecf47b462aaea29d6` |
| closer-mop (class, slot, generic-function and method introspection; callable instances) | https://github.com/atgreen/closer-mop | `72d078b5a59070c725232777b52164e111462846` |
| ironclad (branch `egcl`: native AES, GHASH and SHA-256 block functions; needs EGCL with PR #410) | https://github.com/atgreen/ironclad | `57db5ee19708a72b9b7a405e5b620bb39f7439b2` |
| pure-tls (branch `egcl`: environment lookup, cached trust stores, native text conversion) | https://github.com/atgreen/pure-tls | `480bbab6635d65cb74aa6c66f19f0d249beeb537` |
| chipz (branch `egcl`: decompressing streams on EGCL's Gray streams) | https://github.com/atgreen/chipz | `a48e6e741a353a14ab4430eb1519e2f3d5b94aef` |
| fset | https://github.com/atgreen/fset | `553d6a75f6520ef4435748f16b0f619e71988628` |
| cffi (and cffi-grovel, cffi-toolchain, cffi-libffi, cffi-uffi-compat, uffi) | https://github.com/atgreen/cffi | `ee7e4ea5238efce6ce4be7d6f0f29699884ad791` |
| iolib (and iolib.base, iolib.conf) | https://github.com/atgreen/iolib | `57bc68250f498d48a6d0a3d07ccd2b36b8a561ea` |
| swank (in the slime tree, branch `egcl`) | https://github.com/atgreen/slime | `69afcb088f6463850c532a2f573d9ec5d875fdb9` |
| slynk (in the sly tree, branch `egcl`) | https://github.com/atgreen/sly | `e81f332eb458e33cd650f8601f655da58be18b98` — the EGCL backend, moved out of `lib/slynk/` |
| Quicklisp client (native TCP and directory enumeration) | https://github.com/atgreen/quicklisp-client | `2115f1d96963bf48d6a4c5c7111189bd326bfbed` |
| archive (branch `egcl`: pathname metadata and stat accessors) | https://github.com/atgreen/archive | `3e500919b0226c10a848aefe8e57a8cff02b2a6b` |
| zip (branch `egcl`: native file permissions) | https://github.com/atgreen/zip | `6f3efd09130475bd43de6efc1f83da8dae0ac7b9` |
| unix-opts (branch `egcl`: argv and exit status) | https://github.com/atgreen/unix-opts | `07e240c4b1ac8d3ade055f770713c995f39b7c4d` |
| with-user-abort (branch `egcl`: interrupt condition) | https://github.com/atgreen/with-user-abort | `9c0b83eb51cd8553d8b731eef5d6163eb82d0fc8` |
| local-time (branch `egcl`: subsecond wall clock) | https://github.com/atgreen/local-time | `0ac828f4c978696298e41d1f86c8af923a240700` |
| parachute (branch `egcl`: process exit status) | https://github.com/atgreen/parachute | `99ee47171ec686f7b1d80b1afcd588bb3ed300d5` |
| alexandria (branch `egcl`) | https://github.com/atgreen/alexandria | `ec09d2346f2314fe799cd76002e465b88623417d` |
| babel (branch `egcl`) | https://github.com/atgreen/babel | `75fc2911f83711278d1717e5786bbc7cc4a5da7c` |
| cl-interpol (branch `egcl`) | https://github.com/atgreen/cl-interpol | `3326c4758ec7f07ed32667a0e82bf261d7b7130e` |
| named-readtables (branch `egcl`) | https://github.com/atgreen/named-readtables | `587960ee3b46e291723aba4bfa8ef74ebd86c151` |
| cl-utilities (branch `egcl`) | https://github.com/atgreen/cl-utilities | `ef354659a27dc903f638397526d47eab951a019e` |
| esrap (branch `egcl`) | https://github.com/atgreen/esrap | `b77d22cbcf49eb8d8b50c64b304d906df315fd7b` |
| global-vars (branch `egcl`) | https://github.com/atgreen/global-vars | `cd58cd24bca9615cacdbd54201e3213020b84e86` |
| iterate (branch `egcl`) | https://github.com/atgreen/iterate | `73bcbed445417d8b746b0b41b945edd73f48d415` |
| split-sequence (branch `egcl`) | https://github.com/atgreen/split-sequence | `289ad9090c45d1eedc0f85e812a615ae8dca8e16` |
| string-case (branch `egcl`) | https://github.com/atgreen/string-case | `5f26c530c1bf3d59896516610161d3112929b92e` |
| cl-json (branch `egcl`) | https://github.com/atgreen/cl-json | `ec31cbb368ada69444d6fce7d2ef64f97ceb43c8` |
| cl-selfupdate (branch `egcl`) | https://github.com/atgreen/cl-selfupdate | `28da63816581df8825fb71c66ce6c555a45a9d7d` |
| drakma (branch `egcl`) | https://github.com/atgreen/drakma | `d94490cfc29a9822e77fe7baded9f17cfa5c13d3` |
| puri (branch `egcl`) | https://github.com/atgreen/puri | `d5c83b66a09cda3dfb79f1f0009017920b87b519` |
| quri (branch `egcl`) | https://github.com/atgreen/quri | `93872698b99965b28b63b707e83f99d5cfee55cc` |
| rewrite-cl (branch `egcl`) | https://github.com/atgreen/rewrite-cl | `110e6fe4904d3610659ea0b163ee0a68ebdc8416` |
| trivial-custom-debugger (branch `egcl`) | https://github.com/atgreen/trivial-custom-debugger | `8a35b1980298c3fdf88628129217d945e3c5abd8` |
| trivial-file-size (branch `egcl`) | https://github.com/atgreen/trivial-file-size | `a897f75c4ded70249e1a694f38ca45a68d8de492` |
| rt (branch `egcl`) | https://github.com/atgreen/rt | `b7bf6c2025541eb7ba3b68ab399d972365fc65c2` |
| parse-declarations (branch `egcl`) | https://github.com/atgreen/parse-declarations | `c07a536a11de5321c1cf50c5aba883cbf4950b6a` |
| lift (branch `egcl`) | https://github.com/atgreen/lift | `3a940176d0925aaabfd7e443e687a50c4cdb0b77` |
| doc (branch `egcl`) | https://github.com/atgreen/doc | `c4d6d18e9c7e63a6f4eefbe93b156dea0511617a` |
| mgl-pax (branch `egcl`) | https://github.com/atgreen/mgl-pax | `8b55f9ce85ab5a979985055595aeced454b0760c` |
| introspect-environment (branch `egcl`) | https://github.com/atgreen/introspect-environment | `1bcd945c2e2a6358557272886c03bd120ecc95f7` |
| trivia (branch `egcl`) | https://github.com/atgreen/trivia | `be03bc1ce6b6ad7217c4838a9ef0ea4b9d579d60` |
| type-i (branch `egcl`) | https://github.com/atgreen/type-i | `eb2335d44542febcfaaf91adf6d3522787f78f87` |
| misc-extensions (branch `egcl`) | https://github.com/atgreen/misc-extensions | `d401b504e55d396e8fa9df7fe9af4ae68651c301` |
| serapeum (branch `egcl`) | https://github.com/atgreen/serapeum | `43b66df1da96578efc90555d4fee1543590295e6` |
| trivial-macroexpand-all (branch `egcl`) | https://github.com/atgreen/trivial-macroexpand-all | `594238e46b4fbcf5baa35b4e6b1bbb0d2e486f1a` |

The [Quicklisp client scenario](../tests/quicklisp-client/README.md) fetches this
exact client through ocicl, checks its native connection and filesystem
interfaces, installs a fresh local distribution with two dependent releases,
and reloads them in a new process after the HTTP server stops. An optional
public smoke test downloads Alexandria from the dated official distribution
and verifies its source comes from the fresh Quicklisp home, not an inherited
ASDF project. See the [Quicklisp setup instructions](manual/user/how-to/asdf.md#quicklisp).
This needs the runtime fixes through PR #90; v0.0.3 alone is insufficient.

The separate [native usocket scenario](../tests/usocket-fork/README.md) checks
incremental loopback I/O through cold and cached loads, cross-thread server
accept/close, octet element-type aliases, listener options, and read-timeout
expiry and recovery. It requires the native client primitives, shared listener
support from PR #60, and typed I/O timeouts from PR #72. This is not
a claim of HTTPS support or a complete USOCKET backend.

The [Drakma scenario](../tests/drakma/README.md) checks plain HTTP GET and POST
through USOCKET, Chunga, and Flexi Streams, including response text decoding.
Use USOCKET's `egcl-support` branch; its default branch lacks the EGCL backend.

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

Install all documented forks into an existing project with the
[installer](../scripts/install-egcl-forks):

```sh
install-egcl-forks /path/to/your/project
```

The main `egcl` RPM installs this command in `/usr/bin`. From a source checkout,
run `scripts/install-egcl-forks` instead.

Omit the project directory to install into the current directory. Preview the
commands with `--dry-run` (before the optional project directory). The script
requires `git` and an `ocicl` version supporting Git sources on `PATH`, stops
on the first failed install, and fetches each fork's default branch, except
USOCKET (`egcl-support`), Atomics, closer-mop, Ironclad, pure-tls, chipz, SLIME, SLY and the
Quicklisp client, Archive, ZIP, unix-opts, with-user-abort, local-time,
Parachute and the additional audited forks (`egcl`), where their ports live. No commit revisions
are specified. It creates `ocicl.csv` if needed so installation stays in the
selected directory. ocicl records the resolved commits in that file;
commit it in your project for reproducibility. The script downloads and
registers systems; it does not load or compile them in EGCL.

When adding a port, update both the table above and the installer's library
list. The table records previously tested revisions; the installer fetches
the current branch tips. `trivial-garbage` is the GC portability library;
Swank and Slynk are installed from the `slime` and `sly` repositories.

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

The swank fork lives on the slime tree's `torcl` branch and adds
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

The [CL-FAD scenario](../tests/cl-fad/README.md) verifies native EGCL environment
lookup and an optional UIOP fallback on otherwise unsupported implementations.
Directory listing and recursive deletion still require an EGCL port.

The [Atomics scenario](../tests/atomics-fork/README.md) exercises native CAS and
`cl-cancel`'s CAS-dependent lazy atomic-state initialization. The adapter
supports cons cells, simple vectors, symbol values/plists, structure slots, CLOS
slots, and special variables, with Atomics' Boolean success result. This is a
CAS port, not a claim that every Atomics operation is supported: its
`ATOMIC-INCF`/`ATOMIC-DECF`, custom places, and memory references remain unsupported.

The [trivial-backtrace scenario](../tests/trivial-backtrace/README.md) checks
printed and structured backtraces, live condition capture, and output destinations
through cold and cached ASDF loads and all four execution modes. Its EGCL adapter
uses `EGCL-DEBUG` owned snapshots. Argument values are included only when available;
native argument locations and source locations are not yet exposed.

The [portability fork scenario](../tests/portability-forks/README.md) loads the
six audit ports through immutable ocicl sources. It checks pathname metadata,
TAR and ZIP writing/reading, permission bits, raw arguments, interrupt condition
translation, clock precision and process exit statuses. These are focused
compatibility checks, not claims that the full upstream suites pass.
Archive's integer-file-descriptor stat remains unsupported; Parachute timeouts
remain unenforced on EGCL. The existing unix-opts suite has three failures also
reproduced without the adapter (`bliss-c1s72`).

The `bliss-wne9.10` audit does not justify adding EGCL to every SBCL branch.
Many branches are optional optimizations with portable alternatives, including
Alexandria, Babel, global-vars, named-readtables and trivial-file-size.
CFFI already has an EGCL fork; its separate static-image operation needs an
EGCL runtime-linking implementation, not an alias to SBCL's core format.
Capability-dependent follow-up work is tracked by `bliss-wne9.10.2`, including
Parachute timeout enforcement and deeper introspection-library coverage.

## Audited ocicl libraries

The additional forks above follow the upstream sources identified by each
`ocicl/PROJECT/README.org`. Alexandria, Iterate, RT, and Parse Declarations
preserve their non-GitHub upstream history in `atgreen` repositories. The
[complete pinned inventory](../tests/audited-forks/forks.json) records each
source, tested commit, executable smoke-test paths, and coverage limits.

The [audited-forks scenario](../tests/audited-forks/README.md) installs these
commits through ocicl and runs their public API checks in isolated EGCL
processes. Named Readtables uses EGCL's native macro-character enumeration,
which avoids scanning all Unicode characters when loading dependent libraries.
Libraries whose portable implementation already works retain that code and
add explicit EGCL validation rather than changing implementation feature flags.

Passing smoke tests do not establish complete upstream-suite conformance.
In particular, CL-JSON fluid-object decoding remains unsupported; LIFT and
Parachute do not enforce their timeout fallback; documentation tests cover base
sections rather than full rendering and navigation; Drakma does not perform a
network request in this scenario; CL-Selfupdate does not replace executables.

The Trivial Macroexpand All fork requires a runtime exporting
`EGCL-EXT:MACROEXPAND-ALL` from
[runtime PR #641](https://github.com/atgreen/evergreen/pull/641). It preserves
lexical macro environments and quoted
data; unsupported atomic updates and macro-definition forms signal
`PROGRAM-ERROR`, and compiler macros are outside this API. The scenario must
run on that runtime before claiming combined compatibility of its consumers.

Cold CL-Unicode data generation exceeded the 600-second EGCL check limit
(`bliss-ks0tt`). The audited CL-INTERPOL scenario supports `--prepare-unicode`
to generate portable Unicode Lisp data with SBCL in a separate cache before
EGCL compiles and tests it; that is distinct from a cold EGCL build.

The [Gendl MOP scenario](../tests/gendl-mop/README.md) pins the unmodified
public DEVO source and runs its 166-check kernel smoke test. It needs no
compatibility fork at the pinned revision. Its README distinguishes prepared
source/lock checks from completed runtime validation.
