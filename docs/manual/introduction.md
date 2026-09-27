# Introduction

TorCL implements Common Lisp with a Rust bootstrap runtime and a tiered native
compiler. This manual describes the behavior specific to TorCL: how a Lisp
process starts, how code is compiled, which extensions are available, and what
an application can expect from the runtime.

It assumes familiarity with Common Lisp. The
[first-program tutorial](user/tutorials/first-program.md) is available for an
initial hands-on session, but the chapters below are an implementation manual,
not a language tutorial.

## Conformance and compatibility

ANSI Common Lisp is the compatibility target. TorCL is under active development
and does not claim complete conformance. Exporting a standard symbol or accepting
a declaration is not evidence that every associated behavior is implemented.
The test suites and the behavior described in this manual are more useful than
a blanket “supported” label.

Selected SBCL-compatible interfaces help existing libraries run on TorCL.
Compatibility does not make every `SB-*` package available, and a similar
function name does not guarantee the same lambda list. Use the actual TorCL
signature, especially for subprocesses and foreign calls.

## Public and internal names

`COMMON-LISP-USER` (nickname `CL-USER`) is the initial application package.
`TORCL-EXT` contains implementation extensions, `TORCL-FFI` the public foreign
interface, and `TORCL-THREAD` native threads and synchronization.
`TORCL-THREADS` is a compatibility nickname for `TORCL-THREAD`.
`TORCL-CLTL2` supplies lexical-environment extensions.
Loading the `torcl-jvm` ASDF system provides `TORCL-JAVA` (nickname `JAVA`) for
[Java integration](java.md), and `TORCL-JVM` for descriptor-based Java calls.

The bootstrap also uses `TORCL::%...` entry points and `TORCL-INTERNAL` state.
Do not build an application API around those internal names. They connect Lisp
wrappers to the runtime and may change with the implementation.

## Implementation characteristics

Cold code can execute before native compilation. T1 and T2 compilation are
selected by runtime behavior and backend support; `compile` is not a request
for immediate T2 machine code. Native code can deoptimize when an assumption
fails. See [Compilation](compiler.md).

Heap images and compiled files are different artifacts. A `.fasl` file is
loaded with `load`; a saved core is restored with `--image`. Executable images
contain their runtime and remain specific to its architecture and operating
system. See [Saved images](user/reference/images.md).

TorCL has static and dynamic builds. Dynamic library loading requires a dynamic
runtime built with the FFI feature. Architecture ports also differ in native
compilation, callbacks, and fiber switching. Consult the
[platform reference](user/reference/platforms.md) before selecting a deployment
runtime.

## Scope of this edition

This edition follows the development checkout, not a frozen release. Older
installed packages may predate the behavior described here. Planned interfaces
in `spec/` are not automatically part of the callable Lisp API. In particular,
the native-thread interface described here is implemented; the proposed public
`TORCL-FIBER` interface is not installed by the current bootstrap.

## Reporting a problem

Include the TorCL version, target architecture, static/dynamic build, source
revision when available, exact command, a small reproducer, and both standard
output and standard error. State whether the problem occurs when loading source,
loading a compiled file, or restoring an image.

For a tier-dependent failure, include the relevant `TORCL_*` environment
variables and compare the same input under ordinary execution and
`TORCL_BACKEND=tree-walker`. Preserve wrong results as well as crash reports.
Compiler tier changes should not change the program's meaning.
