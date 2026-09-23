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

## 6a. Profiling artifacts live in RAM

`/tmp` here is tmpfs. A `perf record` of a babel run is 60–200 MB and a saved
bliss image is 23 MB, so a session that profiles repeatedly quietly accumulates
gigabytes of *memory*. This investigation reached 1.3 GB and got a `cargo test`
run OOM-killed mid-suite.

Delete `*.data` captures and stale images as you go, or write them somewhere
disk-backed.

## 6b. A bytecode-instruction counter cannot tell "native" from "tree-walked"

A counter in `run_loop` reads zero for a function that was promoted to T1/T2
native **and** for one that is tree-walked in `eval_form` — neither executes
bytecode. Twice in this investigation a flat counter was read as evidence of
tree-walking when the real cause was promotion.

The tell that finally worked: call the function **fewer times than the
promotion threshold** (`BLISS_T0_T1_THRESHOLD`, default 10). Below it a
compiled body still runs bytecode, so a genuinely tree-walked body is the only
thing that reads zero. `BLISS_DISABLE_T2=1` alone does *not* isolate this — T1
still promotes, and all three of this session's readings were bit-identical
with and without it.

Counting a call site is not the same as counting execution tiers. To answer
"which tier ran this?", use the threshold, or read the dispatch site.

## 6c. When wall-clock is too noisy, count instructions

Wall-clock on this box could not resolve a ~2% change: with a load average
around 1.5 (a browser, a shell, the agent itself) and core 0 sitting at 1.8 GHz
under the `powersave` governor, six alternating reps of the babel benchmark
spread 14.59–17.21 s — a 2.6 s range against a 0.4 s effect. Five of six paired
reps favoured the patched binary and the sixth contradicted it by more than the
effect size. That is not a measurement.

Retired-instruction count is nearly deterministic for a deterministic program,
and is immune to both frequency scaling and to another process stealing the
core. The same comparison, three reps each:

      patched   134.34G  134.23G  134.85G    mean 134.47G
      baseline  136.56G  136.97G  136.74G    mean 136.76G   -> -1.67%

The ranges do not overlap. That is a measurement.

This is a **hybrid** CPU, so `perf` exposes one counter per core type and an
unpinned run reports `<not counted>` for whichever PMU it missed. Pin the
process AND name the P-core's PMU explicitly:

```bash
taskset -c 0 perf stat -e cpu_core/instructions/u -x, \
    taskset -c 0 ./bliss-cli --no-init --load bench.lisp
```

Caveat: instructions are not time. A change that trades many cheap instructions
for few expensive ones (a cache-missing load, a division) can cut time while
raising the count, or the reverse. Use it to resolve a small effect you cannot
otherwise see, and confirm direction with wall-clock once the machine is quiet.

## 6d. musl's allocator is not glibc's, and the difference is the thread cache

The same source built for `x86_64-unknown-linux-musl` and
`x86_64-unknown-linux-gnu` ran a babel load in 21.54 s and 13.95 s. That is a
libc difference, not a code difference, and it is worth knowing before
attributing a 35% swing to anything you changed.

Isolating it needs the right knob. `MALLOC_ARENA_MAX=1` costs only 2%, which
looks like "per-thread behaviour is irrelevant" and is the wrong conclusion —
arena count and glibc's **tcache** are separate layers, and `arena_max` does not
touch the latter:

```text
  glibc default                13.95 s
  glibc tcache=0               17.99 s   (+4.04)   <- GLIBC_TUNABLES=glibc.malloc.tcache_count=0
  glibc tcache=0, arena_max=1  19.08 s   (+5.13)
  musl mallocng                21.54 s   (+7.59)
```

~53% of the gap is the thread cache. bliss now ships its own for musl
(bliss-05as), which recovered most of it.

Corollary for profiling: `__lock` and `__unlock` high in a musl profile read
like contention and usually are not — mallocng takes its lock on every
operation, so the symbol is really "you called malloc a lot". Confirm with a
single-threaded run or an allocation count before optimising for parallelism.

## 6e. Getting perf call graphs out of the static musl binary

The standing note in this file is that perf "cannot unwind out of musl's
malloc", and by default it cannot unwind much of anything here: the release
build has no frame pointers, and `--call-graph dwarf` also comes back with bare
leaves. That leaves you with self-time only, which is how several hot symbols in
this investigation sat unattributed for a long time.

Build with frame pointers and unwinding works:

```bash
RUSTFLAGS="-C force-frame-pointers=yes" \
    cargo build --release --target x86_64-unknown-linux-musl
taskset -c 0 perf record -F 999 --call-graph fp -- taskset -c 0 ./bliss-cli …
perf report --children --comms <binary-name>          # inclusive
perf report --no-children --symbols memcpy -g caller  # who calls this leaf
```

Two traps that cost time here:

- **`--comms` takes the BINARY name**, so a copied/renamed binary (`bliss-FP`)
  needs that name, not `bliss-cli`. The wrong filter silently reports nothing at
  all rather than erroring.
- This is a hybrid CPU: an unpinned run lands on E-cores and reports
  `cpu_atom/cycles`, with far fewer samples. Pin with `taskset -c 0` to get
  `cpu_core` and a usable sample count.

Frame pointers cost a little speed, so profile with them and *measure* without
them.

## 7. Same path, different build

`target/<target>/release/bliss-cli` is written by every `cargo build`, whatever
features or `RUSTFLAGS` were set. A workspace warning-check silently replaced an
`alloc-count` binary mid-investigation; a `RUSTFLAGS` rebuild replaced a binary
while a test suite was running, invalidating that run. Rebuild deliberately
before each measurement, and do not run `cargo` while a suite holds the lock.
