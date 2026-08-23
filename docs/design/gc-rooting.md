# Design: enforce GC rooting by type (bliss-a03)

Status: **proposal** · Owner: — · Related: bliss-6b2, bliss-aid, bliss-h6z,
bliss-011, bliss-wlf, bliss-noh, bliss-8qf, bliss-wpe, bliss-52d

## 1. Problem

bliss has a **precise, moving** minor GC. Any allocation can relocate a nursery
object and must update every reference the collector can find. But a `BlissVal`
is a `Copy` tagged word (`BlissVal(u64)`), so Rust code copies live references
into locals, `Vec`s, `HashMap`s, and closure captures freely — and the collector
cannot find those unless the code **opted in** to rooting. Every omission is a
latent, load-dependent segfault / abort / silently-wrong result.

This is not a handful of bugs; it is a **recurring class**: bliss-6b2,
asdf-6b2/bliss-aid, h6z, bliss-011, bliss-wlf (a whole-subsystem reader + lowerer
+ env sweep), bliss-noh (macroexpand.rs), bliss-8qf (macro expansion still
corrupts under stress). Each was fixed case-by-case by adding *more* rooting.

Two concrete weaknesses drive the churn:

**(a) Rooting is opt-in and invisible in the type system.** Nothing distinguishes
"a `BlissVal` it is safe to hold across an allocation" from "one that will dangle."
The reviewer must reason, per call site, about which locals cross which allocating
calls. Humans (and agents) miss cases; the failure surfaces only under GC pressure.

**(b) The mechanism is a per-thread `Vec` behind a `GcWorld` mutex.** Every root
construction and destruction takes a **global lock** plus a `HashMap<ThreadId>`
lookup. (For strict-LIFO drops `rposition` finds the tail immediately, so the
asymptotics are fine; out-of-order drops degrade to `O(n)`. The dominant cost is
the lock + lookup per operation, on every rooted local of every recursion level —
**measured at ~127 ns per root op**, §9.)

### Inventory (the cost of the status quo)

~20 registered root scanners plus several bespoke guard types, each hand-rolled:

| Scanner / guard | Location |
|---|---|
| `scan_host_roots`, `scan_shadow_roots` | gc.rs (the shared primitives) |
| `scan_evaluator_global_roots` | cli.rs |
| `scan_live_env_roots` / `LiveEnvRootGuard` | cli.rs |
| `scan_suspended_frames`, `scan_loop_roots`, `scan_vec_roots` / `VecRootGuard` | cli.rs |
| `scan_bytecode_roots`, `scan_lowerer_consts` / `LowererConstGuard` | bytecode.rs |
| `scan_global_macro_roots`, `scan_macrolet_capture_roots`, `scan_active_expansion_envs` | macroexpand.rs (last two: bliss-noh) |
| `scan_clos_state_roots` | clos.rs |
| condition / format / pathname / stream / hashtable / devtools scanners | bliss-stdlib |
| `FrozenMacroCapture` table, `MACRO_FN_CACHE` | cli.rs |

Several register **raw stack addresses** in thread-locals (use-after-scope if a
guard is forgotten); `MACROLET_CAPTURE_TABLE`/`MACRO_FUNCTION_REGISTRY` grow
unbounded; a few scan by calling `Arc::make_mut` **during** a collection (allocates
mid-GC).

## 2. Current mechanism (accurate baseline)

- **Scanner protocol.** `register_root_scanner(fn(&mut dyn FnMut(*mut BlissVal)))`.
  The collector calls every scanner during both the mark and relocate passes with
  a visitor that receives each root **slot** (`*mut BlissVal`), updating it in
  place. Scanners run under the heap lock and must not allocate. (gc.rs:2095)
- **`StackRoot::new(&mut BlissVal)`** — registers the *address of an existing
  local*; the GC rewrites the local in place, so reads stay direct (free). Cost:
  locked push on new; locked `O(n)` remove on drop. (gc.rs:2248)
- **`HostRoot<T: TraceHostRoots>`** — owns a `Box<T>`, registers the box address;
  `Deref`/`DerefMut`. `TraceHostRoots` is implemented for `BlissVal`, `Vec`,
  `Option`, and 2-tuples. For `Vec<BlissVal>` accumulators. (gc.rs:2211)
- **`ShadowRootScope` / `ShadowRoot`** — handle model: `root(v) -> ShadowRoot`,
  read back via `.get()` / `.set()`. Every access takes the mutex and does a
  `HashMap<ThreadId, Vec<BlissVal>>` lookup. (gc.rs:2373)
- **Cross-thread.** Threads already publish per-safepoint state
  (`thread::publish_stack(sp, fp)` / `published_stack()`, `all_thread_ids()`), and
  the STW collector reads parked threads' published frame pointers. Root registries
  are global (`HashMap<ThreadId, …>`) so the collector sees every thread's roots.

**Observation:** `StackRoot` is already SpiderMonkey's `Rooted<T>` in spirit
(root the local, rewrite in place) — just stored as raw addresses in a
mutex-guarded `Vec` instead of an intrusive, thread-local, lock-free list. And
`ShadowRoot` is already a `Handle`. The seed of the target design exists; it is
neither unified nor cheap nor enforced.

## 3. Goals / non-goals

**Goals.** (1) Make "held a live `BlissVal` across an allocation" a *structural*
error or a mechanical lint, not a reasoning task. (2) Remove the per-op lock and
the `O(n)` drop. (3) Collapse the bespoke stack/handle scanners into one
mechanism. (4) Migratable incrementally — no big-bang rewrite; each phase ships.

**Non-goals.** Changing the collector algorithm, object model, or the
`register_root_scanner` protocol for genuinely *global* roots (symbol table, CLOS
state, global macro table stay explicit persistent roots). Perfect compile-time
enforcement on day one.

## 4. Prior art

- **V8 `HandleScope` / `Handle<T>`.** Handles point into a contiguous per-thread
  handle arena (a bump stack). Create = bump a pointer (no lock); scope destroy =
  reset the pointer; GC scans the arena as a contiguous range. Reads = one
  indirection. This is the model bliss's `ShadowRoot` approximates but pays a lock
  per access for.
- **SpiderMonkey `Rooted<T>` / `Handle<T>`.** `Rooted<T>` is an **intrusive,
  thread-local, singly-linked list**: construction links `self` onto a thread-local
  head (`next = head; head = &self`), destruction unlinks (`head = next`) — O(1),
  lock-free. The value lives *inline* in the `Rooted`, so reads are direct and the
  GC rewrites it in place. `Handle<T>` is a `&Rooted<T>` for passing without
  re-rooting. This is the closest fit to bliss's `StackRoot`, minus the lock.
- **`gc-arena` (Rust).** Branded `'gc` lifetimes: `Gc<'gc, T>` values cannot escape
  the `arena.mutate(|mc, root| …)` closure, and a `&Mutation<'gc>` capability token
  is required to allocate. The borrow checker makes "hold a `Gc` across the end of
  the mutation" a compile error — *rooting-free by construction*. Gold standard for
  "impossible by construction" in safe Rust, but every value type gains a `'gc`
  lifetime and must derive `Collect`.
- **JNI local references.** Per-frame local-ref tables auto-freed on frame exit;
  the API only hands out managed refs, never raw pointers.

## 5. Proposed design

Two separable parts. Part A is a mechanical, high-value win that unifies and
speeds up the *mechanism*. Part B is the *enforcement*, on a spectrum; recommend
starting at the weakest useful rung and ratcheting.

### Part A — one lock-free intrusive root mechanism

Replace the mutex-guarded per-thread `Vec`s (`shadow_roots`, `host_roots`) with a
**thread-local intrusive list**, SpiderMonkey-style:

```rust
// Thread-local head, published to the collector at each safepoint.
thread_local! { static ROOT_HEAD: Cell<*mut RootLink> = const { Cell::new(null_mut()) }; }

// One list node type; `trace` handles BlissVal, Vec<BlissVal>, or any TraceHostRoots.
struct RootLink { next: *mut RootLink, trace: unsafe fn(*mut RootLink, &mut dyn FnMut(*mut BlissVal)) }

pub struct Rooted<T: TraceHostRoots> { link: RootLink, value: T, _pin: PhantomPinned }
// new(): link.next = ROOT_HEAD.get(); ROOT_HEAD.set(&mut self.link)   — O(1), no lock
// drop(): ROOT_HEAD.set(self.link.next)                              — O(1), no lock
// Deref/DerefMut to T; GC rewrites `value` in place.
```

- **`Rooted<BlissVal>`** subsumes `StackRoot` (root a local — but now the value
  lives *in* the `Rooted`, avoiding the raw-address-of-local footgun) and, via a
  `Handle<'r> = &'r Rooted<…>` alias, `ShadowRoot`'s pass-by-reference use.
- **`Rooted<Vec<BlissVal>>`, `Rooted<(BlissVal, BlissVal)>`, …** subsume
  `HostRoot<T>` (reuse the existing `TraceHostRoots` trait verbatim).
- **Cross-thread:** add `published_root_head` next to `published_fp` in the
  per-thread state (thread.rs). A thread publishes its `ROOT_HEAD` at the same
  safepoint it already publishes `fp`; the STW collector walks each parked
  thread's published list. No global lock on the mutator hot path.
- **Cost:** construction/destruction O(1) and lock-free (the mutator hot path
  touches only a thread-local `Cell`); the per-op global lock and `HashMap`
  lookup disappear. GC-time cost is one pointer-chase per live root. **Measured
  (§9): 3.9 ns vs 127.4 ns per root op — 32.5×.**

Migration is mechanical because the surface barely changes: `StackRoot::new(&mut x)`
→ `let x = Rooted::new(x)` (then use `*x`); `HostRoot::new(v)` → `Rooted::new(v)`;
`ShadowRootScope`+`root` → `Rooted` + `Handle`. `TraceHostRoots` is unchanged.

> Note: `Rooted` must not move after being linked (the list holds `&mut self.link`).
> Enforce with `PhantomPinned` + a `rooted!(x = v)` macro that shadows the binding,
> exactly as SpiderMonkey's `Rooted`/`RootedGuard` and `gc-arena`'s macros do.

### Part B — enforcement, weakest-useful-first

1. **Lint / checker (ship first).** A `dylint`/clippy-style lint (or a small
   custom AST pass in CI) that flags the smell the whole bug class reduces to: a
   bare `BlissVal`-typed binding that is *read after* an intervening call to a
   known-allocating function. Start with a hand-maintained allowlist of allocating
   entry points (`alloc_typed`, `alloc_cons`, `eval_form`, `apply_function`,
   `macroexpand*`, `resolve_sym`, …). Not sound, but turns "reason per site" into
   "CI tells you," at near-zero code churn. High value, low risk.

2. **Handle-only alloc/eval APIs (the realistic target).** Make the allocating and
   evaluating entry points *take and return* `Rooted`/`Handle`, not bare `BlissVal`
   (V8's discipline). You can still read a bare value; you just cannot thread one
   *through* an allocation without rooting it, because the API won't accept it.
   This makes the common cross-an-alloc mistakes ill-typed. Introduce behind new
   signatures and migrate call sites incrementally.

3. **Branded `'gc` lifetimes (evaluate; likely too invasive to retrofit).**
   Adopt `gc-arena`-style `BlissVal<'gc>` + a `Mutation<'gc>` capability, so
   holding a value across the mutation boundary is a *compile error*. This is the
   only rung that is fully sound, but it threads a `'gc` lifetime through the entire
   codebase and every value-holding type — a multi-month change. Recommend a
   time-boxed spike on one leaf module (the reader, which is already
   self-contained) to measure the blast radius before committing.

**Recommendation:** ship Part A + Part B rung 1 first (mechanical, decisive perf +
a CI safety net), adopt rung 2 as the steady-state target, and spike rung 3 to
decide whether "impossible by construction" is affordable here.

## 6. What folds away

With Part A, the bespoke *stack/handle* scanners collapse into the one intrusive
list: `LiveEnvRootGuard`, `LowererConstGuard`, `VecRootGuard`, the
`MACROLET_CAPTURE_TABLE` + `ACTIVE_EXPANSION_ENVS` from bliss-noh, and the
`FrozenMacroCapture`/expansion-env machinery become `Rooted<…>` locals or a single
`Rooted`-backed registration. Genuinely *global/persistent* roots (symbol table,
CLOS state, global macro table, active bytecode functions) stay as explicit
`register_root_scanner` persistent roots — but that is a handful, not twenty, and
each is clearly "a global," not "a forgotten local."

## 7. Incremental migration plan

1. **Land `Rooted<T>` (Part A)** in bliss-rt alongside the existing primitives;
   prove equivalence with the GC fuzzers (`BLISS_GC_STRESS`/`POISON`). No call-site
   changes yet.
2. **Convert one hot subsystem** end-to-end (recommend the bytecode lowerer or
   macroexpand.rs, both freshly swept so the diff is legible) from
   `StackRoot`/`HostRoot`/bespoke tables to `Rooted`; delete the scanners it
   subsumes. Measure compile-load time (the `O(n²)` fix should show up).
3. **Ship the lint (rung 1)** in CI against the whole tree; triage findings.
4. **Migrate remaining subsystems**, deleting bespoke scanners as they are
   subsumed; keep persistent-root registrations.
5. **Introduce handle-only signatures (rung 2)** for the core alloc/eval entry
   points; migrate callers.
6. **Spike branded `'gc` (rung 3)** on the reader; decide.

Each step is independently shippable and independently validated by the GC
fuzzers.

## 8. Risks / open questions

- **`extern "C"` c2i boundary.** Compiled code calls back into Rust; a `Rooted`
  created in a c2i callback links onto the same thread-local head — fine — but the
  boundary must remain a clean safepoint (it already is). Verify `Rooted` in a c2i
  adapter is scanned correctly under `BLISS_GC_STRESS`.
- **`Rooted` immovability.** Relies on the `rooted!` macro / `PhantomPinned`
  discipline; a moved `Rooted` corrupts the list. The macro removes the footgun but
  must be the only blessed constructor.
- **Cross-thread publish race.** Publishing `ROOT_HEAD` must use the same
  acquire/release protocol as `published_fp`, and the list must be quiescent at the
  safepoint (it is: a parked thread makes no `Rooted` transitions).
- **`Arc::make_mut`-during-scan** (present in the Environment scanners today) should
  be removed as those callers migrate — scanning must never allocate.
- **Perf must be measured, not assumed.** The `O(n²)` claim and the lock-removal
  win should be confirmed on a real asdf/alexandria load before/after.

## 9. Proof of concept — DONE (landed with this doc)

Part A's core is implemented and validated:

- **`Rooted<T>` / `RootedGuard` / `rooted!`** in `bliss-rt/src/gc.rs`
  ("Intrusive, lock-free precise roots" section): intrusive node with the value
  inline, thread-local head cell, one process-wide `scan_rooted_lists` scanner
  over a registry of head-cell addresses (registered once per thread, removed by
  a TLS destructor on thread exit). Immobility is enforced by the `rooted!`
  macro's shadowing: the guard's borrow pins the node for exactly the linked
  extent. Out-of-order drops unlink from mid-list (defensive `O(n)` walk; normal
  lexical LIFO hits the `O(1)` head fast path). Unit tests cover link/unlink,
  scan visibility of writes, mid-list drop, and unwind.
- **First consumer:** `walk_cons` (macroexpand.rs) — the hottest rooted
  recursion, one activation per cons of every macroexpanded form — converted
  from four `StackRoot`s to `rooted!`.
- **Validation:** normal-operation macrolet/symbol-macrolet output identical on
  both backends; the canonical bliss-noh reproducer passes
  `BLISS_GC_STRESS=1 BLISS_GC_POISON=1`; all 22 bliss-compiler test suites pass;
  no new failure class under stride sweep (residual stride-dependent wrong
  answers are the pre-existing bliss-8qf corruption, present before this change).
- **Benchmark** (`cargo run --release -p bliss-rt --example bench_roots`; 2000
  nested scopes × 4 roots × 500 iters, LIFO):

  | primitive | ns / root-op |
  |---|---|
  | `StackRoot` (mutex + registry) | 127.4 |
  | `rooted!` (intrusive) | **3.9** |

  **32.5×** on the root-op hot path.

**Not yet done from Part A:** migrating the remaining
`StackRoot`/`HostRoot`/bespoke users (plan §7 step 4 — tracked as bliss-yab).

## 10. Amendments from the migration (bliss-yab / bliss-jaf)

- **`RootedRef<T>` / `rooted_ref!`** — the borrowed-target counterpart of
  `Rooted`: roots EXISTING data (a local, a `Vec`, an `Env`/`Lowerer`/
  `Environment` struct) in place on the same intrusive list, with `StackRoot`'s
  released-borrow contract (caller keeps the target live and immobile). This is
  what `StackRoot`/`VecRootGuard`/`LiveEnvRootGuard`/`LowererConstGuard`/
  `ExpansionEnvGuard` migrate to; `TraceHostRoots` impls for `Env`, `Lowerer`,
  and `Environment` let one guard root a whole struct via its existing
  `visit_gc_roots`.
- **Cross-thread publication resolved without new machinery:** the registered
  head-cell address (stable for the thread's lifetime, deregistered by a TLS
  destructor) *is* the thread's published root list. A separate per-safepoint
  `published_root_head` atomic would be redundant — nothing would read it until
  the bliss-h6z.5 STW protocol exists, and that protocol only needs the
  quiescence guarantee it must provide for every legacy registry anyway.
  Recorded as a handoff note on bliss-h6z.5.
- **Fiber constraint (inherited, now documented):** root guards link onto the
  *carrier* thread's list, so a fiber must not suspend across a live guard.
  This is the same constraint the `ThreadId`-keyed legacy registries already
  imposed; bliss-h6z.5 must design for it if guard scopes ever span suspension
  points.
- **Part B rung 1 shipped as `tools/gc-root-lint`** (bliss-jaf): a syn-based
  checker flagging producer-bound / `BlissVal`-typed locals read after a
  known-allocating call without rooting, ratcheted by a checked-in baseline
  (`--check` fails only on findings not in `tools/gc-root-lint/baseline.txt`;
  `--bless` regenerates it). Keys are `file:function:variable` so unrelated
  edits don't churn the baseline.
