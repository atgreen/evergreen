# TorCL support for CFFI

Use the [atgreen fork and ocicl Git pin](../README.md):

```sh
ocicl install git+https://github.com/atgreen/cffi@8fc4b2b439525e87795efdfb848a1761869adc2a
```

The fork adds two files and touches two system definitions; everything else is
upstream CFFI, and no other implementation's behaviour changes.

| File | What it is |
|---|---|
| `src/cffi-torcl.lisp` | The CFFI-SYS backend over `TORCL-FFI`: pointers, foreign memory, calls, callbacks, libraries |
| `src/cffi-torcl-fsbv.lisp` | Structures by value, through `TORCL-FFI:FOREIGN-CALL-BUFFERED` |
| `cffi.asd` | Accepts `:torcl` and loads those two |
| `cffi-tests.asd` | Does not pull in `cffi-libffi` on TorCL |

## Why there is no libffi here

TorCL applies the target ABI's aggregate rules itself: `FOREIGN-CALL-BUFFERED`
takes the address of each argument and of the result. So a TorCL image needs
neither libffi nor a C compiler to call a function that takes or returns a
structure by value — which is what `cffi-libffi` exists to provide elsewhere.
`cffi-libffi` still loads on TorCL, and loading it replaces the native path with
libffi's; there is no reason to.

The runtime lays a structure out from a description of its fields, so the
description the fork builds is checked against the layout CFFI computed — same
size, same alignment, same field offsets — and a structure it cannot describe is
reported rather than called. Padding fields are not invented to make offsets
agree: that would change how the ABI classifies the structure and call it wrongly
rather than not at all.

## What the port established about TorCL

CFFI's suite passes **342 of 344** tests on x86-64 Linux. Reaching that took
seven TorCL conformance fixes, each found by a failing CFFI test and each checked
against SBCL's answer for the same form: `bliss-nj6id` (`:argument-precedence-order`
ignored, and method specificity summed instead of compared), `bliss-cb3c7`
(`define-symbol-macro` through `eval` discarded), `bliss-msyk` (a `setf` place
rewritten by a compiler macro), `bliss-hfn71` (`deftype` expanded only one
level), `bliss-jre1u` (function docstrings never recorded), `bliss-06l4z`
(`foreign-free` refusing a re-read pointer) and `bliss-bpjw6` (`loop` leaving its
iteration variable one short in `finally`, which silently truncated every string
Babel encoded into a caller-sized buffer).

The two remaining failures are `FUNCALL.NIL-SKIP` (TorCL's `COMPILE` does not
macroexpand its lambda expression — `bliss-bd6r2`) and
`STRING.ENCODINGS.ALL.BASIC`, which is a Babel bug that fails on SBCL too.

Foreign calls and callbacks are architecture-specific, and loading a shared
library needs a dynamic build:

```sh
cargo build --release --target x86_64-unknown-linux-gnu --features torcl-rt/c-ffi
```
