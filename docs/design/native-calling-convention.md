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

`push_frame` (a EgclStack bump-allocator) and `bind_params` (args → slots) *are*
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

**HotSpot VM** (interpreter → C1 → C2 with OSR + deopt — egcl's model):

- **`r15` is pinned to the `JavaThread` pointer.** All thread/VM state is reached
  through it; no TLS call. (This is egcl's `NATIVE_ENV` problem, solved with a
  register.)
- **c2i/i2c adapter stubs** bridge the interpreter's stack convention and the
  compiled register convention — literally egcl's `c2i` — but *compiled→compiled*
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

**Takeaway table** (egcl thread-local → the established fix):

| egcl per-call thread-local | Established fix |
|---|---|
| `NATIVE_ENV` (env pointer) | **Dedicate a register** (HotSpot `r15`). egcl already pins r14=slots, r15=operand-stack. |
| `NATIVE_ENV_FRAME` | Carry in the env register / frame header; only closures need it. |
| `NATIVE_ERROR` (caller-checked) | **Unwind** through the frame chain / handler tables (egcl already has the EgclStack + catch/handler stack). |
| `NATIVE_DEOPT` (caller-checked) | **Trap at the guard**; callee resumes on its own frame. egcl already has state-transfer deopt (bliss-izt.2). |
| SIGSEGV recovery IPs (per-c2i toggle) | Structural safepoints + a fixed handler, not per-call. |
| redefinition / recompile safety | **fdefn-style indirection** (SBCL) or **patched call sites** (HotSpot): call through a stable per-function entry slot updated on (re)compile/deopt. |

egcl already has the two hardest prerequisites — **stack maps** (`CodeInfo`) and
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
  with an unwind through the EgclStack frame chain to the nearest handler
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
  cap (or EgclStack overflow guard) so deep recursion stays bounded.

## 4. Risks and honest caveats

- **GC/safepoint contract.** Direct calls must preserve precise rooting at the
  call safepoint (the caller's operand stack + the new callee frame) — the
  hardest invariant; validate under `EGCL_GC_STRESS`/`POISON`.
- **Deep recursion.** The whole point (bliss-x5y.8) of the current bounce is to
  keep native recursion bounded; a direct call uses the C stack, so the depth cap
  / overflow guard must still fire a catchable STORAGE-CONDITION, not a C-stack
  abort.
- **Distributed payoff.** The ~1.44µs is spread across many small operations;
  even halving it takes asdf from ~6s to perhaps ~4.5s. Stage 0 exists to confirm
  the number before committing to Stages 1–4.
- **Complexity egcl may not need.** SBCL/Chez are fast with **no deopt at all**.
  egcl's tiering+deopt is HotSpot-shaped; the per-call state that blocks direct
  calls exists *because* of deopt/speculation. If native-call throughput is the
  priority over speculation, the simpler direction is fewer moving parts, not
  more.
- **The deeper lever is call *count*.** asdf's dominant issue is the *number* of
  `ensure-inherited`/`ensure-symbol` calls (UIOP's O(n²)), which this does not
  touch. Direct calls reduce per-call cost; they do not change the algorithm.

## Concrete implementation (derived while starting Stage 1)

Register model & prereqs (confirmed against the codegen):
- T1 emit uses only r14 (frame slots) and r15 (operand-stack top) among callee-
  saved regs; **r12/r13/rbx are free**. Reserve **r12 = current `*mut EgclStack`**.
- `NATIVE_ENV` is a `const`-init thread-local `Cell` — a ~few-ns read, NOT a
  syscall. Dropping it (the doc's original "Stage 1") is ~0 gain AND unnecessary:
  for native→native the callee reads the already-correct `NATIVE_ENV`, and the
  restricted (non-closure) shape never touches `NATIVE_ENV_FRAME`. **Skip
  env-in-register.** The register that IS needed is the EgclStack pointer.
- GC scans a thread's frames from the **safepoint-published** `fp`/`sp` snapshot
  (`EgclStack::publish`), not live `fp`/`sp_offset`. So the emitted frame push
  MUST update `EgclStack.fp` and `sp_offset` (and pop must restore them), or a
  GC while the callee runs won't scan the callee frame → live args/locals move →
  stale pointers. Reference the field offsets via `core::mem::offset_of!`
  (stable) so codegen tracks the real layout — no hardcoding.

ABI surgery (the crash-on-mismatch step; do carefully, validate before commit):
- Entry ABI `fn(*mut u64) -> u64` → `fn(*mut u64, *mut EgclStack) -> u64`
  (rdi=slots, rsi=stack).
- Prologue (bytecode.rs ~15473): `push r14; push r15; sub rsp,8; mov r14,rdi;
  lea r15,[r14+8n]` → `push r14; push r15; push r12; mov r14,rdi; mov r12,rsi;
  lea r15,[r14+8n]` (3 pushes keep rsp 16-aligned; the `sub rsp,8` is dropped).
- EVERY epilogue `add rsp,8 (48 83 C4 08); pop r15; pop r14; ret` → `pop r12
  (41 5C); pop r15; pop r14; ret`. Sites: Return (~16018), back-edge loop-exit
  (~15988), OSR stub (~16101), naked recovery (11600). `pop r12` has the same
  rsp effect as `add rsp,8`, so alignment is preserved.
- Update the 4 transmute sites (13417, 15000 run_native, 16818, 17456) to pass
  the stack pointer in rsi.
- T2 (egcl-compiler/codegen.rs prologue ~232): mirror the r12 preservation and
  exclude r12 from its allocator; until then, restrict direct-call EMISSION to
  T1 callers (T2 entries still accept the extra rsi arg harmlessly).

Direct-call emission (restricted shape: already-native, fixed-arity,
non-variadic, non-closure, `NATIVE_DEPTH < cap`; else fall through to c2i):
1. Depth guard: read/bump `NATIVE_DEPTH`; over cap → c2i fallback.
2. Bounds-check `sp_offset + framebytes <= capacity` (via r12 + offset_of!);
   overflow → c2i fallback (which raises the catchable STORAGE-CONDITION).
3. Write the callee `Frame` header at `base+sp_offset`, zero its slots, copy the
   nargs from the caller operand stack into the callee slots, set the callee's
   r14; update `EgclStack.fp` = new frame and `sp_offset` += framebytes.
4. Direct `CALL` the callee entry (native→native — NO set_recovery toggle, NO
   catch_unwind; those are only for Rust c2i crossings). `NATIVE_ENV` already
   correct; recovery IPs already `native_recovery` (set by the outer run_native).
5. On return: restore `fp`/`sp_offset`, restore `NATIVE_DEPTH`; check
   `NATIVE_ERROR`/`NATIVE_DEOPT` (thread-locals) — if set, bounce to a Rust
   handler that runs the existing deopt/error path; else push the result.
   (Stages 2–3 can later move error to unwinding and deopt to self-resume,
   removing these post-checks; not required for a first working version.)

Validation gates each step: acceptance, bytecode_differential (tier identity),
t1_native, t1_deopt, egcl-rt lib single-threaded (SIGSEGV recovery), and
EGCL_GC_STRESS=1/POISON on a native-call-heavy form + deep recursion (depth cap).

## Stage 2 — direct-call emission (concrete spec; measurement-prototype first)

Prereq DONE: Stage 1 (c72eeaf) reserves r12 = *mut EgclStack in the native ABI.

Emit-time gate (in emit_native_x86's `Instr::CallNamed { sym, nargs }` arm, behind
EGCL_NN_DIRECT for the prototype): the callee `sym` is (a) currently in
NATIVE_REGISTRY (native entry E, code_info CI, num_slots NS), (b) fixed-arity ==
nargs and non-variadic (registry_get(sym)), (c) not a closure (absent from
CLOSURE_ENV and CLOSURE_CONTROL). Else emit the existing c2i path.

Offsets via `core::mem::offset_of!` (robust): Frame{prev_fp@0,return_pc@8,
function@16,code_info@24,flags@32,num_locals@36}, HDR=size_of::<Frame>()=40;
EgclStack{base,capacity,sp_offset(Cell),fp(Cell)}. FRAMEBYTES = HDR + 8*NS.

Encoding notes: r12 and r15 as a base need a SIB byte (ModRM rm=100, SIB=0x24);
r14 does not (rm=110). imm64 (E, CI) load via `mov r64, imm64`; NIL/FLAG_CALL fit
imm32 (sign-extended store `C7`).

Emitted sequence at the call site (rsp is 16-aligned here):
```
  mov  rcx, [r12+BASE]          ; base
  mov  rdx, [r12+SPOFF]         ; old sp_offset (restore value)
  lea  rax, [rcx+rdx]           ; frame_start = base + sp_offset
  mov  r10, [r12+FP]            ; old fp (restore value + prev_fp)
  mov  [rax+0],  r10            ; prev_fp
  mov  q[rax+8], 0              ; return_pc
  mov  q[rax+16],NIL            ; function
  mov  r11, CI ; mov [rax+24],r11
  mov  d[rax+32],FLAG_CALL
  mov  w[rax+36],NS
  ; copy nargs args: for i in 0..nargs: [rax+HDR+8i] = [r15-8*nargs+8i]
  ; zero slots nargs..NS to NIL
  mov  [r12+FP], rax            ; publish new fp (GC scans callee frame)
  lea  r11, [rdx+FRAMEBYTES] ; mov [r12+SPOFF], r11
  push r10 ; push rdx           ; spill restore values (keeps 16-align)
  lea  rdi, [rax+HDR]           ; arg0 = callee slots
  mov  rsi, r12                 ; arg1 = stack
  mov  rax, E ; call rax        ; DIRECT native→native (no toggle/catch_unwind)
  pop  rcx ; pop r10            ; old sp_offset, old fp
  mov  [r12+FP], r10 ; mov [r12+SPOFF], rcx   ; pop callee frame
  sub  r15, 8*nargs ; mov [r15],rax ; add r15,8   ; pop args, push result
```
The callee's prologue preserves r12/r14/r15 (push/pop), so they survive the call.

Prototype limitations (measurement only; add before production):
- No depth guard (deep recursion → C-stack overflow). Add: read NATIVE_DEPTH
  (or rely on a C-stack guard page); over cap → c2i fallback.
- No bounds check (sp_offset+FRAMEBYTES <= capacity). Add → c2i fallback / raise.
- No error/deopt post-check. Add: after the call, test NATIVE_ERROR/NATIVE_DEOPT
  and bounce to a Rust handler; else use rax.
- No stability guard: E/NS baked at emit time. If the callee is redefined or
  deopted, the baked entry is stale. Add a guard (compare the callee's current
  entry to a stable per-fn cell) or invalidate the caller on callee change.

Measure EGCL_NN_DIRECT on t0bench (expect ~0.52µs → ~0.2-0.3µs) to confirm the
win before hardening. Then add the four guards, drop the flag, and extend to T2
callers (exclude r12 from the T2 allocator first).
