# Benchmarks (egcl vs SBCL)

Spec G2 makes competitive peak throughput a top-level goal ("within 2x of
SBCL on cl-bench within 2 years"); R10.09/R10.10 mandate a standing suite,
R10.59 min-of-N≥5 runs. This directory is the first slice (bliss-jpd0): a
Gabriel-benchmark core that runs **unmodified on both implementations**, plus
a runner that reports the per-benchmark ratio and whether egcl actually
promoted the workload to T2.

```sh
cargo build --release -p egcl --bin egcl
tests/benchmarks/run.sh              # all benchmarks
tests/benchmarks/run.sh tak fib      # a subset
N=9 tests/benchmarks/run.sh          # more repetitions
```

Method notes (details in `run.sh`'s header): timing is in-process via
`get-internal-real-time` (startup excluded), one untimed warmup precedes the
timed run (peak-tier measurement), the minimum of N runs is reported (wall
clock drifts under load), and the `T2` column distinguishes codegen quality
from promotion-coverage gaps — a `no` there means the number is an
interpreter/T1 number, not a T2 codegen number.

`sumloop` is not a Gabriel benchmark: a hot loop entered once, it tracks the
on-stack-replacement gap (bliss-izt) directly.

`native-call4` measures one million uninlined, four-argument calls to a leaf
that returns its fourth argument without arithmetic or allocation.
`native-call-base` performs the same loop and accumulation without that call.
Both validate the sum and warm the functions through repeated invocations.
Run `tests/benchmarks/run.sh native-call-base native-call4` to compare them.
The time difference estimates added call cost, including argument setup and
dispatch; it is not a measurement of one machine instruction. Four arguments
include one overflow stack argument under the proposed x86-64 Lisp ABI.

The runner's `T2=yes` means some function was published at T2. Before attributing
these timings to native calling conventions, retain an `EGCL_T2_LOG` trace and
verify publication of both `NATIVE-CALL4-LEAF` and `NATIVE-CALL4-LOOP` (and
`NATIVE-CALL-BASE-LOOP` for the baseline). Inspect the generated call site as
well: promotion alone does not prove that it uses the new register ABI.
Record the binary revision, build profile, tier settings and host load; use
release builds on a quiet host for performance comparisons. Sub-millisecond
differences are below this runner's timer resolution. Compare on 64-bit
implementations, where the sum fits a fixnum; a 32-bit implementation may box
the accumulation. The two reported minima come from separate runs, so their
difference is an estimate rather than an exact per-call latency.

`struct-access`, `struct-predicate`, `struct-typecase` and `struct-constructor`
time DEFSTRUCT accessors (through an `:include`d parent), predicates, TYPECASE
over structure types and keyword constructors from warmed callers. These are
the operations EGCL-hosted SBCL compilation spends its time on
(bliss-gg3i1.7), so they track that work's progress directly.

Still to come (tracked on bliss-jpd0): the rest of the Gabriel core (boyer,
browse, puzzle, triangle, …) as stdlib coverage allows, cl-bench proper,
`thresholds.toml` (R10.62) once numbers stabilize, and archived trend results
(R10.10).
