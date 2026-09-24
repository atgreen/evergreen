# Gray protocol imports for TorCL

Use the [atgreen fork and ocicl Git pin](../README.md) for new test scenarios.
The patch below is retained as a historical reference, not the installation path.

`package.patch` adds the TorCL package selection to trivial-gray-streams 2.1
(tested against ocicl `trivial-gray-streams-20260818-257d73e`). It imports the
existing `TORCL-GRAY-STREAMS` protocol, preserving the identity of the generic
functions used by TorCL's standard stream operations.

Apply it from the dependency's source directory after building a TorCL version
that provides `TORCL-GRAY-STREAMS`:

```sh
patch -p1 < /path/to/torcl/ports/trivial-gray-streams-torcl/package.patch
```

Restart TorCL and force recompilation of trivial-gray-streams and its dependent
Flexi Streams files; previously compiled methods refer to the old symbols.
In the restarted process, with the patched dependency in ASDF's source registry:

```lisp
(asdf:load-system :flexi-streams :force :all)
(eq 'trivial-gray-streams:stream-read-char
    'torcl-gray-streams:stream-read-char) ; => T
```

This patch is local, not an upstreamed release. An ocicl dependency update may
replace it.

This is a package-import compatibility patch, not a claim that every optional
Gray protocol extension has been implemented. The Flexi Streams UTF-8 line
reader is exercised during CL-UNICODE's Unicode-data build.
