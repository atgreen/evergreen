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
