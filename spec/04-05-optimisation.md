# §4.5 Optimisation Passes

**Scope:** This section specifies the T2 optimising compiler's
middle-end passes — their semantics, ordering, key data structures,
and correctness constraints. All passes operate on the block-based
SSA IR defined in §4.3. The pass manager drives passes in a fixed
pipeline order (A4.02) with an optional profile-guided re-run loop.

---

## 4.5.1 Requirements

| ID | Requirement |
|----|-------------|
| R4.31 | The optimiser MUST perform type propagation using the CL type lattice (D4.08) so that downstream passes and code generation can eliminate type checks. |
| R4.32 | Inlining MUST respect `notinline` declarations and `(optimize (speed 0))`. The inliner MUST enforce configurable budget and depth limits (A4.09). |
| R4.33 | Escape analysis MUST identify heap-allocated objects that do not escape their allocating function and MUST support stack allocation and scalar replacement for such objects. |
| R4.34 | Loop-invariant code motion (LICM) MUST detect natural loops via back-edge analysis and MUST NOT hoist expressions with observable side effects. |
| R4.35 | Dead code elimination (DCE) MUST remove instructions with no live uses that are proven side-effect-free. |
| R4.36 | Strength reduction MUST replace known expensive arithmetic patterns (e.g., multiplication by a power of two) with cheaper equivalents. Constant folding MUST evaluate constant expressions at compile time using CL semantics (including numeric contagion and rounding rules). |
| R4.37 | Null-check elimination MUST use dominator-tree analysis to remove redundant `nil` guards when a dominating check has already been performed on the same SSA value. |

---

## 4.5.2 Pass Ordering — A4.02

Passes execute in the following fixed pipeline. The pass manager runs
the pipeline once; if PGO data is available (§4.6), it re-enters at
step 2 with updated profiles.

```text
A4.02 — T2 Optimisation Pipeline

  ┌─────────────────────────────────────────────┐
  │ 0. IR Construction (§4.3)                   │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 1. Early Canonicalisation                   │
  │    - constant folding (§4.5.9)              │
  │    - strength reduction (§4.5.8)            │
  │    - algebraic simplification               │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 2. Type Propagation (§4.5.3)                │
  │    - forward data-flow over the SSA graph   │
  │    - inserts/removes type-check nodes       │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 3. Inlining (§4.5.4)                        │
  │    - uses type info to devirtualise calls    │
  │    - expands call sites inline per A4.09     │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 4. Re-run Type Propagation                  │
  │    - propagate types into inlined bodies     │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 5. Escape Analysis (§4.5.5)                 │
  │    - connection-graph construction           │
  │    - stack allocation / scalar replacement   │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 6. Loop Optimisations                       │
  │    a. Loop detection (back-edge analysis)    │
  │    b. LICM (§4.5.6)                         │
  │    c. Strength reduction on IVs (§4.5.8)    │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 7. Null-Check Elimination (§4.5.10)         │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 8. Late DCE (§4.5.7)                        │
  │    - remove dead code exposed by all above   │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 9. Late Canonicalisation                    │
  │    - final constant folding                  │
  │    - peephole patterns                       │
  └──────────────────────┬──────────────────────┘
                         ▼
  ┌─────────────────────────────────────────────┐
  │ 10. Lowering to machine IR (§4.7)           │
  └─────────────────────────────────────────────┘
```

**Invariant:** After every pass the IR MUST be valid SSA — every use
dominated by its def, no unreachable nodes remain linked to the
schedule (DCE cleans stragglers).

---

## 4.5.3 Type Propagation / Inference

### 4.5.3.1 CL Type Lattice — D4.08

TorCL models CL types as a bounded lattice used for forward data-flow
analysis on the SSA graph.

```text
D4.08 — Type Lattice

              ⊤  (T — universal type, "unknown")
             /|\
            / | \
     fixnum  cons  single-float  ...  (CLOS class types)
       |       |        |
   (eql 0)  null    (eql 1.0)        ... singleton types
        \      |      /
         \     |     /
           ⊥  (NIL — empty type, "unreachable")
```

| Element | Representation | Notes |
|---------|---------------|-------|
| `⊤` (`T`) | Top — any CL value | Used as initial state for unanalysed values |
| Primitive types | `fixnum`, `cons`, `single-float`, `double-float`, `character`, `symbol`, `function`, `simple-vector`, `simple-base-string`, … | Map directly to tag bits or object-header class-id |
| Compound types | `(or fixnum null)`, `(integer 0 255)`, `(simple-array single-float (*))` | Stored as normalised unions / interval ranges |
| Singleton types | `(eql nil)`, `(eql 0)` | Most precise; arise from constant folding |
| `⊥` (`nil`) | Bottom — no value can inhabit this type | Indicates unreachable code |

**Meet** (`∧`, `type-intersection`): greatest lower bound — the most
specific type that is a subtype of (or equal to) both operands.

**Join** (`∨`, `type-union`): least upper bound — the most general
type that is a supertype of (or equal to) both operands. Used at
merge points (φ-nodes) where incoming alternatives are combined.

Rules:

- `meet(⊤, X) = X`
- `meet(⊥, X) = ⊥`
- `meet(fixnum, (or fixnum null)) = fixnum`
- `meet((integer 0 10), (integer 5 20)) = (integer 5 10)`
- If two types are disjoint: `meet(fixnum, cons) = ⊥`

### 4.5.3.2 Forward Data-Flow Algorithm

```text
Algorithm: TypePropagation(IR)
  Input:  SSA IR graph G
  Output: type(v) for every value node v

  1. Initialise type(v) ← ⊤  for all v
     Initialise type(v) ← known_type(v)  for constants / arguments
        with declared types
  2. Worklist W ← all nodes in reverse post-order
  3. while W is non-empty:
       v ← pop(W)
       new_type ← transfer(v)     // compute output type from input types
       if new_type ≠ type(v):
         type(v) ← new_type
         for each user u of v:
           add u to W
  4. Return type map
```

**Transfer functions** (selected):

| Node kind | Transfer rule |
|-----------|---------------|
| `Const(x)` | `(eql x)` |
| `Arg(i)` with declared type τ | τ |
| `Add(a, b)` | `numeric-contagion(type(a), type(b))` — follows CL rules (§12.1) |
| `TypeCheck(v, τ)` | On true-branch: `meet(type(v), τ)`. On false-branch: `type-difference(type(v), τ)` |
| Block param (incoming args a₁, a₂, …) | `join(type(a₁), type(a₂), …)` |
| `Call(f, args)` | Derived from `ftype` declarations or inferred return types |
| `Cons(car, cdr)` | `cons` |
| `Car(v)` | if `type(v) ⊆ cons` then `T` else `⊤` (may signal error path) |

Static operand proof from declarations or inference takes precedence over a
runtime type profile when selecting a typed arithmetic opcode. Once the checked
entry edge has refined an argument to `fixnum` or `single-float`, dominated
operations MUST consume that proof without repeating the tag guard. This does
not by itself remove arithmetic overflow checks: those require a range proof.

**Convergence:** The lattice has finite height (bounded by the number
of distinct CL types the compiler tracks — capped at 64 union
elements). The transfer functions are monotone. The algorithm
converges in O(V × H) steps where V is node count and H is lattice
height.

### 4.5.3.3 Type-Check Elimination

When `type(v) ⊆ τ`, any `TypeCheck(v, τ)` node is redundant and MUST
be replaced by its input value, removing the branch. This is the
primary mechanism for eliminating runtime type dispatch in hot code.

---

## 4.5.4 Inlining

### 4.5.4.1 Inlining Decision Flowchart — A4.09

```text
A4.09 — Inlining Decision

  Call site C, callee F

  1. Is F declared NOTINLINE?
     ├─ YES → do NOT inline. Stop.
     └─ NO ──▶ continue

  2. Is F declared INLINE or does F have (optimize (speed 3))?
     ├─ YES → set priority ← HIGH
     └─ NO  → set priority ← NORMAL

  3. Is F unknown / not yet compiled?
     ├─ YES → do NOT inline. Stop.
     └─ NO ──▶ continue

  4. Compute cost(F) = number of IR nodes in F's body

  5. cost(F) ≤ SMALL_THRESHOLD (default 30 nodes)?
     ├─ YES → INLINE unconditionally. Stop.
     └─ NO ──▶ continue

  6. Current inlining depth ≥ MAX_INLINE_DEPTH (default 6)?
     ├─ YES → do NOT inline. Stop.
     └─ NO ──▶ continue

  7. Remaining node budget ≥ cost(F)?
     ├─ NO  → do NOT inline. Stop.
     └─ YES ──▶ continue

  8. PGO data available for C?
     ├─ YES → calls(C) / invocations(caller) ≥ HOT_THRESHOLD?
     │         ├─ YES → INLINE. Deduct cost(F) from budget. Stop.
     │         └─ NO  → do NOT inline. Stop.
     └─ NO ──▶ continue

  9. priority = HIGH?
     ├─ YES → INLINE. Deduct cost(F) from budget. Stop.
     └─ NO  → do NOT inline. Stop.
```

### 4.5.4.2 Parameters

| Parameter | Default | Configurable | Description |
|-----------|---------|-------------|-------------|
| `SMALL_THRESHOLD` | 30 IR nodes | Yes (`torcl.opt.inline.small`) | Functions at or below this size are always inlined (unless `notinline`) |
| `MAX_INLINE_DEPTH` | 6 | Yes (`torcl.opt.inline.depth`) | Maximum nesting depth of inlined calls |
| `NODE_BUDGET` | 10 000 IR nodes | Yes (`torcl.opt.inline.budget`) | Total IR growth budget per compilation unit |
| `HOT_THRESHOLD` | 80% | Yes (`torcl.opt.inline.hot-pct`) | Minimum executions of the call site per 100 caller invocations; loops may exceed 100% |

The numerator and denominator MUST cover the same profiling window. T0 records
both while interpreting saved bytecode bodies; T1 continues both counters, with
the call-site increment attached to its c2i call boundary. A large eligible body
at or above `HOT_THRESHOLD` receives the remaining compilation-unit node budget
as its size allowance. Hotness never overrides `NOTINLINE`, legality, recursion,
depth, or the node budget.

### 4.5.4.3 Inlining Mechanics

When a call site is selected for inlining:

1. Clone F's IR sub-graph into the caller's graph.
2. Replace F's `Arg(i)` nodes with the actual argument values at the
   call site.
3. Replace F's `Return(v)` node(s) with direct data-flow edges to the
   call site's users.
4. If F has multiple returns, introduce a merge region and φ-node.
5. Remove the original `Call` node.
6. Schedule type propagation re-run (pipeline step 4 in A4.02).

**Self-recursion:** MUST NOT inline a function into itself beyond depth
1 (one level of unrolling), regardless of budget.

---

## 4.5.5 Escape Analysis

### 4.5.5.1 Connection Graph — D4.09

Escape analysis builds a **connection graph** to determine whether
heap-allocated objects escape their allocating function.

```text
D4.09 — Connection Graph

  Nodes:
    - ObjectNode(alloc-site)   — each allocation in the function
    - ReferenceNode(ssa-value) — each SSA value that may point to an object
    - FieldNode(object, slot)  — each slot of a heap object

  Edges:
    - PointsTo:  ReferenceNode → ObjectNode
    - Deferred:  ReferenceNode → ReferenceNode
    - Field:     ObjectNode → FieldNode → ObjectNode  (nested refs)
```

### 4.5.5.2 Escape States

| State | Meaning |
|-------|---------|
| `NoEscape` | Object is referenced only within the allocating function; never stored to heap or returned. |
| `ArgEscape` | Object is passed as an argument to a callee but the callee does not capture it (known from callee analysis or `dynamic-extent` declaration). |
| `GlobalEscape` | Object may be reachable from the heap, returned, or stored into a global. |

### 4.5.5.3 Algorithm

```text
Algorithm: EscapeAnalysis(IR)
  1. Build connection graph for all allocation sites.
  2. Propagate escape state to fixed point:
       - store to heap field       → GlobalEscape
       - returned from function    → GlobalEscape
       - passed to unknown callee  → GlobalEscape
       - passed to known callee with dynamic-extent param → ArgEscape
       - all other references      → NoEscape
  3. For each NoEscape allocation:
       a. If object size is known at compile time and ≤ 256 bytes:
            → replace heap alloc with stack alloc (alloca)
       b. If all field accesses are known:
            → apply scalar replacement (decompose into SSA values)
  4. For each ArgEscape allocation:
       → stack allocation is legal if the callee is inlined or if the
         callee provably does not capture the reference past its return.
```

### 4.5.5.4 Scalar Replacement

When an object is `NoEscape` and all accesses are statically resolved:
remove the `Alloc` node; replace each `Store(obj, slot, val)` with an
SSA definition; replace each `Load(obj, slot)` with the reaching SSA
definition (standard SSA rename). This eliminates allocation entirely.

**Limitation:** Scalar replacement MUST NOT be applied when the object
is used in a `typep`/`type-of` check that inspects the object header,
unless the check can be constant-folded away.

---

## 4.5.6 Loop-Invariant Code Motion (LICM)

### 4.5.6.1 Loop Detection

Natural loops are identified by finding **back-edges** in the dominator tree:

1. Compute dominator tree.
2. For each edge (A → B): if B dominates A, then (A → B) is a back-edge.
3. For each back-edge (A → B): the natural loop body = {B} ∪ {N : N reaches A without passing through B}.
4. Build LoopTree: nested loops form parent-child relationships.

### 4.5.6.2 Invariant Identification

An instruction I inside loop L is **loop-invariant** if all operands
are defined outside L or are themselves loop-invariant, AND I has no
observable side effects (§4.5.7.1), AND I does not throw (or throwing
is provably impossible given type information).

### 4.5.6.3 Hoisting Rules

1. I MUST dominate all loop exits that use its result.
2. I MUST NOT be a memory load unless the loaded address is provably
   loop-invariant AND no store in the loop may alias it.
3. After hoisting, insert I in the loop pre-header block.
4. MUST NOT hoist `write-barrier` calls; barriers MUST remain adjacent
   to their stores.

---

## 4.5.7 Dead Code Elimination (DCE)

### 4.5.7.1 Side-Effect Classification

Every IR node carries a side-effect flag:

| Class | May be eliminated? | Examples |
|-------|-------------------|----------|
| `Pure` | Yes | arithmetic, type checks, constant loads |
| `ReadOnly` | Yes (if result unused) | heap loads (when no aliasing store exists) |
| `SideEffecting` | No | stores, I/O, function calls with unknown effects, `setq` |
| `Control` | No | branches, returns, unwind-protect boundaries |

### 4.5.7.2 Liveness Algorithm

```text
Algorithm: DCE(IR)
  1. Mark all SideEffecting and Control nodes as LIVE.
  2. Worklist W ← all LIVE nodes.
  3. while W is non-empty:
       n ← pop(W)
       for each input i of n:
         if i is not LIVE:
           mark i as LIVE
           add i to W
  4. Remove all non-LIVE nodes from the graph.
  5. Remove control-flow edges to blocks that are now empty and
     simplify the CFG (merge single-predecessor/successor blocks).
```

**CL-specific concern:** A call to a user-defined function MUST be
classified as `SideEffecting` unless the function is declared `pure`
(a TorCL extension, §9) or the compiler can prove the function body
is free of side effects via inlining and analysis.

---

## 4.5.8 Strength Reduction

### 4.5.8.1 Arithmetic Simplifications

| Pattern | Replacement | Condition |
|---------|-------------|-----------|
| `(* x 2^n)` | `(ash x n)` | `x` is `fixnum` |
| `(* x 0)` | `0` | `x` is `fixnum` (no NaN concern) |
| `(* x 1)` | `x` | any numeric type |
| `(+ x 0)` | `x` | any numeric type |
| `(- x 0)` | `x` | any numeric type |
| `(/ x 1)` | `x` | any numeric type |
| `(/ x 2^n)` | `(ash x (- n))` | `x` is non-negative `fixnum` |
| `(mod x 2^n)` | `(logand x (1- 2^n))` | `x` is non-negative `fixnum` |
| `(expt x 2)` | `(* x x)` | any numeric type |
| `(logand x -1)` | `x` | any integer type |
| `(logior x 0)` | `x` | any integer type |

### 4.5.8.2 Induction Variable Strength Reduction

For loop induction variables (IVs):

1. Identify basic IVs: variables of the form `iv ← iv + stride` at
   each iteration.
2. Identify derived IVs: expressions `a * iv + b` that are linear
   functions of a basic IV.
3. Replace derived IV computations with incremental adds:
   `derived ← derived + a * stride` at the back-edge, initialised to
   `a * iv_init + b` in the pre-header.

---

## 4.5.9 Constant Folding

### 4.5.9.1 CL-Semantic Evaluation

Constant folding evaluates expressions whose operands are all
compile-time constants. The evaluator MUST replicate CL semantics:

| Concern | Rule |
|---------|------|
| Numeric contagion | `(+ 1 1.0)` → `2.0` (single-float), not `2` |
| Integer overflow | Fixnum overflow → bignum promotion (never silent wrap) |
| Division | `(/ 1 3)` → `1/3` (ratio), not `0.333…` |
| Float rounding | Follow IEEE 754 round-to-nearest-even |
| Character operations | `(char-code #\A)` → `65` |
| Sequence operations | NOT folded (too complex; deferred to runtime) |

### 4.5.9.2 Compile-Time Safety

Constant folding MUST NOT:

- Fold expressions that would signal a runtime error (e.g.,
  `(/ 1 0)`) — the error must be raised at runtime on the path
  that actually executes the expression.
- Fold `(coerce x 'single-float)` when `x` is a ratio that would
  lose precision — the semantics require runtime conversion.
- Evaluate calls to user-defined functions at compile time unless they
  are declared `foldable` (TorCL extension, §9).

---

## 4.5.10 Null-Check Elimination

### 4.5.10.1 Dominating Check Analysis

Many CL operations require a `nil` check (e.g., `car`/`cdr`). Redundant checks are eliminated using dominator-tree information:

1. Compute dominator tree.
2. For each `NullCheck(v)` node N: walk up the dominator tree. If a `NullCheck(v)` or `TypeCheck(v, τ)` (where `meet(τ, null) = ⊥`) already dominates N on the non-nil path → N is redundant; replace with its input value.
3. If `type(v)` from type propagation proves v is non-nil (e.g., `type(v) = cons`), eliminate the check directly.

### 4.5.10.2 Interaction with Inlining

After inlining, callee null checks may become redundant because the call site already validated the argument. The pipeline order (inlining step 3, null-check elimination step 7) ensures these are caught.

---

## 4.5.11 Error Handling

| Failure Mode | Response |
|-------------|----------|
| Type lattice overflow (union exceeds 64 elements) | Widen to `⊤`; log diagnostic. Correctness preserved. |
| Inlining budget exhausted | Stop inlining; continue with remaining passes. |
| Escape analysis encounters opaque callee | Conservatively mark as `GlobalEscape`. |
| LICM encounters potential alias | Do NOT hoist; leave instruction in loop. |
| Constant folding encounters error form | Do NOT fold; preserve runtime semantics. |

All optimisation passes MUST be **semantics-preserving**: if analysis
is uncertain, the pass MUST default to the conservative (no-transform)
behaviour. A miscompilation is a critical bug (P0).

---

## 4.5.12 Concurrency

Optimisation passes run single-threaded per compilation unit. Multiple
compilation units MAY be optimised concurrently on separate threads.
Shared read-only data structures (type lattice definitions, known
function signatures) are immutable and require no synchronisation.
Per-function IR graphs are thread-local during optimisation.

---

## 4.5.13 Configuration

| Knob | Default | Env / Option | Effect |
|------|---------|-------------|--------|
| `torcl.opt.level` | `2` | `TORCL_OPT_LEVEL` | 0 = no opts, 1 = fold + DCE only, 2 = full pipeline, 3 = aggressive (larger inline budgets) |
| `torcl.opt.inline.small` | `30` | — | Small-function threshold (IR nodes) |
| `torcl.opt.inline.depth` | `6` | — | Max inline depth |
| `torcl.opt.inline.budget` | `10000` | — | Total IR node budget |
| `torcl.opt.inline.hot-pct` | `80` | — | Minimum call-site executions per 100 caller invocations |
| `torcl.opt.escape.stack-max` | `256` | — | Max bytes for stack allocation |
| `torcl.opt.licm.enable` | `true` | — | Enable/disable LICM |
| `torcl.opt.type-union-cap` | `64` | — | Max union elements before widening to ⊤ |

---

## 4.5.14 Test Strategy

| Area | Method |
|------|--------|
| Type propagation | Unit: feed IR with known types, assert propagated types. Golden-file tests for φ-node merges. |
| Inlining | Unit: call graphs with various sizes/depths/declarations, assert match A4.09. |
| Escape analysis | Unit: known escape patterns, assert stack-alloc / scalar-replace. Negative: escaping objects. |
| LICM | Assert hoisted invariants in pre-header. Negative: side-effecting instructions stay. |
| DCE | Dead branches removed. Negative: side-effecting dead-looking code preserved. |
| Strength reduction | Pattern-match tests per §4.5.8.1 row. IV reduction with known loops. |
| Constant folding | CL-semantic: contagion, ratios, bignum overflow. Errors NOT folded. |
| Null-check elimination | Dominator scenarios; assert elimination count. |
| End-to-end | T2-compiled output = T0 interpreter output. `ansi-test` identical at all tiers. |
| Regression | Every miscompilation → minimised test in `tests/unit/opt/`. |
