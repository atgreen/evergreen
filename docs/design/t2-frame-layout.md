# Design: the T2 native frame and its activation slots

Status: **as-built** · Related: bliss-kqdr, bliss-jtc.4, bliss-ht4, bliss-x9c9,
bliss-jtc.9 · Spec: `spec/04-10-t2-frame-state.md` (IR-level FrameState),
`spec/04-07-codegen.md`

`spec/04-10-t2-frame-state.md` specifies `FrameState` — the *IR-level* mapping
from interpreter slots to SSA values that deopt reconstructs from. It does not
describe the **physical** frame T2 code runs on. That gap is not cosmetic: it
allowed bliss-kqdr, a silent wrong-result bug where a hot loop returned the
adjacent frame's raw pointer instead of its own parameter. This document records
the layout as built, and the invariants that must hold when entering T2 code.

## 1. Two stacks, not one

A running native function touches two distinct stacks. Confusing them is the
single easiest way to introduce a memory-safety bug here.

- **The host (C) stack**, addressed off `rsp`. Holds T2's spill slots, the
  register save area around c2i helper calls, and the saved slots pointer.
  Private to one native activation; nothing outside the function reads it.
- **The EgclStack** (`crates/egcl-rt/src/stack.rs`), a bump-allocated region
  of `Frame`s. This is what the GC scans and what deopt reconstructs from. It is
  shared: the callee's frame is pushed immediately above the caller's.

"Slots" in T2 always means EgclStack activation slots, never host-stack spills.

## 2. Frame layout on the EgclStack

`Frame` is a fixed 40-byte header (`#[repr(C)]`, D2.02) followed by a
variable-length slot area:

```
frame_ptr ──► +0x00  prev_fp     *mut Frame     link to the previous frame
              +0x08  return_pc   *const u8
              +0x10  function    EgclVal
              +0x18  code_info   *const CodeInfo   safepoint/stack map
              +0x20  flags u32 | num_locals u16 | _pad u16
              +0x28  slots[0]    EgclVal       ◄── the "slots pointer"
              +0x30  slots[1]
              ...
              +0x28 + 8*(num_locals-1)  slots[num_locals-1]
              ──────────────────────────────────────── end of this frame
              the NEXT frame's header starts here
```

Two consequences that matter:

- **The slots pointer is `frame_ptr + 0x28`, not `frame_ptr`.** The native entry
  ABI is `extern "C" fn(*mut u64) -> u64` and receives the *slots* pointer. So
  in a disassembly `[rdi+0x40]` is `slots[8]`, not "offset 0x40 of the frame".
  Getting this wrong makes slot indices look like header fields and vice versa.
- **There is no guard region after the slot area.** A store to `slots[i]` with
  `i >= num_locals` lands in the next frame's header — silently, and typically
  on `prev_fp` or `return_pc`, which is why such a bug surfaces as a *plausible
  looking raw pointer* rather than a crash.

`push_frame` never reallocates: it fails (returns `None` → `StackOverflow`) when
capacity is exhausted. So a slots pointer stays valid for the life of its frame;
a stale-pointer theory for corruption here is wrong by construction.

## 3. How many slots a frame gets

This is the invariant bliss-kqdr violated.

| Entry path | Frame pushed by | `num_slots` used |
|---|---|---|
| Full call into T1/T2 | `run_native` | `nc.num_slots` |
| Full call into T0 | `run` | `BytecodeFunction::num_slots()` |
| T1→T2 back-edge OSR | *nobody — reuses the live T1 frame* | whatever T1 pushed |

`nc.num_slots` is **not** the bytecode slot count. For T2 it is

```
total_slots = bf.num_slots() + artifact.shadow_root_slots
```

computed by `validate_t2_root_sync` (`cli/bytecode.rs`). The shadow root slots
are appended *after* the bytecode's own slots, and T2 addresses them by absolute
index (`emit.rs`: `let slot = activation_slots as i32 + i as i32`). They are how
native code publishes its live GC references at a safepoint, so the collector
can find and relocate values that would otherwise live only in registers.

**Invariant.** T2 code may only be entered on a frame whose `num_locals >=
nc.num_slots`. Enter it on a smaller frame and every shadow-root store is an
out-of-bounds write into the neighbouring frame.

Two guards enforce this, and a third was missing:

1. `validate_t2_root_sync` refuses to install code with both
   `shadow_root_slots != 0` and `compiled_entry != 0` — the direct native→native
   entry cannot guarantee a large enough frame, so that combination never ships.
2. The emitter declines an OSR *entry point* whose live slots are not all
   `Tagged`, rather than emitting a transfer it cannot perform soundly
   (bliss-ht4).
3. **Missing until bliss-kqdr:** `c2i_t1_backedge` invoked the T2 OSR entry with
   the still-live *T1* frame. Nothing checked that the T1 activation was big
   enough for T2's shadow roots. It now declines the OSR entry when
   `t2.num_slots > body.num_slots()`.

## 4. What a T2 call site does with the frame

Around a call, T2 does **not** assume its values survive in registers. It syncs
them into activation slots and reloads them afterwards:

```asm
; before the call — publish locals into the activation
mov rdx,[rsp+0A8h]        ; saved slots pointer
mov [rdx+40h],<o>         ; slots[8]  (a shadow root slot)
mov [rdx+48h],<r>         ; slots[9]
call ...
; after the call — RELOAD, because a GC during the call may have moved them
mov rdx,[rsp+0A8h]
mov rax,[rdx+40h]
mov [rsp+28h],rax
```

The reload is deliberate and must not be "optimised away": the moving collector
updates the activation slot, not the host-stack spill, so after any call that
can allocate, the activation is the authoritative copy. This is the same reason
`rooted!`/`rooted_ref!` exist on the Rust side (`docs/design/gc-rooting.md`).

It also means a too-small frame corrupts in the nastiest possible way: the store
goes out of bounds, and the reload faithfully brings the neighbouring frame's
header back as if it were your value.

## 5. Debugging checklist

When a native tier produces a wrong *value* (not a crash):

- **Identify the tier precisely.** `EGCL_FORCE_TIER=interp|t0|t1|t2` pins tier
  selection process-wide; a value that differs between them is a miscompile.
  `scripts/tier-diff.sh` does this over a corpus.
- **Isolate caller from callee.** Excluding one function from T2 separates "bad
  codegen here" from "bad codegen in what I call". There is no per-name knob for
  this any more — `EGCL_T2_EXCLUDE` was removed once the bug it was written for
  (bliss-lwws, narrowed to REDUCE) was fixed, because it sat on the per-dispatch
  path. Reintroduce it locally if you need it, or bisect with
  `EGCL_FORCE_TIER` over a corpus.
- **Know which knob gates which transfer.** `EGCL_OSR_THRESHOLD` gates the
  T0/T1 OSR entry. The **T1→T2** back-edge transfer is gated by
  `EGCL_T1_T2_BACKEDGE_THRESHOLD` / `EGCL_LOOP_HEAT_THRESHOLD`. In bliss-kqdr
  the wrong knob made an OSR bug look unrelated to OSR.
- **Suspect frame sizing when the garbage is a plausible address.** A raw
  pointer appearing where a Lisp value belongs, that *changes with the callee's
  frame size*, means an out-of-bounds slot access, not a tagging error. Note
  heap objects are 8-byte aligned, so a raw pointer has fixnum tag bits and
  prints as a huge FIXNUM.
- **Perturb the frame.** Adding unused locals to the caller changes its slot
  count; if that makes the bug disappear, the fault is layout-dependent.
- `EGCL_T2_DISASM=<NAME>` dumps the emitted code; `EGCL_RA_DBG=1` dumps each
  value's stable home plus every FrameState-carrying instruction.

## 6. Why the tier-differential corpus did not catch bliss-kqdr

`tests/differential/tier-corpus.lisp` exercises CLOS dispatch, NLX, MV state and
loops across all four tiers, but its loops run far below the T2 promotion
threshold, so the T1→T2 OSR transfer never fires. Corpus coverage of a *shape*
does not imply coverage of a *tier transition*. Tests that must exercise
promotion belong in `crates/egcl/tests/osr.rs`, which drives the real binary
with forced tiers and explicit thresholds.
