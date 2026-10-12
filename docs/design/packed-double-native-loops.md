# Native packed double-float loops

On Linux x86-64, mapped T2 can keep intermediate values from simple rank-one
`DOUBLE-FLOAT` arrays in raw F64 homes. The initial specialization covers closed
expressions built from `AREF`, binary `+` and `*`, ending in `(SETF (AREF ...))`:

```lisp
(defun scale-and-add (src dst n scale)
  (dotimes (i n)
    (let ((x (aref src i)))
      (setf (aref dst i) (+ (* x scale) x))))
  dst)
```

Use simple packed double-float vectors and a double-float scalar, such as `2d0`.
The initial mapped entry does not admit parameter type declarations. Arguments
and returns remain tagged Lisp values; the raw intermediates cannot escape into
ordinary call arguments, returns, or block-edge arguments. Local noncapturing
block/tagbody control uses the existing managed activation. Dynamic control
scopes and multiple-value bodies are outside this specialization's admission.

Compilation checks canonical builtin identity and retains live binding guards
for mutable array primitives. Each access guards the array representation and
element type, the fixnum index, and the bounds before accessing memory. Scalar
unboxing requires a double-float object. A failed guard resumes the original
boxed semantics through precise deoptimization. For example, a fixnum scalar
is valid for the Lisp expression above, but takes this recovery path.

## Collection and recovery

The tagged array base remains a GC root across collecting calls and polls.
Generated code derives the element address again at each access, using the
current base after relocation. It never carries an interior data pointer across
a collecting call. Raw F64 homes are excluded from the root set, even when their
bits resemble a tagged pointer.

Recovery metadata distinguishes tagged and raw slots. Deoptimization roots the
tagged sources before boxing raw doubles and roots each completed box before
another allocation. The resulting boxed payload is then passed to the existing
frame reconstruction path.

## Entry and validation limits

This slice supports full function invocation only. The compiler builds a
separate packed candidate before generic numeric speculation and clears OSR
entries only in that candidate's mapped clone. The original tagged compilation
path is unchanged. Representation-aware raw OSR imports are separate work
tracked by `bliss-0zgjy.5`; this change does not claim OSR acceleration.

With `EGCL_T2_LOG=stderr`, successful emission reports `raw_f64_values=N`.
A nonzero count followed by the mapped-T2 installation message identifies the
installed representation; it does not by itself prove execution or performance.
The public-loop tests additionally observe the executing installed owner, force
both arrays to relocate, check subsequent results without deoptimization, and
exercise scalar-guard recovery. Compiler tests cover extended object headers,
type/bounds guards, and moving collection in generated code.

No speedup is claimed here. Paired public-kernel measurements and their path
evidence are tracked separately in `bliss-0zgjy.6`.
