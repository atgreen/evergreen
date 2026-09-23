# Design: persistent tiered-code cache (bliss-eoma)

Status: **proposal** · Owner: — · Related: bliss-jtc (S5 hotspot-engine),
bliss-p1t, bliss-qerr, spec §4.4, §6.11

## 1. Goal

A freshly started bliss should reach T2 speed on code it has already seen hot,
without re-running the counters that discovered it. Today every process starts
cold: invocation counters at zero, no profile, no native code, and the whole
tier-up ladder re-climbed from scratch. What a previous run *learned* — which
functions are hot, which speculation at which site was right, which loops OSR'd —
is thrown away at exit.

Two distinct wins hide behind one idea, and they are worth separating because
they have very different cost and risk:

- **Warm start.** Start compiling the right functions immediately instead of
  waiting for 100k back-edges to prove it. Needs only a *profile*.
- **Zero-latency start.** Skip compilation entirely on the second run. Needs
  persisted *native code*.

This document stages them so the first is delivered and measured before the
second is committed to.

## 2. What already exists

Material amounts of this are built. Reusing them is not optional; rebuilding any
of it would be waste.

| Piece | Where | State |
|---|---|---|
| Background T2 worker pool | `cli/bytecode.rs:15214` (`t2_compile_queue`) | built — `BLISS_T2_THREADS` default 2 |
| Bounded priority compile queue | same, `T2QueueState` | built — `BLISS_COMPILE_QUEUE_SIZE` default 64, drop-not-block |
| GC-safe off-thread compilation | `job.input.with_gc_stable(compile_t2_artifact)` | built |
| Install on the owning mutator | completion channel → `T2Completion` | built |
| Guards + deopt to T0 | `t2/speculate.rs`, `t2/deopt.rs` | built |
| Deopt-count blacklisting | `deopt_blacklist_threshold()`, default 8 | built — explicitly HotSpot's policy |
| Generation invalidation | `bump_direct_call_gen()` | built |
| `.bfasl` container | `spec/06-11-bfasl.md` | specified; sections `CACHED_T1`(10), `RELOCATIONS`(8), `STACKMAPS`(7), `DEPENDENCIES`(9) reserved |
| `content_hash` staleness | R6.70 | specified |
| Platform/ABI exact-match gate | R6.65 | specified |

Spec R4.26 already *mandates* that T2 compile on a background thread, and
§4.4.5.4 already fixes the ownership split (compile off-thread, install on the
mutator). So "start a background thread to tier up code" is not new work — it is
existing work that currently has no source of requests other than live counters.

**The genuinely new work is: a durable source of tier-up decisions, and a way to
persist the code itself.**

## 3. What blocks persisting T2 code

T2 bakes five classes of process-local fact into the emitted instruction stream.
Enumerated here because the design hinges on which are *relocated* and which are
*designed away*.

1. **Runtime helper pointers.** `bytecode.rs:17744`:
   ```rust
   let load_global_addr = c2i_load_global as extern "C" fn(u64) -> u64 as usize as u64;
   ```
   emitted as a `mov imm64`. Also `call_slice`, `store_global`, `load_function`,
   `mv`, `recovery_toggle`. These change with every rebuild of the binary *and*
   with ASLR on every start.
2. **Constant-pool slot addresses.** `FramedCode::heap_constant_slots`
   (`t2/emit.rs:577`) — raw addresses of cells living inside `Arc`'d bytecode
   bodies. Note the *cell* address is baked; the collector updates the `BlissVal`
   inside it. That indirection is already the right shape for relocation.
3. **Symbol indices as immediates.** `t2/emit.rs:1343`:
   ```rust
   mov_imm64(a, 7, (((slot as u64) << 32) | sym as u64) as i64);
   ```
   Symbol indices are interning-order dependent, so they are not stable run to run.
4. **Direct-call targets** to other native code — already generation-guarded
   (bliss-zhvn), so stale targets fail safe.
5. **Inlined callee bodies** (`inline_bodies`) — not an address, a *staleness
   dependency*. See §5.

### 3.1 Relocate, or design away?

Two ways to handle (1) and (3):

- **Relocate:** record every baked-immediate site and patch the byte stream at
  load. Works, but every new emitter site is a new chance to forget a relocation
  record, and the failure mode is a wild jump.
- **Design away (preferred):** emit runtime calls indirect through a per-process
  helper table — `call [rbx + helper_offset]` rather than `mov imm64; call` — and
  put symbols in a per-method symbol table relocated once at load by re-interning
  *by name*. `.bfasl` already mandates by-name symbol identity for constants
  (R6.66), so this is the same rule extended to code.

The second collapses N relocations per method into one base pointer plus one
table, and makes the code position-independent with respect to the runtime. It
costs one indirection per runtime call, which should be measured but is very
likely noise against the call itself. This is broadly how AOT-capable JITs handle
it, and it is what makes the cached artifact robust rather than a byte-patching
exercise.

**Whichever is chosen, the emitter must record its own baked sites.** It already
does this for constants (`heap_constant_slots`); the pattern extends.

## 4. Why caching speculative code is safe

The obvious objection to caching T2 is that T2 code is *speculative* — it commits
to one type per site with no fallback path, on the strength of a profile that may
not describe the next process at all.

The answer is that **every speculation T2 makes already emits its guard, and the
deopt path already exists.** `t2/speculate.rs` is explicit: one path, no fallback,
guard-flagged, `FrameState` retained, deopt to the interpreter at the site's `bcp`
if the operand type is wrong. A cached method whose assumption does not hold in a
new process takes exactly the same path as one whose profile shifted mid-run: it
deopts, the profiler observes the new type, a later recompile commits to that
type instead. If it keeps being wrong, `deopt_blacklist_threshold()` uninstalls
and blacklists it.

So a wrong cached method is a **performance** event, not a correctness event.
That is the property that makes this whole design tractable, and it should be
stated in the spec amendment as the governing invariant.

## 5. Where that argument does *not* hold

The safety argument covers guarded assumptions. It does not cover facts T2 proved
at compile time from global state and therefore did not guard:

- **Constant-folded globals.** If a value folded into cached code differs in the
  next process — different load order, a redefinition, a different `defconstant`
  — the folded code is simply wrong, with no guard to catch it.
- **Inlined callee bodies.** If a callee is redefined between the run that wrote
  the cache and the run that loads it, the inlined copy is stale. The direct-call
  generation counter covers *calls*; it does not cover a body already inlined into
  someone else's code.

This is exactly what HotSpot's per-nmethod `Dependencies` exist for, and it is the
part of this design that must be specified before any code is written:

> **Every cached method carries an assumption record naming every global
> definition it depended on and the identity (content hash) it assumed. The
> loader validates the whole record before installing, and discards the method on
> any mismatch. A method whose dependencies cannot be checked is not cacheable.**

Discarding is always safe — the bytecode is still there, and the function simply
starts at T0 like today.

## 6. Staged plan

### Stage 1 — persist the profile (`.bprof`)

A sidecar beside the `.bfasl`, keyed by the same `content_hash`, recording:
per-function invocation and back-edge counts, which per-site speculations
*succeeded*, which deopted or got blacklisted, IC shapes, and OSR loop headers.

On load, seed the profile and counters so the existing queue enqueues the right
functions immediately, and so T2 commits to last run's winning speculation rather
than rediscovering it.

This is ART's `.prof` model and it is the best value-to-effort ratio in the whole
design:

- architecture-neutral — survives rebuilds, ASLR, and a different machine
- tiny relative to code
- **no new correctness surface**: a profile is a hint, every speculation still
  emits its guard, and a stale profile costs at worst a deopt that existing
  machinery already handles
- it independently answers the question Stage 3 depends on — *does warm start
  actually pay?* — before any relocation work is committed to

Recording which speculations *failed* matters as much as which succeeded: it lets
the next run skip a speculation that is known-bad for this workload rather than
re-earning eight deopts to blacklist it again.

### Stage 2 — eager background tier-up

The queue exists; this adds a new source of requests (profile-hot functions,
enqueued at load rather than at counter threshold) and a scheduling policy.

**Define "spare capacity" concretely rather than adaptively.** Keep the two
existing workers, drop them to `SCHED_IDLE`/`nice`, and bound in-flight compiles.
Adaptive idle-detection is a large amount of machinery in service of a guess, and
it is hard to test.

**Caveat that must be measured, not assumed:** eager compilation during a load
adds CPU to a phase that is already CPU-bound. The measured babel profile
(bliss-p1t) shows a warm no-op `asdf:load-system` dominated by interpreter
dispatch, allocation, and symbol-name strings. Compiling harder during that
window could make cold loads *slower*. Gate Stage 2 on a benchmark that measures
load time, not just steady-state throughput.

### Stage 3 — `CACHED_T2` native code

Only after Stages 1–2 have quantified the remaining win. Three separable pieces:

- **3a — relocation-friendly emission** (§3.1): helper table + per-method symbol
  table, emitter records its own baked sites.
- **3b — assumption records + load-time validation** (§5).
- **3c — the `CACHED_T2` section, cache key, and loader.**

The cache key is a conjunction; any mismatch means ignore the section and fall
back to bytecode:

```
(bliss binary build id, target triple + ABI, bfasl content_hash,
 compiler policy flags, bytecode_version)
```

This is exactly the model `.bfasl` already defines for `CACHED_T1` (R6.61, R6.64,
R6.65) extended one tier, which is why the vehicle is a new section rather than a
new file format.

## 7. Vehicle: per-`.bfasl`, not whole-image

Decided 2026-09-22. bliss already has whole-image save/restore
(`scripts/build-image.lisp`, `save-lisp-and-die`), and an image is in several ways
the *easier* place to put native code: it can freeze symbol indices and heap
layout together with the code, so most of §3 evaporates.

It was nonetheless rejected as the vehicle because it is coarse — any change to
any component means rebuilding the whole image — whereas a per-`.bfasl` section
composes with ASDF's existing per-file compile/load model, invalidates per file,
and benefits a user who loads one new library into an otherwise unchanged world.
The image route remains available as a later optimization for the base system,
where the coarseness costs nothing.

## 8. Scope boundary — SUPERSEDED, see §8a

This section argued, from a measurement of `BLISS_DISABLE_T2=1` that showed
"~3%, within noise, one run faster *with* T2", that **"T2 is currently worth
approximately nothing on this workload"**, and concluded that caching T2 would
have approximately zero effect on babel load time.

**That measurement was wrong, and the conclusion drawn from it was wrong.** It
is kept here only because the reasoning error is instructive; §8a has the
corrected numbers. Two specific failures:

- The `~3%` reading did not survive a clean re-measurement (`taskset -c 0`, no
  `systemd-run` wrapper, retired-instruction counts). The real effect is ~30%.
- Earlier in the same investigation an 18% T2 regression *was* observed and then
  explained away as an artifact of the wrapper in `scripts/bliss-limited.sh`.
  That dismissal removed a real signal. A surprising result is not automatically
  an artifact.

## 8a. T2 is a net LOSS on loading (measured, bliss-fhci)

Babel load (cold + 10 no-op re-loads), `taskset -c 0`, 3 reps, non-overlapping
ranges:

| | cold | reload x10 | user |
|---|---|---|---|
| T2 on (default) | 7.85 s | 16.76 s | 26.57 s |
| T2 off | **4.13 s** | **12.61 s** | **18.62 s** |
| | −47% | −25% | **−30%** |

Retired instructions (frequency-independent; the wall-clock above was near this
machine's noise floor, see `measuring-performance.md` §6c):

|  | T2 on | T2 off | |
|---|---|---|---|
| after the tiering fixes | 134.6 G | 94.7 G | on is **+42%** |
| before them | 149.9 G | 105.5 G | on is **+42%** |

So roughly **30% of the work in a babel load is the optimizing compiler
running**, not the code it produces. The identical 42% ratio on the pre-fix
binary shows this is not a consequence of bliss-5rcy / bliss-ljmj moving more
code onto the compiled tier — it was always true.

### §8b. Attributed: it is the compiler running, not the code it emits

`BLISS_T2_DISCARD=1` pays the full T2 compilation cost and then installs
nothing, so execution stays at T1. Moving the threshold cannot separate
"compiling costs time" from "the emitted code is slower" — both scale with the
number of functions promoted — but this does. Babel load, 3 reps,
non-overlapping ranges:

| | cold | reload x10 | real |
|---|---|---|---|
| T2 off (no compile, no T2 code) | 4.22 s | 12.66 s | **19.92 s** |
| T2 discard (compile only) | 8.24 s | 16.92 s | **29.31 s** |
| T2 on (compile + T2 code) | 7.80 s | 16.75 s | **28.63 s** |

Decomposing T2's 8.71 s penalty:

```text
  compilation      +9.39 s
  emitted code     -0.68 s     (T2 code is FASTER than T1, slightly)
  ------------------------
  net              +8.71 s
```

**Essentially all of it is the compiler running, and the code it produces is a
net win.** That is exactly the shape a cache fixes: it removes the 9.39 s from
the load path and keeps the 0.68 s. Projected babel load with a warm cache:

```text
  28.63 s  ->  ~19.2 s      (-33%)
```

This is the number §8 said would be "approximately zero".

### §8c. …but most of that saving has since been taken directly (bliss-fhci)

Splitting the compile cost again — `BLISS_T2_NO_QUEUE` snapshots without
queueing — showed that **82% of it was not the compiler at all**, but the
mutator-side inline-candidate snapshot: `snapshot_t2_input` walked the whole
registry (9,626 functions) and deep-cloned every invoked one, on each of 109
promotions, to hand the compiler an average of 3 candidates it could actually
consult.

Scoping that to the transitive call-graph closure (aaeb60a) took the babel load
from 28.66 s to 21.64 s (−24.5%) on its own. The decomposition now reads:

| | before | after |
|---|---|---|
| T2 net penalty | +8.80 s | **+1.61 s** |
| snapshot + compile | +9.39 s | +2.17 s |
| emitted code | −0.66 s | −0.56 s |

**So the cache's remaining headroom on this workload is ~2.2 s of a 21.6 s load,
not ~9.4 s.** The epic is still right for application steady state and for
avoiding recompilation across runs, but it should no longer be justified by
babel load time — that argument has largely been spent. Re-measure before
scheduling Stage 3.

### §8d. Final: the prize is now ~1.6% of a babel load

Re-measured on current HEAD, after the inline-candidate fix (aaeb60a), the
tiering fixes (bliss-5rcy / ljmj / ccso / ptv4) and the musl thread cache
(bliss-05as). Retired instructions, 2 reps each:

| | instructions | vs off |
|---|---|---|
| T2 off | 68.89 G | — |
| T2 discard (compile, install nothing) | 69.96 G | **+1.07 G** — the compile cost |
| T2 on | **66.80 G** | **−2.09 G net** |

**T2 is now a net WIN of 3.0%**: it costs 1.6% to compile and returns 4.5% from
the code it emits. When this investigation started it was a 42% instruction
penalty.

**A perfect cache can therefore save at most ~1.6% of a babel load.** The epic
should no longer be justified by load time at all — that argument has been
spent three times over, in three different directions:

| | claim | status |
|---|---|---|
| §8 | caching saves ~0% ("T2 is worth nothing here") | wrong — T2 was worth *less* than nothing |
| §8a/b | caching saves ~9.4 s (~33%) | wrong — 82% of that was redundant snapshot work |
| §8c | caching saves ~2.2 s | superseded |
| §8d | caching saves ~1.07 G instructions (~1.6%) | current |

What remains valid for the epic is **cross-run and cross-image reuse** — not
paying to rediscover the same 109 promotions in every process — and application
steady state, which this benchmark does not measure at all. Stage 3 should be
scheduled on those grounds or not at all.

The general lesson is the one this investigation keeps repeating: measure which
*half* of a cost you are about to optimise before building the expensive fix.

**Consequence for this epic: the case is much STRONGER than §8 allowed, and for
a reason §8 never considered.** §8 weighed only the *benefit* side of a cache —
"it makes good code arrive sooner, and good code is worth nothing here" — and
never weighed the *cost* side: that the compiler runs **during the load**. A
cache takes that cost off the load path. That is a load-time win, and it does
not depend on per-call overhead (bliss-htff) coming down first, which is what §8
gated it on.

It also reorders the stages. The cheapest large win is not the on-disk format at
all — it is getting T2 **off the load critical path**: defer tier-up to a
background thread (Stage 2), or raise the T2 threshold while loading. That
captures most of the 30% before any persistence work is needed, and it is the
half of the original proposal that §8's framing talked us out of.

Do **not** respond to this by disabling T2. It exists for application steady
state, which this benchmark does not measure at all.

Making loading fast still also wants: bliss-htff (per-call overhead), bliss-qerr
(`symbol_bare_name` on the per-call fast path), the BBU bytecode path
(R6.71–R6.79), bliss-p1t, and the remaining tiering work in
`method-tiering.md`.

## 9. Spec impact

- **§6.11** — new `CACHED_T2` section kind, its cache key, and the assumption
  record. Extends the existing `CACHED_T1` policy (R6.65) rather than replacing
  it; the portability principle is unchanged (native code is never a required
  payload, always ignorable).
- **§4.4** — the durable-profile source of compilation requests, and the
  governing invariant from §4: cached speculative code is guarded code, so a
  wrong cache deopts rather than misbehaving.
- **§4.9** — the `.bprof` format and what the profiler must record to produce it.
