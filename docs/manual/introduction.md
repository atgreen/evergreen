# Introduction

!!! warning "Evergreen is an experiment and probably does not work."

    Under active development: most of it is likely broken at any given moment,
    and what works may not match this manual. It may never work. Everything here
    describes what Evergreen is trying to be, not something to depend on.

Evergreen Common Lisp (EGCL) implements Common Lisp with a Rust bootstrap
runtime and a tiered native compiler. This manual describes the behavior
specific to EGCL: how a Lisp process starts, how code is compiled, which
extensions are available, and what an application can expect from the runtime.

It assumes familiarity with Common Lisp. The
[first-program tutorial](user/tutorials/first-program.md) is available for an
initial hands-on session, but the chapters below are an implementation manual,
not a language tutorial.

## Conformance and compatibility

ANSI Common Lisp is the compatibility target. EGCL is under active development
and does not claim complete conformance. Exporting a standard symbol or accepting
a declaration is not evidence that every associated behavior is implemented.
The test suites and the behavior described in this manual are more useful than
a blanket “supported” label.

Selected interfaces have semantics familiar to SBCL users, but EGCL exposes
them through its own packages, not `SB-*` aliases or re-exports. A similar
function name does not guarantee the same lambda list. Use the documented EGCL
signature, especially for subprocesses and foreign calls.

## Public and internal names

`COMMON-LISP-USER` (nickname `CL-USER`) is the initial application package.
`EGCL-EXT` contains implementation extensions, `EGCL-FFI` the public foreign
interface, and `EGCL-THREAD` native threads and synchronization.
`EGCL-THREADS` is a compatibility nickname for `EGCL-THREAD`.
`EGCL-CLTL2` supplies lexical-environment extensions.
Loading the `egcl-jvm` ASDF system provides `EGCL-JAVA` (nickname `JAVA`) for
[Java integration](java.md), and `EGCL-JVM` for descriptor-based Java calls.

The bootstrap also uses `EGCL::%...` entry points and `EGCL-INTERNAL` state.
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

EGCL has static and dynamic builds. Dynamic library loading requires a dynamic
runtime built with the FFI feature. Architecture ports also differ in native
compilation, callbacks, and fiber switching. Consult the
[platform reference](user/reference/platforms.md) before selecting a deployment
runtime.

## Logical-block printing

`PPRINT-LOGICAL-BLOCK` groups output sent to its bound stream. Use the lexical
`PPRINT-POP` and `PPRINT-EXIT-IF-LIST-EXHAUSTED` macros to traverse the supplied
list without losing dotted-tail and print-limit handling. A `NIL` stream
variable designator rebinds `*STANDARD-OUTPUT*`; `T` rebinds `*TERMINAL-IO*`.

```lisp
(let ((*print-pretty* t) (*print-right-margin* 20))
  (pprint-logical-block (nil '(alpha beta gamma) :prefix "(" :suffix ")")
    (loop
      (pprint-exit-if-list-exhausted)
      (write (pprint-pop))
      (pprint-exit-if-list-exhausted)
      (write-char #\Space)
      (pprint-newline :fill))))
```

The block supports ordinary and per-line prefixes, a suffix, nested blocks,
conditional newlines, and `PPRINT-INDENT`. `PPRINT-FILL` and `PPRINT-LINEAR`
provide the standard list-printing patterns. These interfaces do not imply
support for user-defined pprint dispatch tables; `SET-PPRINT-DISPATCH` and
`PPRINT-DISPATCH` remain separate implementation work.

## Scope of this edition

This edition follows the development checkout, not a frozen release. Older
installed packages may predate the behavior described here. Planned interfaces
in `spec/` are not automatically part of the callable Lisp API. Both the native-thread interface and the x86-64 `EGCL-FIBER` interface
described here are installed by the standard bootstrap.

## Reporting a problem

Include the EGCL version, target architecture, static/dynamic build, source
revision when available, exact command, a small reproducer, and both standard
output and standard error. State whether the problem occurs when loading source,
loading a compiled file, or restoring an image.

For a tier-dependent failure, include the relevant `EGCL_*` environment
variables and compare the same input under ordinary execution and
`EGCL_BACKEND=tree-walker`. Preserve wrong results as well as crash reports.
Compiler tier changes should not change the program's meaning.
