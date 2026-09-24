# Babel load performance: state, method, and open leads

Written as a handoff. Everything here is measured; where something is a guess it
says so. If you read one section, read **"How to measure in this codebase"** —
this system defeats reasoning-from-source with unusual consistency, and most of
the wasted effort in this investigation came from skipping it.

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
