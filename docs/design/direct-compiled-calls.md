# Direct calls between compiled functions: measured gaps and plan

Status: **proposal for review** · Beads: `bliss-gg3i1.7.25`, `bliss-gg3i1.7.26`
(under `bliss-gg3i1.7`) · Contract: [x86-64 native Lisp ABI](../../spec/04-07-x86-64-lisp-abi.md),
roadmap [§11.4.3](../../spec/11-phasing-roadmap.md#1143-x86-64-native-cross-function-calls)
(`bliss-t8a48`).

This is not a new ABI. The ordinary register ABI in the spec already describes
compiled code calling compiled code without Rust in between: three register
arguments and an overflow area, versioned call-cell descriptors, precise native
frames, and managed state materialized only at cold boundaries. Calls between
two ordinary-ABI bodies already work that way. This document measures where
the hosted SBCL compile still leaves that path, ranks the gaps by the time
they cost, and proposes the order to close them.

## 1. Where the time goes

`perf record -e cycles:u -g --user-callchains` on a frame-pointer release build
of main `cc6aa1d1` (with the decline census of #1022, disabled), compiling
SBCL's `src/compiler/sset` after files 1–66 for effect (the repro driver),
55.7 s compile, 55,382 main-thread samples at 999 Hz, classified by
`census.py`:

| share | where |
|---|---|
| **50.0%** | Rust runtime helpers called from JIT code, including the hash/registry lookups under them |
| 18.5% | tree-walker, including lookups under it |
| 13.5% | T0 bytecode interpreter, including lookups under it |
| 4.7% | `resolve_type_spec` |
| **8.3%** | JIT-compiled code itself (T1, T2, unmapped stubs) |
| 2.9% | GC |

Half the CPU is the call boundary between compiled bodies and the runtime,
and the compiled bodies themselves are a sixth of that. The JIT entry helper
each of those samples went through:

| share of CPU | helper JIT code called | main path under it |
|---|---|---|
| 13.1% | `checked_call_published` | `mapped_ordinary::call` (8.4%): a mapped-ABI caller calling an ordinary-ABI callee through a lease |
| 11.4% | `ordinary_calls::named_entry` | FUNCALL and builtin bridges, `call_table::warm`/`cold` |
| 6.3% | `nested::prepare_nested` | a mapped child activation per call |
| 3.6% | `call_table::warm_slice` | `call_table::invoke` → `run_native_callable` |
| 2.8% | `call_or_throw` | |
| ~13% | cold slices, builtin slices, struct helpers, others | |

No leaf dominates. The largest are activation recycling (2.0%), child
preparation, lease construction and drop, execution-local lookups, root
retention, owner-directory checks and polls, each 1–2%. That is why trimming
individual steps has not paid: `#1000`/`#1001`/`#1011` cut managed preparations
31.6% with no wall-time gain, and admitting env-free closures to the lean
FUNCALL removed 4.1M of sset's 12.9M preparations for 1.7% (8-run ABBA,
overlapping ranges). The cost is the *existence* of a managed transition per
call, not any one of its steps.

`perf`'s frame-pointer unwind stops at the native segment stack switch, so it
cannot name the compiled caller of each helper. The activation census
(`EGCL_ACTIVATION_CENSUS` with `_ORIGINS`, #994) supplies that attribution.

## 2. Which calls leave the ordinary path

sset makes 12,943,079 managed activation preparations. By site, ingress and
receiver:

| share | preparations | path | cause, in the spec's terms |
|---|---|---|---|
| 31.9% | 4,125,219 | lean root via `named_entry` | T2 FUNCALL of a closure whose entry word was never armed |
| 27.8% | 3,596,980 | general root, Rust outer, closure receiver | Rust code (MAPHASH, MAPC, SORT and other builtins) calling a closure back |
| 15.7% | 2,035,097 | general root, mapped named | a variadic callee: FIND alone is 1.26M |
| 5.3% | 682,583 | general root via `named_entry` | callee with no ordinary entry |
| 4.0% | 514,196 | lean root, Rust outer, plain function | Rust code calling a compiled function |
| 2.9% | 374,288 | nested child, Rust outer, closure | as above, nested |
| ~12% | | other combinations | |

Installed code on sset: 3,299 bodies with the ordinary ABI and 1,927 mapped.
Only about 190 mapped installations record an ordinary-entry decline (primitive
lowering 87, parameter shape 31, possibly-multiple-value return 22, MAKE-CLOSURE
12, IR verification 8, protected scopes 10). Most mapped installations are
top-level forms (496), lambdas (125) and FLET closures (36): bodies that capture
an environment, which the generic tagged emitter rejects before the ordinary
step (spec, *Implementation scope*).

Mapping these to the spec's listed gaps:

| gap (spec wording) | evidence on sset |
|---|---|
| closures' entry words not armed for FUNCALL | 31.9% of preparations, 6.79M FUNCALLs reached `named_entry` |
| captured environments | the closure callees of the 27.8% and 2.9% rows; 657 mapped closure installations |
| variadic native bodies | 15.7% (FIND and other `&key` callees) |
| callable-object and builtin adapters still building managed state | the Rust-outer rows, ~35% together |
| protected control scopes (`bliss-umrdy`) | 10 declines; small here |
| raw numerics (`bliss-ps221`), unsupported opcodes | about 100 declines; their call share is not yet attributed |

## 3. Plan

Each step closes one gap, keeps every other path working through its existing
adapter, and must show a measured result before the next starts (section 4).
The order is by measured share.

**Step 1 — FUNCALL of receiver-free closures (done locally,
`bliss-gg3i1.7.27`).** `closure_entry::teach` and
`call_table::private_probe_target` required the explicit-callable slot, which
only tiny leaf closures get. Code with no slot and no captured environment
never reads its receiver, so every instance may enter it through the template
stub. sset: preparations 12.95M → 8.83M, `named_entry` FUNCALLs 6.80M → 2.69M,
compile median 55.57 s → 54.64 s (−1.7%, ranges overlap). Small because each
avoided transition was cheap; it removes the largest single counted source at
no cost, and it is a prerequisite for step 2's FUNCALL path.

**Step 2 — ordinary entries for closures with captured environments.** The
spec already reserves `RAX` for the closure, and Backpack gives native code
direct access to heap lexical environments (#968, #971, #1016). A captured
closure body would load its environment from the closure in `RAX` in the
prologue and otherwise run as an ordinary tagged body; its template stub
enters it for every instance, as step 1 does for receiver-free code. Deopt,
GC maps and unwind need the closure as an incoming root, which the entry map
already covers ("entry publication covers the closure"). This retires the
closure half of the 27.8% and 2.9% rows once their callers are ordinary, and
most of the remaining FUNCALL misses.

**Step 3 — a lean outer adapter for Rust callers.** MAPHASH, MAPC, SORT,
REDUCE and similar builtins call their function argument from Rust through
`apply_function`, which builds a general root activation each time (27.8% +
4.0%). The spec's outer callable-object adapter enters a segment once; a
callback loop inside one builtin call should enter the callee's ordinary entry
for each element without a new activation, reusing one segment for the whole
builtin call. Alternative where it is simpler: compile the hot iteration
builtins as Lisp so their callback becomes an ordinary compiled call.

**Step 4 — variadic ordinary entries.** `&rest`/`&key` bodies get an ordinary
entry that binds from the register/overflow arguments and the count in `RCX`,
as the existing `&key` ordinary binding (`OrdinaryKeywordArguments`) does for
fixed keyword shapes, falling back to the general binder only on an unseen
keyword or an argument error. FIND alone is 1.26M preparations.

**Step 5 — mapped callers.** `checked_call_published` through
`mapped_ordinary::call` (8.4% of CPU) is a mapped-ABI caller calling an
ordinary callee. Steps 2–4 move most callers to the ordinary ABI; what remains
(protected scopes, raw numerics) is `bliss-umrdy` and `bliss-ps221`, ranked by a
fresh measurement at that point.

**Step 6 — the per-entry anchor (`bliss-gg3i1.7.25`).** Once compiled-to-compiled
calls stay native, the remaining crossings are segment entries from Rust.
Replace their guard set (catch, dynamic-scope, block/tag vectors swapped out
and back, depth, fault recovery, segment) with one linked record holding
integer watermarks, reached from `R13`. That record is also what lets an unwind
cross a Rust helper frame, removing `try_run_enabled`'s decline when a segment
is already active. Running T1 on the interpreter's own frame is a further,
separate design question and is not proposed here.

## 4. Measurement protocol

Every step reports, on the same base with and without the change:

1. **Wall time:** sset compile time, ABBA order, at least four runs per arm,
   own SBCL tree per run, host-1 FASLs built by the same binary as the arm when
   the change affects compilation output. Report each run and the medians, and
   say plainly when ranges overlap.
2. **Where the time went:** `perf -e cycles:u -g --user-callchains` on a
   frame-pointer build (`RUSTFLAGS="-C force-frame-pointers=yes"`), classified
   by `census.py`; the step should shrink the helper share it targets.
3. **What it removed:** activation-census preparations by row, and
   `EGCL_BUILTIN_CENSUS` route counts, so a wall result can be tied to the
   transitions it changed.
4. **node** (`src/compiler/node`, about 35–50 minutes) before a step is called
   done, since sset is a fifth of a minute of real compile work and node is
   where host-2 spends its time.

Counts alone are not results: removing 42% of native/T0 crossings (#1023) and
32% of managed preparations (step 1) each moved sset by less than the run-to-run
noise.

## 5. Risks

- **Step 2 changes the entry contract for closures.** The closure arrives in
  `RAX` and must be an incoming root from the first instruction; the entry map,
  arity-error stubs and deopt continuations must all include it. GC stress with
  observed relocation of the closure is required, not only a clean stress run.
- **Step 3 holds one segment across many callback calls.** Fiber suspension,
  signal delivery and non-local exits out of a callback must still find the
  builtin's Rust frame; the single anchor of step 6 may be a prerequisite.
- **Payoff is bounded by what remains.** Compiled code is 8% of sset's CPU and
  the interpreter and tree-walker another 32%. Even removing the whole 50%
  helper share is about 2x on sset, not 10x; the interpreter share needs
  admission work (the decline census in #1022) in parallel.
- **This reworks the Backpack call protocol.** Steps 2 and 3 should be reviewed
  against `bliss-gg3i1.7.15` before implementation.
