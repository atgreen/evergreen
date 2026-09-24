# Babel load performance: state, method, and open leads

Written as a handoff. Everything here is measured; where something is a guess it
says so. If you read one section, read **"How to measure in this codebase"** —
this system defeats reasoning-from-source with unusual consistency, and most of
the wasted effort in this investigation came from skipping it.

## Shift integers instead of exponentiating (2026-09-24, bliss-7jt1)

A fresh load-only profile after `d4427b8` put minor GC at 9.8% inclusive,
but also exposed **8.0% under ASH**. The bootstrap implementation calculated
powers of two and multiplied or divided, paying for general rational arithmetic,
GCD, interpreter calls, and temporary values merely to shift integers.
`bliss-stdlib::numbers::ash` now shifts fixnums directly and bignums limb-wise.
Both tree-walked and compiled builtin calls use that kernel; the bootstrap
workaround is gone. Right shifts preserve negative rounding and huge-count
saturation. Both arguments remain type-checked, including zero cases.

Five alternating CPU-0-pinned release runs against `d4427b8`, each a fresh
process with its build-specific FASL cache populated, before any test jobs:

| Median | Before | Direct integer shifts |
|---|---:|---:|
| Initial Babel FASL load | 1.362 s | **1.250 s (8.2% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| Bliss / SBCL | 4.8× | **4.4×** |
| Initial-load instructions (three separate isolated windows) | 6.250 G | **5.583 G (10.7% less)** |
| Whole-process instructions | 19.502 G | 18.827 G |
| Minor-GC pause during load | 0.146229 s | 0.146936 s |

Every measured load still performs one minor and zero major collections.
The change removes roughly 5.6 MB of temporary Lisp allocation per load;
it does not defer collection or move work outside the timer. Load wall samples
span 0.922–1.366 s before and 1.109–1.255 s after. Whole-process medians are
3.83 s before and 3.71 s after, with wide ranges of 2.79–3.86 s and 3.11–3.74 s.
The isolated instruction counts support the improvement despite timing noise.
This remains a substantial gap to SBCL, not parity.

The regression first failed with `undefined function: ASH` before bootstrap
(R5.06). Tests now check 247 operand/count combinations through ordinary calls,
FUNCALL, and APPLY in interpreter, T0, and T1 modes, including function
rebinding. A direct-kernel test checks 150,000 small shifts without allocating
Lisp heap objects, fixnum boundaries, and oversized counts. A source-free FASL
test rejects legacy source and EvalSource actions, then exercises allocating
native calls under GC stress strides 1, 7, and 31 with poisoning and verification.
Normal and stress-20,000/poison/verify Babel runs match a fresh SBCL run on all
15,106 reverse-table entries. Review is adversarial self-review, not independent.
All 12 Dietz ANSI ASH tests also pass, including randomized fixnum/bignum shifts,
huge negative counts, type errors, and argument evaluation order.

The full non-CLI workspace gate reports 1,767 passed, six failed, four ignored.
The CLI gate reports 715 passed, three failed, three ignored, plus an aborted
parallel unit-test process. Its serial rerun passes all 34 units. All 357
acceptance, 49 FASL, and 12 bytecode differential tests pass. Known failures
remain in STRINGP metadata (`bliss-4ihx`), four sequence fixtures (`bliss-0bdl`),
two Lisp-containing doctests (`bliss-pqyy`), asynchronous T2 observation
(`bliss-ugmu`, also fails alone), and the parallel runtime lifecycle
(`bliss-lb6.20`). The additional reader property-test failure is an unchanged
generator/oracle mismatch: `Symbol("NIL")` renders as `NIL` and reads as `Nil`
(`bliss-stz9`). Workspace check passes with the tracked unused-mut warning;
root lint has six baseline findings and zero new ones. Strict clippy still
stops at nine baseline runtime errors; a separate non-strict stdlib run finds
nothing in the new module. Existing spec-coverage and rustfmt failures remain
(`bliss-kjjd`, `bliss-2uj1`); none of the formatting diagnostics target the new
module or tests. These gates are not represented as fully green.

The valid profile is `/tmp/babel-current-single.perf`; an earlier recording
around three child processes lost symbol mappings and was discarded as evidence.
Measurement artifacts are `/tmp/babel-ash-final-*`, with the benchmark driver
`/tmp/babel-ash-final-bench.sh`. Remaining copy/C3 and external-root leads are
tracked in `bliss-uqle` and `bliss-399z`; neither is a proven next optimization.

## Walk dead nursery objects once (2026-09-24, bliss-j0wa)

Minor GC separately walked all nursery objects to repair TLAB gaps, find pinned
regions, and build the exact-object-start index. Its evacuation pass then walked
them again to skip the dead objects. Preparation now combines the first three
walks; evacuation enumerates the live bitmap directly, in the original address
order. This removes work, without enlarging the nursery or deferring collection
past the load timer. Pinned-region retention, persistent forwarding, large
filler footprints, and exact-start validation remain intact.

Five alternating CPU-0-pinned release runs against `b31a3d0`, using fresh
processes and populated build-specific FASL caches:

| Median | Before | Single preparation walk + live iteration |
|---|---:|---:|
| Initial Babel FASL load | 1.412 s | **1.361 s (3.6% less)** |
| SBCL initial load, same batch | 0.287 s | 0.287 s |
| Bliss / SBCL | 4.9× | **4.7×** |
| Minor-GC pause during load | 0.195733 s | **0.146118 s (25.3% less)** |
| Initial-load instructions (three separate isolated windows) | 6.426 G | **6.256 G (2.6% less)** |
| Whole-process instructions | 19.689 G | 19.496 G |

Every measured Bliss load performs one minor and zero major collections.
Wall samples remain noisy: 1.073–1.415 s before, 1.036–1.362 s after.
Whole-process medians, including ASDF startup, were 3.51 s before and 3.85 s
after despite fewer instructions; this batch does **not** establish a startup
speedup. The isolated instruction check ran concurrently with tests, so its wall
times are not used. The five-run wall batch finished before workspace tests.

The deterministic regression first failed at **8,101 header reads for 2,000
mostly-dead objects**. It now reads **2,099** headers, passes a bound of fewer
than 2,256 reads, and checks the surviving cons's relocation and contents.
Other new tests cover bitmap
word/region boundaries and reset, zero-filled TLAB gaps, pinned forwarding
stubs, and large-header filler spans. All 625 runtime tests pass, including
61 unit tests.
Normal and stress/poison/verify runs match SBCL on all **15,106** Babel
reverse-table entries. An every-allocation stress/poison/verify run also
preserves source-free native-cons output `CONS-STRESS (100 99 0 1)`.
Review is adversarial self-review, not independent.

The complete workspace gate records **2,512 passed, seven failed, seven
ignored**: non-CLI 1,767/5/4 and CLI 745/2/3. All 357 acceptance, 48 FASL,
and 12 bytecode differential tests pass. The failures are the tracked compiler
STRINGP metadata test (`bliss-4ihx`), four sequence fixtures (`bliss-0bdl`), and
two Lisp-containing doctests (`bliss-pqyy`). The previously flaky T2 observation
test passes in this run. Workspace check passes with the existing unused-mut
warning (`bliss-d3hs`); root lint reports six baseline findings and zero new
ones. Clippy still reports nine existing runtime diagnostics (`bliss-5vr`),
spec coverage has 14 uncovered stage-5 requirements (`bliss-kjjd`), and existing
rustfmt drift remains (`bliss-2uj1`); none of its diagnostics target new code.

Several tempting alternatives were measured and discarded in this iteration:
static multiple-value classifier shortcuts and function-cell checks each saved
less than 1% (`bliss-amyr`); keyword-only direct-slot binding saved 0.33%
(`bliss-8cko`); allowing cold restart instructions to deopt promoted
`MAKE-ACTION-STATUS` to T1 but saved only 0.7% (`bliss-yun1`). Blanket invocation
uncommon traps broke bootstrap with an unbound `TESTFN` (`bliss-r03w`). Removing
the runtime destructuring-lowering restriction still left the observed ASDF
functions uncompiled (`bliss-jx2n`). All those probes were removed. Lisp-leaf
sample percentages were not removable dispatch costs.

Refresh the now-stale load-only profile next (`bliss-ixj5`). The remaining
external-root scanning lead is `bliss-399z`. The previous phase
probe's 33 ms marking and 33 ms relocation are attribution leads, not a proven
optimization or a reason to skip root scans.

## Reuse minor-GC evacuation destinations (2026-09-24, bliss-b03z)

A throwaway action-timing probe ruled out a large remaining binary-decoder win.
Alexandria's source-only `types` and `numbers` FASLs cost about 44 ms combined;
ASDF's source definitions cost about 114 ms. Babel's major initializers already
execute compiled load thunks. The 426 ms `enc-jpn` load includes the 267 ms minor
GC, rather than representing 426 ms of Lisp execution. Missing numeric literal
formats are tracked in `bliss-ujsc`, not treated as the primary remaining gap.

The GC probe found **346,384 live copies and 317,068 failed initial copy
attempts**. After the first survivor region filled, the collector kept trying
that same full region for every object, then searched all regions for another
destination. The copy phase alone took 101 ms. The collector now retains its
current survivor and old-generation destinations until they fill. Object-size
checks, pinned-host exclusion, root relocation, and collection timing remain
unchanged. All throwaway profiling instrumentation was removed.

Five alternating CPU-0-pinned release runs, each a fresh process loading an
already populated build-specific FASL cache, against `ff97468`:

| Median | Before | Destination reuse |
|---|---:|---:|
| Initial Babel FASL load | 1.476 s | **1.410 s (4.5% less)** |
| SBCL initial load, same batch | 0.287 s | 0.287 s |
| Bliss / SBCL | 5.1× | **4.9×** |
| Minor-GC pause during load | 0.258 s | **0.196 s (23.8% less)** |
| Initial-load retired instructions (three isolated windows) | 7.090 G | **6.422 G (9.4% less)** |
| Whole-process retired instructions | 20.349 G | 19.687 G |

Both versions collect exactly once (minor, not major) inside the timer. Load
samples span 1.255–1.489 s before and 1.267–1.421 s after. Whole-process wall
time, including ASDF startup, was especially noisy: medians 3.62 s before and
3.88 s after, despite fewer instructions. Do not infer a startup speedup from
this batch. The initial-load improvement is incremental; parity is not close.
The instruction-window repeats vary by less than 0.1% per binary. CLI tests
were still active during that count-only check, so its wall timings are not
used; the five-run wall-time batch above ran before workspace validation.

The deterministic regression first failed at 2,001 searches for 2,000 conses.
It now bounds searches by the number of destination regions, checks every
list element, and covers both survivor copying and immediate promotion.
The full runtime suite passes all 621 tests. Normal execution and
`BLISS_GC_STRESS=20000 BLISS_GC_POISON=1 BLISS_GC_VERIFY=1` agree with SBCL on
all 15,106 Babel reverse-table entries. An every-allocation stress/poison/verify
probe also preserves source-free native-cons results. Root lint has no new
findings. Review is adversarial self-review, not independent.

The complete workspace gate records **2,507 passed, eight failed, seven
ignored**, split into serial non-CLI tests and process-isolated CLI tests with
eight workers. All 357 acceptance and 48 FASL tests pass. The eight failures
are the tracked baseline compiler STRINGP metadata test (`bliss-4ihx`), four
sequence fixtures (`bliss-0bdl`), two Lisp-containing doctests (`bliss-pqyy`),
and asynchronous tier-observability assertion (`bliss-ugmu`). Workspace check
passes with the existing unused-mut warning (`bliss-d3hs`). Clippy still reports
nine existing runtime diagnostics (`bliss-5vr`); spec coverage has 14 uncovered
stage-5 requirements (`bliss-kjjd`). Widespread existing rustfmt drift is tracked
separately in `bliss-2uj1`.

The pre-change phase probe also measured roughly 60 ms indexing nursery
objects, 33 ms marking external roots, and 33 ms relocating external roots.
These are further profiling leads, not proven optimizations. The analogous
per-object major-GC destination search is filed separately as `bliss-2f9l`;
major GC is not on this initial-load path.

## Remove repeated runtime work from first FASL load (2026-09-23)

The next profile led to three general changes (`bliss-djhf`, `bliss-vbar`,
`bliss-7pyh`):

- Emit native `AllocCons` instructions, so portable quasiquote bytecode no
  longer disqualifies an entire function/loop from T1 or OSR.
- Remove T1's obsolete whole-function purity gate. Guards already resume at
  the exact failing bytecode instruction, so arithmetic after an impure call
  can speculate without replaying that call. Tests check actual deoptimization
  and exact side-effect counts through both invocation promotion and OSR.
- Borrow class metadata during read-only slot-owner traversal, instead of
  cloning every visited class, its slots, and its default initargs. The cycle
  set also borrows class names. There is no new cache or invalidation policy;
  instance-slot shadowing and the existing missing-graph fallback are preserved.

Three repeated isolated-load measurements against `89bab35`: median **7.769 G →
7.088 G instructions**, **8.8% less**. Native cons plus the purity-gate removal
alone measured 7.593 G; borrowing class metadata accounts for most of this
iteration's improvement. These are fresh processes loading existing FASLs;
source-compilation runs are excluded, and one minor GC remains inside the timer.

Five alternating CPU-0-pinned release runs, after all test processes finished:

| Metric (median) | `89bab35` | This change |
|---|---:|---:|
| Initial Babel FASL load | 1.628 s | **1.475 s (9.4% less)** |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| Bliss / SBCL | 5.7× | **5.2×** |
| Whole-process wall time, including ASDF startup | 4.07 s | 3.96 s |

Baseline load samples ranged 1.383–1.632 s, candidate 1.474–1.490 s, SBCL
0.285–0.287 s. Both Bliss versions collected once during each timed load;
candidate GC time was about 0.260 s versus 0.255 s baseline, so the gain did
not come from deferring collection. This is incremental progress, not parity.

Rejected experiments matter for the next session:

- Lowering named OSR thresholds to 200: 7.732 G instructions versus 7.767 G
  default. Anonymous threshold 10: 7.757 G; anonymous OSR disabled: 7.759 G.
  Earlier promotion is not a large remaining lever in this workload.
- Enabling OSR uncommon traps: 7.655 G, also small.
- Native `MakeClosure` added only about 1% beyond native cons. That prototype
  was removed before landing: baked body pointers need an ownership audit
  across self-redefinition and direct native calls (`bliss-sg8w`, `bliss-ku9w`).
  A passing Lisp redefinition probe did not establish ownership safety.

A fresh frame-pointer profile (1,542 load-only samples) puts class-slot lookup
at 1.4% inclusive, down from roughly 6%. Remaining sampled costs include minor
GC 17%, source-reading/evaluation 10.7%, extended LOOP evaluation 6.8%,
multiple-value classification 5.8%, and variadic binding 4.7%. These overlap;
do not sum them. Constant materialization is only about 1.6%. Next leads are
remaining source-evaluated initializers (`bliss-mbzt`) and redundant
multiple-value classification (`bliss-amyr`), not a binary-decoder rewrite.

The real Babel load under `BLISS_GC_STRESS=20000 BLISS_GC_POISON=1
BLISS_GC_VERIFY=1` matches normal execution and SBCL for all 15,106 entries in
the two reverse tables. The focused final suites pass: 48 FASL tests, five OSR,
14 T1-deoptimization tests (one ignored), and five T1-native tests. Root lint
has zero new findings. Review was adversarial self-review, not independent.

The final workspace test run recorded **2,506 passed, eight failed, seven
ignored**. All 357 CLI acceptance tests pass. Failures remain the same tracked
baseline issues listed in the preceding iteration: `bliss-4ihx`, `bliss-0bdl`,
`bliss-pqyy`, and `bliss-ugmu`. Workspace check passes with the existing warning
(`bliss-d3hs`); Clippy still stops at nine runtime diagnostics (`bliss-5vr`),
and spec coverage at 14 uncited stage-5 requirements (`bliss-kjjd`).

## Compile encoding-table initializers (2026-09-23, bliss-bd8a)

The next initial-load profile isolated `asdf:load-system` with a `perf` control
FIFO, excluding ASDF startup. About 25% of samples were source evaluation inside
the FASL loader. Action timing identified two expensive reverse-table builders:
`+UNICODE-TO-JIS-X-0208+` and `+UNICODE-TO-KSC-5601+`. Their `LOOP` forms fell back
to `EvalSource` because the bytecode compiler lacked `ACROSS` and `OF-TYPE`
iteration clauses. Binary decoding was not the bottleneck.

Five alternating CPU-0-pinned release runs, each in a fresh process with its
build-specific FASL cache already populated, against saved baseline `66c6d4f`:

| Metric (median) | Before | Compiled initializers |
|---|---:|---:|
| Initial Babel FASL load | 1.964 s | 1.631 s (**17.0% less**) |
| SBCL initial load, same batch | 0.286 s | 0.286 s |
| Bliss / SBCL | 6.9× | **5.7×** |
| Initial-load retired instructions (three isolated windows) | 9.569 G | 7.761 G (**18.9% less**) |
| Initial-load allocation | 32.56 MB | 26.83 MB (**17.6% less**) |
| Whole-process retired instructions | 22.975 G | 21.177 G |
| Whole-process wall time, including ASDF startup | 4.26 s | 4.14 s |

No measured run compiled source. Both versions still perform **one minor GC**
during the timed load; collection was not deferred or moved outside the timer.
Wall time remains noisy even when pinned: baseline load samples ranged from
1.775–1.968 s, candidate 1.185–1.645 s, SBCL 0.186–0.288 s. Compare within this
batch, not against the earlier entry's absolute times. The three isolated
instruction measurements reproduce within 0.2% for each binary.

The compiler now lowers those clauses into portable bytecode. `ACROSS` evaluates
its vector and active length once, reads elements as the loop advances, and
supports strings, bit vectors, fill pointers, and multiple iteration drivers.
`OF-TYPE` is accepted as an unspecialized declaration. This is general compiler
support, not a Babel-specific cache or a change to when load-time work occurs.

GC stress exposed three correctness bugs, fixed alongside the optimization:

- Recursive `LOOP IT` substitution must root both the source tail and rebuilt
  head. Losing either could silently force source fallback during compilation.
- The reader must zero packed bit-vector payloads before OR-ing in one bits.
  Poisoned/reused nursery bytes otherwise changed `#*101` into `#*111`.
- Closure construction must root its cloned bytecode body **before** allocating
  the installed lambda list. Otherwise moved literal constants become stale
  before registry installation. A stride-7 regression catches this; collection
  on every allocation happened to promote the literal early and miss it.

Validation includes source-free FASL action assertions, deleting source before
load, every-allocation GC/poison testing, and a real Babel load with
`BLISS_GC_STRESS=20000 BLISS_GC_POISON=1 BLISS_GC_VERIFY=1`. All 15,106 entries in
the two reverse tables match SBCL, and stressed output matches normal output.

The workspace test run (non-CLI serial, CLI four threads) recorded **2,502 passed,
eight failed, seven ignored**. The failures are separately tracked baseline
issues: compiler STRINGP metadata (`bliss-4ihx`), four invalid sequence fixtures
(`bliss-0bdl`), two unlabelled Lisp rustdoc examples (`bliss-pqyy`), and a
40-call asynchronous tier-promotion assertion (`bliss-ugmu`). The latter also
fails on the saved baseline in all five control runs, with correct Lisp values.
All 47 FASL tests pass. Root lint has zero new findings; workspace check passes
with the existing unused-mut warning (`bliss-d3hs`). Clippy remains blocked by
existing runtime lints (`bliss-5vr`), and spec coverage by 14 uncited stage-5
requirements (`bliss-kjjd`).

The next larger hypothesis is selective native execution of one-shot FASL
initialization loops (`bliss-y625`): ordinary hotness thresholds may not pay off
before a load-time loop finishes. Profile the new load before changing policy,
and include compilation cost in the first-load timer. Class-metadata cloning
(`bliss-7pyh`) accounted for about 5% of the pre-change initial-load profile;
it is not an explanation of the entire remaining gap.

## Initial FASL load update (2026-09-23, bliss-w337)

The active target is now the **first** `asdf:load-system` in a fresh process,
with already-compiled FASLs, not no-op reloads. Five alternating CPU-0-pinned
release runs against saved baseline `94f7570`, with SBCL in the same batch:

| Metric (median) | Before | Nursery bitmaps |
|---|---:|---:|
| Initial Babel FASL load | 2.428 s | 1.686 s (**30.6% less**) |
| SBCL initial load | 0.247 s | 0.247 s |
| Bliss / SBCL | 9.8× | **6.8×** |
| Minor GC during initial load | 0.925 s | 0.234 s |
| Whole process, including ASDF startup | 4.67 s | 4.08 s |
| Whole-process retired instructions | 24.585 G | 22.976 G |
| Peak process RSS | 393 MiB | 168 MiB |

Both versions perform one minor GC during the timed load; neither changes the
nursery size or moves collection outside the timer. Every run returns
`BABEL-CHECK #(72 101 108 108 111)`, with no source compilation in the measured
runs. Whole-process instructions are **not** isolated load-phase instructions.

The structural change replaces the minor collector's per-object address
`HashMap` and live-object `HashSet` with two compact bitmaps. A region-offset
table packs only nursery regions: 64 MiB of nursery needs 1 MiB of bitmap
storage. One bitmap records exact object starts, the other liveness; interior
addresses and non-nursery references are still rejected. Marking reads layout
from intact object headers before evacuation rather than duplicating metadata
for every nursery object. Persistent forwarding is resolved before marking.
This removes hash-table construction and random lookups, not necessary GC work.

Reproduce the load phase from the repository root with this shared script:

```lisp
#+sbcl (require :asdf)
#+bliss (load "lib/asdf.bfasl")
(asdf:initialize-source-registry
 `(:source-registry (:tree ,(truename "ocicl/")) :inherit-configuration))
(time (asdf:load-system :babel))
(format t "~&BABEL-CHECK ~S~%"
        (babel:string-to-octets "Hello" :encoding :utf-8))
```

Use `scripts/bliss-limited.sh taskset -c 0` for each process. Bliss uses
`--no-init --load`; SBCL uses
`--noinform --no-sysinit --no-userinit --non-interactive --load`. Populate each
binary's build-hash-specific FASL cache first, then alternate saved baseline and
candidate binaries. `/usr/bin/time` and `perf stat -e cpu_core/instructions/u`
around the process measure the whole-process columns.

The remaining gap is real. The historical leads below remain context, not a
fresh attribution of the remaining initial-load time.

## 1. Earlier baseline (before the initial-load follow-up)

Benchmark: `(asdf:load-system :babel)` — one cold load plus ten no-op re-loads,
from fasls, `--no-init`.

```
start of investigation   ~36.8 s
now                      ~14.4 s
```

Against SBCL 2.6.8 on the same machine, alternating runs, both warm from fasls:

| | bliss | SBCL | ratio |
|---|---|---|---|
| cold load from fasl | 3.15 s | 0.31 s | ~10x |
| no-op re-load (each) | 0.89 s | 0.0046 s | **~193x** |

Cold loading is no longer the outlier. **The gap lives in the no-op re-load**,
which is pure ASDF traversal with nothing to install.

Method tier split over a load went 16.5% → 96.9% of method invocations compiled.
That seam is essentially closed; do not expect more from it.

## 2. How to measure in this codebase

**Guessing from source has failed roughly seven times in a row here.** Every
significant find came from a tool. The traps are written up in
`measuring-performance.md` §6a–§6e; the essentials:

- **perf cannot unwind the release binary.** Build with
  `-C force-frame-pointers=yes` and use `--call-graph fp`. `--call-graph dwarf`
  does not work here. `--comms` takes the BINARY name, so a renamed copy matches
  nothing — silently, reporting zero rows rather than erroring.
- **Pin to a P-core.** This is a hybrid CPU; an unpinned run lands on E-cores,
  reports `cpu_atom/cycles`, and collects far fewer samples. `taskset -c 0`.
- **perf only gets you to the inlined caller.** For the real site, break in gdb.
  You need real DWARF, and `RUSTFLAGS="-C debuginfo=2"` does NOT produce it —
  use `CARGO_PROFILE_RELEASE_DEBUG=2`. Then `break <mangled symbol>`,
  `ignore 1 <N>` to skip startup, `continue`, `bt`.
- **Wall clock on this machine cannot resolve anything under ~5%.** Load average
  swings results by 15%+. Use `taskset -c 0 perf stat -e cpu_core/instructions/u`
  — retired instructions are near-deterministic here and reproduce to five
  digits. Confirm direction with wall clock only when the box is quiet.
- **Discard the first run of a freshly built binary.** It is a cold page-cache
  artifact and looks like a large regression or win. Also note each binary has
  its own fasl cache keyed by build hash, so a first run may recompile babel.
- **A few gdb samples are not a distribution.** This one bit me: two samples both
  hit macro-environment cloning, I called it "what memcpy is doing", fixed it,
  and memcpy did not move at all (12.3% → 12.87%). Tally at least a dozen.
- **Benchmark the workload you are trying to improve.** A per-call microbenchmark
  and a no-op ASDF re-load exercise almost disjoint code here. Three large
  per-call wins moved the re-load by nothing (§4).

## 3. The current re-load profile

Frame-pointer build, `taskset -c 0`, 40 no-op re-loads, mutator self time:

```
12.87%  memcpy                      <- largest single entry, SOURCE UNKNOWN
 5.97%  cli::eval_list              <- still TREE-WALKING during the traversal
 2.57%  cli::symbol_bare_slice
 2.31%  core::hash::sip::Hasher
 2.29%  __rustc::__rust_dealloc
 2.29%  memcmp
 2.25%  symbols::find_index
 2.21%  __rustc::__rust_alloc
 2.11%  gc major_gc closure
 1.62%  hashbrown HashMap::insert
 1.60%  __libc_malloc_impl
 1.60%  cli::accessor_slot_name
```

`run_loop` is NOT in this list. That is the single most important fact about the
re-load: it is not dominated by compiled call dispatch, which is why per-call
work does not move it.

## 4. The pattern to internalise before optimising

Four fixes, each a large win on a call benchmark and worth ~nothing on the load:

| change | its own benchmark | babel |
|---|---|---|
| `8faef9b` block registration | per-call −45.7% | −0.07% |
| `04aae0e` operator classifiers | per-call −12.1% | −0.8% |
| `6a283ac` closure registry hash | closures −11.8% | ~0 |
| `bliss-ccso` call-next-method tiering | 13.2x on a real body | 0 |

Each was a genuine defect worth fixing. None of them was a *load* fix. Before
starting anything, profile the re-load and confirm your target appears in it.

## 5. Open leads, in profile order

1. **memcpy, 12.87%.** The largest entry and still unattributed. Ruled out:
   `Env::child_with_parent` (clones ten collections per scope but is 0.03% of the
   re-load) and the global macro `Environment` clone (fixed; memcpy unchanged).
   Sample properly — a dozen-plus gdb backtraces, tallying frames #6–#9 — before
   forming a theory.
2. **`eval_list` at 5.97% SELF.** Something substantial in the ASDF traversal is
   still tree-walked rather than compiled. Find out what and why it never
   promotes. The tier-split probe recipe is in `method-tiering.md`.
3. **The symbol/name cluster, ~11% combined**: `symbol_bare_slice` 2.6 +
   `find_index` 2.3 + memcmp 2.3 + SipHash 2.3 + `HashMap::insert` 1.6. Tracked
   as bliss-qerr. A NEGATIVE result is recorded there: borrowing instead of
   cloning `package_use_list` changed nothing, because the cost is the RwLock
   acquisition and hash lookup, not the copy.
4. **Closure construction, ~5 µs — about 14 function calls** (bliss-gajx). A
   closure is not a GC object; it is parked in a process-wide
   `HashMap<u64, Closure>` that the collector must scan, and ~33% of closure
   construction time is GC. Pruning sooner is *worse* (measured: 7.1 → 11.8 →
   34.7 µs/call as the prune floor drops), so that knob is already right. The
   structural fix is making closures first-class GC heap objects. Relevant to
   ASDF, whose hot methods build a closure per call via `do-asdf-cache`.

## 6. Things already tried that did not work

Recorded so they are not repeated:

- **Caching T2 compiler output** (bliss-eoma). Prize is now ~1.6% of a load: T2
  costs 1.07 G instructions to compile and returns 3.16 G, i.e. it is a net 3%
  WIN. It began this investigation as a 42% penalty; the change was aaeb60a.
  See `code-cache.md` §8d — that section's claim has been wrong three times in
  three different directions, so re-measure before believing any of it.
- **Integer control tokens** (bliss-taqn, closed). Ceiling ≤7% on
  catch/handler-case-heavy code and ~0 elsewhere, against a blast radius that
  includes a new `BlissError` variant, because tokens ride through errors as
  strings with prefix parsing.
- **Replacing the allocator wholesale** (bliss-05as). mimalloc will not build for
  musl here; dlmalloc builds and is 8% SLOWER. What worked was adding the one
  thing musl lacks — a per-thread free-list cache — which is now default-on for
  musl and was worth 28.7%.
- **Lowering `BLISS_CLOSURE_PRUNE_FLOOR`** — strictly worse, see §5.4.

## 7. Known-flaky / pre-existing failures

Do not spend time on these; both reproduce without any local change:

- `cli::bytecode::jtc4_stack_map_tests::run_native_rewrites_stack_guard_sigsegv_to_stack_overflow`
  — load-sensitive, ~2.5% in the full gate, 0/10 standalone. Tracked as
  bliss-zzty, which also fixed three real cross-thread signal-delivery races
  found while chasing it. An early 0/15 baseline made this look like a
  regression; it was sampling noise. Use n>=40 for flaky baselines here.
- `bliss-compiler --test t2_integration stringp_reaches_string_typecheck_through_inline_metadata`
  — fails at HEAD with everything stashed.

## 8. Useful knobs

```
BLISS_DISABLE_T2=1            no T2
BLISS_T2_DISCARD=1            compile at T2, install nothing  (separates compile
                              cost from emitted-code cost)
BLISS_T2_NO_QUEUE=1           snapshot for T2, never queue    (isolates the
                              mutator-side snapshot)
BLISS_BAIL_TRACE=1            record why the lowerer bailed; read with
                              (bliss-ext:bail-report)
BLISS_DIRECT_BUILTIN_STATS=1  direct-builtin hit/fallback counts
BLISS_GC_STRESS=1 BLISS_GC_POISON=1   mandatory for anything that allocates
```

The bail reporter now reports the reason from the attempt that actually decides,
and the innermost cause rather than the outermost wrapper — it used to report
neither, which hid `bliss-ptv4` behind a wrong answer for a while.
