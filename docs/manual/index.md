# Evergreen Common Lisp — User Manual

!!! warning "This is an experiment, not a product. It probably does not work."

    Evergreen is under active development. Most of it is likely broken at any
    given moment, and the parts that do work may not behave the way this manual
    says they should. It may never work.

    This manual documents what Evergreen is *trying* to be. Where it and the
    implementation disagree, the implementation is what you actually get — and
    neither is a promise. Do not depend on it for anything that matters.

    That is the point rather than an apology for it: Evergreen exists to find out
    whether a real language implementation — a tiered JIT, a moving generational
    collector, a standard library — can be built through AI-driven development.
    The answer is not in yet.

This manual describes EGCL's implementation of Common Lisp and its extensions.
It concentrates on behavior specific to EGCL: startup, compilation, debugging,
memory, foreign calls, Java integration, concurrency, and application delivery.

The manual follows the development checkout. For differences between targets,
consult [Platform support](user/reference/platforms.md). For a particular
operator, use the [Symbol index](symbol-index.md); for a topic, use the
[Concept index](concept-index.md).

## Contents

1. [Introduction](introduction.md) — conformance, compatibility, public packages,
   implementation characteristics, and reporting problems.
2. [Starting and stopping](starting.md) — invocation modes, initialization,
   batch behavior, command-line arguments, and saved worlds.
3. [Compilation](compiler.md) — function and file compilation, native tiers,
   declarations, diagnostics, and compiler observations.
4. [Conditions and debugging](debugger.md) — condition handling, restarts,
   debugger commands, frame inspection, and error reproduction.
5. [Memory and garbage collection](memory.md) — Lisp and foreign memory,
   explicit collection, finalization, and measurement.
6. [Foreign function interface](foreign.md) — C types, pointers, allocation,
   shared libraries, calls, callbacks, and lifetime rules.
7. [Java integration](java.md) — JVM lifecycle, Java calls and bindings,
   Lisp callbacks, reference scopes, and the descriptor API.
8. [Threads and synchronization](concurrency.md) — native threads, joins,
   mutexes and condition variables. [Fibers and scheduler groups](fibers.md)
   covers the fiber API and its current availability.
9. [Operating-system interface](user/reference/extensions.md) — environment,
   current directory, subprocesses, and process arguments.
10. [Profiling and efficiency](profiling.md) — engine reports, exact counts,
   event recording, sampling, and measurement effects.
11. [Application delivery](user/explanation/images.md) — core images,
    executables, cross-target creation, and Android's application lifecycle.

## Recipes and supplementary reference

- [Build and install EGCL](user/how-to/build.md)
- [Run a first Lisp program](user/tutorials/first-program.md)
- [Load an ASDF system](user/how-to/asdf.md)
- [Save an executable](user/how-to/save-executable.md)
- [Build for another platform](user/how-to/cross-build.md)
- [Build an Android app](user/how-to/android.md)
- [Command-line options](user/reference/cli.md)
- [Image dictionary](user/reference/images.md)
- [Android project dictionary](user/reference/android.md)

## Appendices

- [Runtime contributor notes](contributing/index.md)
- [Runtime fiber API](contributing/reference/fibers.md)
- [Symbol index](symbol-index.md)
- [Concept index](concept-index.md)
- [Writing and building the manual](meta/documentation-guidelines.md)
