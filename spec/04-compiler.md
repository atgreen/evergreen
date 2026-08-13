# §4  Compiler Pipeline

**Scope:** This chapter specifies Bliss's complete compilation pipeline —
from source text to executing machine code.  The pipeline follows the
HotSpot model: cold code is interpreted (T0), warm code is baseline-compiled
(T1), and hot code is aggressively optimised (T2).  Between tiers, On-Stack
Replacement (OSR) transfers control without deoptimising the call stack
unless an uncommon trap fires.

The pipeline comprises nine subsystems, each detailed in its own subsection
file:

| Subsection file | §    | Topic |
|-----------------|------|-------|
| `04-01-reader.md` | §4.1 | CL Reader |
| `04-02-macroexpand.md` | §4.2 | Macro Expansion |
| `04-03-ir.md` | §4.3 | Intermediate Representation (Sea-of-Nodes SSA) |
| `04-04-tiered.md` | §4.4 | Tiered Compilation (T0 / T1 / T2) |
| `04-05-optimisation.md` | §4.5 | Optimisation Passes |
| `04-06-osr.md` | §4.6 | On-Stack Replacement & Deoptimisation |
| `04-07-codegen.md` | §4.7 | Code Emission & Register Allocation |
| `04-08-inline-caches.md` | §4.8 | Inline Caches |
| `04-09-profiling.md` | §4.9 | Profiling Infrastructure |

Current source map:

```
crates/bliss-compiler/src/
├── reader.rs          §4.1  CL reader (Rust bootstrap)
├── macroexpand.rs     §4.2  Macro expansion engine
├── ir.rs              §4.3  Sea-of-nodes IR core types
├── tiered.rs          §4.4  Tiered execution orchestration for T0/T1/T2
├── opt.rs             §4.5  Optimisation pipeline and pass coordination
├── osr.rs             §4.6  On-stack replacement and deoptimisation support
├── codegen.rs         §4.7  Machine code emission and backend glue
├── ic.rs              §4.8  Inline cache runtime
├── profiling.rs       §4.9  Profiling counters and type recording
├── error.rs           Shared compiler diagnostics and error types
└── lib.rs             Crate entrypoint and subsystem wiring
```

Planned finer-grained decomposition such as dedicated `readtable`,
environment, interpreter, baseline, or per-pass/backend modules remains
permitted by later implementation work, but those paths are not present
in the current repository and therefore are not part of the current
source map.

---

## 4.0  Requirements Summary

All requirements are prefixed **R4**.  Detailed requirements appear in each
subsection; the master list is collected here for cross-referencing.

### 4.0.1  Reader (§4.1)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.01 | Implement the CLHS §2.2 reader algorithm (steps 1-10) | MUST |
| R4.02 | Readtable MUST be a first-class, per-thread-bindable object | MUST |
| R4.03 | Support all standard `#` dispatch macros (CLHS §2.4.8) | MUST |
| R4.04 | Reader MUST parse all CL number syntaxes (integer, ratio, float, complex) respecting `*read-base*` | MUST |
| R4.05 | Package-qualified symbols (`pkg:sym`, `pkg::sym`, `#:sym`, keyword) MUST be resolved per CLHS §2.3.5 | MUST |
| R4.06 | Reader MUST be reentrant — no global mutable state except via dynamic variables | MUST |
| R4.07 | Provide `set-macro-character`, `set-dispatch-macro-character`, `get-macro-character`, `make-readtable`, `copy-readtable` | MUST |
| R4.08 | Reader errors MUST signal conditions of type `reader-error` with source position (file, line, column) | MUST |
| R4.09 | Reader SHOULD support `#n=` / `#n#` circular structure notation | SHOULD |

### 4.0.2  Macro Expansion (§4.2)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.10 | `macroexpand-1` and `macroexpand` MUST conform to CLHS §3.1.2.1.2 | MUST |
| R4.11 | Macro expansion MUST iterate `macroexpand-1` until no further expansion occurs | MUST |
| R4.12 | Compiler macros (`define-compiler-macro`) MUST be consulted before regular macros when compiling | MUST |
| R4.13 | Symbol macros (`define-symbol-macro`, `symbol-macrolet`) MUST be expanded in variable position | MUST |
| R4.14 | An environment protocol MUST be provided: `variable-information`, `function-information`, `declaration-information` (CLtL2 §8.5) | MUST |
| R4.15 | Expansion hooks (`*macroexpand-hook*`) MUST be called per CLHS | MUST |
| R4.16 | Macro expansion MUST detect and signal circular expansion with a clear diagnostic | MUST |

### 4.0.3  Intermediate Representation (§4.3)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.17 | IR MUST use sea-of-nodes SSA form with explicit control and memory edges | MUST |
| R4.18 | Node types MUST include at minimum: Constant, Parameter, Phi, Call, Branch, TypeCheck, Box, Unbox, MemoryAccess, Region, Merge, Return, Start | MUST |
| R4.19 | Edge types MUST distinguish data (def-use), control, and memory dependency edges | MUST |
| R4.20 | IR MUST preserve enough source information to generate accurate backtraces | MUST |
| R4.21 | IR graph MUST be verifiable: a verification pass MUST check SSA dominance, type consistency, and edge well-formedness before codegen | MUST |
| R4.22 | IR SHOULD support speculative guards (with associated uncommon-trap metadata) for profile-driven optimisation | SHOULD |

### 4.0.4  Tiered Compilation (§4.4)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.23 | The compiler front-end MUST lower any valid executable CL form to portable Bliss bytecode before T0 execution | MUST |
| R4.24 | T0 MUST maintain per-function invocation counters (§4.9) | MUST |
| R4.25 | T1 MUST compile a CL function from Bliss bytecode to native code with < 1 ms latency for typical functions (≤ 200 bytecode instructions) | MUST |
| R4.26 | T1 code MUST include profiling stubs for T2 promotion | MUST |
| R4.27 | T2 MUST apply the full optimisation pass pipeline (§4.5) | MUST |
| R4.28 | Tier promotion thresholds MUST be configurable at runtime | MUST |
| R4.29 | Compilation MUST occur on a background thread; it MUST NOT block the calling thread except at safepoints | MUST |
| R4.30 | T2 compilation MAY be deferred or skipped under memory pressure | MAY |

### 4.0.5  Optimisation Passes (§4.5)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.31 | Type propagation MUST infer types from declarations, type checks, and call return types using the CL type lattice | MUST |
| R4.32 | Inlining MUST respect a configurable budget (default: 50 IR nodes per inline site) and MUST NOT inline across `notinline` declarations | MUST |
| R4.33 | Escape analysis MUST identify heap allocations that can be stack-allocated or scalar-replaced | MUST |
| R4.34 | LICM MUST hoist invariant computations out of loops identified by back-edge analysis | MUST |
| R4.35 | DCE MUST remove nodes with no live uses and no side effects | MUST |
| R4.36 | Constant folding MUST evaluate constant expressions at compile time using CL semantics (e.g. `(+ 1 2)` → `3`) | MUST |
| R4.37 | Pass ordering MUST be deterministic and documented | MUST |

### 4.0.6  On-Stack Replacement (§4.6)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.38 | OSR entry MUST transfer execution from T0/T1 code into T2 code at loop back-edges | MUST |
| R4.39 | OSR exit (deoptimisation) MUST reconstruct an interpreter frame from compiled state when a guard fails | MUST |
| R4.40 | Uncommon traps MUST record the reason (type mismatch, uninitialized variable, etc.) for re-profiling | MUST |
| R4.41 | A function that deoptimises more than a configurable threshold (default: 4) times MUST be blacklisted from T2 for a backoff period | MUST |

### 4.0.7  Code Emission (§4.7)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.42 | x86-64 backend MUST be the primary target and MUST support SSE2+ for floating point | MUST |
| R4.43 | AArch64 backend MUST be provided as a secondary target | MUST |
| R4.44 | Register allocation MUST use linear-scan over SSA live ranges | MUST |
| R4.45 | Emitted code MUST be patchable: inline cache sites, safepoint polls, and call sites MUST be patchable without stopping the world | MUST |
| R4.46 | Code MUST be emitted into GC-managed code regions (§3) | MUST |
| R4.47 | Emitted code MUST include GC stack maps for every safepoint | MUST |

### 4.0.8  Inline Caches (§4.8)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.48 | Monomorphic inline caches MUST cache a single type→method mapping for generic dispatch | MUST |
| R4.49 | Polymorphic inline caches MUST support up to 4 type→method entries before falling back to megamorphic lookup | MUST |
| R4.50 | IC transitions (mono → poly → mega) MUST be atomic with respect to concurrent callers | MUST |
| R4.51 | Type-check ICs MUST eliminate redundant type checks for known monomorphic call sites | MUST |
| R4.52 | IC state MUST be resettable by the GC (when methods are redefined or classes change) | MUST |

### 4.0.9  Profiling Infrastructure (§4.9)

| ID | Requirement | Level |
|----|-------------|-------|
| R4.53 | Every interpreted/T1 function MUST maintain a 32-bit invocation counter | MUST |
| R4.54 | Every loop back-edge in T0/T1 MUST maintain a 32-bit back-edge counter | MUST |
| R4.55 | Type profile recording MUST log the observed types at call sites and branches (ring buffer, max 8 entries) | MUST |
| R4.56 | Counter overflow MUST trigger tier promotion (not wrap) | MUST |
| R4.57 | Profiling data MUST be accessible to the optimising compiler without stopping the profiled thread | MUST |
| R4.58 | Profiling overhead for T0 MUST be < 10% and for T1 MUST be < 5% relative to un-instrumented execution | MUST |

---

## 4.1  CL Reader — Overview

See `spec/04-01-reader.md` for the full specification.

The reader converts a character stream into Lisp objects.  It follows the
CLHS §2.2 reader algorithm (10 steps), with the readtable governing
character syntax types (constituent, whitespace, macro, etc.).  Key design
points:

- **Readtable** is a struct containing a 128-entry ASCII fast-path table and
  a `HashMap<char, CharTraits>` for extended Unicode.  Each entry stores the
  syntax type and an optional macro function.
- **Reader state** is bundled in a `ReaderState` struct holding the input
  stream, current readtable, `*read-base*`, `*read-suppress*`,
  `*read-eval*`, and a circular-reference label map (`#n=`/`#n#`).
- **Number parsing** uses a deterministic grammar covering integer, ratio,
  float (with exponent markers `s/f/d/l/e`), and complex `#C(r i)`.
- **Error handling:** all reader errors produce a `reader-error` condition
  with a source location `(file, line, column)`.

## 4.2  Macro Expansion — Overview

See `spec/04-02-macroexpand.md` for the full specification.

Expansion runs after reading and before IR construction.  The core loop:

```text
A4.01 — Macro Expansion Algorithm
1. Given form F and environment E:
2. If F is a symbol: check symbol-macro in E → expand → goto 1.
3. If F is (OP . ARGS) where OP has a compiler-macro in E:
   a. Call compiler-macro expander; if it declines (returns F unchanged), continue.
   b. If changed, set F = result, goto 1.
4. If OP has a macro-function in E:
   a. Call *macroexpand-hook* with expander, F, E.
   b. Set F = result, goto 1.
5. Return F (fully expanded).
```

The **environment protocol** provides `variable-information`,
`function-information`, and `declaration-information` per CLtL2 §8.5,
returning information about lexical bindings, declarations, and their
properties.

## 4.3  Intermediate Representation — Overview

See `spec/04-03-ir.md` for the full specification.

Bliss uses a **sea-of-nodes SSA IR** inspired by HotSpot C2 and Graal:

**Node types (D4.01):**

| Node Kind | Inputs | Output | Description |
|-----------|--------|--------|-------------|
| `Start` | — | control | Entry point of the graph |
| `Return` | control, value | — | Function return |
| `Constant` | — | value | Compile-time constant |
| `Parameter` | — | value | Function parameter (index) |
| `Phi` | control, value* | value | SSA merge of values |
| `Region` | control* | control | Control-flow merge |
| `Branch` | control, condition | control, control | Conditional fork |
| `Call` | control, memory, target, args* | control, memory, value | Function call |
| `TypeCheck` | value | value | Runtime type guard (+ uncommon trap) |
| `Box` | value | value | Tag an unboxed scalar |
| `Unbox` | value | value | Untag to raw scalar |
| `MemLoad` | control, memory, base, offset | memory, value | Heap load |
| `MemStore` | control, memory, base, offset, value | memory | Heap store |
| `Safepoint` | control, memory | control, memory | GC safepoint poll |

**Edge types (D4.02):**

| Edge Kind | Semantics |
|-----------|-----------|
| Data (def-use) | Value produced by one node, consumed by another |
| Control | Sequencing: determines execution order for side-effecting nodes |
| Memory | Serialises memory operations (load/store ordering) |

The sea-of-nodes representation eliminates the need for a separate CFG:
control and data flow are unified in the same graph, enabling global code
motion by default.

## 4.4  Tiered Compilation — Overview

See `spec/04-04-tiered.md` for the full specification.

**T0 — Bytecode Interpreter:**  Executes portable Bliss bytecode produced by
the compiler front-end from macroexpanded forms.  Maintains invocation and
back-edge counters (§4.9).  Used for cold code, `eval`, bootstrap execution,
and architecture-independent FASL loading.  The interpreter uses an operand
stack, lexical frame slots, and bytecode PCs for source maps, OSR, and
deoptimisation.

**T1 — Baseline Compiler:**  Performs a single-pass lowering from bytecode to
native code without constructing the full sea-of-nodes IR graph.  Inserts
profiling stubs at call sites and back-edges.  Target latency: < 1 ms for a
200-instruction function.  No optimisation beyond peephole lowering and
constant folding of immediates.

**T2 — Optimising Compiler:**  Builds the sea-of-nodes IR (§4.3), runs the
optimisation pass pipeline (§4.5), lowers to machine-specific nodes, performs
register allocation (§4.7), and emits optimised native code.  Runs on a
background compilation thread.  Code is installed atomically via pointer
swap on the function object.

## 4.5  Optimisation Passes — Overview

See `spec/04-05-optimisation.md` for the full specification.

**Pass ordering (A4.02):**

```text
1. Build IR from AST
2. Type propagation (forward data-flow, CL type lattice)
3. Constant folding
4. Inlining (profile-guided, budget-limited)
5. Escape analysis → stack allocation / scalar replacement
6. Type propagation (re-run after inlining)
7. Loop-invariant code motion
8. Strength reduction
9. Dead code elimination
10. Null-check elimination
11. Lower to machine-specific nodes
12. Register allocation (linear scan)
13. Code emission
```

The pass manager runs passes in this fixed order.  The type-propagation pass
uses a forward data-flow analysis over the sea-of-nodes graph, propagating
CL type specifiers (including `and`, `or`, `not`, `satisfies` where
statically decidable).  Inlining respects `(declare (inline f))` and
`(declare (notinline f))`, with a default budget of 50 IR nodes per call
site and a maximum inlining depth of 5.

## 4.6  On-Stack Replacement — Overview

See `spec/04-06-osr.md` for the full specification.

**OSR Entry (A4.03):** When a bytecode back-edge counter exceeds the T2
threshold, the running T0/T1 frame is suspended at the safepoint.  The T2
compiler is invoked (if not already running).  Once T2 code is ready, the
interpreter/T1 frame's live locals at the bytecode PC are mapped to the T2
code's SSA variables via an **OSR entry map**, and execution transfers to the
T2 loop header.

**OSR Exit / Deoptimisation (A4.04):** When an uncommon trap fires (type
guard failure, uninitialized variable, changed class), the T2 frame is
deconstructed:
1. Read live SSA values from registers/stack via the GC stack map.
2. Reconstruct an interpreter-compatible `ValueStack` frame.
3. Set the program counter to the corresponding bytecode PC.
4. Resume in the interpreter (T0).

Uncommon traps record their reason in a per-function **deopt log** for
re-profiling.

## 4.7  Code Emission — Overview

See `spec/04-07-codegen.md` for the full specification.

Code emission targets x86-64 (primary) and AArch64 (secondary).  The
pipeline:

1. **Lowering:** IR nodes → machine-specific `MachNode`s (e.g.,
   `AddI64` → `x86_add_rr`).
2. **Register allocation:** Linear-scan over SSA live ranges.  Spill slots
   allocated in the function's stack frame.  Callee-saved registers are
   preserved per the System V ABI (x86-64) / AAPCS64 (AArch64).
3. **Encoding:** `MachNode`s → bytes in a `CodeBuffer`.  Relocations
   emitted for external references (GC roots, IC sites, safepoints).
4. **Installation:** Copy `CodeBuffer` into a GC-managed code region.
   Patch the function object's entry point atomically.

**Patchable sites:**  Call sites, IC stubs, and safepoint polls are emitted
with `nop` sleds or indirect jumps sized to allow atomic patching (CAS on
x86-64; instruction cache flush on AArch64).

## 4.8  Inline Caches — Overview

See `spec/04-08-inline-caches.md` for the full specification.

Inline caches accelerate generic function dispatch and type checks by
caching observed type→target mappings at each call/check site.

**IC state machine (D4.03):**

```text
  Uninitialized ──→ Monomorphic ──→ Polymorphic (≤4) ──→ Megamorphic
       │                                                       │
       └──────── (first miss) ──→ ... ──→ (>4 types) ─────────┘
```

- **Monomorphic:** Guards on a single class wrapper pointer; fast path is a
  single compare + direct call.
- **Polymorphic:** Linear search over ≤ 4 `(class, method)` pairs.
- **Megamorphic:** Falls back to the generic dispatch hash table.

IC sites are patched in place (§4.7) without stopping the world.  The GC
resets IC state when method definitions change or class hierarchies are
modified (R4.52).

## 4.9  Profiling Infrastructure — Overview

See `spec/04-09-profiling.md` for the full specification.

**Data structures (D4.04):**

| Structure | Location | Size | Purpose |
|-----------|----------|------|---------|
| Invocation counter | Function header | 4 bytes | Count calls, trigger T1 promotion |
| Back-edge counter | Per loop header (in code) | 4 bytes | Count iterations, trigger T2/OSR |
| Type profile | Per call-site / branch | 8 entries × 8 bytes | Record observed types for speculative optimisation |

Counters are incremented with non-atomic `add` (exact counts are not
required; races are tolerable since they only affect promotion timing).
When a counter reaches its threshold, it is set to a **sentinel value**
that traps into the tier-promotion logic rather than wrapping.

Type profiles use a ring buffer of class-wrapper pointers.  The T2 compiler
reads the profile to insert speculative type guards.

---

## 4.10  Error Handling

| Phase | Error type | Strategy |
|-------|-----------|----------|
| Reader | `reader-error` condition | Signal with source location; abort read of current form |
| Macro expansion | `program-error` condition | Signal; includes expansion backtrace |
| IR construction | Internal `CompilerError` | Log + fall back to T0 interpretation |
| Optimisation | Internal `CompilerError` | Abort T2, retain T1 code |
| Code emission | Internal `CompilerError` | Abort T2, retain T1 code |
| OSR / Deopt | — | Always succeeds (by design); failure is a panic (bug) |

## 4.11  Concurrency

- The **reader** holds no global locks; readtable is per-thread (bound via
  `*readtable*`).
- **Macro expansion** acquires read locks on packages for symbol resolution.
- **T2 compilation** runs on dedicated background threads (one per core,
  configurable via `BLISS_COMPILE_THREADS`).  It reads profiling data
  lock-free (§4.9) and installs code via atomic pointer swap.
- **IC patching** uses CAS on x86-64; on AArch64 it uses a store + `ISB`
  barrier.  No stop-the-world required.

## 4.12  Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `BLISS_T1_THRESHOLD` | 10 | Invocation count to trigger T1 compilation |
| `BLISS_T2_THRESHOLD` | 5000 | Invocation count to trigger T2 compilation |
| `BLISS_OSR_THRESHOLD` | 10000 | Back-edge count to trigger OSR entry |
| `BLISS_INLINE_BUDGET` | 50 | Max IR nodes per inline expansion |
| `BLISS_INLINE_DEPTH` | 5 | Max inlining depth |
| `BLISS_COMPILE_THREADS` | `num_cpus / 2` | Background compilation threads |
| `BLISS_DEOPT_BLACKLIST` | 4 | Deopt count before T2 blacklist |
| `BLISS_IC_POLY_MAX` | 4 | Max polymorphic IC entries before megamorphic |

## 4.13  Test Strategy

| Target | Method |
|--------|--------|
| Reader | Round-trip tests: `(read (write-to-string obj))` = `obj`; edge cases for all dispatch macros; fuzz with random byte streams |
| Macro expansion | Unit test each special form expander; test `*macroexpand-hook*`; circular expansion detection |
| IR | Verifier pass after every build; unit tests for each node type; graph equivalence checks |
| T0 interpreter | ANSI test suite subset (all non-compiler-specific tests) |
| T1 baseline | Compile + run subset of ANSI tests; latency benchmarks |
| T2 optimiser | Each pass tested in isolation with crafted IR graphs; full pipeline via cl-bench |
| OSR | Forced OSR at every back-edge via test flag; deopt stress test |
| Codegen | Disassembly golden tests; ABI conformance tests |
| Inline caches | Transition tests (mono→poly→mega); concurrent stress tests |
| Profiling | Counter accuracy tests; profile-guided compilation end-to-end |
