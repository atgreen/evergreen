# TorCL support for iolib

Use the [atgreen fork and ocicl Git pin](../README.md):

```sh
ocicl install git+https://github.com/atgreen/iolib@81ac1fdc376fbcdb491fe758a3bf957ec0a9c175
```

The fork is one line of library change, in `src/new-cl/pkgdcl.lisp`:

```lisp
#+torcl            :torcl-gray-streams
#-(or abcl allegro cmu scl clisp ecl ccl openmcl lispworks sbcl torcl)
```

`DEFINE-GRAY-STREAMS-PACKAGE` names the package each implementation keeps the
Gray-stream protocol in and signals "Your CL implementation isn't supported" for
any it does not know. TorCL's is `TORCL-GRAY-STREAMS`, and all 28 symbols iolib
imports from it are already exported — checked one at a time — so nothing is
stubbed.

## What loads

All of it: `iolib/syscalls`, `iolib/multiplex`, `iolib/streams`,
`iolib/zstreams`, `iolib/sockets`, `iolib/pathnames` and `iolib`, plus
`iolib.base` and `iolib.conf`. Groveling runs, and the C library dependency
(libfixposix) loads through the CFFI backend (`../cffi-torcl/`) — iolib is the
first real consumer of that backend beyond CFFI's own suite.

## The four TorCL fixes it took

Each was found by iolib failing, and each is a bug any library could have hit:

| Fix | What was wrong |
|---|---|
| DEFCONSTANT marked at compile time | `COMPILE-FILE` seeded a `defconstant` like a `defparameter`, so alexandria's `DEFINE-CONSTANT` reported "already bound non-constant variable" when the fasl loaded. iolib's *first* constant hit it. |
| `(setf (compiler-macro-function …))` | iolib's `DEFALIAS` installs a compiler macro through that place; TorCL could read it but not set it. |
| The initialization protocol's initargs | `MAKE-INSTANCE` keyed them by slot name, so an `:after` method's `&key components` bound NIL. iolib's `FILE-PATH` `CHECK-TYPE`s its `:components`, so every `PARSE-FILE-PATH` failed. |
| LOOP `sum`/`count` identity | split-sequence reads its own accumulator in a clause *before* the summing clause, so `(split-sequence #\. "0.0.0.0" :count 5)` was a type error and no IP address could be parsed. |

The last two answered wrongly rather than loudly, which is why they survived this
long: nothing before iolib had asked those questions.
