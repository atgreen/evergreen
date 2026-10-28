# Structural value facts in T2

T2 inference returns numeric type/range/representation facts and a structural
fact snapshot. `value_facts::StructuralFacts` is also available independently
when a verifier or structural transform needs no numeric analysis. Rebuild it
after changing instructions, edge arguments, or OSR entries. Origin and
alternate-entry dominance queries are memoized within that snapshot; cached
answers do not cross IR rewrites.

The shared domain distinguishes exact object identity, successful simple-string
layout guards, and nullable packed-U8 layout witnesses. A broad STRING or VECTOR
type is insufficient to justify direct storage access. A nullable witness
proves which object was inspected; it does not prove that the result is non-NIL.
Consumers retain the witness failure branch and value checks. A separate extent
proof can remove a capacity comparison; layout evidence alone cannot.

Successful simple-string layout guards also imply STRING in numeric type
inference. The implication is one-way: a STRING type does not imply storage
layout. Type predicates return tagged Booleans, not narrowed argument copies;
only a unique true branch refines their operand, and those optional edge facts
remain disabled for OSR functions until imports can establish them.

Identity follows value-preserving Guard results and closed forwarding phi graphs.
Every incoming edge participates, every node must reach one actual seed, and a
query visits at most 512 nodes. TypeCheck produces a Boolean and is never an
identity edge. Representation conversions are not identity edges either.

String loads must depend on the successful guard's refined result through every
forwarding path. Merely naming the original unchecked input is insufficient.
The verifier checks that dependency and dominance after validating structural
SSA. Direct guard results remain admissible when the guard reexecutes after OSR;
alias reuse remains conservative because OSR can independently import aliases.

Guard elimination consumes the shared inference result to reuse equivalent
structural evidence through distinct SSA names. Rewrites preserve the surviving
proof value and rewrite FrameState references. Facts are rebuilt after each
rewrite; at most 32 evidence rewrites run per invocation. Remaining checks are
safe. Mapped functions retain their existing guards/probes until structural
import checks can establish equality of imported aliases.

The octet-loop transform still selects profitable witness placement and retains
its specialized store kernel. Its identity/dominance verification now uses the
shared analysis. The late ordinary pipeline also removes duplicate explicit
layout probes. Layout facts never cache interior pointers: consumers reload the
current rooted object after moving GC. Only immutable storage properties belong
in this domain. Mutable contents, adjustable logical lengths, displacement, and
adjustable backing storage require separate effect and alias reasoning.

Allocation postconditions have explicit nullable SSA producers.
`PackedU8AllocationLayout` names both the subject and the original canonical
constructor result. Validated inline allocation supplies its header width;
the collecting slow return undergoes a real layout probe. Neither a constructor
name nor a mutable element-type descriptor makes the whole Call result packed-U8.
Poll, multiple-value, exact-result and final-home restrictions still apply.

`PackedU8AllocationExtent` names the subject, layout witness and constructor
result. Fast allocation captures the initialized length. Otherwise, a successful
layout probe permits reading the actual immutable capacity, with a separate
fixnum-representability check; an unsupported layout or capacity produces NIL.
The proof depends on checked allocator/storage contracts, not an expectation
that an oversized request would run out of memory.

An extent consumer requires correlated array, layout and length evidence across
every forwarding phi. It normally checks its end index against the scalar extent.
The verifier permits omitting that capacity comparison when the end is the exact
extent, a direct fixnum dimension, or a checked MIN capped by one of those values.
Type, nonnegative-index, span and nullable-witness checks remain. A mutable list's
old CAR is never interchangeable with the dimension actually accepted by allocation.
OSR imports and mismatched evidence retain checks or decline the optimization.

Existing string layout operations have verifier and native-emitter support but
no current source-to-IR producer. Their structural integration is exercised by
native IR tests, including wrong-type recovery at the original guard. It is not
a claim that ordinary Lisp LENGTH/CHAR calls currently use those operations.

## Examples and optimization boundaries

A type predicate returns a Boolean. Only its true branch establishes the
argument constraint:

```lisp
(defun classify-and-add (x)
  (if (typep x 'fixnum)
      (+ x 10)                 ; FIXNUM operand; overflow still needs handling
      (+ x 0.5d0)))            ; false branch retains general numeric semantics
```

Testing a predicate without branching on it establishes no argument constraint:

```lisp
(defun predicate-and-sum (x)
  (let ((p (typep x 'fixnum)))
    (cons p (+ x 1))))          ; X can still be a double or a bignum
```

For a repeated packed-byte store, explicit IR layout evidence can be shared
outside the repeated kernel. An identity-preserving alias carries the same
object relation; a conditional choice between different arrays does not:

```lisp
(let* ((output (make-array size :element-type '(unsigned-byte 8)))
       (destination output))
  ;; A proof about OUTPUT can serve repeated stores through DESTINATION.
  ...)

(let ((destination (if condition output another-array)))
  ;; A proof only about OUTPUT is insufficient for DESTINATION.
  ...)
```

The schematic forms illustrate proof rules, not promises that every surface
syntax already selects an intrinsic. The record-writer CLI integration exercises
an actual recognized LDB/ASH byte loop. The native string test demonstrates both
structural guard reuse and automatic removal of a redundant STRING type guard
from the stronger successful layout proof. Its committed prefix effect and
original failure state are checked explicitly.

## Numeric speculation consumes negative constraints

Before selecting a numeric specialization, T2 consults inferred operand types.
A nonempty type disjoint from all numeric types rules out that guess. Unknown
and bottom facts do not veto speculation, and numeric-containing unions still
use the existing profile and floating-point contagion rules. This is a negative
constraint, not permission to omit a runtime guard.

```lisp
(defun predicate-or-add (x addp)
  (let ((p (typep x 'fixnum)))
    (if addp (+ p 1) p)))
```

The normal branch can warm the function without exercising the addition. Even
if that cold addition receives a fixnum guess, inference proves that `p` is a
Boolean. T2 keeps the original generic call, which signals TYPE-ERROR when
reached, rather than installing an arithmetic guard that must deoptimize.
Errors, preceding effects, exceptional continuations, and multiple values keep
the original call's behavior.

Each immutable selection phase lazily computes at most one inference snapshot,
shared across its candidates. The snapshot is discarded before mutation. The
same rule applies before variadic-double splitting and before double-only
expansion, so those entrances cannot bypass the constraint. Operands whose
declared types are already numeric need no additional inference query.

## Inferred numeric family selection

At a site with admitted profile feedback, inferred operand types can now select
a numeric family ahead of a stale profile. The existing declared-type path is
unchanged. A new inferred-positive path never overrides `None` feedback: even
fixnum operands can overflow, and double operands can encounter zero divisors,
negative square roots, or unordered comparisons. Those failures still retire
the speculation through the existing feedback policy.

```lisp
(defun inferred-double-join (flag other arithmetic)
  (let ((n (if flag 1.25d0 2.5d0)))
    (if arithmetic (+ n other) n)))
```

The builder gives the join value an unknown declared type. Inference joins two
double constants and proves DOUBLE-FLOAT. A cold fixnum guess for the addition
can therefore select the existing guarded double path instead, including its
existing fixnum contagion rules for `other`. Single-float and fixnum joins use
their corresponding families; ambiguous unions still require profile checks.
No inferred fact changes the SSA value's declaration or representation merely
to pass information between these decisions.

Double heap literals retain their semantic type only when both the builder's
declaration and immutable captured double metadata agree. Inference does not
read a live or movable heap object. This preserves the literal's type through
joins without claiming that an arbitrary heap constant is a double.

The double-only phase captures operand type bits before rewriting calls, then
discards the complete analysis snapshot. Earlier expansions preserve the same
successful Lisp result values, so captured semantic bits remain valid and can
be intersected with strengthened declarations in program order. Conflicting
facts reject specialization. Exact literal conversions, operand guards, overflow,
NaN and domain checks, recovery states, and multiple-value effects remain in
place. The per-use interface below also makes branch-local constraints
available for original inputs without changing their global declarations.

## Requested-site inference

`infer(f)` retains its whole-function interface. `InferenceResult::ty(value)`
answers with the global fact for that SSA definition. It cannot describe a
constraint that applies to only some uses of an original parameter.

A consumer can instead request the instructions whose operands it will inspect:

```rust
let facts = infer_for_sites(f, &sites);
let operand_type = facts.ty_before(site, operand);
```

Inference runs once. A final scoped replay uses the frozen final global facts,
including any widening, to record facts immediately **before** the requested
instructions. Only their direct operands are recorded. An unrequested site or
nonoperand query falls back to the global fact; it does not trigger another
analysis or establish a new scoped proof. A guard cannot establish its own
precondition, and a later predicate cannot refine an earlier use.

The replay retains existing scope rules: a successful guard refines later uses,
a true predicate edge refines its unique dominated region, and a shared target
of both branch outcomes establishes neither outcome. False predicate edges do
not acquire the positive constraint. Branch/guard facts must dominate across
all entries, including OSR imports. Optional predicate-edge narrowing remains
disabled in OSR functions.

The result is an immutable snapshot. Consumers must discard it after changing
instructions, operands, CFG edges, or OSR records. Repeated site queries are
lookups in that snapshot, not independent inference runs. A rewrite batch can
retain captured facts only when its transformations preserve the relevant
values and control paths; otherwise it must rebuild the analysis.

For example, `x` remains the original parameter here:

```lisp
(defun scoped-double-add (x other arithmetic)
  (if (typep x 'double-float)
      (if arithmetic (+ x other) x)
      :not-double))
```

The true branch proves DOUBLE-FLOAT at the addition. The global type of `x`
remains unknown. With admitted profile feedback, the site fact can select the
guarded double arithmetic family ahead of a cold fixnum guess. An exact double
input needs no fixnum-to-double contagion conversion; `other` still needs its
own checks and the existing contagion rules. On x86-64 Linux, the canonical
DOUBLE-FLOAT type predicate lowers to an exact heap-tag/widetag test. A distinct
user-package type symbol with the same printed leaf name keeps generic TYPEP
semantics.

## Verified local refinements

Guard discharge keeps a conversion's result SSA identity. For an UnboxFloat
whose exact pre-state proves DOUBLE-FLOAT, it removes the redundant check and
recovery attachment while retaining the payload conversion. The verifier
recomputes the scoped proof independently: clearing a guard flag is not
permission to load an arbitrary pointer. The pure x86 path masks the tagged
pointer and loads the double payload without a call, safepoint, or failure
branch. It never caches an interior pointer across moving GC.

Ranges need an explicit local carrier when the machine emitter consumes
SSA-declared bounds:

```lisp
(defun bounded-increment (x)
  (if (typep x 'fixnum)
      (if (>= x -20)
          (if (<= x 20) (+ x 1) :outside)
          :outside)
      :outside))
```

At the addition, `x` is in [-20, 20]. A fresh pure FIXNUM Guard result can carry
that proven range to this arithmetic use. It is a verified SSA copy, not a new
runtime check. The original value, its other uses, and the math's FrameState
inputs remain unchanged. The existing add/sub emitter can then prove the result
fits and omit JO. It still checks overflow for a range that can cross a fixnum
endpoint; operand type alone does not justify omitting that check.

Every pure refinement must independently imply its complete asserted type and
inclusive range at its own pre-state. A forged stronger range, a false-branch
use, or a use preceding its proof fails verification. The initial pure numeric
refinement producer/emitter integration is limited to x86-64 Linux and declines
functions with OSR entries. This conservative consumer restriction does not
remove the all-entry safety requirements of the inference API.

## Successful-continuation argument contracts

`normal_return_facts::visit_argument_facts` describes original-argument facts,
not just result types. Audited checked tagged arithmetic and type/layout guards
establish their argument constraints only after the instruction succeeds or in
blocks it strictly dominates through every entry. The same callback serves
fixed-point inference and the final site replay. A later use can therefore
reuse a check without narrowing an earlier use or the original SSA declaration.

These initial contracts belong to fixed IR operations whose failed checks leave
optimized execution. They are not summaries inferred for arbitrary Lisp calls.
A generic Call or Invoke may handle an error, use replacement arguments, or
return without validating an argument. Its normal return establishes no such
fact. Predicates still require their unique true edge. Pure refinement copies
relay their independently verified result proof without creating a new fact
about their original input.

## Operand proofs, result vetoes, and feedback

An operand-family proof and a mathematical result interval answer different
questions. FIXNUM operands can produce a bignum. If proven integer bounds put
an operation's entire mathematical result outside the fixnum domain, result
analysis vetoes the fixnum guess and preserves the generic call. An interval
that overlaps the domain does not authorize guard removal. Overflow omission
requires the stronger proof that every result fits, consumed through the
verified local operand ranges above.

These facts are structural proofs, not sampled value ranges. They do not turn
an explicit declined `None` profile entry into a new positive speculation.
An explicit per-body, per-site failed-family policy also vetoes declared-type
selection and alternate numeric producers, including peeled and packed-double
candidates. Valid fixnum operands cannot resurrect a specialization whose
result overflowed. New samples and profile decay retain that rejection; a new
definition resets it. Only matching installed numeric guard metadata establishes
this failure. Packed-array layout/index failures have separate guard semantics.
Domain checks, overflow checks, exceptional continuations, and multiple-value
behavior stay unless their own precise preconditions have been proved. In
particular, a DOUBLE-FLOAT type proof alone does not prove a divisor is nonzero
or a square-root operand is nonnegative.

## Guarded expectations from callee bodies

Return-type expectations are separate from proven types. For example:

```lisp
(defun as-float (x) (float x))
(defun increment-converted ()
  (let ((n (as-float 5)))
    (incf n)))
```

An immutable snapshot of `as-float`, analyzed with the actual argument's type,
can predict a SINGLE-FLOAT result. T2 can choose single-float arithmetic on the
first increment even when that arithmetic has never run. A double argument or
an explicit float prototype determines the corresponding format. Analysis is
bounded; unknown calls, recursion and unsupported bodies remain conservative.

The prediction never changes the call's result declaration or ordinary type
inference. Numeric specialization still checks the actual returned value,
using the arithmetic operation's recovery state after the call. This matters
when a helper redefines the callee while the caller is suspended: generation
validation rejects stale background compilations, but cannot validate a
continuation that has already entered native code. A failed result check must
resume without replaying the call or its effects.

The first implementation consumes expectations for eligible binary numeric
sites, respects failed-site feedback and excludes opaque NOTINLINE dispatch.
It leaves function calls and their multiple-value behavior intact. Captured
callee bodies already participate in worker completion generation validation;
workers read sanitized snapshots, not movable heap constants. Set
`EGCL_DISABLE_RETURN_TYPE_HINTS=1` to disable this analysis for comparison.

Helper availability does not require a T0 invocation sample: a helper already
running in T1 may have none. The owner captures a bounded set of small,
registered direct callees in source order, with separate limits on traversal
and copied bytecode. These hint-only bodies never become inlining candidates
or additional native call targets. The disable flag skips their capture too,
so an ablation includes both capture and analysis cost.
