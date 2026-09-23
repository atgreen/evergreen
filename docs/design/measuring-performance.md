# Measuring performance in bliss

Status: **notes from practice** · Related: bliss-iry5, bliss-ou03, bliss-sgis,
bliss-yb10, AGENTS.md ("Never compare timings of two debug binaries",
"Benchmarking a tiered loop")

Written after an investigation into `asdf:load-system :babel` that produced
several confident, wrong conclusions before producing a right one. Each section
below cost at least one wasted iteration. AGENTS.md already warns about debug
profiles and tiered-loop thresholds; this covers the traps below those.

## 1. There are two heaps, and only one of them is expensive

| | allocator | cost | how to measure |
|---|---|---|---|
| Lisp values (cons, string, instance) | GC nursery, bump pointer | ~free per byte | `ROOM` → `total consed` |
| Rust side (`Vec`, `String`, `Rc`, `HashMap`) | musl `malloc` | expensive | `alloc-count` feature |

In a release profile of a call-heavy workload, `alloc_slot` (the Lisp bump
allocator) was **1.9%** while `__libc_malloc_impl` + `__libc_free` + musl's
`__lock`/`__unlock` were **~26.5%**.

**Consequence:** `ROOM`'s `total consed` is the wrong instrument for a speed
question. Measured directly: a change that cut a no-op babel reload's consing by
4.3% (96.3 MB → 92.1 MB) changed CPU time by **exactly nothing** (24.005 s →
24.03 s). Lisp consing that dies immediately is close to free, because minor-GC
cost scales with what *survives*.

Use `ROOM` for footprint and GC-pressure questions. Use `alloc-count` for speed.

## 2. perf cannot unwind out of musl's malloc

`perf record --call-graph dwarf` symbolises bliss frames fine, but musl has no
CFI for `malloc`, so the callers of `__libc_malloc_impl` do not resolve — the
stack above it comes back blank. The same is true for `alloc_slot`.

That is why the `alloc-count` cargo feature exists (`crates/bliss/src/main.rs`).
It wraps the global allocator:

```bash
cargo build --release -p bliss-cli --features alloc-count

# totals
BLISS_PROBE_ALLOC_COUNT=1 ./bliss-cli … 2>&1 | grep @ALLOCS

# attribution: backtrace on every Nth allocation
BLISS_PROBE_ALLOC_SAMPLE=10000 BLISS_PROBE_ALLOC_SAMPLE_OUT=/tmp/bt.txt ./bliss-cli …
```

Then aggregate `/tmp/bt.txt` (blocks separated by `=====`) on the first frame
that is not allocator machinery. Full symbolisation works on the musl static
build.

**Totals need a counting run; distributions need a sampling run. Never take a
total from a sampling run** — capturing a backtrace allocates, which inflated one
measurement from ~104M to 143.75M.

## 3. Subtract a baseline, and take more than one of each

Every workload measurement is a delta against a startup-only run. Both sides
vary, and a single baseline is not enough: one run of this shape produced a
*negative* delta and a spurious "7.9M → 4.25M improvement" that vanished on
repetition. Three baselines and three workload runs, compared by median, gave
the truth (7.86M → 7.88M, i.e. no change).

## 3a. Discard the first run after building an image

Two measurements in this investigation came back at 61.9M allocations where
every neighbour was ~26M — a 36M spike, about 4.6 `make-plan`s worth. Both were
the *first* run against a freshly written image. Eight consecutive runs of the
same binary afterwards were all 25.8–26.0M with no outlier.

Throw the first run away, or warm the image before measuring.

## 4. This machine's CPU makes small wall-clock deltas meaningless

Two effects, both documented in AGENTS.md for other reasons:

- **Hybrid cores.** P-cores 0–5, E-cores 6–13. A run landing on an E-core is
  ~2x slower. Pin with `taskset -c 0`.
- **Frequency scaling.** Cores idle at 400–1600 MHz. A benchmark run *second*
  benefits from the ramp-up the first one caused.

That second one is a systematic bias, not noise: always running `before` then
`after` reported a **~8%** win that shrank to **~1.3%** once the order was
alternated. Alternate the order, pin the core, and prefer CPU time (`%U`) over
wall clock. Also watch for your own background builds — a stray `find` inflated
three consecutive measurements by ~2x.

## 5. Measure which sites are hot; do not infer them

The repeated failure mode in this investigation was picking an optimisation
target by reading code, confirming it was *plausible*, fixing it, and measuring
zero. It happened with the T2 compile queue (a real pathology on a path never
taken: 0 queue-full events even at `BLISS_COMPILE_QUEUE_SIZE=1`), with CLOS slot
access, with pathname accessors chosen from a microbenchmark rather than the
workload, and with `get_record` cloning (refuted: ~28,000 record accesses cannot
explain 7.86M allocations).

A chain of measured numbers that are individually consistent is **not**
attribution. `make-pathname` 105 allocations → `apply-output-translations` 6,479
→ `input-files` 10,610 → `required-components` 1.37M → `make-plan` 11.7M was
every-level-measured and still led to the wrong root cause, because the link
from the bottom of the chain to a specific line was inferred.

The cheap instruments that actually settled these:

- an env-gated `eprintln!` at a candidate site, counted with `sort | uniq -c`,
  with markers on `*error-output*` so the measurement window excludes startup
  (putting markers on stdout once made per-call figures ~130x too high)
- `BLISS_PROBE_ALLOC_SAMPLE` for allocation attribution
- ablation: delete the work behind an env flag and measure the ceiling *before*
  designing a fix

## 6. A loop that discards its result measures nothing

`(dotimes (i 50000) (class-of *o*))` allocates zero — the call is dead-code
eliminated. Consume the result (`(setf *sink* …)`) or the probe is meaningless.

## 7. Same path, different build

`target/<target>/release/bliss-cli` is written by every `cargo build`, whatever
features or `RUSTFLAGS` were set. A workspace warning-check silently replaced an
`alloc-count` binary mid-investigation; a `RUSTFLAGS` rebuild replaced a binary
while a test suite was running, invalidating that run. Rebuild deliberately
before each measurement, and do not run `cargo` while a suite holds the lock.
