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

## 8. Scope boundary: this is not a load-time fix (measured)

Worth stating plainly so the epic is not mis-aimed. Measured 2026-09-22 against
SBCL 2.6.8, same babel, same source registry, both warm from fasls (bliss-yb10):

| | SBCL | bliss | gap |
|---|---|---|---|
| cold load, fresh process | 0.170 s | 5.36 s | 31x |
| no-op re-load, each | 0.0024 s | 2.05 s | **~850x** |

bliss takes ~2 s to decide it has nothing to do — about 40% of a full cold load
— where SBCL takes 2.4 ms. Four hypotheses were tested and three rejected:

- **Not tiering failing to fire.** 62 functions promote to T2 across 11 babel
  loads, and they are the right ones (UIOP/PATHNAME, ASDF/PLAN, ASDF/SESSION).
- **Not the absence of a JIT.** `BLISS_DISABLE_T2=1` vs default, 3 interleaved
  runs: 20.7/22.5/20.2 s vs 19.9/20.8/20.6 s. ~3%, within noise, one run faster
  *with* T2. **T2 is currently worth approximately nothing on this workload.**
- **Not extra work.** Filesystem syscalls per no-op re-load: bliss 1176, SBCL
  906 — only 1.3x. Both do ~1000; bliss executes the surrounding Lisp ~850x
  slower.
- **It is raw per-call execution speed**, and it persists at T2. A recursive
  function that *does* reach T2 costs 1.04 µs/call against SBCL's 2.5 ns —
  ~400x while fully native. Per-call runtime overhead (~20% signal checking,
  12% malloc/free, 8.6% locks, 7.3% TLS, 7.2% function lookup by *name*)
  dwarfs the quality of the emitted code. See bliss-htff.

**Consequence for this epic:** caching T2 would have approximately zero effect on
babel load time. A cache makes T2 code arrive sooner; T2 code currently saves
nothing there. This does not invalidate the epic — it is aimed at application
steady state, not loading — but it does reorder it. Until per-call overhead comes
down, better code delivered sooner buys little, because the code is not where the
time goes. Stage 3c is gated on bliss-htff for exactly this reason.

Making *loading* fast is a different problem with different levers: bliss-htff
(per-call overhead), bliss-qerr (`symbol_bare_name`, now P1 — it is on the
per-call fast path, not only the load path), the BBU bytecode path (R6.71–R6.79),
and bliss-p1t.

## 9. Spec impact

- **§6.11** — new `CACHED_T2` section kind, its cache key, and the assumption
  record. Extends the existing `CACHED_T1` policy (R6.65) rather than replacing
  it; the portability principle is unchanged (native code is never a required
  payload, always ignorable).
- **§4.4** — the durable-profile source of compilation requests, and the
  governing invariant from §4: cached speculative code is guarded code, so a
  wrong cache deopts rather than misbehaving.
- **§4.9** — the `.bprof` format and what the profiler must record to produce it.
