# Typed Fibonacci after bounded pure-recursion evaluation

Measured runtime revision: `3b30cff234cd3ec3b930e01588471fb9db33f27e`.
Related work: [PR #326](https://github.com/atgreen/evergreen/pull/326),
Beads `bliss-albpd.1.4`, `bliss-tw3g5.1`, and `bliss-dpuvm`.

## Release measurements

Three serial processes per implementation, seven timed `fib(35)` calls per
process, pinned to CPU 0. Each process warmed with 6000 calls to `fib(8)`;
EGCL additionally waited for and asserted installed T2. Every process checked
inputs 0 through 40 against an independent iterative oracle before timing.
Every timed result was 9227465. Runs used `scripts/egcl-limited.sh` with
`EGCL_MEM_MAX=1G`, no swap, and `EGCL_TIMEOUT=180`.

| Implementation | Median | Minimum | Maximum |
|---|---:|---:|---:|
| EGCL release | 30 ms | 29 ms | 31 ms |
| Tuned SBCL 2.6.8 | 33 ms | 33 ms | 34.001 ms |

Raw elapsed ticks, one row per process:

```text
EGCL (1000 ticks/second)
29 30 30 29 30 30 30
30 30 30 31 30 30 30
29 30 29 30 30 29 30

SBCL (1000000 ticks/second)
33000 33000 33000 33000 33000 34001 33000
33000 34000 33000 33000 33000 33000 33000
34000 33000 33000 33000 33000 33000 33000
```

The approximately 9% lower median elapsed time is specific to this workload,
host, and source tuning. These measurements do not prove general compiler
parity, equivalent safety guarantees, or completion of the architectural epic.
The earlier seven-sample 93 ms EGCL baseline used a development build, not a
matched release baseline; do not present it as a release before/after result.

## Sources and reproduction

EGCL source (unchanged by the optimization):

```lisp
(declaim (optimize (speed 3) (safety 0) (debug 1)))
(declaim (ftype (function ((integer 0 40)) (integer 0 102334155)) fib))
(defun fib (n)
  (declare (type (integer 0 40) n))
  (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
```

The FTYPE declaration above is currently ignored by EGCL; it is not evidence
of result-type propagation. Checked parameter metadata is retained, but the
full parameter interval is not yet propagated as a T2 range fact.

SBCL source, compiled to a FASL before the timed process loads it:

```lisp
(eval-when (:compile-toplevel)
  (setf sb-ext:*inline-expansion-limit* 12))
(declaim (optimize (speed 3) (safety 0) (debug 0)
                  (space 0) (compilation-speed 0)))
(defun fib (n)
  (declare (type (unsigned-byte 64) n))
  (labels ((rec (n)
             (declare (type (unsigned-byte 64) n))
             (if (< n 2) n
                 (the (unsigned-byte 64)
                      (+ (rec (- n 1)) (rec (- n 2)))))))
    (declare (inline rec)
             (ftype (function ((unsigned-byte 64))
                              (values (unsigned-byte 64) &optional)) rec))
    (rec n)))
```

Shared driver, run after loading and warming the appropriate function:

```lisp
(defun reference-fib (n)
  (let ((a 0) (b 1))
    (dotimes (i n) (psetq a b b (+ a b)))
    a))
(locally (declare (notinline fib))
  (dotimes (n 41) (assert (= (fib n) (reference-fib n))))
  (format t "CORRECTNESS 0..40 OK~%")
  (dotimes (sample 7)
    (let* ((start (get-internal-real-time)) (answer (fib 35))
           (elapsed (- (get-internal-real-time) start)))
      (assert (= answer 9227465))
      (format t "FIB35-SAMPLE ~D ~D ~D ~D~%"
              sample elapsed internal-time-units-per-second answer)
      (finish-output))))
```

Keep NOTINLINE local to the caller. A global proclamation before lazy
compilation disables the self-call proof and measures a different path.

EGCL build command, inside the existing 6G/900s capped build scope:
`CARGO_INCREMENTAL=0 cargo build --release -j 2 -p egcl --bin egcl`.
The existing `target/hotspot-build` cache was reused, targeting
`x86_64-unknown-linux-musl`; no additional release cache was created.

SHA-256 fingerprints:

```text
EGCL release executable
468a81cc0f7f222a45087df05f9f2e435b4e6a089828670ef2972c3a82731eac
SBCL executable
e6ff14f816e7a236010eaec058c886751d93fbbfde26c8dbfed312c351a019b6
SBCL tuned FASL
fdf562a8091a2418810517af77da94b2093e1e651858452d97a779c2222cf4d1
```

## What the machine code establishes

EGCL's installed artifact is 2372 bytes, including cold paths. At both direct
self-call sites it polls before trying the existing `n < 2` pure prefix and
new exact-input results for child arguments 2, 3, and 4. The generic compiler
evaluator derives those constants from source-proven pure recursion; it has
no Fibonacci-specific rule. Misses still make direct self-calls.

Remaining EGCL recursion uses tagged fixnums and the ordinary Lisp ABI,
including physical-stack admission, result guards, multiple-value handling,
and overflow branches. SBCL's tuned local REC uses raw unsigned integers,
expanded recursive branches, and a single raw result; its shown additions
have no overflow branches. Its 2784-byte listing includes the outer wrapper
and local helper, so code size alone does not compare hot-path work.

FTYPE result propagation, nested-deoptimization self-inlining, and a private
unboxed local-call ABI remain incomplete. Full-branch integration and
cross-platform gates are separate from this benchmark validation.
