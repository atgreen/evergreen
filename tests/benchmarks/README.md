# Benchmarks (torcl vs SBCL)

Spec G2 makes competitive peak throughput a top-level goal ("within 2x of
SBCL on cl-bench within 2 years"); R10.09/R10.10 mandate a standing suite,
R10.59 min-of-N≥5 runs. This directory is the first slice (bliss-jpd0): a
Gabriel-benchmark core that runs **unmodified on both implementations**, plus
a runner that reports the per-benchmark ratio and whether torcl actually
promoted the workload to T2.

```sh
cargo build --release -p torcl --bin torcl
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

Still to come (tracked on bliss-jpd0): the rest of the Gabriel core (boyer,
browse, puzzle, triangle, …) as stdlib coverage allows, cl-bench proper,
`thresholds.toml` (R10.62) once numbers stabilize, and archived trend results
(R10.10).
