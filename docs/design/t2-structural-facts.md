# Structural value facts in T2

T2 inference returns numeric type/range/representation facts and a structural
fact snapshot. `value_facts::StructuralFacts` is also available independently
when a verifier or structural transform needs no numeric analysis. Rebuild it
after changing instructions, edge arguments, or OSR entries.

The shared domain distinguishes exact object identity, successful simple-string
layout guards, and nullable packed-U8 layout witnesses. A broad STRING or VECTOR
type is insufficient to justify direct storage access. A nullable witness
proves which object was inspected; it does not prove that the result is non-NIL.
Consumers retain the witness failure branch, bounds checks, and value checks.

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
in this domain. Mutable contents, lengths, displacement, and adjustable backing
storage require separate effect and alias reasoning.

Inline allocation currently has a conditional postcondition: only its validated
bump-allocation success proves the small packed-U8 layout. Its collecting slow
call remains generic. The existing adjacent-probe optimization retains its
poll, multiple-value, exact-result and final-home restrictions. Do not mark the
whole constructor Call result as packed-U8 based on its name or a mutable type
descriptor. Making fast/slow allocation paths explicit in IR is a further step
needed before general IR passes can consume that conditional postcondition.

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
