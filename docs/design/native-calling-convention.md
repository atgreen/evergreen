# Design: direct native→native calls via an ABI redesign (bliss-zhvn)

Status: **proposal** · Owner: — · Related: bliss-zhvn, bliss-gq5, bliss-jtc.9,
bliss-x5y.8, bliss-izt.2, bliss-4u5u, bliss-011, bliss-cfo6

## 1. Problem

Every native (T1/T2) call to another Lisp function bounces back into Rust:

```
native caller
  → c2i_call_slice            (extern "C"; emit_c2i_helper_call wraps it:
                               register spill + TWO c2i_set_native_sigsegv_recovery
                               calls bracketing the window)
  → c2i_call_args             (guard_c2i = std::panic::catch_unwind)
  → run_native                (per-call orchestration, below)
  → the callee's native entry fn(*mut u64) -> u64
```

A measured **~1.44µs per T0/native user call** (t0bench: a warmed `(defun callee
(x) x)` called 20M times from a compiled loop — `callee-call 37103ms − baseline
8385ms`, /20M). For the ASDF O(n²) path (bliss-gq5) this per-call cost, times the
call volume, is a large share of the load.

The cost is **not** the registry hash lookups: converting every hot integer-keyed
registry (`REGISTRY`, `NATIVE_REGISTRY`, `INVOKE_COUNTS`, `CALL_SITE_PROFILE`, …)
from SipHash to the in-tree `FxBuildHasher` moved the t0bench call cost 0% and
asdf load 0%. A monomorphic call-site inline cache (the original bliss-zhvn idea)
would skip only those lookups plus ~2 profiling probes, so it is a **confirmed
dead end**.

The cost is the **c2i bounce + `run_native` orchestration**, and most of it can
be neither skipped for the common case nor safely emitted inline in x86, because
the callee's per-call state lives in **Rust `thread_local!` + `RefCell`/`Cell`**:

- `NATIVE_ENV` — the `&mut Env` the callee's `c2i_*` callbacks read.
- `NATIVE_ENV_FRAME` — the callee's captured heap env frame.
- `NATIVE_ERROR` — save/reset/restore for first-error-wins nesting; the entry
  signals a Lisp error **through this slot**, not its return value.
- `NATIVE_DEOPT` / `NATIVE_DEOPT_RESUME` — reset before, read after; the entry
  signals a speculation-guard failure through these, and `run_native` then runs
  substantial deopt logic (backoff / blacklist / profile-decay / T0 resume).
- SIGSEGV null/stack **recovery IPs** — set to the native handler before the
  entry, restored after; `emit_c2i_helper_call` *also* toggles recovery around
  the whole c2i window to protect the Rust dispatch frame.

`push_frame` (a BlissStack bump-allocator) and `bind_params` (args → slots) *are*
emittable in x86, but emitting correct TLS access + the RefCell indirection for
the state above — and the deopt/error handler — in hand-written assembly is
impractical and dangerously fragile. So there is **no safe incremental win**
here: the enabler is an ABI/execution-model change that moves this per-call state
off thread-locals and into the calling convention.

## 2. How other systems avoid per-call state

Every mature native Lisp/VM keeps per-call state out of thread-locals. The
tiered, deopting ones (HotSpot) show the exact recipe.

**SBCL / CMUCL.** Register-based convention; `RCX` carries the arg count for full
calls; MV uses an unknown-values return protocol. A **thread-structure pointer**
is kept in a reserved register (for special bindings + the pseudo-atomic GC flag)
— no TLS lookup in compiled code. Calls indirect through an **`fdefn`** (function
cell): redefinition rewrites the fdefn, so callers pick up new code with no
call-site patching; within a component, **local-call** optimization uses a
tailored near-inlined linkage. **No per-call error/deopt slot:** errors are
condition-system **unwinds** through the frame chain, and SBCL simply **does not
speculate-and-deopt** (a failed type assumption is a `type-error`, not a tier
transition). GC is precise/moving with compiler stack maps at safepoints.

**Clozure CL / ECL / Chez Scheme (and Racket).** Same shape: register convention
(Chez with proper tail calls), function-cell indirection, moving GC via stack
maps, and **no deopt**. ECL compiles to C, so calls are C calls and errors are
`longjmp`. Their speed is direct calls + register allocation, not speculation.

**HotSpot VM** (interpreter → C1 → C2 with OSR + deopt — bliss's model):

- **`r15` is pinned to the `JavaThread` pointer.** All thread/VM state is reached
  through it; no TLS call. (This is bliss's `NATIVE_ENV` problem, solved with a
  register.)
- **c2i/i2c adapter stubs** bridge the interpreter's stack convention and the
  compiled register convention — literally bliss's `c2i` — but *compiled→compiled*
  calls skip the adapter.
- Static/special calls are **direct call instructions patched at the site** when
  the target compiles; virtual calls use **inline caches** (monomorphic → direct
  call). Recompilation/redefinition re-patches or marks the method "not entrant."
- **Deopt is not per-call.** A guard failure is an **uncommon trap**: the code
  calls the runtime *at the trap point*, which rebuilds interpreter frames from
  **per-compilation debug info** (scope descriptors + oop maps). The caller checks
  nothing; invalidated methods deopt lazily at safepoints.
- **Exceptions** unwind via **per-method exception tables** (PC-range → handler).
- **GC** is safepoint + oop-map based throughout.

**Takeaway table** (bliss thread-local → the established fix):

| bliss per-call thread-local | Established fix |
|---|---|
| `NATIVE_ENV` (env pointer) | **Dedicate a register** (HotSpot `r15`). bliss already pins r14=slots, r15=operand-stack. |
| `NATIVE_ENV_FRAME` | Carry in the env register / frame header; only closures need it. |
| `NATIVE_ERROR` (caller-checked) | **Unwind** through the frame chain / handler tables (bliss already has the BlissStack + catch/handler stack). |
| `NATIVE_DEOPT` (caller-checked) | **Trap at the guard**; callee resumes on its own frame. bliss already has state-transfer deopt (bliss-izt.2). |
| SIGSEGV recovery IPs (per-c2i toggle) | Structural safepoints + a fixed handler, not per-call. |
| redefinition / recompile safety | **fdefn-style indirection** (SBCL) or **patched call sites** (HotSpot): call through a stable per-function entry slot updated on (re)compile/deopt. |

bliss already has the two hardest prerequisites — **stack maps** (`CodeInfo`) and
**state-transfer deopt on the same frame** (bliss-izt.2) — so it is closer than
it looks. The work is relocating env/error/deopt off thread-locals into the ABI.

## 3. Staged plan

Each stage is independently landable and validated (acceptance,
bytecode_differential for tier consistency, t1_native/t1_deopt, GC stress, deep
recursion). Direct calls are only emitted for a **restricted, safe callee shape**
first (already-native, fixed-arity, non-variadic, non-closure, under the
`NATIVE_DEPTH` cap), with a **c2i fallback** for everything else and a **stale
guard** (callee generation / entry unchanged) that falls back on redefinition or
deopt.

- **Stage 0 — prototype + measure (throwaway).** Behind an env flag, emit a
  direct call for the one restricted shape, checking deopt/error thread-locals
  inline and bouncing to a Rust handler on either. Measure t0bench + asdf to
  confirm the payoff justifies the redesign risk. If the win is marginal, stop
  here and record it.
- **Stage 1 — env in a register.** Pin a callee-saved register to the current
  `Env` (HotSpot-`r15` style); rewrite `c2i_load_env`/`c2i_store_env`/… to read
  it; drop the `NATIVE_ENV` thread-local. Keeps the c2i bounce for now.
- **Stage 2 — error unwinding.** Replace the caller-checked `NATIVE_ERROR` slot
  with an unwind through the BlissStack frame chain to the nearest handler
  (reuse the catch/handler stack), so a callee need not have its error inspected
  by the caller.
- **Stage 3 — self-contained deopt.** Make a guard failure resume T0 on the
  callee's own frame at the trap (bliss-izt.2 already does the resume); drop the
  caller-checked `NATIVE_DEOPT`. Move the backoff/blacklist accounting to the
  trap handler.
- **Stage 4 — direct call emission.** With env/error/deopt off thread-locals,
  emit `push_frame` + arg-bind + a **direct `CALL`** to the callee's entry
  (loaded from a stable per-function slot for redefinition safety), pop, and use
  the result; bounce to Rust only on the rare trap. Enforce the `NATIVE_DEPTH`
  cap (or BlissStack overflow guard) so deep recursion stays bounded.

## 4. Risks and honest caveats

- **GC/safepoint contract.** Direct calls must preserve precise rooting at the
  call safepoint (the caller's operand stack + the new callee frame) — the
  hardest invariant; validate under `BLISS_GC_STRESS`/`POISON`.
- **Deep recursion.** The whole point (bliss-x5y.8) of the current bounce is to
  keep native recursion bounded; a direct call uses the C stack, so the depth cap
  / overflow guard must still fire a catchable STORAGE-CONDITION, not a C-stack
  abort.
- **Distributed payoff.** The ~1.44µs is spread across many small operations;
  even halving it takes asdf from ~6s to perhaps ~4.5s. Stage 0 exists to confirm
  the number before committing to Stages 1–4.
- **Complexity bliss may not need.** SBCL/Chez are fast with **no deopt at all**.
  bliss's tiering+deopt is HotSpot-shaped; the per-call state that blocks direct
  calls exists *because* of deopt/speculation. If native-call throughput is the
  priority over speculation, the simpler direction is fewer moving parts, not
  more.
- **The deeper lever is call *count*.** asdf's dominant issue is the *number* of
  `ensure-inherited`/`ensure-symbol` calls (UIOP's O(n²)), which this does not
  touch. Direct calls reduce per-call cost; they do not change the algorithm.
