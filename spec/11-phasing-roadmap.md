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
- The implementation MUST complete Stage 0 as defined in
  `spec/stages.json` and §0.4 before this phase is considered done.
- Scope is limited to the narrow vertical slice needed for the Stage 0
  gate: core datatypes, read/print round-trip for those datatypes,
  self-evaluating forms, `quote`, `if`, `progn`, and the primitive
  functions named by the Stage 0 gate.
- Supporting runtime work such as the object model, allocator, and REPL
  loop MAY be broader internally, but Phase 0 is accepted only by the
  Stage 0 gate, not by subsystem completeness claims.

**Milestone criterion:** the Stage 0 gate passes through the real
`torcl` binary: `torcl --eval "(+ 1 2)"` prints `3`, core datatypes
round-trip read→print unchanged, and a short nested arithmetic/list
script prints correct results.

**Dependencies:** None (pure Rust).

## 11.3  Phase 1 — Core Runtime

**Goal:** Enough runtime and stdlib to load CL source files and begin
writing the standard library in CL.

**R11.02** Deliverables:
- This phase aggregates Stage 1 and Stage 2 only after Stage 0 remains
  passing.
- Phase 1a is Stage 1: closures, recursion, non-local exits, and full
  lambda-list behaviour sufficient for the Stage 1 gate.
- Phase 1b is Stage 2: `defmacro`, backquote/unquote/splice,
  `macroexpand[-1]`, and the standard control/binding macros sufficient
  for the Stage 2 gate's real source-loading scenario.
- Runtime, package, stream, GC, and compiler work MAY advance in
  parallel, but no Phase 1 milestone may redefine acceptance more
  broadly than the Stage 1 and Stage 2 gates.

**Milestone criterion:** the Stage 1 gate and then the Stage 2 gate
pass through the real `torcl` binary with no regression of Stage 0.

**Dependencies:** Phase 0 complete.

## 11.4  Phase 2 — Self-Hosting Compiler

**Goal:** The optimising compiler is written in TorCL CL, compiled
by T1, and produces T2-quality code.

**R11.03** Deliverables:
- This phase aggregates Stage 3, Stage 4, and Stage 5 in order, while
  preserving all earlier gates.
- Phase 2a is Stage 3: data structures, sequence/library breadth,
  `FORMAT`, pathnames, and package behaviour needed by the Stage 3 gate.
- Phase 2b is Stage 4: conditions and CLOS needed by the Stage 4 gate.
- Phase 2c is Stage 5: tiered execution, profiling, inline caches, OSR,
  and deoptimisation needed by the Stage 5 gate.
- Compiler self-hosting work, image save/restore, and bootstrap
  refactoring MAY proceed within this phase only insofar as they do not
  bypass or replace the stage gates as acceptance criteria.

**Milestone criterion:** the Stage 3, Stage 4, and Stage 5 gates pass
in order through the real `torcl` binary with no regression of earlier
stages.

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
| C15 | Code emitter (x86-64) | `lib/compiler/emit-x86.lisp` | Requires C13, C14; emits bytes into CodeBuffer via Rust FFI to `crates/torcl-compiler/src/codegen/x86_64.rs` |
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
- Stage 6 MUST pass first: real ASDF loading through the real loader,
  followed by at least one real library call with a real result.
- Once Stage 6 is stable, this phase hardens the implementation for
  release: compliance, performance, embedding, tooling, sandboxing, and
  documentation.
- Production criteria MUST be interpreted as additions on top of the
  completed stage ledger, not as substitutes for it.

**Milestone criterion:** the Stage 6 gate passes through the real
`torcl` binary, earlier gates still pass, and the production-readiness
targets in §10 and §7 are met.

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

**R11.05** TorCL follows Semantic Versioning 2.0.0 for its public API
(C-ABI surface and `TORCL-EXT` package). Internal APIs (Rust crate
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
#+torcl-tier2
(load "lib/compiler/opt/pass-mgr.lisp")

;; Provide a stub when CLOS is not yet bootstrapped
#-torcl-clos
(defun make-instance (class &rest initargs)
  (error "CLOS not yet available — load clos/boot.lisp first"))
```

| Feature keyword | Pushed when |
|-----------------|-------------|
| `:torcl` | Always (identifies the implementation) |
| `:torcl-phase-0` | Phase 0 builds |
| `:torcl-phase-1` | Phase 1+ builds |
| `:torcl-phase-2` | Phase 2+ builds |
| `:torcl-phase-3` | Phase 3+ builds |
| `:torcl-tier2` | T2 compiler is loaded and functional |
| `:torcl-clos` | CLOS bootstrap is complete |
| `:torcl-unicode` | Full Unicode support is loaded (vs. ASCII-only bootstrap) |
| `:torcl-threads` | Thread subsystem is active |
| `:torcl-image` | Image save/restore is available |

**R11.14** Feature keywords MUST be pushed onto `*FEATURES*` at the
point where the subsystem is verified functional, not merely loaded.

**R11.15** All `#+torcl-*` / `#-torcl-*` conditionals in stdlib source
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

This section documents key differences between TorCL and SBCL to aid
users porting existing code.

### 11.12.1  Key Differences

| Area | SBCL | TorCL | Migration action |
|------|------|-------|------------------|
| Package for extensions | `SB-EXT`, `SB-THREAD`, etc. | `TORCL-EXT`, `TORCL-THREAD`, etc.; `TORCL` and `TORCL-THREADS` are deprecated compatibility spellings | Use compatibility package (see below) or `#+`/`#-` conditionals |
| Thread API | `sb-thread:make-thread` | `torcl-thread:make-thread` (BORDEAUX-THREADS compatible) | Use `bordeaux-threads` for portability |
| GC control | `(sb-ext:gc)`, `sb-ext:*gc-run-time*` | `(torcl-ext:gc)`, `torcl-ext:*gc-run-time*` | Rename calls |
| Compiler policy | `(declare (optimize (speed 3)))` | Same ANSI syntax; TorCL interprets values similarly | No change needed |
| `defglobal` | `sb-ext:defglobal` | `torcl-ext:defglobal` | Rename or use compat package |
| Image save | `sb-ext:save-lisp-and-die` | `torcl-ext:save-image` | Rename; keyword args differ (see §7) |
| Foreign calls | `sb-alien:define-alien-routine` | `torcl-ffi:define-foreign-function` | Rewrite FFI declarations |
| Weak pointers | `sb-ext:make-weak-pointer` | `torcl-ext:make-weak-pointer` | Rename |
| MOP | Full AMOP via `sb-mop` | Partial MOP via `torcl-mop` (Phase 2); full AMOP deferred | Test MOP usage; use `closer-mop` shim |
| `*posix-argv*` | `sb-ext:*posix-argv*` | `torcl-ext:*command-line-arguments*` | Rename |
| Exit | `sb-ext:exit` | `torcl-ext:exit` | Rename; compatible keyword args |

### 11.12.2  Compatibility Package

**R11.18** TorCL MUST provide a `TORCL-SBCL-COMPAT` package that
re-exports TorCL equivalents under SBCL symbol names for the most
commonly used SBCL extensions:

```lisp
(defpackage :torcl-sbcl-compat
  (:use :cl)
  (:export
   ;; sb-ext equivalents
   #:defglobal #:save-lisp-and-die #:exit #:quit
   #:*posix-argv* #:gc #:*gc-run-time*
   #:make-weak-pointer #:weak-pointer-value
   ;; sb-thread equivalents
   #:make-thread #:join-thread #:thread-alive-p
   #:make-mutex #:grab-mutex #:release-mutex #:with-mutex
   #:make-condition-variable #:condition-wait #:condition-notify
   #:condition-broadcast))
```

The compatibility package may additionally export deprecated aliases such as
`MAKE-WAITQUEUE`, but new examples use `MAKE-CONDITION-VARIABLE`.

### 11.12.3  Porting Checklist

1. Replace `#+sbcl` conditionals with `#+torcl` (or `#+(or sbcl torcl)`
   for code that should work on both).
2. Replace `sb-ext:`, `sb-thread:`, `sb-alien:` package prefixes with
   TorCL equivalents, or `(:use :torcl-sbcl-compat)`.
3. Audit FFI declarations — TorCL uses `torcl-ffi:define-foreign-function`
   with keyword syntax closer to CFFI than to `sb-alien`.
4. Test `defstruct` `:include` chains — TorCL Phase 2 supports them but
   layout may differ from SBCL (check `torcl-ext:struct-slot-offset`).
5. Replace `sb-mop` usage with `closer-mop` or `torcl-mop`; verify
   that only the supported MOP subset is used (see §5.3).
6. Test `LOOP` — TorCL implements ANSI `LOOP` exactly; some SBCL `LOOP`
   extensions (e.g., `LOOP FOR x ACROSS-OF-TYPE`) are not supported.
7. Run the application's test suite under TorCL with
   `(pushnew :sbcl *features*)` removed.
8. Check startup time — TorCL images are not identical to SBCL cores;
   rebuild via `torcl-ext:save-image`.

## 11.13  Deprecation Policy for Internal APIs

Internal APIs (Rust crate public interfaces, `TORCL-INTERNALS` package
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
; WARNING: TORCL-INTERNALS::%OLD-ALLOC is deprecated.
;          Use TORCL-INTERNALS::%REGION-ALLOC instead.
;          Will be removed in TorCL 1.5.
```

### 11.13.2  Scope

| API surface | Covered by policy | Notes |
|-------------|------------------|-------|
| `COMMON-LISP` package | N/A — ANSI standard, never deprecated | |
| `TORCL` public extensions | YES — full deprecation lifecycle | |
| `TORCL-INTERNALS` | Best-effort notice; MAY be removed in any minor release before 1.0 | |
| Rust crate public items | YES after 1.0; unstable before 1.0 | |
| C-ABI (`libtorcl.so`) | YES — full lifecycle; minimum 3 minor releases | Longest window due to ABI stability expectations |
| Image format | Breaking changes require major version bump after 1.0 | Images are not forward-compatible |

**R11.21** The `TORCL` package MUST maintain a machine-readable
deprecation registry accessible via
`(torcl:deprecated-symbols)` → list of `(symbol replacement removal-version)`.

## 11.14  Phase Exit Gates

Phase names and milestone narratives are useful, but later phases also
need an explicit “ready to advance” contract. The gates in this section
apply in addition to the phase descriptions above.

**R11.22** A phase MUST NOT be declared complete until its required
functional deliverables, its mandatory tests, and its stated operating
profiles all pass together on every tier-1 platform for that phase.

**R11.23** Advancement from Phase 0 to Phase 1, Phase 1 to Phase 2, and
Phase 2 to Phase 3 MUST be recorded by a release checklist artifact in
`spec/releases/` that links the satisfied requirements, unresolved
waivers, benchmark results, and known limitations.

**R11.24** When a phase gate is missed, the roadmap MUST prefer holding
the current phase and reducing scope for the next phase over silently
weakening already-published acceptance criteria.

### 11.14.1  Gate Checklist by Phase

| Transition | Mandatory gate items |
|------------|----------------------|
| Phase 0 → Phase 1 | Reader, evaluator, minimal GC, and REPL work; `cargo test` passes for all crates that exist; bootstrap examples load from source without an image |
| Phase 1 → Phase 2 | `LOAD`/`COMPILE-FILE`, packages, conditions, streams, threads, and T1 compile path are stable; bootstrap stdlib load works from `lib/boot.lisp`; runtime image can be created and reloaded |
| Phase 2 → Phase 3 | Triple-build verification passes; T2, OSR/deopt, CLOS, image save/load, and profiling are active; differential testing against SBCL and performance tracking are green |
| Phase 3 → Stable releases | `ansi-test` has zero unexpected failures; supported release artifacts are reproducible; C-ABI and extension packages are documented; security and portability gates pass |

### 11.14.2  Expected Release Artifacts per Phase

| Phase | Required artifacts |
|-------|--------------------|
| Phase 0 | Source checkout, `cargo test` logs, bootstrap transcript |
| Phase 1 | Bootstrap image, stdlib load transcript, crate-level test report |
| Phase 2 | Triple-build report, benchmark snapshot, self-compile transcript |
| Phase 3 | Full release bundle from §7, benchmark report, compatibility notes, security review summary |

## 11.15  Stage Roadmap Within the Phases

The broad implementation phases in §11.2–§11.5 are delivery epochs.
Within them, progress is controlled by the finer-grained **stages** in
`spec/stages.json`. These stages are the normative sequence for
day-to-day implementation and acceptance, because they correspond to the
smallest end-to-end slices a user can actually run.

### 11.15.1  Stage-to-Phase Mapping

| Stage | Name | Governing phase | Primary chapters in scope |
|-------|------|-----------------|---------------------------|
| 0 | Read, print, evaluate | Phase 0 | §1, §4.1, bootstrap printing portions of §5.9 |
| 1 | Core evaluator | Phase 0 / early Phase 1 | §2, evaluator-facing parts of §4.3 |
| 2 | Macros | Phase 1 | §4.2 and loader/bootstrap support across §2 and §5 |
| 3 | Data and library breadth | Phase 1 / Phase 2 | §5.1, §5.5, §5.6, §5.7 |
| 4 | Conditions and CLOS | Phase 2 | §5.3, §5.4 |
| 5 | HotSpot-inspired engine | Phase 2 | §3, §4.4–§4.9, §13 |
| 6 | Real-world payload and self-hosting | Phase 3 entry gate | §6, §7, §8, §9, later §11 |

This mapping is intentionally overlapping: a phase may contain multiple
stages, and a subsystem chapter may contribute requirements to more
than one stage. The stage manifest, not the phase narrative alone,
determines what is presently in scope for acceptance gating.

### 11.15.2  Stage Advancement Rules

**R11.25** Stages MUST advance strictly in numeric order from 0 through
6. A later stage's work MAY land behind flags or inactive paths, but its
gate MUST NOT be considered complete before every earlier stage gate is
passing.

**R11.26** The authoritative record of current implementation scope MUST
be `spec/stages.json`. Human-readable roadmap text, release notes, CI
configuration, and traceability reports MUST remain consistent with that
manifest.

**R11.27** When a requirement in a multi-stage file is intended to enter
scope before the rest of that file, it MUST be tagged inline with its
stage by appending a trailing suffix (`[S0]` ... `[S6]`) per
`spec/conventions.md`; otherwise it
inherits the file-level stage assignment from `spec/stages.json`.

**R11.28** Stage advancement MUST be conservative: if the nominal stage
payload partially works but its published gate is still incomplete, the
project MUST remain on the earlier stage and report partial progress as
intermediate implementation status rather than as a completed stage.

### 11.15.3  Stage Gate Ledger

The stage ledger below restates the current gate contract in the human
roadmap so that planning, acceptance, and release notes all refer to the
same gates.

| Stage | Gate summary |
|-------|--------------|
| 0 | `torcl --eval "(+ 1 2)"` prints `3`; core datatypes round-trip read→print; a nested arithmetic/list script prints correct results |
| 1 | Recursive factorial/Fibonacci, higher-order list processing, closures with captured state, and non-local exits run correctly through the CLI |
| 2 | A real multi-form `.lisp` file that defines and uses its own macros plus standard macros loads and runs to a correct result |
| 3 | A library-heavy program using strings/sequences/hash tables/formatted output produces correct observable results |
| 4 | Programs using user-defined classes/generic functions and condition handling/restarts run correctly end-to-end |
| 5 | A hot loop is observably promoted through tiers with identical results at each tier; invalidated speculation deoptimizes and still returns the correct result |
| 6 | Real ASDF loads without special-casing, and a real library call such as `(uiop:getenv "PATH")` returns the process PATH |

### 11.15.4  Relationship to Phase Exit Gates

**R11.29** Phase exit gates in §11.14 MUST be interpreted as aggregate
checkpoints over the stage ledger, not as permission to skip unfinished
earlier stages. If a phase-level checklist item depends on an earlier
stage gate, failure of that stage gate blocks the phase exit.

**R11.30** Release artifacts, milestone reports, and acceptance
transcripts SHOULD identify both the broad phase and the exact stage
that was current when the evidence was produced, so that claims about
project maturity remain unambiguous.
