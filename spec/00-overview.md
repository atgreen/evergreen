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
| T0 | **Tree-walk interpreter** | First call | Direct AST eval |
| T1 | **Baseline compiler** | Call count ≥ 10 | Unoptimised native code |
| T2 | **Optimising compiler** | Hot loop / call count ≥ 5000 | Optimised native via IR |

Tier transitions are managed by the **Profiling Subsystem** (§4.5) which
inserts counters and traps, exactly as HotSpot's C1/C2 pipeline does.

### 1.2  Language Used per Layer

| Layer | Language | Rationale |
|-------|----------|-----------|
| Platform abstraction, GC inner loops, bootstrap reader/eval | Rust | Safety, performance, no runtime dependency |
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

## 3  Directory Structure (Source Tree)

```
bliss/
├── Cargo.toml              # workspace root
├── spec/                   # this specification
├── crates/
│   ├── bliss-rt/           # §2 runtime core (Rust)
│   │   ├── src/
│   │   │   ├── object.rs       # tagged pointer, object header
│   │   │   ├── heap.rs         # heap regions, allocation
│   │   │   ├── gc/             # §3 garbage collector
│   │   │   ├── thread.rs       # native thread + green thread
│   │   │   ├── ffi.rs          # C-ABI bridge
│   │   │   ├── signal.rs       # POSIX signal handling
│   │   │   └── startup.rs      # image load, init sequence
│   │   └── Cargo.toml
│   ├── bliss-compiler/     # §4 compiler pipeline (Rust bootstrap)
│   │   ├── src/
│   │   │   ├── reader.rs       # CL reader
│   │   │   ├── macroexpand.rs  # macro expansion
│   │   │   ├── ir.rs           # intermediate representation
│   │   │   ├── baseline.rs     # T1 baseline codegen
│   │   │   ├── opt.rs          # T2 optimising backend
│   │   │   └── osr.rs          # on-stack replacement
│   │   └── Cargo.toml
│   ├── bliss-stdlib/       # §5 standard library (CL sources)
│   │   ├── src/
│   │   │   ├── clos/
│   │   │   ├── conditions.lisp
│   │   │   ├── sequences.lisp
│   │   │   ├── streams.lisp
│   │   │   └── ...
│   │   └── bliss-stdlib.asd
│   └── bliss-cli/          # REPL + command-line driver
│       ├── src/main.rs
│       └── Cargo.toml
├── lib/                    # CL library code loaded at boot
│   ├── boot.lisp           # minimal bootstrap sequence
│   └── compiler.lisp       # self-hosted compiler (later)
└── tests/
    ├── ansi-test/          # git submodule → ansi-test suite
    ├── unit/               # per-module Rust tests
    └── integration/        # end-to-end CL test scripts
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
