# §14  SIMD

**Scope:** How EGCL exposes single-instruction/multiple-data hardware to Lisp
code: the SIMD pack data type (§14.3), the `EGCL-SIMD` package family and its
`SB-SIMD` compatibility layer (§14.4), unboxed codegen and register allocation
for vector values (§14.5), instruction-set detection and dispatch across image
save/restore (§14.6), and the staged plan that gets there from where EGCL is
today (§14.2). SIMD is stage 7 (`numeric-performance`) work: every requirement
in this chapter is tagged `[S7]`, and none of it is gated before stage 6's Gate
passes.

This chapter exists because SIMD is the first EGCL feature whose design is
*mostly* a question of what to refuse. The hardware differs wildly, two prior
art designs solve the portability problem in opposite ways, and EGCL currently
lacks the two representations SIMD is built on. §14.1 records that prior art and
§14.2 records the honest prerequisite chain, so the design decision in §14.3 can
be read against both.

---

## 14.1  Prior art

### 14.1.1  sb-simd (SBCL contrib, Marco Heisig)

SBCL's `sb-simd` is the closest thing to a reference design for a Common Lisp
SIMD interface, and EGCL adopts most of its shape. Its properties, from
`contrib/sb-simd/sb-simd.texinfo` and `contrib/sb-simd/code/`:

| Aspect | sb-simd's choice |
|---|---|
| Naming | `X.Y` where `X` ∈ {`f32`, `f64`, `s8`…`s64`, `u8`…`u64`} and `Y` is the element count: `f64.4`, `u8.32` |
| Packages | One package **per instruction set** — `SB-SIMD-SSE2`, `SB-SIMD-AVX`, `SB-SIMD-AVX2`, `SB-SIMD-FMA` … — each exporting only what that set really provides |
| Type model | A SIMD pack is an ordinary CL object with a type and a class; it has an **unboxed** register form and a **boxed** heap form, and boxing is avoided by inlining or by keeping packs in specialized arrays rather than passing them as arguments |
| Casts | For each `X.Y` a function `X.Y` coerces-or-broadcasts; **every** operation implicitly casts its arguments, so `(f32.8+ x 5)` works |
| Reinterpretation | `X.Y!` reinterprets bits, zero-extending or truncating |
| Comparisons | Return a **mask** — a pack of unsigned integers whose elements are all-ones or all-zeros — not a generalized boolean |
| Conditionals | `X.Y-if` selects element-wise from two packs under a mask; both arms are always evaluated |
| Loads/stores | `X.Y-aref` / `X.Y-row-major-aref` and their `setf` forms, reading `Y` consecutive elements **out of a specialized array**; unaligned by default, with separate non-temporal variants that do require alignment |
| Scalar twins | Every `X.Y-OP` has a scalar `X-OP` with identical semantics — useful as a reference implementation for tests and as a target for autovectorisation |
| Unavailable instructions | Always *defined*, but bound to a function that signals `MISSING-INSTRUCTION` (`code/missing-instruction.lisp`) — never silently emulated |
| Availability | `cpuid` feature predicates at load time (`code/cpu-identification.lisp`) |
| Image portability | `instruction-set-case`, compiling to a jump table whose index is **recomputed on every image startup**, so an image dumped on AVX2 still runs on SSE2 |

The two ideas EGCL takes wholesale are the `X.Y` naming scheme (it is terse,
sorts well, and makes the element width impossible to misread) and
`instruction-set-case` (an image-based system *must* answer "what if the machine
changed since the core was dumped", and no other design in the survey even
poses the question).

The idea EGCL takes with modification is per-instruction-set packages exposing
hardware faithfully. It is the right primitive layer, but as the *only* layer it
pushes every portability decision onto the user.

### 14.1.2  Go's `simd` / `archsimd` experiment (go.dev/blog/simd-experiment)

Go's experiment answers the layering question sb-simd leaves open, by shipping
**two** APIs:

- `archsimd` — architecture-dependent, minimal abstraction, amd64 first
  (Go 1.26), then arm64/NEON and WebAssembly (Go 1.27). This is sb-simd's layer.
- `simd` — a *portable* API whose types (`simd.Uint8s`, `simd.Float32s`) are
  deliberately **width-agnostic**. Code asks a vector its `.Len()` at run time
  and strides by it; partial tails come back from `LoadVPart` as a vector plus a
  count. The package exposes only the **intersection** of operations across the
  architectures Go expects to support, and fills the gaps with emulation.

Three further details are worth recording because they are decisions, not
details:

1. **Specialisation by AST rewriting, not intrinsics.** The compiler clones each
   function that mentions a `simd` type into `@simd128` / `@simd256` / `@simd512`
   / `@simd0` (emulated) variants, and hoists the dispatch to the outermost
   function whose *signature* mentions a vector type. Their own summary of the
   trade-off: the overhead is hoisted "as high as necessary to avoid dispatch
   within SIMD computations, but not higher" — at the cost of code duplication.
2. **Feature selection is a runtime knob**, not a build flag: `GODEBUG=simd=0`
   forces emulation, `simd=256` demands a width and panics if absent, `simd=+256`
   is permissive. That makes *testing* the emulated and narrow paths possible on
   one machine — the hard part of shipping a portable SIMD API.
3. **Escape hatches both ways**: `ToArch()` / `Int8sFromArch()` convert between
   the portable and architecture-specific types, so the portable layer never
   becomes a ceiling.

Open problems Go calls out, which apply verbatim to EGCL: a conservative
operation set (only what is safe across architectures they expect), preferring
whole-vector reductions over horizontal operations to stay width-independent,
emulation of cryptographic primitives needing constant-time care, and
partial-support CPUs (NEON without PMULL) currently forcing a full downgrade to
emulation.

### 14.1.3  What EGCL concludes

**Design decision (2026-09-26).** EGCL adopts sb-simd's naming, type model,
array-centric loads/stores, mask/`-if` conditionals, scalar twins and
`instruction-set-case`; and adopts Go's *layering* — a faithful
architecture-specific layer with a portable layer above it — while rejecting
Go's silent emulation as the default for the architecture-specific layer.

Rationale for each half:

- **Why not portable-only (Go's `simd` alone).** A Lisp SIMD user is usually
  hand-writing one kernel whose whole purpose is to beat the scalar loop. If a
  width-agnostic API silently emulates `pmull`, the kernel is slower than the
  scalar code it replaced and nothing says so. sb-simd's
  `MISSING-INSTRUCTION` is the more honest default for that user.
- **Why not architecture-only (sb-simd alone).** EGCL targets x86-64 *and*
  aarch64 (§7). An API where portable code must be written twice, by hand,
  around `#+` feature tests will simply not be used portably — the ecosystem
  evidence is that `sb-simd`-using libraries are x86-64-only in practice.
- **Why the portable layer must be opt-in and explicit.** Emulation is a
  legitimate answer, but only when the programmer asked for "correct everywhere"
  rather than "fast here". Making that a choice of *package* (`EGCL-SIMD` vs
  `EGCL-SIMD-AVX2`) rather than a hidden fallback keeps the two goals apart.

---

## 14.2  Prerequisites — why SIMD is stage 7

EGCL cannot express a SIMD pack today, and the gap is not in the SIMD layer.
Two representations have to exist first, and both are large:

**(a) Specialized numeric arrays do not exist.** `ElementTypeTag` in
`crates/egcl-rt/src/object.rs` defines `U8`…`I64`, `SingleFloat`,
`DoubleFloat`, but only `Bit` is ever constructed:
`MAKE-ARRAY`'s `:element-type` is consulted for character, bit and `nil` and
everything else falls through to `%make-simple-vector`. So
`(make-array n :element-type '(unsigned-byte 8))` is a general vector of boxed
fixnums, `ARRAY-ELEMENT-TYPE` answers `T`, and `upgrade_elt` upgrades
`(unsigned-byte 8)` to `T` to stay consistent with that (see `bliss-iqyv` and
§14.7). `X.Y-aref` has nothing to load *from*: there is no contiguous machine
representation of eight `f32`s anywhere in the heap.

**(b) The JIT has no vector or float registers.** The T1/T2 emitter shares one
322-line x86-64 assembler (`crates/egcl-rt/src/asm.rs`) with **no XMM/YMM
support at all** — zero SSE/AVX encodings, and no floating-point registers in
the allocator (§4.7). Single-floats are immediates carrying their bits in the
high 32 of a `EgclVal`; double-floats are heap objects. A `f64.4` cannot be
returned in a register because the register file the compiler models does not
contain one.

Consequently the plan below spends its first two phases on work that is
valuable *with or without* SIMD — packed arrays and unboxed float arithmetic are
the dominant numeric win on their own — and only then adds vectors. Any plan
that starts at `f64.4+` is describing a different compiler.

| Phase | Deliverable | Gate |
|---|---|---|
| P0 | Specialized arrays for `u8/u16/u32/u64`, `s8…s64`, `f32`, `f64`: packed storage, `ARRAY-ELEMENT-TYPE`, `AREF`/`(setf AREF)`, `UPGRADED-ARRAY-ELEMENT-TYPE`, sequence functions, printer, BFASL externalisation | `(typep (make-array 4 :element-type '(unsigned-byte 8)) '(simple-array (unsigned-byte 8) (4)))` is true; a `u8` array costs 1 byte/element; ansi-test array chapter no worse |
| P1 | XMM/YMM in the assembler and register allocator; unboxed `f32`/`f64` locals and array element access in T1/T2; boxing only at frame boundaries | A `double-float` dot-product loop over specialized arrays runs with no heap allocation in steady state and within 2× of SBCL |
| P2 | `EGCL-SIMD-SSE2`/`-AVX`/`-AVX2`/`-FMA` packages: pack types, casts, constructors, arithmetic, comparisons/masks, `-if`, `X.Y-aref`, scalar twins, `MISSING-INSTRUCTION` | A hand-written `f64.4` kernel beats the scalar loop by ≥2× on the same machine; every unavailable instruction signals `MISSING-INSTRUCTION` |
| P3 | `instruction-set-case` + save/restore-safe dispatch; `SB-SIMD*` compatibility packages | An image dumped with AVX2 selected runs correctly when restored with `EGCL_SIMD_MAX=sse2`; sb-simd's own test-suite shape passes against the compatibility packages |
| P4 | Portable `EGCL-SIMD` layer: width-agnostic types, `.LEN`-driven striding, partial-tail loads, explicit emulation | The same source kernel produces correct results at 128-bit, 256-bit and emulated widths, selected at run time |
| P5 | aarch64/NEON behind the same two layers | The P4 kernel runs correctly and faster-than-scalar on aarch64 |

P0 and P1 are prerequisites, not SIMD; they are listed here because this chapter
is the first place that needs them and because their absence is the reason this
chapter is not stage 5 work.

---

## 14.3  Requirements

### 14.3.1  Data model

| ID | Requirement | Level |
|---|---|---|
| R14.01 | EGCL MUST represent a SIMD pack as a first-class object with a type and a class, usable as an argument, a return value and an array element `[S7]` | MUST |
| R14.02 | A SIMD pack MUST have an unboxed register representation used whenever its type is statically known within a function, and a boxed heap representation otherwise `[S7]` | MUST |
| R14.03 | SIMD pack types MUST be named `X.Y` for element type `X` ∈ {`f32`, `f64`, `s8`, `s16`, `s32`, `s64`, `u8`, `u16`, `u32`, `u64`} and element count `Y`, matching sb-simd `[S7]` | MUST |
| R14.04 | The boxed representation MUST carry its element type and count, and the GC MUST treat a boxed pack's payload as untraced bytes `[S7]` | MUST |
| R14.05 | `TYPE-OF`, `CLASS-OF`, `TYPEP` and the printer MUST report SIMD packs consistently with §1's type lattice `[S7]` | MUST |
| R14.06 | EGCL MUST NOT support SIMD packs of complex numbers or characters in stage 7, matching sb-simd's exclusion `[S7]` | MUST NOT |

### 14.3.2  Operations

| ID | Requirement | Level |
|---|---|---|
| R14.10 | For each `X.Y` EGCL MUST provide a cast function `X.Y` that passes a pack through, and broadcasts a scalar after coercing it to `X` `[S7]` | MUST |
| R14.11 | Every SIMD operation MUST implicitly cast each argument through R14.10, so `(f32.8+ x 5)` is legal `[S7]` | MUST |
| R14.12 | For each `X.Y` EGCL MUST provide `make-X.Y`, `X.Y-values`, and the reinterpreting cast `X.Y!` (truncating excess bits, zero-filling missing ones) `[S7]` | MUST |
| R14.13 | Associative operations MUST accept any number of arguments and combine them tree-wise, returning the identity for zero arguments where one exists `[S7]` | MUST |
| R14.14 | Comparisons MUST return a mask — a pack of unsigned integers whose elements are all-ones or all-zeros — and MUST NOT return a generalized boolean `[S7]` | MUST |
| R14.15 | For each `X.Y` EGCL MUST provide `X.Y-if` selecting element-wise from two packs under a mask `[S7]` | MUST |
| R14.16 | For each SIMD operation `X.Y-OP` EGCL MUST provide a scalar twin `X-OP` with identical element semantics and argument order `[S7]` | MUST |
| R14.17 | EGCL SHOULD prefer whole-vector reductions over horizontal operations in the portable layer, so portable kernels stay width-independent `[S7]` | SHOULD |

### 14.3.3  Loads and stores

| ID | Requirement | Level |
|---|---|---|
| R14.20 | For each `X.Y` EGCL MUST provide `X.Y-aref` and `X.Y-row-major-aref`, plus their `setf` forms, reading and writing `Y` consecutive elements of a specialized array of element type `X` `[S7]` | MUST |
| R14.21 | Array references MUST compile to unaligned loads and stores by default `[S7]` | MUST |
| R14.22 | Non-temporal accessors, where an instruction set provides them, MUST be exposed separately and MUST document their alignment requirement `[S7]` | MUST |
| R14.23 | A SIMD load or store whose index is out of bounds MUST signal a `TYPE-ERROR` under safety ≥ 1, accounting for the full `Y`-element span `[S7]` | MUST |
| R14.24 | Specialized-array storage MUST be laid out so that a `Y`-element span is contiguous and readable by one machine instruction `[S7]` | MUST |

### 14.3.4  Availability, dispatch and image portability

| ID | Requirement | Level |
|---|---|---|
| R14.30 | EGCL MUST provide one package per supported instruction set, exporting only the operations that set genuinely provides `[S7]` | MUST |
| R14.31 | Every operation of every *defined* instruction set MUST be fbound; an operation the current CPU does not provide MUST signal `EGCL-SIMD:MISSING-INSTRUCTION` naming the instruction set and the instruction `[S7]` | MUST |
| R14.32 | The architecture-specific packages MUST NOT silently emulate a missing instruction `[S7]` | MUST NOT |
| R14.33 | EGCL MUST detect instruction-set availability at image startup, not at build time `[S7]` | MUST |
| R14.34 | EGCL MUST provide `instruction-set-case`, selecting among per-instruction-set implementations through a jump table whose index is recomputed on every image startup `[S7]` | MUST |
| R14.35 | A saved image MUST remain correct when restored on a machine supporting fewer instruction sets than the one that saved it; code compiled for an unavailable set MUST NOT be entered `[S7]` | MUST |
| R14.36 | `EGCL_SIMD_MAX` MUST cap the instruction sets reported as available, so narrower and emulated paths are testable on one machine `[S7]` | MUST |
| R14.37 | EGCL MUST provide `SB-SIMD`, `SB-SIMD-SSE2`, `SB-SIMD-AVX`, `SB-SIMD-AVX2` and `SB-SIMD-FMA` compatibility packages re-exporting the EGCL equivalents, per R9.02's pattern `[S7]` | MUST |

### 14.3.5  Portable layer

| ID | Requirement | Level |
|---|---|---|
| R14.40 | EGCL MUST provide a portable `EGCL-SIMD` package whose vector types do not name a width, and which exposes the element count at run time `[S7]` | MUST |
| R14.41 | The portable layer MUST expose only operations available across every architecture EGCL supports, and MUST emulate the remainder rather than signalling `[S7]` | MUST |
| R14.42 | The portable layer MUST provide a partial-tail load returning both a vector and a valid-element count `[S7]` | MUST |
| R14.43 | The portable layer MUST provide conversions to and from the architecture-specific pack types in both directions `[S7]` | MUST |
| R14.44 | Selecting the emulated path MUST be possible at run time without recompilation `[S7]` | MUST |
| R14.45 | An emulated cryptographic primitive MUST NOT introduce data-dependent timing `[S7]` | MUST NOT |

### 14.3.6  Compilation

| ID | Requirement | Level |
|---|---|---|
| R14.50 | The assembler MUST emit SSE2/AVX/AVX2/FMA encodings for the operations the SIMD packages export, and the register allocator MUST model XMM/YMM registers with their own spill slots `[S7]` | MUST |
| R14.51 | The T2 optimiser MUST keep a SIMD pack unboxed across a loop body when its type is statically known, and MUST NOT box it at a back edge `[S7]` | MUST |
| R14.52 | Deoptimisation MUST materialise a boxed pack for any unboxed vector live in a deopt frame state, per §4.10 `[S7]` | MUST |
| R14.53 | A SIMD operation whose arguments are constant SHOULD be constant-folded at compile time `[S7]` | SHOULD |
| R14.54 | EGCL MUST NOT autovectorise scalar loops in stage 7; vectorisation is explicit `[S7]` | MUST NOT |
| R14.55 | Callee-saved vector register state MUST be preserved across a safepoint and across a GC, and a boxed pack reachable only from a vector register MUST be a GC root `[S7]` | MUST |

---

## 14.4  Package layout

```text
EGCL-SIMD              portable layer (R14.40–R14.45); width-agnostic
EGCL-SIMD-SSE2         \
EGCL-SIMD-SSE3          |  architecture-specific (R14.30–R14.32);
EGCL-SIMD-SSSE3         |  one package per instruction set, exporting
EGCL-SIMD-SSE4.1        |  exactly what that set provides
EGCL-SIMD-SSE4.2        |
EGCL-SIMD-AVX           |
EGCL-SIMD-AVX2          |
EGCL-SIMD-FMA          /
EGCL-SIMD-NEON         aarch64 (P5)
SB-SIMD, SB-SIMD-*      compatibility re-exports (R14.37)
```

A program picks its layer by choosing a package, which is the whole point of
§14.1.3: `EGCL-SIMD-AVX2` means "I want these instructions and I want to be
told if they are absent"; `EGCL-SIMD` means "I want this to run everywhere".

---

## 14.5  Data structures

**D14.01 — boxed SIMD pack.** `ObjectHeader` (§1.3) with a new `type_id`
`SIMD_PACK`, followed by one byte of `ElementTypeTag`, one byte of element
count, six bytes of padding, then the packed payload (16 or 32 bytes). The
payload is **untraced**: R14.04 requires the GC to skip it, exactly as it skips
bit-vector payload (§3).

**D14.02 — unboxed pack.** An XMM or YMM register, or a 16/32-byte
naturally-aligned stack slot in the T1/T2 frame. Not a `EgclVal`: it has no
tag and cannot be stored in a general vector or passed through a generic call
without boxing first. §4.10's frame-state map records, for each deopt site, the
element type and count needed to rebox (R14.52).

**D14.03 — instruction-set record.** Name, feature predicate, the set it
extends, and the operations it provides. Populated at startup from `cpuid`
(x86-64) or `HWCAP` (aarch64); the jump-table index behind
`instruction-set-case` is derived from it on every image start (R14.34).

---

## 14.6  Instruction-set detection and image portability

The hazard is specific to image-based systems and worth stating plainly: a core
saved on a machine with AVX2 can be restored on a machine without it, and any
natively compiled code in that core that used `vpaddq` is a SIGILL waiting to
happen. EGCL's answer has three parts:

1. **Detection at startup** (R14.33) — never a build-time constant.
2. **`instruction-set-case`** (R14.34) — the only sanctioned way to have several
   implementations of one kernel, dispatching through an index recomputed at
   startup.
3. **Discarding non-portable native code on restore** (R14.35) — code compiled
   against an instruction set the restored machine lacks MUST NOT be entered.
   Since EGCL's tiers recompile from bytecode on demand (§4.4), the sound
   implementation is to record the instruction-set requirement in each compiled
   function's metadata and treat a now-unavailable requirement as an
   invalidation, dropping the function to T0 and letting it re-promote.

`EGCL_SIMD_MAX` (R14.36) exists because otherwise none of this is testable
without a museum of hardware; Go's `GODEBUG=simd=` knob is the precedent.

---

## 14.7  Divergences from sb-simd

Recorded per §9.1's "documented divergence" rule:

| Divergence | Why |
|---|---|
| A portable layer exists (`EGCL-SIMD`) | sb-simd has none, and EGCL targets two architectures (§14.1.3) |
| `EGCL_SIMD_MAX` | sb-simd offers no way to exercise narrow paths on wide hardware |
| Instruction-set requirements recorded per compiled function | sb-simd relies on the user reaching for `instruction-set-case`; EGCL also has to protect its own tiered output (R14.35) |
| No `sb-simd` `:ALLOW-OTHER-KEYS`-style pun on package availability | EGCL packages are always present and always fbound (R14.31) |

## 14.8  Error handling

- A missing instruction signals `MISSING-INSTRUCTION` (R14.31), a subtype of
  `ERROR`, carrying the instruction set and instruction names.
- An out-of-bounds SIMD array access signals `TYPE-ERROR` over the full span
  (R14.23) — the common bug is striding to `(length a)` instead of
  `(- (length a) (1- Y))`, and a load that silently read past the end would be a
  memory-safety hole, not merely a wrong answer.
- A cast that cannot coerce its argument to the element type signals
  `TYPE-ERROR` from the cast, not from the operation.
- Requesting an unavailable width from the portable layer is **not** an error
  (R14.41): it emulates.

## 14.9  Concurrency

Vector registers are per-thread state like any other register file. The only new
contract is R14.55: a safepoint or GC that interrupts a kernel holding live
unboxed packs must preserve them, and a *boxed* pack whose only reference is in
a vector register must be found by the root scan (§3, §13). The existing
callee-saved discipline in §4.7 extends to XMM/YMM; nothing in the lock order
(§13) changes.

## 14.10  Configuration

| Variable | Default | Effect |
|---|---|---|
| `EGCL_SIMD_MAX` | unset (use everything `cpuid`/`HWCAP` reports) | Caps the instruction sets reported available: `EGCL_SIMD_MAX=sse2` makes AVX operations signal `MISSING-INSTRUCTION` and the portable layer fall back (R14.36) |
| `EGCL_SIMD_EMULATE` | `0` | Forces the portable layer onto its emulated path regardless of hardware (R14.44) |

## 14.11  Test strategy

1. **Scalar twins as oracles.** R14.16 exists partly for this: every `X.Y-OP`
   test asserts element-wise equality against `Y` applications of `X-OP`. This
   is the cheapest correctness net and it needs no reference hardware.
2. **Differential against SBCL.** Where `sb-simd` provides the same operation,
   run the same kernel under both and compare; the compatibility packages
   (R14.37) make the source identical.
3. **Width matrix.** Every portable-layer test runs at each available width and
   emulated, driven by `EGCL_SIMD_MAX`/`EGCL_SIMD_EMULATE` (R14.36, R14.44) —
   otherwise the emulation path is dead code that happens to compile.
4. **Save/restore.** Dump an image with AVX2 active, restore it under
   `EGCL_SIMD_MAX=sse2`, and assert correct results with no SIGILL (R14.35).
5. **GC interaction.** Run every kernel under `EGCL_GC_STRESS=1` and
   `EGCL_GC_POISON=1` and diff the output against a non-stress run — the
   standing rule for allocating paths, and the only way R14.55 is really
   checked.
6. **No-allocation assertions.** P1's and R14.51's whole point is that a hot
   vector loop does not allocate; the test asserts a zero delta in bytes
   allocated across the loop, not merely a time.
7. **Bounds.** A load at `(- (length a) 1)` with `Y` = 4 must signal, not read
   neighbouring memory (R14.23).

## 14.12  Open questions

1. **Does `f32`/`f64` unboxing land as part of P1 or as a separate numeric-tower
   change?** P1 as scoped here delivers unboxed floats only inside compiled
   code; a `double-float` returned through a generic call is still boxed. Making
   unboxed floats pervasive is a larger change to §1's value representation and
   is deliberately not in this plan.
2. **How wide should the portable layer's type set be?** Go ships the
   intersection across architectures it *predicts*. EGCL supports two
   architectures today; committing to the intersection of a hypothetical
   RISC-V/PowerPC future would narrow the API for no present benefit. The
   conservative reading — intersect x86-64 and aarch64 only, and widen later —
   risks a breaking change if a third architecture arrives.
3. **Should `instruction-set-case` be available to the tiered compiler itself**,
   i.e. should EGCL's own sequence/string primitives be vectorised through this
   API once it exists? That is where most users would actually get their speedup
   (`search`, `position`, `count`, UTF-8 decode) without writing any SIMD.
   Deliberately out of scope here; it needs its own plan, and it is the strongest
   argument for doing P0/P1 regardless of whether P2+ ever happens.
4. **Scalable vectors (SVE, RISC-V V).** sb-simd's fixed `X.Y` naming cannot
   express a width the hardware chooses at run time, and Go defers SVE too. The
   portable layer's width-agnostic types are the forward-compatible half of this
   design; the architecture-specific half would need a different naming scheme.
