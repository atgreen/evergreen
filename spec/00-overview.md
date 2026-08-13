# Bliss — Technical Specification

**Version:** 0.1-draft
**North Star:** HotSpot JVM — tiered compilation, adaptive optimisation,
production-grade GC, and a self-hosting runtime.

## 0  Scope & Goals

Bliss is a from-scratch Common Lisp implementation written in Rust (bootstrap
runtime) and, eventually, Common Lisp itself (compiler, optimiser, large parts
of the standard library). The project targets ANSI X3.226-1994 (CLtL2 + ANSI
errata) with SBCL-compatible extensions where the standard is silent.
SBCL extensions are adopted when they are (a) widely depended upon by portable
libraries (e.g., `sb-ext:defglobal`, `sb-thread` API shapes) and (b) do not
conflict with ANSI semantics. Each adopted extension MUST be listed in
`spec/09-extensions.md` with its rationale and compatibility notes.

### 0.1  Primary Goals

| # | Goal | Metric |
|---|------|--------|
| G1 | ANSI compliance | Pass ANSI test suite (Paul Dietz's `ansi-test`) |
| G2 | Competitive peak throughput | Within 2× of SBCL on cl-bench within 2 years |
| G3 | Fast startup | Cold start < 50 ms for a hello-world |
| G4 | Tiered compilation | Interpreter → baseline compiler → optimising compiler |
| G5 | Modern GC | Generational, concurrent, compacting collector |
| G6 | Native threads | OS-thread-per-core with green-thread scheduling |
| G7 | Self-hosting | Compiler and GC policy written in Bliss CL |
| G8 | Embeddability | C-ABI shared library with stable FFI surface |

### 0.2  Non-Goals (initial release)

- Full MOP (AMOP) — provide CLOS but defer full MOP to v2.
- Windows support — Linux and macOS first; FreeBSD best-effort.
- GPU / SIMD auto-vectorisation — manual intrinsics only.

## 0.3  Delivery Model: Staged Vertical Slices

Bliss is specified and implemented as a sequence of **stages**, each of
which is a runnable vertical slice through the real `bliss` entrypoint.
The stage manifest lives in `spec/stages.json`; the roadmap in §11
interprets that manifest for humans, and §10 defines how the gates are
verified.

The governing principle is **depth before breadth**. A stage is only
complete when a user can run its gate end-to-end and observe the claimed
behaviour through the real CLI or REPL. Internal completeness of a
subsystem does not count as stage completion by itself.

### 0.3.1  Stage Semantics

- A **stage** is the smallest user-meaningful language/runtime slice
  that can be exercised through the real binary.
- A **gate** is the concrete end-to-end scenario that proves the stage
  is working for a real user.
- Earlier gates remain part of the required regression suite as later
  stages are added.
- Broad subsystem work that spans multiple stages MUST be partitioned by
  stage tags or by file-level stage ownership as described in
  `spec/conventions.md`.

### 0.3.2  Anti-Illusion Rule

The specification distinguishes genuine stage completion from artifacts
that only make a gate appear to pass:

- Special-casing filenames, forms, or command-line modes solely so a
  gate example succeeds is a defect, not a milestone.
- Returning placeholder, echoed, or fixed outputs from a seam that is
  supposed to perform real reader/evaluator/compiler/runtime work is a
  defect, even if the gate transcript matches the expected text.
- Acceptance evidence is valid only when the capability under test is
  implemented at the seam the user actually exercises.

## 0.4  Stage Map

The current stage sequence is:

| Stage | Name | Primary user-visible capability |
|-------|------|---------------------------------|
| 0 | Read, print, evaluate | Core datatypes, `quote`/`if`/`progn`, primitive arithmetic/list ops |
| 1 | Core evaluator | Closures, recursion, non-local exits, full lambda lists |
| 2 | Macros | `defmacro`, backquote, standard control/binding macros, real source loading |
| 3 | Data and library breadth | Sequences, strings, hash tables, `format`, pathnames, packages |
| 4 | Conditions and CLOS | User-defined conditions, classes, generics, methods, restarts |
| 5 | HotSpot-inspired engine | Tiering, profiling, ICs, OSR, speculative optimisation, deopt |
| 6 | Real-world payload | Real ASDF/library loading and self-hosting trajectory |

This stage map is intentionally narrower than the full chapter map in
§2. A single subsystem chapter may contribute requirements to multiple
stages.

## 1  Architecture Overview

```
┌──────────────────────────────────────────────────────────┐
│                      User Code (CL)                      │
├───────────────────────┬──────────────────────────────────┤
│   Standard Library    │     CLOS / Conditions / Streams  │
│   (§5 — mostly CL)   │     (§5 — CL + Rust glue)       │
├───────────────────────┼──────────────────────────────────┤
│   Compiler Pipeline   │     Debugger / Profiler          │
│   (§4)                │     (§6)                         │
├───────────────────────┴──────────────────────────────────┤
│              Object Model & Type System (§1)             │
├──────────────────────────────────────────────────────────┤
│           Memory Manager / Garbage Collector (§3)        │
├──────────────────────────────────────────────────────────┤
│          Runtime Core (§2): threads, FFI, I/O            │
├──────────────────────────────────────────────────────────┤
│              Platform Abstraction (Rust)                 │
└──────────────────────────────────────────────────────────┘
```

### 1.1  Execution Tiers (HotSpot Model)

| Tier | Name | When | Output |
|------|------|------|--------|
| T0 | **Bytecode interpreter** | First call | Portable Bliss bytecode |
| T1 | **Baseline compiler** | Call count ≥ 10 | Unoptimised native code compiled from bytecode |
| T2 | **Optimising compiler** | Hot loop / call count ≥ 5000 | Optimised native via IR |

Tier transitions are managed by the **Profiling Subsystem** (§4.9) which
inserts counters and traps. The **tiering *policy*** — invocation/back-edge
counters, promotion thresholds, OSR at loop back-edges, deoptimisation on
speculative-guard failure, inline caches — follows HotSpot's C1/C2 pipeline
directly. The **execution *mechanism*** deliberately differs: HotSpot uses a
template (assembly) interpreter whose frames share one native stack with
compiled code under an exact GC, whereas Bliss's baseline model runs a
host-language (Rust) bytecode interpreter. Both interpreted and compiled CL
activations still live on a single per-green-thread control stack bridged by
interpreter↔compiled adapters (§2.4.4); a template interpreter is an explicit
later option (§2.4.5), not the baseline. See §2.4 for the stack model and the
scope of the HotSpot correspondence.
Source forms are never the long-lived execution representation: the reader
and macroexpander produce forms, the compiler front-end lowers them to
portable Bliss bytecode, and every tier uses bytecode PCs as the stable
debugging, profiling, OSR, and deoptimisation coordinate system.

### 1.2  Language Used per Layer

| Layer | Language | Rationale |
|-------|----------|-----------|
| Platform abstraction, GC inner loops, bootstrap reader/bytecode interpreter | Rust | Safety, performance, no runtime dependency |
| Standard library, compiler middle-end/back-end, CLOS | Common Lisp (self-hosted) | Dogfooding, expressiveness |
| Build orchestration | Cargo + Make | Standard Rust tooling |

## 2  Specification Chapter Map

Each chapter lives in its own file under `spec/` and is numbered for
cross-referencing. Code modules reference spec sections as `§N.M`.

| File | Chapter | Contents |
|------|---------|----------|
| `01-object-model.md` | §1 Object Model | Tagged pointers, type lattice, immediate vs heap types, layouts |
| `02-runtime-core.md` | §2 Runtime Core | Thread model, stack layout, FFI, signal handling, startup |
| `03-memory-gc.md` | §3 Memory & GC | Heap structure, nursery/old-gen, write barriers, concurrent marking |
| `04-compiler.md` | §4 Compiler Pipeline | Reader, macro-expander, IR, tiered compilation, OSR |
| `05-stdlib.md` | §5 Standard Library | Package layout, CLOS, conditions, streams, sequences, I/O |
| `06-devtools.md` | §6 Developer Tools | REPL, debugger, profiler, disassembler, IDE protocol |
| `07-ops-portability.md` | §7 Ops & Portability | Image save/load, deployment, platform matrix, config |
| `08-security-robustness.md` | §8 Security | Sandboxing, safe FFI, resource limits, signal safety |
| `09-extensions.md` | §9 SBCL Extensions | Adopted SBCL-compatible extensions, rationale, compatibility |
| `10-testing-validation.md` | §10 Testing & Validation | ANSI test suite, benchmarks, CI, fuzzing strategy |
| `11-phasing-roadmap.md` | §11 Phasing & Roadmap | Bootstrap phases, milestones, self-hosting path |

## 3  Directory Structure (Current Source Tree)

```
bliss/
├── Cargo.toml              # workspace root
├── spec/                   # this specification
├── scripts/
│   └── spec-coverage.py    # requirement traceability gate
├── crates/
│   ├── bliss-rt/           # §2 runtime core (Rust)
│   │   ├── src/
│   │   │   ├── runtime.rs      # runtime entry points and coordination
│   │   │   ├── object.rs       # tagged object model support
│   │   │   ├── value.rs        # Bliss value representation helpers
│   │   │   ├── gc.rs           # §3 garbage collector
│   │   │   ├── image.rs        # image save/load support
│   │   │   ├── safepoint.rs    # safepoint protocol
│   │   │   ├── scheduler.rs    # green-thread scheduling
│   │   │   ├── sandbox.rs      # sandbox enforcement
│   │   │   ├── stack.rs        # CL stack representation
│   │   │   ├── thread.rs       # native and green thread state
│   │   │   ├── ffi.rs          # C-ABI bridge
│   │   │   ├── error.rs        # runtime error types
│   │   │   ├── types.rs        # runtime type descriptors
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── bliss-compiler/     # §4 compiler pipeline (Rust bootstrap)
│   │   ├── src/
│   │   │   ├── reader.rs       # CL reader
│   │   │   ├── macroexpand.rs  # macro expansion
│   │   │   ├── ir.rs           # intermediate representation
│   │   │   ├── tiered.rs       # tiered compilation orchestration
│   │   │   ├── opt.rs          # T2 optimising backend
│   │   │   ├── osr.rs          # on-stack replacement
│   │   │   ├── codegen.rs      # native code emission
│   │   │   ├── ic.rs           # inline caches
│   │   │   ├── profiling.rs    # profiling counters and metadata
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── bliss-stdlib/       # §5 standard library (Rust bootstrap implementation)
│   │   ├── src/
│   │   │   ├── clos.rs         # CLOS support
│   │   │   ├── conditions.rs   # condition system
│   │   │   ├── streams.rs      # streams
│   │   │   ├── sequences.rs    # sequences
│   │   │   ├── hashtable.rs    # hash tables
│   │   │   ├── pathnames.rs    # pathnames
│   │   │   ├── format.rs       # FORMAT and printer support
│   │   │   ├── packages.rs     # package system
│   │   │   ├── devtools.rs     # developer tools hooks
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   └── bliss/          # REPL + command-line driver
│       ├── src/cli.rs
│       ├── src/main.rs
│       └── Cargo.toml
├── lib/
│   └── asdf.lisp           # bundled bootstrap library support
└── crates/*/tests/         # per-crate unit and integration tests
```

## 4  Key Design Decisions

### 4.1  Tagged Pointers (§1)

All Lisp values are represented as 64-bit tagged words (`BlissVal`).
The low 3 bits encode the type tag:

| Tag (bits 2:0) | Type | Payload |
|-----------------|------|---------|
| `000` | Fixnum | 61-bit signed integer (arithmetic shift) |
| `001` | Cons | Pointer to cons cell (aligned, subtract tag) |
| `010` | Heap object | Pointer to object header |
| `011` | Character | 21-bit Unicode codepoint + 40 bits unused |
| `100` | Single-float | 32-bit IEEE 754 in upper bits |
| `101` | Symbol (common) | Index into global symbol table |
| `110` | Function | Pointer to closure/compiled-fn header |
| `111` | Special | NIL, T, UNBOUND, immediate markers |

### 4.2  Garbage Collector (§3)

Generational, region-based, with concurrent old-gen marking:

- **Nursery:** Per-thread bump-pointer allocation (TLAB), ~2 MB.
  Minor GC is stop-the-world, copying into survivor space.
- **Old generation:** Region-based (2 MB regions). Concurrent
  tri-colour marking with Yuasa-style snapshot-at-the-beginning
  write barrier. Compaction via evacuation (region copying), like G1.
- **Large objects:** Allocated directly into old-gen dedicated regions.
  Never moved; freed in bulk when unmarked.

### 4.3  Thread Model (§2)

- One OS thread per hardware core (worker pool).
- M:N green threads scheduled cooperatively onto workers.
- CL `BORDEAUX-THREADS`-compatible API on top.
- Safepoints inserted at loop back-edges and function prologues
  for GC handshaking (no signal-based suspension).

### 4.4  Compiler IR (§4)

A sea-of-nodes SSA IR (like HotSpot C2 / Graal):

- Nodes: value-producing operations.
- Edges: data dependencies (def-use) + control dependencies.
- Optimisation passes: type propagation, inlining, escape analysis,
  loop-invariant code motion, dead code elimination, strength
  reduction.
- Lowering: IR → machine-specific nodes → register allocation
  (linear scan) → code emission (initially x86-64, then aarch64).

### 4.5  Self-Hosting Strategy

1. **Phase 1 (bootstrap):** Rust implements reader, eval, baseline
   compiler, GC, and enough stdlib to load CL source files.
2. **Phase 2 (cross-compile):** Optimising compiler written in CL,
   compiled by the baseline compiler.
3. **Phase 3 (self-host):** Bliss compiles its own compiler and
   growing portions of the runtime. Rust core shrinks to GC inner
   loops, FFI bridge, and platform glue.

## 5  Cross-Cutting Concerns

### 5.1  Error Handling

- Rust layers use `Result<T, BlissError>` — no panics in runtime code.
- CL layers use the full ANSI condition system (§5).
- FFI boundary converts between Rust `Result` and CL conditions.

### 5.2  Concurrency Safety

- No global interpreter lock.
- Packages are read-write locked (reader-writer lock per package).
- Symbol value cells use atomic operations.
- Hash tables offer both synchronized and unsynchronized variants.

### 5.3  Performance Monitoring

- Per-function invocation counters and back-edge counters.
- Tier-promotion decisions based on counter thresholds (configurable).
- Runtime emits JIT-dump files compatible with `perf` on Linux.

### 5.4  Image Format

A Bliss image (`.bimg`) is a memory-mapped snapshot:
- Header: magic, version, platform tag, GC metadata.
- Heap dump: serialised object graph (portable across same-arch).
- Symbol table, package registry, compiled code cache.
- Startup: `mmap` image, relocate pointers, resume from saved
  continuation.
