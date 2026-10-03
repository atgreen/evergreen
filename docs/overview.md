# EGCL architecture overview

Evergreen Common Lisp (EGCL) is a Common Lisp implementation whose bootstrap
system is written in Rust. Its long-term direction is a self-hosting Lisp with
a HotSpot-inspired execution engine: cold code starts cheaply, hot code is
promoted to native tiers, and optimized code can deoptimize without changing
program semantics.

This document describes the architecture of the implementation in this
repository. The normative design and roadmap live in the
[technical specification](../spec/00-overview.md); some of that design is still
being integrated into the bootstrap system.

## System map

The Cargo dependency graph is intentionally layered:

```text
crates/egcl -------------------------------> crates/egcl-rt
    |                                               ^
    +---> crates/egcl-compiler --------------------+
    |               ^                               ^
    |               |                               |
    +---> crates/egcl-stdlib ----------------------+
```

`crates/egcl-rt` is the foundation and has no dependency on the compiler or
standard library. `crates/egcl-compiler` depends on the runtime's value and
bytecode representations. `crates/egcl-stdlib` builds on both. The `egcl`
package depends on all three and connects them to a runnable Lisp.

| Component | Primary responsibility |
| --- | --- |
| `crates/egcl-rt` | VM substrate: tagged values, heap object layouts, allocation and GC, function metadata, Lisp stacks, safepoints, threads and fibers, synchronization, images, BFASL support, FFI, sandboxing, and OS services |
| `crates/egcl-compiler` | Reusable compiler facilities: reader, macro-expansion environment, profiling and tier policy types, intermediate representations, optimization, register allocation, OSR/deoptimization metadata, and native code emission |
| `crates/egcl-stdlib` | Rust bootstrap implementation of library behavior: packages, CLOS, conditions and restarts, streams, sequences, hash tables, `FORMAT`, pathnames, time, and developer tools |
| `crates/egcl` | Process-facing integration: CLI and REPL, loading, evaluator state, tree-walking fallback, form-to-bytecode lowering, T0 interpretation, T1 emission, tier dispatch, and bridges between compiler, runtime, and stdlib |
| `lib/boot.lisp` | Lisp-side bootstrap prelude. It implements macros and ordinary functions that do not belong in the evaluator |
| `lib/asdf.*` | Bundled real-world library payload |

The large `crates/egcl/src/cli.rs` is therefore both a user interface and a
bootstrap integration layer. It is not the desired final home for library
behavior. When the evaluator exposes a Common Lisp operation, it should
delegate to `egcl-stdlib`; new package, stream, sequence, condition, pathname,
or similar semantics belong in the stdlib crate first.

## From source text to execution

The default execution path combines compilation with a compatibility fallback:

```text
source text
    |
    v
reader -> Lisp forms -> macro expansion -> top-level dispatcher
                                              |
                              +---------------+----------------+
                              |                                |
                              v                                v
                    supported compiled form          unsupported/bootstrap form
                              |                                |
                              v                                v
                     portable bytecode                 tree-walking evaluator
                              |
                 +------------+-------------+
                 |            |             |
                 v            v             v
             T0 bytecode   T1 baseline   T2 optimizing
             interpreter      native         native
```

1. `egcl-compiler::reader` turns characters into `EgclVal` forms and interns
   symbols in the runtime symbol registry.
2. The compiler macro-expander processes forms using an environment assembled
   from the live bootstrap evaluator. This bridge lets macros defined by loaded
   Lisp code participate in later compilation.
3. The top-level dispatcher in `crates/egcl/src/cli/bytecode.rs` preserves
   Common Lisp top-level semantics for forms such as `EVAL-WHEN`, `PROGN`, and
   defining forms. A `DEFUN` is installed in the evaluator and, when supported,
   lowered to a `BytecodeFunction`.
4. The bytecode backend executes supported functions at T0. If lowering cannot
   represent a form yet, compilation bails out and the tree-walking evaluator
   remains the semantic fallback and differential-testing oracle.
5. Invocation and loop-back-edge counters attached to stable heap function
   objects drive promotion. T1 emits baseline native code directly from
   bytecode. T2 builds block-based SSA, applies profile-guided optimization,
   allocates registers, and emits optimized native code on background compiler
   threads.
6. Hot loops may enter native code through on-stack replacement. Speculative
   guards carry enough frame state to resume T0 at a bytecode position when an
   assumption fails. Interpreter-to-compiled and compiled-to-interpreter
   adapters allow calls to cross tier boundaries.

Portable bytecode is the common coordinate system across the tiers. Bytecode
program counters identify source locations, profiling sites, stack maps, OSR
entries, and deoptimization continuations. A tier transition is an execution
policy decision; it must not change observable Common Lisp behavior.

The bytecode backend is the default. `EGCL_BACKEND=tree-walker` selects the
fallback directly, chiefly for diagnosis and differential tests. The compiler
still has coverage gaps, so real programs commonly use compiled and
tree-walked paths in the same process.

## Runtime object and memory model

Every Lisp value crosses subsystem boundaries as a 64-bit `EgclVal`. Low tag
bits distinguish immediates such as fixnums and characters from conses,
general heap objects, symbols, and functions. Most heap objects begin with an
`ObjectHeader` containing their type, size, identity hash, and structural GC
bits. Conses use their own compact layout.

Interpreted function objects are heap-resident and identity-stable. In addition
to their lambda/body information they own invocation and back-edge counters,
the active entry point, current tier, and compilation flags. Redefinition
updates this state in place so inline caches, profiling, and deoptimization can
refer to a stable function identity.

The collector in `egcl-rt` is precise, generational, moving, and region based:

- Mutators allocate through thread-local allocation buffers in nursery
  regions.
- A stop-the-world minor collection copies live nursery objects to survivor or
  old-generation regions and rewrites references to their new addresses.
- Major collection marks older regions and reclaims or evacuates them; large
  and pinned objects have separate movement rules.
- Write barriers and remembered metadata preserve old-to-young and concurrent
  marking invariants.
- Symbol cells, heap fields, Lisp stack maps, evaluator environments, bytecode
  literal pools, stdlib registries, and scoped Rust locals all contribute
  precise roots.

Because nursery objects move, a raw `EgclVal` held only in a Rust local is not
safe across an allocation. Allocating code roots live host values with
`egcl_rt::rooted!` or `egcl_rt::rooted_ref!`; the collector then rewrites the
root in place. It is equally important not to hold a `RefCell` borrow of
GC-scanned state while allocating, because collection re-enters root scanners.
The detailed contract is in [GC rooting](design/gc-rooting.md).

## Lisp and host state

EGCL is partway through moving bootstrap data into uniform heap
representations. The runtime heap and global symbol cells already provide the
shared representation used by interpreted and compiled code. The bootstrap
`Env`, however, still owns some lexical environments, macro definitions,
closures, CLOS metadata, handlers, restarts, and multiple-value state in Rust
structures. Those structures implement root tracing so their `EgclVal` fields
remain valid across moving collections.

Subsystems with host-side registries install explicit GC and image hooks. This
keeps their heap references traceable and lets image restoration rebuild state
that is not encoded solely by walking heap objects. Converging the remaining
host registries on shared heap layouts is an active architectural migration,
not a separate object model that new code should extend.

## Standard-library boundary

`egcl-stdlib` owns reusable Common Lisp library semantics. The CLI maps Lisp
calls onto those APIs and supplies evaluator callbacks where the library needs
to invoke Lisp code. Examples include CLOS method execution, condition
handlers, stream operations, `FORMAT`, generic sequence operations, hash-table
storage and operations, and pathname services.

`lib/boot.lisp` is the next layer up. It defines language facilities that can
already be expressed in Lisp, reducing special cases in the evaluator and
advancing the self-hosting path. The intended direction is to move more of the
compiler, optimizer, GC policy, and standard library into Lisp while retaining
the low-level Rust runtime.

## Stacks, threads, and platform services

Each managed fiber owns a `EgclStack`, separate from the carrier thread's Rust
stack. T0 and native frames use a shared walkable layout with frame metadata and
stack maps, allowing the GC, debugger, OSR, and deoptimizer to describe live
values consistently. Safepoint polling coordinates stack publication, garbage
collection, cooperative scheduling, and pending signal delivery.

`egcl-rt` also contains the M:N fiber scheduler, native-thread support,
synchronization primitives, timers, asynchronous file-descriptor waits, signal
handling, and the foreign-function bridge. Linux runtime services use direct
syscalls where practical, which permits a fully static default build. That
default build loads supported shared ELF libraries through a pure-Rust loader;
the optional `c-ffi` feature selects libc's `dlopen` backend for dynamically
linked targets.

The runtime substrate is broader than the currently integrated Lisp surface.
In particular, some evaluator and definition registries are still host-local,
so complete cross-thread sharing and the final self-hosted concurrency model
remain staged work.

## Startup and persistence

`crates/egcl/src/main.rs` delegates process startup to the CLI driver. The
driver parses options, installs signal and registry hooks, creates the initial
environment, and then chooses between image and bootstrap paths:

1. After creating a fresh host `Env`, the driver restores any heap core image
   embedded in the executable or named by `--image`. Restoration happens before
   the Lisp bootstrap prelude and replaces the provisional heap state while
   rebuilding the registries needed for the saved Lisp world.
2. Without a restored core, `lib/boot.lisp` is read and evaluated unless the
   raw `--no-bootstrap` mode was requested.
3. The interactive init file and user loads (including portable BFASL library
   units) are applied as appropriate for the selected mode. Startup images must
   be heap cores; BFASL and source files are loaded with `--load`, not `--image`.
4. The driver evaluates `--eval` forms, loads a file, runs a script or saved
   top-level, or enters the REPL.

There are two complementary persistence formats:

- BFASL (`egcl-rt::bfasl`) packages portable compiled bytecode units plus
  source/debug metadata. When a source unit cannot yet be externalized as
  bytecode, the current compiler can emit a loadable source-form fallback.
- Core images (`egcl-rt::image`) snapshot the runtime heap and associated
  registries for fast restoration and saved executables.

## Repository guide

The most useful entry points for following a feature end to end are:

| Area | Start here |
| --- | --- |
| Process modes and bootstrap evaluation | `crates/egcl/src/cli.rs` |
| Bytecode lowering, T0/T1 dispatch, and tier integration | `crates/egcl/src/cli/bytecode.rs` |
| Tagged values and heap layouts | `crates/egcl-rt/src/value.rs`, `crates/egcl-rt/src/object.rs`, and `crates/egcl-rt/src/function.rs` |
| GC and root protocol | `crates/egcl-rt/src/gc.rs` and `docs/design/gc-rooting.md` |
| Lisp stacks, threads, fibers, and safepoints | `crates/egcl-rt/src/stack.rs`, `crates/egcl-rt/src/thread.rs`, `crates/egcl-rt/src/scheduler.rs`, and `crates/egcl-rt/src/safepoint.rs` |
| Reader and macro expansion | `crates/egcl-compiler/src/reader.rs` and `crates/egcl-compiler/src/macroexpand.rs` |
| Optimizing compiler | `crates/egcl-compiler/src/t2/` |
| Library subsystems | `crates/egcl-stdlib/src/` |
| Lisp bootstrap layer | `lib/boot.lisp` |
| Lisp-visible implementation extensions | `docs/egcl-lisp-api.md` |
| Normative requirements and staged roadmap | `spec/INDEX.md` and `spec/stages.json` |

Tests normally live with the crate that owns the behavior. Cross-cutting Lisp
acceptance and differential cases live under `tests/`, and parser/runtime
fuzzers live under `fuzz/`. The stage gate in `spec/stages.json` identifies the
current vertical slice; specification coverage is checked by
`scripts/spec-coverage.py`.

The central architectural test for a change is ownership: put representation,
allocation, stacks, or OS machinery in `egcl-rt`; compiler analysis in
`egcl-compiler`; Common Lisp library behavior in `egcl-stdlib` or Lisp; and
only process orchestration and the remaining bootstrap bridges in
`crates/egcl`.
