# TorCL Specification — Master Index

**Version:** 0.1-draft
**Last updated:** 2026-08-11

This index provides at-a-glance navigation across all specification
chapters, sections, and subsections. Code references spec sections
as `§N.M`; requirements as `R N.xx`; data structures as `D N.xx`;
algorithms as `A N.xx`. See `conventions.md` for full notation.

## Chapters

| File | § | Title | Key Topics |
|------|---|-------|------------|
| `00-overview.md` | §0 | Scope & Goals | Goals G1–G8, architecture diagram, design decisions, directory layout |
| `01-object-model.md` | §1 | Object Model | TorclVal tagged pointers, type lattice, heap layouts, immediates |
| `02-runtime-core.md` | §2 | Runtime Core | Thread model, stack layout, safepoints, FFI, signals, startup |
| `03-memory-gc.md` | §3 | Memory & GC | Nursery/old-gen, TLABs, concurrent marking, compaction, finalization |
| `04-compiler.md` | §4 | Compiler Pipeline | Overview + 10 sub-chapters (§4.1–§4.10) |
| `05-stdlib.md` | §5 | Standard Library | Overview + 7 companion files spanning §5.1–§5.9 |
| `06-devtools.md` | §6 | Developer Tools | REPL, debugger, profiler, SLIME/SLY protocol |
| `06-11-bfasl.md` | §6.11 | TorCL FASL format | `.bfasl` portable compiled-artifact format (R6.60–R6.70) |
| `07-ops-portability.md` | §7 | Ops & Portability | Image format, deployment, platform matrix, build, logging |
| `08-security-robustness.md` | §8 | Security & Robustness | Sandbox, safe FFI, resource limits, reader hardening, fuzzing |
| `09-extensions.md` | §9 | SBCL-Compatible Extensions | Adopted extensions, rationale, compatibility mapping |
| `10-testing-validation.md` | §10 | Testing & Validation | ANSI test suite, benchmarking, CI, fuzzing strategy |
| `11-phasing-roadmap.md` | §11 | Phasing & Roadmap | Bootstrap phases, milestone criteria, self-hosting path |
| `12-glossary.md` | §12 | Glossary & Cross-Reference | Term glossary, D/A/R ID index, goal traceability |
| `13-concurrency.md` | §13 | Concurrency Model | Memory model, lock ordering, green threads, safepoints |
| `14-simd.md` | §14 | SIMD | SIMD packs, `TORCL-SIMD*` packages, instruction-set dispatch, image portability (R14.01–R14.55) |

## Compiler Sub-Chapters (§4)

| File | § | Title | Requirements |
|------|---|-------|--------------|
| `04-01-reader.md` | §4.1 | CL Reader | R4.01–R4.09 |
| `04-02-macroexpand.md` | §4.2 | Macro Expansion | R4.10–R4.16 |
| `04-03-ir.md` | §4.3 | Intermediate Representation | R4.17–R4.22 |
| `04-04-tiered.md` | §4.4 | Tiered Compilation | R4.23–R4.30 |
| `04-05-optimisation.md` | §4.5 | Optimisation Passes | R4.31–R4.37 |
| `04-06-osr.md` | §4.6 | OSR & Deoptimisation | R4.38–R4.41 |
| `04-07-codegen.md` | §4.7 | Code Emission & Register Allocation | R4.42–R4.47 |
| `04-08-inline-caches.md` | §4.8 | Inline Caches | R4.48–R4.52 |
| `04-09-profiling.md` | §4.9 | Profiling Infrastructure | R4.53–R4.58 |
| `04-10-t2-frame-state.md` | §4.10 | T2 Frame State & Deopt-Preserving Optimisation | R4.59–R4.68 |

## Standard Library Sub-Chapters (§5)

| File | § | Title | Requirements |
|------|---|-------|--------------|
| `05-01-packages-bootstrap.md` | §5.1 | Package System | R5.51–R5.58 |
| `05-02-clos.md` | §5.3 | CLOS | R5.66–R5.79 |
| `05-03-conditions.md` | §5.4 | Condition System | R5.91–R5.110, R5.203 |
| `05-04-streams.md` | §5.5 | Streams | R5.111–R5.130 |
| `05-05-sequences-hashtables.md` | §5.6–§5.7 | Sequences & Hash Tables | R5.131–R5.155 |
| `05-06-format-printer.md` | §5.9 | FORMAT & Pretty-Printer | R5.156–R5.180 |
| `05-07-pathnames.md` | §5.8 | Pathnames | R5.181–R5.202 |

## Requirement ID Ranges

| Chapter | Range | Count (approx.) |
|---------|-------|-----------------|
| §1 Object Model | R1.01–R1.xx | ~20 |
| §2 Runtime Core | R2.01–R2.xx | ~25 |
| §3 Memory & GC | R3.01–R3.xx | ~20 |
| §4 Compiler | R4.01–R4.68 | ~68 |
| §5 Standard Library | R5.01–R5.203 | ~60 |
| §6 Developer Tools | R6.01–R6.09 | ~15 |
| §7 Ops & Portability | R7.01–R7.18 | ~18 |
| §8 Security | R8.01–R8.15 | ~15 |
| §9 Extensions | R9.01–R9.xx | (new) |
| §10 Testing | R10.01–R10.xx | (new) |
| §11 Phasing | R11.01–R11.xx | (new) |

## Cross-Cutting Concern Map

| Concern | Primary § | Also appears in |
|---------|-----------|-----------------|
| Thread safety | §2 | §3 (GC safepoints), §5.1 (packages), §5.5 (streams), §8 |
| Error handling | §5.4 (conditions) | §2 (signals→conditions), §4.1 (READER-ERROR), §8 |
| Performance | §4.4 (tiered), §4.9 (profiling) | §3 (GC pauses), §10 (benchmarks) |
| ANSI compliance | §0 (G1) | §5 (978 symbols), §10 (ansi-test) |
| Security | §8 | §2 (FFI), §4.1 (reader hardening) |
| Self-hosting | §0 (§4.5) | §11 (phasing roadmap) |
