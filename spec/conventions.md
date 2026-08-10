# Spec Conventions

## Numbering

- Chapters use integers: §1, §2, …
- Sections: §1.1, §1.2, …
- Subsections: §1.1.1, §1.1.2, …
- Requirements use the prefix **R** followed by chapter and sequence:
  `R1.01`, `R1.02`, `R2.01`, etc.
- Data structures use the prefix **D**: `D1.01` (BlissVal), `D1.02` (ObjectHeader).
- Algorithms use the prefix **A**: `A3.01` (minor GC copy), `A3.02` (concurrent mark).

## Cross-references

- Refer to other spec sections as `§N.M`.
- Refer to source files as `crates/bliss-rt/src/object.rs` (repo-relative).
- Refer to requirements as `R1.01` etc.

## Requirement levels

Use RFC 2119 keywords: MUST, MUST NOT, SHOULD, SHOULD NOT, MAY.

## Tables

Use Markdown tables for structured enumerations (tag layouts, enum
variants, configuration knobs). Prefer tables over prose for anything
with ≥3 parallel fields.

## Code examples

- Rust: fenced with ```rust
- Common Lisp: fenced with ```lisp
- Assembly / pseudo-code: fenced with ```asm or ```text

## Chapter structure template

Each chapter file should follow:

1. **Title & scope** — one-paragraph summary.
2. **Requirements** — numbered Rn.xx list.
3. **Data structures** — key types, layouts, invariants.
4. **Algorithms & control flow** — pseudocode or prose for non-trivial logic.
5. **Error handling** — failure modes, recovery strategies.
6. **Concurrency** — thread-safety contracts, lock ordering.
7. **Configuration** — tunables, defaults, environment variables.
8. **Test strategy** — how to verify this subsystem.
