# §11  Phasing & Roadmap

**Scope:** Implementation phasing from bootstrap to self-hosting, with
milestone definitions, dependency ordering, and success criteria for
each phase.

## 11.1  Phase Overview

```
Phase 0       Phase 1          Phase 2            Phase 3
Bootstrap     Core Runtime     Self-Hosting        Production
(Rust only)   (Rust + CL)      Compiler            Ready
──────────────────────────────────────────────────────────────→
  reader        stdlib           T2 optimiser        ansi-test
  T0 eval       T1 codegen       CLOS MOP            cl-bench
  GC nursery    GC full          image save          SLIME/SLY
  object model  conditions       OSR/deopt           embeddable
  tagged values threads          self-compile        stable API
```

## 11.2  Phase 0 — Bootstrap (Rust Only)

**Goal:** A minimal Lisp that can read, evaluate, and print CL forms.

**R11.01** Deliverables:
- CL reader (`crates/bliss-compiler/src/reader.rs`) — full §4.1
- Tree-walk interpreter (T0) — all 25 special operators
- Object model — BlissVal, cons, symbols, strings, fixnums, floats
- Nursery GC — bump-pointer TLAB allocation, stop-the-world minor GC
- REPL — read-eval-print loop with basic error recovery
- Minimal built-in functions: `CAR`, `CDR`, `CONS`, `EQ`, `+`, `-`,
  `*`, `/`, `<`, `PRINT`, `READ`, `EVAL`, `APPLY`

**Milestone criterion:** `(defun fib (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))` computes `(fib 30)` correctly.

**Dependencies:** None (pure Rust).

## 11.3  Phase 1 — Core Runtime

**Goal:** Enough runtime and stdlib to load CL source files and begin
writing the standard library in CL.

**R11.02** Deliverables:
- Full GC (nursery + old-gen concurrent marking) — §3
- Thread pool + green thread scheduler — §2
- Safepoint infrastructure — §2
- FFI bridge (libffi integration) — §2
- T1 baseline compiler — §4.4 (unoptimised native code)
- Package system — §5.1
- Condition system (bootstrap, Rust-backed) — §5.3
- Basic streams (file-stream, string-stream) — §5.4
- `LOAD` / `COMPILE-FILE` working for `.lisp` sources
- `DEFMACRO`, `MACROLET`, backquote — §4.2

**Milestone criterion:** Bliss can `(load "lib/boot.lisp")` which
defines ~200 stdlib functions in CL, and those functions work correctly.

**Dependencies:** Phase 0 complete.

## 11.4  Phase 2 — Self-Hosting Compiler

**Goal:** The optimising compiler is written in Bliss CL, compiled
by T1, and produces T2-quality code.

**R11.03** Deliverables:
- Sea-of-nodes IR builder (CL) — §4.3
- Optimisation passes (CL) — §4.5
- Register allocator and code emitter (CL + Rust codegen backend) — §4.7
- Inline caches — §4.8
- Profiling counters and tier-promotion logic — §4.9
- OSR and deoptimisation — §4.6
- CLOS (full ANSI, partial MOP) — §5.2
- FORMAT and pretty-printer — §5.6
- Sequences and hash tables (CL fast paths) — §5.5
- Image save/restore — §7

**Milestone criterion:** Bliss compiles its own compiler source with T2
and the resulting code passes all existing tests. Self-compiled compiler
produces identical output to cross-compiled version (bootstrap
verification).

**Dependencies:** Phase 1 complete.

### 11.4.1  Compiler Bootstrap Sequence

The T2 optimising compiler is implemented in CL and bootstrapped in a
strict order dictated by inter-pass dependencies. Each pass is compiled
by T1 (baseline) initially; once all passes exist, the T2 compiler
self-compiles (see §11.9).

**A11.01 — Compiler Pass Implementation Order:**

| Step | Pass / Module | File | Rationale |
|------|---------------|------|-----------|
| C1 | IR node types & graph core | `lib/compiler/ir.lisp` | Foundation — all subsequent passes operate on the IR graph |
| C2 | IR graph builder (AST → IR) | `lib/compiler/ir-build.lisp` | Requires C1; produces the graphs that passes transform |
| C3 | IR graph verifier | `lib/compiler/verify.lisp` | Requires C1; validates IR invariants — essential for debugging subsequent passes |
| C4 | Constant folding | `lib/compiler/opt/fold.lisp` | Simplest optimisation; exercises the IR mutation API; validates C1–C3 end-to-end |
| C5 | Dead code elimination | `lib/compiler/opt/dce.lisp` | Requires C1 (def-use chains); needed before type propagation to reduce noise |
| C6 | Type propagation | `lib/compiler/opt/type-prop.lisp` | Requires C1, C5; provides type info consumed by all downstream passes (inlining, escape analysis, unboxing) |
| C7 | Inlining | `lib/compiler/opt/inline.lisp` | Requires C2 (to build callee IR), C6 (type-guided decisions); inlining opens opportunities for C8–C10 |
| C8 | Escape analysis | `lib/compiler/opt/escape.lisp` | Requires C6 (type info); benefits from C7 (inlined code exposes local allocations) |
| C9 | Loop analysis & LICM | `lib/compiler/opt/licm.lisp` | Requires C6; back-edge detection must precede strength reduction |
| C10 | Strength reduction | `lib/compiler/opt/strength.lisp` | Requires C9 (loop structure); reduces induction-variable arithmetic |
| C11 | Null-check elimination | `lib/compiler/opt/null-elim.lisp` | Requires C6 (type proof of non-null); runs late to exploit accumulated type info |
| C12 | Pass manager & ordering | `lib/compiler/opt/pass-mgr.lisp` | Orchestrates C4–C11 in the fixed order defined by §4.5 (A4.02) |
| C13 | Lowering (IR → MachNode) | `lib/compiler/lower.lisp` | Requires C1; produces machine-specific nodes consumed by C14 |
| C14 | Register allocator | `lib/compiler/regalloc.lisp` | Requires C13; linear-scan over SSA live ranges |
| C15 | Code emitter (x86-64) | `lib/compiler/emit-x86.lisp` | Requires C13, C14; emits bytes into CodeBuffer via Rust FFI to `crates/bliss-compiler/src/codegen/x86_64.rs` |
| C16 | Code emitter (AArch64) | `lib/compiler/emit-aarch64.lisp` | Requires C13, C14; secondary backend, MAY lag C15 |
| C17 | Inline cache runtime | `lib/compiler/ic.lisp` | Requires C15 (patchable call sites); wires IC state machine (§4.8) |
| C18 | Profiling & tier promotion | `lib/compiler/profile.lisp` | Requires C15, C17; installs counters and triggers tier transitions (§4.9) |
| C19 | OSR entry & deoptimisation | `lib/compiler/osr.lisp` | Requires C14 (stack maps), C18 (back-edge counters); the most complex pass — deferred to last |

**R11.06** Each step Cn MUST be testable independently by compiling
a small CL function through the Rust T1 and verifying the output.
Regression tests MUST be added before proceeding to Cn+1.

**R11.07** Steps C1–C12 (IR + optimisation passes) MUST be completed
before C13 (lowering), because the pass manager (C12) is the contract
between the optimiser and the backend.

### 11.4.2  Stdlib Bootstrap Dependency DAG

The standard library has a strict dependency ordering. The following DAG
shows which CL subsystems must be loaded before others. An arrow A → B
means "A must be fully loaded before B can load."

```
                    ┌───────────────────┐
                    │ Rust Primitives   │  (§5.2.1: cons, car, cdr, eq,
                    │ (Phase 1)         │   +, -, typep, defmacro, intern,
                    └────────┬──────────┘   eval, apply, read, print, ...)
                             │
                    ┌────────▼──────────┐
                    │ boot-macros.lisp  │  and, or, when, unless, cond,
                    │ + backquote       │  push, pop, setf (basic), loop
                    └────────┬──────────┘  (simple), destructuring-bind
                             │
              ┌──────────────┼──────────────┐
              │              │              │
     ┌────────▼─────┐ ┌─────▼──────┐ ┌─────▼──────┐
     │ setf.lisp    │ │ defstruct  │ │ list-ops   │
     │ define-setf- │ │ -full.lisp │ │ .lisp      │
     │ expander     │ │ :include   │ │ mapcar,    │
     └────────┬─────┘ │ BOA ctors  │ │ assoc, ... │
              │       └─────┬──────┘ └─────┬──────┘
              └──────────────┼──────────────┘
                             │
                    ┌────────▼──────────┐
                    │ type-system.lisp  │  subtypep, typep (full),
                    │                   │  deftype, type specifiers
                    └────────┬──────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
     ┌────────▼─────┐ ┌─────▼──────┐ ┌─────▼──────────┐
     │ sequences    │ │ hash-table │ │ clos/boot.lisp │
     │ .lisp        │ │ -ext.lisp  │ │ proto-classes, │
     │ map, reduce, │ │ rehash,    │ │ standard-class │
     │ sort, find   │ │ iterator   │ │ wire-up        │
     └────────┬─────┘ └─────┬──────┘ └─────┬──────────┘
              │              │              │
              │              │    ┌─────────▼──────────┐
              │              │    │ clos/generic.lisp  │
              │              │    │ defgeneric,        │
              │              │    │ defmethod, dispatch│
              │              │    └─────────┬──────────┘
              │              │              │
              │              │    ┌─────────▼──────────┐
              │              │    │ clos/slots.lisp    │
              │              │    │ slot-value,        │
              │              │    │ slot-access proto  │
              │              │    └─────────┬──────────┘
              │              │              │
              │              │    ┌─────────▼──────────┐
              │              │    │ clos/combination   │
              │              │    │ .lisp — method     │
              │              │    │ combination types  │
              │              │    └─────────┬──────────┘
              │              │              │
              │              │    ┌─────────▼──────────┐
              │              │    │ clos/change.lisp   │
              │              │    │ change-class,      │
              │              │    │ class redefinition  │
              │              │    └─────────┬──────────┘
              │              │              │
              └──────────────┼──────────────┘
                             │
                    ┌────────▼──────────┐
                    │ conditions.lisp   │  Requires CLOS (conditions are
                    │ handler-bind,     │  CLOS instances — R5.45)
                    │ restarts, signal  │
                    └────────┬──────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
     ┌────────▼─────┐ ┌─────▼──────┐ ┌─────▼──────┐
     │ streams.lisp │ │ pathnames  │ │ format-    │
     │ Gray streams │ │ .lisp      │ │ full.lisp  │
     │ file-stream  │ │ pathname,  │ │ all FORMAT │
     └────────┬─────┘ │ logical    │ │ directives │
              │       └─────┬──────┘ └─────┬──────┘
              │              │              │
              └──────────────┼──────────────┘
                             │
                    ┌────────▼──────────┐
                    │ printer.lisp      │  Requires FORMAT + streams
                    │ pprint-dispatch,  │
                    │ print-object      │
                    └────────┬──────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
     ┌────────▼─────┐ ┌─────▼──────────────▼──────┐
     │ loop-full    │ │ environment.lisp          │
     │ .lisp        │ │ describe, inspect,        │
     │ extended LOOP│ │ documentation             │
     └──────────────┘ └───────────────────────────┘
```

**R11.08** The boot loader (`lib/boot.lisp`) MUST load files in an
order consistent with this DAG. Any violation MUST be caught at load
time with a clear diagnostic naming the missing dependency.

## 11.5  Phase 3 — Production Ready

**Goal:** ANSI-compliant, performant, embeddable, documented.

**R11.04** Deliverables:
- Pass `ansi-test` with zero unexpected failures — §10
- `cl-bench` within 2× of SBCL on reference hardware — §10
- Startup < 50 ms — G3
- SLIME/SLY support — §6
- `libbliss.so` with stable C-ABI — G8
- Sandbox mode — §8
- Full pathname system — §5.7
- Gray streams — §5.4
- SBCL-compatible extensions — §9
- Documentation: user guide, embedding guide, internals guide

**Milestone criterion:** A non-trivial application (e.g., a web server
using Hunchentoot or a subset) runs correctly on Bliss.

**Dependencies:** Phase 2 complete.

## 11.6  Bootstrap Dependency Graph

```
                    ┌─────────────┐
                    │ Phase 0     │
                    │ reader, T0  │
                    │ nursery GC  │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
              ┌─────┤ Phase 1     ├─────┐
              │     │ T1, GC full │     │
              │     │ packages    │     │
              │     └──────┬──────┘     │
              │            │            │
       ┌──────▼──┐  ┌──────▼──┐  ┌──────▼──────┐
       │ stdlib  │  │ threads │  │ conditions  │
       │ (CL)   │  │ FFI     │  │ streams     │
       └────┬────┘  └────┬────┘  └──────┬──────┘
            │            │             │
            └────────────┼─────────────┘
                         │
                  ┌──────▼──────┐
                  │ Phase 2     │
                  │ T2 compiler │
                  │ CLOS, IC    │
                  │ OSR, image  │
                  └──────┬──────┘
                         │
                  ┌──────▼──────┐
                  │ Phase 3     │
                  │ ansi-test   │
                  │ perf tuning │
                  │ SLIME, docs │
                  └─────────────┘
```

## 11.7  Risk Mitigation

| Risk | Impact | Mitigation |
|------|--------|------------|
| GC pause targets not met | G5 violated | Prototype GC early; benchmark continuously (§10) |
| Self-hosting compiler bugs | Phase 2 blocked | Maintain Rust T1 as fallback; differential testing |
| ANSI edge cases | G1 delayed | Run `ansi-test` from Phase 1 onward; fix continuously |
| aarch64 codegen complexity | G8 delayed | Prioritise x86-64; aarch64 as second backend |
| MOP compatibility | Libraries fail | Provide Closer-MOP shim; defer full AMOP to v2 |
| Startup time regression | G3 violated | Measure in CI; image format optimisation (§7) |

## 11.8  Version Numbering

| Version | Corresponds to | Stability |
|---------|---------------|-----------|
| 0.1.x | Phase 0 builds | Experimental |
| 0.2.x–0.4.x | Phase 1 builds | Alpha |
| 0.5.x–0.9.x | Phase 2 builds | Beta |
| 1.0.0 | Phase 3 complete | Stable |

**R11.05** Bliss follows Semantic Versioning 2.0.0 for its public API
(C-ABI surface and `BLISS-EXT` package). Internal APIs (Rust crate
interfaces) use workspace-level versioning without stability guarantees
before 1.0.0.

## 11.9  Bootstrap Verification Protocol

Self-hosting correctness is verified via a **triple-build protocol**
modeled on GCC's bootstrap verification. The protocol proves that the
compiler is a fixed point — compiling itself produces identical output.

### 11.9.1  Triple-Build Stages

```
Stage 1: Rust T1 compiles CL compiler sources → CL₁ (the first CL-compiled compiler)
Stage 2: CL₁ compiles CL compiler sources    → CL₂ (compiler compiled by itself, round 1)
Stage 3: CL₂ compiles CL compiler sources    → CL₃ (compiler compiled by itself, round 2)

Verification: binary-compare(output(CL₂), output(CL₃)) MUST be identical
```

**R11.09** The triple-build MUST be automated via `make bootstrap-verify`
and MUST run in CI on every commit that modifies files under
`lib/compiler/`.

**R11.10** Stage 1 (Rust T1 → CL₁) output is NOT required to match
Stage 2 output, because T1 and CL₁ are different compilers. Only
CL₂ vs. CL₃ output must match (fixed-point proof).

### 11.9.2  Comparison Methodology

**R11.11** Binary comparison MUST compare the emitted machine code bytes
for every compiled function, excluding:
- Absolute addresses (relocations are compared symbolically).
- Timestamps and build metadata embedded in image headers.
- GC metadata layout (compared structurally, not byte-for-byte).

**R11.12** If CL₂ ≠ CL₃, the build MUST fail with a diff report listing
each function whose code differs, including a disassembly diff of the
first divergent function.

### 11.9.3  Failure Recovery

| Failure mode | Action |
|--------------|--------|
| CL₁ crashes during Stage 2 | Fall back to Rust T1; bisect CL compiler change |
| CL₂ ≠ CL₃ (non-determinism) | Audit for pointer-address-dependent code generation, hash table iteration order leaking into codegen, or uninitialised memory reads |
| CL₂ = CL₃ but tests fail | Compiler has a consistent bug — run differential testing against Rust T1 output |

## 11.10  Feature Flags and Conditional Compilation

During development, partially-implemented subsystems MUST be gated
behind feature flags to keep the main branch buildable and testable at
all times.

### 11.10.1  Rust Feature Flags (Cargo)

Feature flags in `Cargo.toml` gate Rust-side code:

| Feature flag | Default | Gates |
|--------------|---------|-------|
| `gc-concurrent` | off (Phase 0–1) | Old-gen concurrent marking (§3); nursery-only when off |
| `tier2` | off (Phase 0–1) | T2 optimising compiler backend; T0+T1 only when off |
| `aarch64-backend` | off | AArch64 code emission (§4.7); x86-64 only when off |
| `image-save` | off (Phase 0–1) | Image save/restore (§7); cold-start only when off |
| `sandbox` | off (Phase 0–2) | Sandbox mode (§8); unrestricted when off |
| `ffi` | on | FFI bridge; can be disabled for minimal/embedded builds |

**R11.13** Cargo features MUST be additive — enabling a feature MUST NOT
break code that compiles without it. The `default` feature set MUST
correspond to the current phase's baseline.

### 11.10.2  CL Feature Flags (*FEATURES*)

CL-side feature gating uses standard `*FEATURES*` and reader conditionals:

```lisp
;; Only load T2 passes when the T2 feature is active
#+bliss-tier2
(load "lib/compiler/opt/pass-mgr.lisp")

;; Provide a stub when CLOS is not yet bootstrapped
#-bliss-clos
(defun make-instance (class &rest initargs)
  (error "CLOS not yet available — load clos/boot.lisp first"))
```

| Feature keyword | Pushed when |
|-----------------|-------------|
| `:bliss` | Always (identifies the implementation) |
| `:bliss-phase-0` | Phase 0 builds |
| `:bliss-phase-1` | Phase 1+ builds |
| `:bliss-phase-2` | Phase 2+ builds |
| `:bliss-phase-3` | Phase 3+ builds |
| `:bliss-tier2` | T2 compiler is loaded and functional |
| `:bliss-clos` | CLOS bootstrap is complete |
| `:bliss-unicode` | Full Unicode support is loaded (vs. ASCII-only bootstrap) |
| `:bliss-threads` | Thread subsystem is active |
| `:bliss-image` | Image save/restore is available |

**R11.14** Feature keywords MUST be pushed onto `*FEATURES*` at the
point where the subsystem is verified functional, not merely loaded.

**R11.15** All `#+bliss-*` / `#-bliss-*` conditionals in stdlib source
MUST be documented in a feature-flag registry file
(`lib/feature-flags.lisp`) listing each flag, its meaning, and when it
activates.

## 11.11  Performance Milestone Targets

Each phase has specific, measurable performance targets against
`cl-bench` (Boehm & Weiser benchmark suite) on **reference hardware**:
x86-64, 8-core, 32 GB RAM, Linux 6.x. SBCL 2.4.x on the same machine
is the baseline (1.0×).

### 11.11.1  Phase 0 Targets

| Benchmark | Target | Notes |
|-----------|--------|-------|
| `fib(35)` (recursive) | ≤ 50× SBCL | T0 interpreter; no compilation |
| `tak` | ≤ 50× SBCL | Function call overhead benchmark |
| Startup (hello-world) | ≤ 200 ms | Cold start, no image |
| GC minor pause | ≤ 10 ms | Nursery only |

### 11.11.2  Phase 1 Targets

| Benchmark | Target | Notes |
|-----------|--------|-------|
| `fib(35)` | ≤ 10× SBCL | T1 baseline native code |
| `tak` | ≤ 10× SBCL | T1 call overhead |
| `deriv` | ≤ 15× SBCL | Cons-heavy, tests GC + allocation |
| `traverse` | ≤ 15× SBCL | List traversal |
| `triangle` | ≤ 20× SBCL | Mostly T0 (complex control flow) |
| Startup | ≤ 100 ms | With boot.lisp load |
| GC minor pause (P99) | ≤ 5 ms | Nursery, improved |
| GC major pause (P99) | ≤ 50 ms | Concurrent mark; stop-the-world compact |
| Compilation latency (T1, 200-node fn) | ≤ 1 ms | Per R4.25 |

### 11.11.3  Phase 2 Targets

| Benchmark | Target | Notes |
|-----------|--------|-------|
| `fib(35)` | ≤ 3× SBCL | T2 optimised; inlining + unboxing |
| `tak` | ≤ 3× SBCL | T2 call overhead |
| `deriv` | ≤ 3× SBCL | Escape analysis reduces consing |
| `traverse` | ≤ 4× SBCL | T2 + type-specialised list ops |
| `triangle` | ≤ 5× SBCL | T2 loop optimisation |
| `boyer` | ≤ 4× SBCL | Symbolic computation |
| `browse` | ≤ 4× SBCL | Pattern matching, list ops |
| CLOS dispatch (1-arg) | ≤ 5× SBCL | IC mono path |
| Hash table insert (100k) | ≤ 3× SBCL | Robin Hood hashing |
| Startup (from image) | ≤ 80 ms | Image mmap load |
| GC major pause (P99) | ≤ 20 ms | Concurrent mark + incremental compact |

### 11.11.4  Phase 3 Targets (Release Quality)

| Benchmark | Target | Notes |
|-----------|--------|-------|
| cl-bench geometric mean | ≤ 2× SBCL | G2 — primary performance goal |
| `fib(35)` | ≤ 1.5× SBCL | Tight numeric loop |
| `tak` | ≤ 1.5× SBCL | Call overhead |
| `deriv` | ≤ 2× SBCL | Allocation + GC |
| `boyer` | ≤ 2× SBCL | Symbolic |
| CLOS dispatch (1-arg) | ≤ 2× SBCL | IC + compiled discriminator |
| CLOS dispatch (N-arg) | ≤ 3× SBCL | Multi-method cache |
| Hash table lookup (100k) | ≤ 1.5× SBCL | Hot cache |
| Startup (from image) | ≤ 50 ms | G3 |
| GC major pause (P99) | ≤ 10 ms | G5 — sub-10 ms target |

**R11.16** Performance targets MUST be tracked in CI via the benchmark
harness (§10). Any regression beyond 10% on a tracked benchmark MUST
block the merge and trigger investigation.

**R11.17** Benchmark results MUST be published with each phase release
as a performance report, including comparison against the previous phase
and against SBCL on the same hardware.

## 11.12  Migration Guide for SBCL Users

This section documents key differences between Bliss and SBCL to aid
users porting existing code.

### 11.12.1  Key Differences

| Area | SBCL | Bliss | Migration action |
|------|------|-------|------------------|
| Package for extensions | `SB-EXT`, `SB-THREAD`, etc. | `BLISS`, `BLISS-THREADS`, etc. | Use compatibility package (see below) or `#+`/`#-` conditionals |
| Thread API | `sb-thread:make-thread` | `bliss-threads:make-thread` (BORDEAUX-THREADS compatible) | Use `bordeaux-threads` for portability |
| GC control | `(sb-ext:gc)`, `sb-ext:*gc-run-time*` | `(bliss:gc)`, `bliss:*gc-run-time*` | Rename calls |
| Compiler policy | `(declare (optimize (speed 3)))` | Same ANSI syntax; Bliss interprets values similarly | No change needed |
| `defglobal` | `sb-ext:defglobal` | `bliss:defglobal` | Rename or use compat package |
| Image save | `sb-ext:save-lisp-and-die` | `bliss:save-image` | Rename; keyword args differ (see §7) |
| Foreign calls | `sb-alien:define-alien-routine` | `bliss-ffi:define-foreign-function` | Rewrite FFI declarations |
| Weak pointers | `sb-ext:make-weak-pointer` | `bliss:make-weak-pointer` | Rename |
| MOP | Full AMOP via `sb-mop` | Partial MOP via `bliss-mop` (Phase 2); full AMOP deferred | Test MOP usage; use `closer-mop` shim |
| `*posix-argv*` | `sb-ext:*posix-argv*` | `bliss:*command-line-arguments*` | Rename |
| Exit | `sb-ext:exit` | `bliss:exit` | Rename; compatible keyword args |

### 11.12.2  Compatibility Package

**R11.18** Bliss MUST provide a `BLISS-SBCL-COMPAT` package that
re-exports Bliss equivalents under SBCL symbol names for the most
commonly used SBCL extensions:

```lisp
(defpackage :bliss-sbcl-compat
  (:use :cl)
  (:export
   ;; sb-ext equivalents
   #:defglobal #:save-lisp-and-die #:exit #:quit
   #:*posix-argv* #:gc #:*gc-run-time*
   #:make-weak-pointer #:weak-pointer-value
   ;; sb-thread equivalents
   #:make-thread #:join-thread #:thread-alive-p
   #:make-mutex #:grab-mutex #:release-mutex #:with-mutex
   #:make-waitqueue #:condition-wait #:condition-notify))
```

### 11.12.3  Porting Checklist

1. Replace `#+sbcl` conditionals with `#+bliss` (or `#+(or sbcl bliss)`
   for code that should work on both).
2. Replace `sb-ext:`, `sb-thread:`, `sb-alien:` package prefixes with
   Bliss equivalents, or `(:use :bliss-sbcl-compat)`.
3. Audit FFI declarations — Bliss uses `bliss-ffi:define-foreign-function`
   with keyword syntax closer to CFFI than to `sb-alien`.
4. Test `defstruct` `:include` chains — Bliss Phase 2 supports them but
   layout may differ from SBCL (check `bliss:struct-slot-offset`).
5. Replace `sb-mop` usage with `closer-mop` or `bliss-mop`; verify
   that only the supported MOP subset is used (see §5.2).
6. Test `LOOP` — Bliss implements ANSI `LOOP` exactly; some SBCL `LOOP`
   extensions (e.g., `LOOP FOR x ACROSS-OF-TYPE`) are not supported.
7. Run the application's test suite under Bliss with
   `(pushnew :sbcl *features*)` removed.
8. Check startup time — Bliss images are not identical to SBCL cores;
   rebuild via `bliss:save-image`.

## 11.13  Deprecation Policy for Internal APIs

Internal APIs (Rust crate public interfaces, `BLISS-INTERNALS` package
symbols, and compiler intrinsics) are not covered by Semver guarantees
before 1.0.0. After 1.0.0, the following policy applies.

### 11.13.1  Deprecation Lifecycle

```
Active ──→ Deprecated ──→ Removed
           (N releases)    (N+2 releases minimum)
```

| Stage | Duration | Behaviour |
|-------|----------|-----------|
| **Active** | Indefinite | Fully supported; no warnings |
| **Deprecated** | Minimum 2 minor releases (e.g., 1.1 → 1.3) | Compile-time `STYLE-WARNING` emitted on use; runtime behaviour unchanged; docs marked `[DEPRECATED]` |
| **Removed** | After deprecation period | Symbol becomes unbound; `FUNCALL` signals `UNDEFINED-FUNCTION` |

**R11.19** Deprecation MUST be announced in release notes with:
- The deprecated API symbol(s).
- The recommended replacement.
- The earliest release in which removal may occur.

**R11.20** Deprecated functions MUST emit a `STYLE-WARNING` at
compile time (via compiler macro or declaration) containing the
replacement suggestion:

```lisp
;; Example deprecation warning
; WARNING: BLISS-INTERNALS::%OLD-ALLOC is deprecated.
;          Use BLISS-INTERNALS::%REGION-ALLOC instead.
;          Will be removed in Bliss 1.5.
```

### 11.13.2  Scope

| API surface | Covered by policy | Notes |
|-------------|------------------|-------|
| `COMMON-LISP` package | N/A — ANSI standard, never deprecated | |
| `BLISS` public extensions | YES — full deprecation lifecycle | |
| `BLISS-INTERNALS` | Best-effort notice; MAY be removed in any minor release before 1.0 | |
| Rust crate public items | YES after 1.0; unstable before 1.0 | |
| C-ABI (`libbliss.so`) | YES — full lifecycle; minimum 3 minor releases | Longest window due to ABI stability expectations |
| Image format | Breaking changes require major version bump after 1.0 | Images are not forward-compatible |

**R11.21** The `BLISS` package MUST maintain a machine-readable
deprecation registry accessible via
`(bliss:deprecated-symbols)` → list of `(symbol replacement removal-version)`.
