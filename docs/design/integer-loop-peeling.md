# Narrowing integer loops

T2 can derive a useful steady-state integer type even when the first loop
iteration needs a bignum. Ironclad's Keccak output extraction is one example:
an unsigned 64-bit lane can be a bignum, but shifting it right by eight bits
leaves an unsigned 56-bit value, which fits EGCL's signed 61-bit fixnum.

The optimization is general. It does not recognize Keccak by name or assume
that a general array contains unsigned 64-bit elements.

## Analyze before cloning

`opt_loop_peel` considers small, innermost, reducible loops with one backedge.
Mixed integer feedback must show repeated fixnum execution as well as bignum
execution. A constant right shift, nonnegative mask, or positive constant
modulus/remainder must normalize a carried value to a closed fixnum domain.
Canonical standard-function identity is required for semantic proofs. Known
zero/one-trip loops and loops exceeding the code budget are declined.

The original loop remains the generic fallback and OSR target. Ordinary entry
runs a cloned first iteration, then tests the normalized carried value. A
fixnum enters a separately cloned remainder with a fixnum phi; a larger
integer continues in the original loop. An arbitrary bignum therefore does
not trigger repeated deoptimization simply because one shift was insufficient.

Region cloning preserves source positions, frame states, rematerialization
recipes and exceptional edges. An SSA repair step merges values used outside
the cloned region. The transformation is transactional and must pass the IR
verifier before publication.

## Extract bits without a byte descriptor

`integer_extract` recognizes a private, constant `(byte width 0)` consumed by
`ldb`, for widths 0 through 60. It proves descriptor provenance and single use,
honors `notinline`, and rejects intervening effects or escaping recovery uses.
The replacement `IntegerLowBits` operation validates the integer and masks its
low bits directly. A negative bignum uses the negated low magnitude limb;
higher limbs are irrelevant to these low bits. No helper call, allocation or
safepoint is needed.

The operation retains the pre-`byte` recovery state. An invalid integer resumes
there, recreating the descriptor and preserving the ordinary error behavior.
Only the proven pure argument load is replayed. A post-`ldb` state normalizes
multiple values. Width zero still checks that its argument is an integer.

## Keep machine integers raw

After numeric selection, `unbox_fixnum` chooses raw machine values for eligible
arithmetic islands with a closed loop phi connected to a right shift or byte
extraction. Isolated additions, additive counters and unrelated comparisons stay
tagged: broad conversion can add work and lose the ordinary calling ABI.
Conversions at calls, returns and
incompatible edges are explicit IR instructions. OSR imports remain tagged;
raw constants needed by an OSR entry are rematerialized.

Raw fixnums still obey the signed 61-bit fixnum range. A signed 64-bit overflow
flag alone is insufficient: multiplication and unchecked-range operations also
check that their result fits a fixnum. A failed guard leaves pre-operation SSA
homes intact. Precise deoptimization retags raw values before interpreter
reconstruction. Raw immediates have distinct frame/value-map locations, so bits
that resemble heap pointers cannot become GC roots.

The tagged T2 artifact is retained as a fallback when the raw candidate cannot
be verified, emitted or installed. This implementation targets x86-64 Linux.

## Validation and measurement

Compiler tests cover the real bytecode builders as well as constructed IR,
region repair, range boundaries, machine extraction, raw arithmetic, recovery
and raw constant maps. Native integration targets are `loop_peeling`,
`integer_extraction` and `raw_fixnum`. They require optimization evidence and
compare enabled/disabled execution, including effects, errors, early exits,
multiple values, large integers and OSR.

For same-binary ablation, set the applicable variable to disable a pass:

- `EGCL_DISABLE_LOOP_PEELING=1`
- `EGCL_DISABLE_INTEGER_EXTRACTION=1`
- `EGCL_DISABLE_RAW_FIXNUM=1`

Use a release binary, pinned library forks, separate EGCL/SBCL compiled caches,
and a warmed caller and callee. Scope counters to the measured batches, outside
loading and compilation. Validate the output and report deoptimization deltas.
Run the crypto round trip separately: a component improvement is not an equal
improvement to the complete test run. Keep diagnostic logging out of timings.
