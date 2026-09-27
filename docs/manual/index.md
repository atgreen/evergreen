# TorCL manual

Common Lisp, from a first expression to a saved application.
{ .lead }

TorCL is a Common Lisp implementation written in Rust, with a tiered execution
engine and tools for building applications for Linux, Windows, and Android.
This manual describes the development version in this checkout. TorCL is still
under active development; its goal of ANSI Common Lisp compatibility is not a
claim of complete conformance.

[Run your first program](user/tutorials/first-program.md){ .md-button .md-button--primary }
[Build an Android app](user/how-to/android.md){ .md-button }

## Use TorCL

Start with the [user guide](user/index.md) to run Lisp, load libraries, save an
executable, or build for another machine. The [platform table](user/reference/platforms.md)
distinguishes execution support from native compiler support.

## Work on TorCL

The [contributor guide](contributing/index.md) covers the runtime's architecture,
GC rules, validation, and performance measurement. The implementation and the
longer-term specification have different roles: a planned interface is not
necessarily a callable interface.

## Find the right kind of page

| If you want to… | Read… |
| --- | --- |
| Learn by completing a small program | A tutorial |
| Finish a particular task | A how-to guide |
| Look up syntax, options, or support | Reference |
| Understand why the system behaves this way | Explanation |

The manual follows the [same documentation rules](meta/documentation-guidelines.md)
throughout. Search is available in the header; the theme switch changes between
light and dark mode.
