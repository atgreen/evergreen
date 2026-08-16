# §4.10 T2 Frame State & Deoptimisation-Preserving Optimisation

## 4.10.0 Scope

The T2 tier is specified across four chapters: §4.3 defines the SSA IR, §4.5 the
optimisation passes, §4.6 the OSR/deopt runtime mechanism, and §4.7 code emission
and register allocation. Each is necessary; none is sufficient on its own,
because they share a load-bearing invariant that no one of them owns.

A speculative optimiser has a second observer besides the program's output: the
**deoptimiser**. At every point where T2 code may fall back to the interpreter,
the runtime must reconstruct a *semantically equivalent interpreter frame*
(§4.6 A4.04). That is only possible if, for every such point, the compiler has
preserved a mapping from each interpreter-visible slot to a way of producing its
value — even after loop-invariant code motion, GVN, dead-code elimination,
unboxing, and register allocation have rewritten the fast path beyond
recognition.

This chapter specifies that mapping — the **FrameState** — as a first-class IR
artefact, the **deopt-preservation invariant** every pass obeys, the
**rematerialisation** mechanism that lets dead-on-the-fast-path values still be
reconstructed on deopt, and the incremental build order for T2. FrameState is the
IR-level source of truth; the §4.6 GC stack map (A4.04) and OSR entry map (D4.09)
are its *lowered projection*, produced by the register allocator (§4.7).

The rule to hold in mind throughout: **the optimiser may transform the fast path
arbitrarily, provided it can still reconstruct a valid interpreter state at every
deopt point.** Everything below is the machinery that makes that checkable rather
than hoped-for.

---

## 4.10.1 Requirements

| ID | Requirement | Level |
|----|-------------|-------|
| R4.59 | Every deoptimising IR node — speculative guard (R4.21), checked/unboxed arithmetic that may trap on overflow or division, assumption node (§4.10.6), and any call or safepoint that may deopt — MUST carry a **FrameState** (D4.15) describing the interpreter state to reconstruct at the corresponding bytecode position. | MUST |
| R4.60 | **Deopt-preservation invariant.** Every optimisation pass MUST keep every FrameState valid: for each interpreter slot it maps, the slot's `ValueSource` MUST remain a constant, a rematerialisation recipe, `Unbound`, or an SSA value that dominates the deoptimising node. A value referenced by any FrameState is **deopt-live** and MUST NOT be deleted unless the pass first substitutes an equivalent `Const`, `Remat`, or dominating SSA value into every FrameState that referenced it. | MUST |
| R4.61 | Every SSA value MUST carry a **ValueRepresentation** (D4.16). A representation-changing operation (box, unbox, widen) MUST be an explicit IR node; representation MUST never change implicitly along an edge. Type (the CL type lattice, D4.08) and representation are distinct: a value may be `fixnum`-typed in either `Tagged` or `UnboxedFixnum` representation. | MUST |
| R4.62 | A value whose only remaining uses are inside FrameStates MAY be **rematerialised** (A4.13): DCE MUST NOT treat a deopt-only use as pinning the computation to the fast path when a rematerialisation recipe (D4.17) exists; it MUST instead replace the FrameState uses with a `Remat` source and remove the fast-path computation. Only operations the compiler has proven pure, total, and cheap are rematerialisable; anything else stays deopt-live. | MUST |
| R4.63 | **Speculative narrowing.** When the type inferencer (§4.5, R4.31) narrows a value from profiling feedback (§4.9), it MUST insert exactly one guard at the earliest point that dominates all speculative uses (typically a loop preheader), and that guard's FrameState MUST record the *pre-narrowing* `ValueSource` (the still-boxed, un-narrowed value) so deopt reconstructs the value the interpreter expects. Per-operation re-guarding of an already-narrowed value is a defect. | MUST |
| R4.64 | The IR verifier (A4.07) MUST reject a FrameState whose local/stack slot counts disagree with the source bytecode's abstract frame at its `bcp`, that references a non-dominating SSA value, that assigns a representation a slot's lowering cannot produce, or whose rematerialisation recipe is cyclic. | MUST |
| R4.65 | FrameState MUST lower (A4.14) to the §4.6 GC stack map consumed by A4.04 (deopt) and to the OSR entry map (D4.09) consumed by A4.03 (OSR). The register allocator (§4.7) MUST bind every `Value` source to a concrete `Location` and emit a rebox sequence for every unboxed representation, or emit the `Remat` recipe into the cold path; a FrameState that cannot be fully lowered MUST fail compilation (the function stays at T1, R4.28), never install partial metadata. | MUST |
| R4.66 | OSR entry MUST be modelled in the IR as a dedicated OSR entry region whose live-in values import the interpreter slots — the structural inverse of a FrameState (block parameters in a block-based SSA, a region + φ set in sea-of-nodes). The imported values' representations MUST match the D4.09 `ConversionKind`s, so that the state A4.03 transfers in is exactly the state the loop body consumes. | MUST |
| R4.67 | A FrameState MUST be a stack of scopes (D4.15 `FrameScope`), innermost last. T2 currently reconstructs exactly one interpreter frame (§4.6 A4.04 inlining note), so the stack has length 1; the shape is mandated now so that frame-level callee inlining can be added later by pushing scopes, without changing A4.04's contract. | MUST |
| R4.68 | Each T2 build stage (§4.10.5) MUST pass both the **tier-differential** harness (observable results identical under T0, T1, and T2 for the same program) and the **deopt-correctness** harness (a forced guard failure at that stage's constructs resumes and returns the interpreted result) before the next stage begins. | MUST |

---

## 4.10.2 Data Structures

### D4.15 — FrameState

```rust
/// The abstract interpreter state a deoptimising node must reconstruct.
/// Attached to guards, checked arithmetic, assumption nodes, and any call or
/// safepoint that may deopt (R4.59). This is the IR-level source of truth;
/// A4.14 lowers it to the §4.6 GC stack map / D4.09 OSR map.
struct FrameState {
    /// Deopt scopes, innermost last (R4.67). Length 1 until frame-level
    /// inlining exists; then one scope per logical CL frame to un-inline.
    scopes: Vec<FrameScope>,
}

struct FrameScope {
    /// The function whose interpreter frame this scope reconstructs.
    function: FnId,
    /// Bytecode position T0 resumes at (becomes `resume_pc`, §4.6 D4.10).
    bcp: u32,
    /// Source for each interpreter local, indexed by local number.
    locals: Vec<ValueSource>,
    /// Source for each live operand-stack slot, bottom-to-top.
    stack: Vec<ValueSource>,
}

/// Where one interpreter slot's value comes from at deopt time.
enum ValueSource {
    /// A live SSA value in representation `repr`. The allocator binds it to a
    /// Location; if `repr` is unboxed, the deopt path reboxes it to a tagged
    /// BlissVal before writing the interpreter slot (§4.6 A4.04 step 3).
    Value { value: SsaValue, repr: ValueRepresentation },
    /// A compile-time-constant immediate BlissVal.
    Const(BlissVal),
    /// Not live in T2. The interpreter slot receives UNBOUND-MARKER; a
    /// reference in the interpreter signals the correct CL error
    /// (§4.6 A4.04 step 6).
    Unbound,
    /// Recompute in the cold deopt continuation from a recipe (A4.13). Recipe
    /// inputs are themselves ValueSources, so rematerialisation composes.
    Remat(RematRecipeId),
}
```

**Invariants:**

1. `scopes` is non-empty; `scopes.last()` is the deoptimising node's own frame.
2. For every `Value { value, .. }`, `value` dominates the deoptimising node
   (R4.60) and its `repr` is producible by that value's definition (R4.64).
3. `locals.len()` and `stack.len()` equal the source bytecode's abstract local
   count and operand-stack height at `bcp` (R4.64).

### D4.16 — ValueRepresentation

The **machine representation** of an SSA value, orthogonal to its CL type
(D4.08). Type answers "what Lisp object is this"; representation answers "in what
physical form does it currently exist." Box/unbox nodes (R4.61) change
representation while preserving type.

| Variant | Storage | GC-visible? | Notes |
|---------|---------|-------------|-------|
| `Tagged` | GPR | Yes (may be a pointer) | A normal BlissVal; the only representation a T0 slot can hold. |
| `UnboxedFixnum` | GPR | No | Raw 61-bit-range `i64`, untagged. Reboxed by tag-shift on deopt. |
| `UnboxedF32` | XMM | No | Raw `f32`; the immediate single-float (§4.10.3) reboxed by `shl 32 | tag`. |
| `UnboxedF64` | XMM | No | Raw `f64`; a boxed CL `double-float` reboxed by heap allocation on deopt. |

Only `Tagged` values may appear in an interpreter slot; every unboxed
`ValueSource::Value` therefore implies a rebox step during A4.04. New unboxed
representations (e.g. `UnboxedI32` for typed arrays) extend this enum and MUST
define their rebox sequence.

### D4.17 — RematRecipe

```rust
/// A pure, total, cheap computation replayed on the cold deopt path to
/// reconstruct a value that DCE removed from the fast path (A4.13, R4.62).
struct RematRecipe {
    op: RematOp,               // Const | Box | Unbox | AddFixnum | ... (pure only)
    inputs: Vec<ValueSource>,  // composes with FrameState sources
    result_repr: ValueRepresentation,
}
```

**Eligibility:** an op is rematerialisable only if it is side-effect-free, cannot
itself deopt, and is cheaper to replay than to keep live (constants, box/unbox,
tag manipulation, non-trapping integer add/sub, address-of-constant). Allocation,
calls, loads from mutable memory, and anything observable are **not**
rematerialisable and keep their value deopt-live instead.

---

## 4.10.3 The Deopt-Preservation Invariant (worked example)

Consider the counted single-float accumulator already handled by T1:

```lisp
(defun fsum (n)
  (let ((s 0.0) (i 0))
    (tagbody top (when (< i n) (setq s (+ s 1.5)) (setq i (+ i 1)) (go top)))
    s))
```

T2, given a profile that `s` is always a single-float and `i`/`n` fixnums,
compiles the loop body with `s` held **unboxed in an XMM register** and `i` as an
**unboxed fixnum in a GPR**, with the type guards hoisted to the preheader
(R4.63). The `(+ s 1.5)` is a raw `addss`; there is no boxed `s` on the fast path
at all.

Now suppose the preheader guard on `s` had instead been left per-iteration and
one iteration sees a double-float. Deopt fires inside the loop. The interpreter
frame for `fsum` needs `s`, `i`, and `n` as ordinary tagged BlissVals at the
`top` bytecode position. The FrameState attached to the guard records exactly
that:

```text
scopes = [ FrameScope {
  function = fsum, bcp = <top>,
  locals = [ n:    Value{ v_n, Tagged },          ; parameter, never unboxed
             s:    Value{ v_s, UnboxedF32 },       ; rebox: (bits<<32)|tag
             i:    Value{ v_i, UnboxedFixnum } ],  ; rebox: (i<<3)
  stack  = [] } ]
```

The invariant (R4.60) is what makes every intervening pass safe:

- **LICM** hoisted the type guards and the `1.5` constant out of the loop — but
  the FrameState still names `v_s`/`v_i`, which remain defined and dominate.
- **Unboxing** rewrote `s` from `Tagged` to `UnboxedF32` — legal precisely
  because the FrameState carries the representation, so A4.04 knows to rebox.
- **DCE** may delete the boxed form of `s` entirely; the tagged value is dead on
  the fast path. It survives only as `Value{ v_s, UnboxedF32 }` plus a rebox
  recipe — see rematerialisation below.
- **Register allocation** binds `v_s` to an XMM register (or spill slot) and
  emits the rebox into the deopt continuation (R4.65).

No pass had to "know about deopt" beyond honouring one rule: do not orphan a
FrameState slot. That is the whole discipline.

---

## 4.10.4 Algorithms

### A4.13 — Deopt-preserving DCE & rematerialisation

```text
ALGORITHM A4.13: DCE-WITH-REMAT(function)
─────────────────────────────────────────
Mark:
  worklist ← all values with a NON-deopt use (real fast-path or effect use)
  propagate liveness backward through operands until fixpoint.
  (FrameState uses do NOT seed the worklist — that is the whole point.)

Sweep, for each value V not marked live:
  IF V has no FrameState uses:
      delete V.
  ELSE IF V is rematerialisable (D4.17 eligibility):
      recipe ← build RematRecipe from V.op and V's operand ValueSources
      replace every FrameState use of V with Remat(recipe)
      delete V from the fast path.
  ELSE:
      V stays live (it is deopt-live, R4.60); keep it, but it MAY sink to
      the coldest block that still dominates all its deopt uses.

Post-condition: no value is live purely to feed a deopt path unless it is
genuinely unrematerialisable; every FrameState slot still resolves.
```

Rematerialisation composes: a recipe's inputs are `ValueSource`s, which may
themselves be `Remat`, so a short pure chain (unbox → add → rebox) collapses into
cold-path code that runs only on the rare deopt.

### A4.14 — FrameState lowering

At code emission (§4.7), after register allocation has assigned a `Location` to
every SSA value, each deoptimising node's FrameState is lowered to the §4.6
compiled forms:

```text
ALGORITHM A4.14: LOWER-FRAMESTATE(fs, alloc) → StackMapEntry
────────────────────────────────────────────────────────────
entry.resume_pc ← fs.scopes.last().bcp
FOR each interpreter slot (idx, source) in fs.scopes.last() locals ++ stack:
  MATCH source:
    Const(k)         → emit "materialise immediate k" descriptor
    Unbound          → emit UNBOUND-MARKER descriptor
    Value{v, repr}   → loc ← alloc.location_of(v)
                       emit { loc, needs_boxing: repr≠Tagged, box_kind(repr) }
                       IF loc holds a GC pointer (repr = Tagged, type may be ptr):
                         set entry.live_ref_bitmap[slot]
    Remat(r)         → emit the recipe as cold-path reconstruction code,
                       recursing on its input ValueSources.
The result is exactly the stack-map shape A4.04 step 1–3 consumes, and the
OSR-entry inverse (D4.09 OsrSlotDesc) for A4.03.
```

This closes the loop: FrameState (IR) → StackMap/OsrEntryMap (metadata) →
A4.03/A4.04 (runtime). §4.6 remains the runtime authority; this chapter is the
authority on the metadata's IR origin and preservation.

---

## 4.10.5 Build Stages

T2 is built as an ordered sequence of runnable slices, each gated by R4.68 (the
tier-differential and deopt-correctness harnesses, extending the existing T1 OSR
differential tests to T2). No stage begins until the previous stage's gate is
green through the real `bliss` binary.

| Stage | Deliverable | Gate |
|-------|-------------|------|
| T2-a | SSA build from bytecode + FrameState construction + **identity lowering** to the label assembler (§4.7 backend). No optimisation. | A hot loop runs correct results through T2; deopt at any guard reconstructs the interpreter frame. Establishes the FrameState model end to end — the hardest, foundational slice. |
| T2-b | Register-allocate the operand stack: loop values live in registers, not the memory operand stack T1 uses. Still no mid-end opt. | Same differential + deopt gate; measurable speedup over T1 from eliminated operand-stack traffic. |
| T2-c | Type/range inference (§4.5, R4.31 + range analysis) consuming §4.9 profiles; **guard hoisting** (R4.63) and overflow-guard elimination via ranges. | Hot loop body is guard-free; a preheader-guard failure deopts correctly. |
| T2-d | Unboxing across the loop (R4.61) with FrameState-driven reboxing (A4.14) and rematerialisation (A4.13). | Unboxed loop; deopt reboxes to the exact interpreted values. |
| T2-e | Classic SSA mid-end: GVN/CSE, LICM (R4.34), deopt-aware DCE (R4.35/A4.13), strength reduction (R4.36), null-check elimination (R4.37); inline-cache-directed inlining (§4.8) and escape-analysis allocation sinking (R4.33). | Differential + deopt gate holds under all passes; benchmark suite (§10) shows the expected wins. |
| T2-f | OSR into fully-optimised T2 (R4.66, §4.6 A4.03): the OSR entry block imports live interpreter state as block parameters. | A loop promotes T0/T1 → T2 mid-flight with identical results; deopt back to T0 still correct. |

Stages T2-a…T2-c decide whether the architecture is sound; T2-d…T2-f are
incremental value on a correct foundation.

---

## 4.10.6 Interactions & Spec Deltas

This chapter tightens three sibling chapters; the following deltas are called out
so they are tracked rather than discovered:

- **§4.5 (Optimisation).** R4.31 already mandates type propagation over the CL
  lattice (D4.08). T2 requires two extensions this chapter depends on: (a) the
  lattice MUST carry **integer range** facts (so a counted loop's `+1` proves
  non-overflow and drops its guard, T2-c) and **representation** facts (D4.16);
  (b) inference is **speculative** — profiling facts (§4.9) enter the lattice as
  guarded assumptions discharged by R4.63 guards, not as proven types. The
  inference algorithm SHOULD be sparse conditional (SCCP-family) type+range
  propagation, flow-sensitive through branch conditions and R4.21 guards.

- **§4.7 (Register allocation) — DECIDED: `regalloc2`.** R4.45 has been amended
  to mandate an SSA-based allocator with live-range splitting — the `regalloc2`
  library (the Cranelift allocator) or equivalent — providing reference-type /
  stack-map tracking out of the box, so effort goes to the FrameState model
  rather than the allocator. Whatever the allocator, it MUST (i) support two
  register classes (GPR for tagged/unboxed-integer/pointer, XMM for unboxed
  float), (ii) honour fixed-register ABI/c2i constraints, and (iii) finalise both
  the GC stack map and every FrameState `Location` binding (R4.65).

- **§4.3 (IR flavour) — DECIDED: block-based typed SSA.** §4.3 has been rewritten
  from sea-of-nodes to a block/CFG SSA (the Cranelift / TurboFan-lite shape):
  basic blocks with typed block parameters (SSA φ), instructions in program
  order, and explicit CFG terminators. This keeps deopt metadata and OSR entry
  regions (R4.66) materially easier to build and verify — a FrameState attaches
  to an instruction and names block-local SSA values — at some cost in the global
  code-motion freedom sea-of-nodes would afford. The memory-token chain, floating
  nodes, and Region/Phi machinery of the old flavour are gone; effect ordering is
  intra-block program order plus alias analysis (§4.3).

- **§4.8 (Inline caches) / assumption nodes.** Assumption-based speculations
  (class-not-redefined, fn-not-redefined) are deoptimising nodes (R4.59) whose
  guard is *global*, not per-operand: they register a dependency in the IC
  invalidation registry (R4.51) and carry a FrameState like any other guard, so
  a redefinition-triggered deopt reconstructs the frame identically.

---

## 4.10.7 Test Strategy

| Test | Scope | Method |
|------|-------|--------|
| FrameState round-trip | Unit | For each T2-a construct, force a deopt at every guard; assert the reconstructed interpreter frame's locals/stack equal a T0 run stopped at the same `bcp`. |
| Tier-differential | Integration | Run the OSR/deopt corpus under T0, T1, and T2 (`BLISS_BACKEND`/threshold env, as the T1 `osr.rs` suite does); assert byte-identical stdout across all three. |
| Preservation invariant | Property/fuzz | After every pass in a randomised pass pipeline, run the IR verifier (A4.07/R4.64); assert no FrameState is orphaned and all sources dominate. |
| Rematerialisation | Unit | Construct a value used only by a FrameState via a pure chain; assert DCE removes it from the fast path and a forced deopt still yields the correct slot value (A4.13). |
| Unbox/rebox correctness | Unit | Unboxed fixnum/f32/f64 loops; force deopt mid-loop; assert reboxed values are bit-identical to interpretation (extends the T1 `t1_deopt` float cases). |
| Range-guard elimination | Unit | Counted loop; assert the overflow guard is absent from T2 code, and that an out-of-declared-range input deopts correctly. |
| OSR-into-T2 | Integration | Promote a running loop T1→T2 via OSR (R4.66); assert identical results and that a subsequent deopt returns to T0 correctly. |
| Allocator maps | Unit | At every safepoint/trap PC, assert a stack map exists (R4.46) and every FrameState slot has a bound Location or a Remat recipe (R4.65). |
