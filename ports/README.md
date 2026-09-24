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

For an existing ocicl project, run there:

```sh
ocicl install git+https://github.com/atgreen/trivial-features@26e8d168deb7d70d762d0d7e41b2919eba4321aa
ocicl install git+https://github.com/atgreen/trivial-gray-streams@e55ad7e91aa5aa92408fe082b2e307f8986be080
```

Restart TorCL after switching implementations of a protocol library. The fork
source directories differ from registry directories, so ASDF uses distinct cache
paths. Record new pins only after testing the new revision.

The older patch files are retained as historical references. The Bordeaux
Threads port still requires an audit (`bliss-59qu`): its documented no-op
synchronization is not a general-purpose threading implementation. Do not copy
that workaround into a published compatibility fork as though it were one.
