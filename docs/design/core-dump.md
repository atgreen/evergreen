# Heap-snapshot core dump (bliss-x0f2)

## Why

`save-lisp-and-die` currently re-serializes the world as **source-form load
actions** (`build_image_from_runtime`): `(defpackage …)`, `(defclass …)`,
`(defun …)`, `(defmacro …)`, `(defparameter …)`, etc., replayed at load. That
captures *code* but not non-serializable runtime *data*: hash-tables, CLOS
instances, closures, streams (`pool.value` returns `None` and skips them). ASDF's
state lives exactly there (`*operations*`, `*defined-systems*` registries;
operation/component instances), so an ASDF-preloaded image (`make image`) can
hold ASDF's code yet fail to load a library:

```
(asdf:load-system :cl-ppcre)
  ; undefined function UIOP/UTILITY::NEST          — dropped macro   (FIXED 1cd81f3)
  ; SETF: unsupported place (operate-level)        — dropped setf-fn (FIXED 1cd81f3)
  ; The variable ASDF/OPERATION::*OPERATIONS* is unbound  — dropped hash-table global
```

A **heap-snapshot core dump** (like SBCL's `save-lisp-and-die`) captures the
whole live world byte-for-byte, so any value round-trips.

## Architecture facts this builds on

- The GC heap is **one contiguous anonymous mmap** `[heap_base, heap_base +
  heap_size)` (gc.rs `init_heap`), carved into fixed-size regions.
- The collector is **precise and moving**: `major_gc` compacts live objects and
  `relocate_slot` / `trace_object` already rewrite *every* pointer in *every*
  reachable object and root. A core dump reuses this machinery.
- The "world" is **split**: Lisp objects live on the heap, but the roots that
  reach them are **Rust-side registries** outside the heap —
  - `torcl_rt::symbols`: the symbol table (name → index, value cells, function
    cells, plist),
  - `torcl-stdlib` packages,
  - cli.rs host registries: process-wide `GLOBAL_MACROS`, thread-local
    `GLOBAL_SETF_FNS`, setf-expanders, and class/generic/method metadata,
  - torcl-stdlib's process-wide `CLOS_STATE`,
  - compiled-function registry (bytecode).
  A core dump must serialize **both** the heap span and these registries.
  (SBCL keeps symbols/packages/functions *on* the Lisp heap, so its core is just
  the heap; torcl's hybrid model makes the registries extra work.)

## What already exists (discovered during M1)

A spec §7.2–7.3 heap-image system is **implemented and unit-tested in-process,
but not wired to the CLI**:

- `torcl_rt::image::save_image` / `load_image` — full file format: `ImageHeader`
  (magic `TORCLIMG`), section table, sha256 checksums, optional zstd, and
  `find_appended_image` (already supports the executable-append case for M4).
- `torcl_rt::gc::serialize_heap_objects` / `restore_heap` — walk every live
  object (`walk_heap`) and re-materialize it (`append_serialized_object`); a
  **relocating** restore, fixed up by `serialize_relocation_table`.
- `serialize_code_cache` / `restore_code_cache` — the compiled bytecode.
- 24 passing tests (`test_image.rs`, `spec_image_ops.rs`) — all **in-process**
  (save then load in the same process).

The gaps that keep it from being a cross-process core dump (what
`save-lisp-and-die` / an installed executable actually need):

1. **Not wired.** `save-lisp-and-die` / `--image` use the source-form path
   (`build_image_from_runtime`), never `save_image`/`load_image`.
2. **Symbols/packages are stubbed for cross-process.** `serialize_symbols` /
   `serialize_packages` return empty by design — they rely on the Rust-side
   symbol/package registry *surviving in-process*. A fresh process has an empty
   registry, so a loaded heap's `SymbolData`/`PackageData` objects exist but
   nothing indexes them. Real cross-process serialization
   (`crate::symbols::serialize`/`restore`, per the code comment) must be used.
3. **torcl-crate registries are invisible to torcl-rt.** `GLOBAL_MACROS`,
   `GLOBAL_SETF_FNS`, setf-expanders, `CLOS_STATE`, class/generic/method tables
   live in the `torcl` crate; `image.rs` (in `torcl-rt`) can't see them. Needs a
   registration hook (like the existing root-scanner registration) so the CLI
   contributes extra image sections.
4. **Off-heap object bodies.** Streams and similar hold raw pointers to off-heap
   Rust allocations that can't be snapshotted — enumerate and forbid/re-open.

So the plan below is **wire + complete the existing system**, not build anew.

## Approach

Fixed-base snapshot with a relocation fallback (note: the existing `restore_heap`
already RE-MATERIALIZES objects and fixes pointers via the relocation table, so
"relocation" is the default path, not just a fallback):

**Dump** (`save-lisp-and-die`, core format):
1. `major_gc` to compact — live data becomes a dense prefix of the heap.
2. Record `heap_base` and the live high-water `live_size`.
3. Write the live span `[heap_base, heap_base + live_size)` verbatim.
4. Serialize each Rust registry: its structure plus the `TorclVal`s it holds
   (which are absolute heap addresses, valid at `heap_base`).
5. Header: magic, version, `heap_base`, `heap_size`, `live_size`, registry
   sections, optional `:toplevel`.

**Load** (`--image` core format):
1. `init_heap` mmap'ing at the recorded `heap_base` via `MAP_FIXED_NOREPLACE`.
2. `read` the live span back into `[heap_base, +live_size)`; rebuild region
   metadata so the allocator continues past `live_size`.
3. If the recorded base was unavailable (ASLR/occupied): map anywhere and run a
   **rebase pass** — walk every heap object's pointer slots and every registry
   `TorclVal`, adding `new_base - old_base` (a uniform delta; reuses the precise
   object walker, not forwarding).
4. Restore the Rust registries (delta-adjusted if rebased).

## Milestones

- **M1 — contiguous heap dump/restore.** Add a heap-span accessor + `major_gc`
  compaction hook; serialize/deserialize the live span; fixed-base remap with a
  delta rebase fallback. Done-signal: a synthetic heap of conses/strings/vectors
  round-trips (dump → fresh process → structurally `equal`), verified by a unit
  test driving the rebase pass directly.
- **M2 — registry serialization.** Symbols (name, value cell, function cell,
  plist), packages, `GLOBAL_MACROS`, `GLOBAL_SETF_FNS`, setf-expanders,
  `CLOS_STATE`/classes/generics/methods, compiled-function registry. Each as a
  section of `(key, TorclVal…)` records, delta-adjusted on rebase. Done-signal:
  a `defvar`/`defun`/`defmacro`/`defclass`/`make-instance`/`make-hash-table`
  world round-trips via core dump (including the hash-table + instance values).
- **M3 — wire to `save-lisp-and-die` / `--image`.** New core format selected by a
  magic header; keep the source-form path as a fallback/`:format` option.
  Done-signal: `(asdf:load-system :cl-ppcre)` succeeds from a core image; ASDF
  `*operations*` is bound.
- **M4 — executable wrapping.** Append the core to a copy of the runtime binary
  (reuse `wrap_executable` / `embedded_image`); `make image` produces a
  self-contained `torcl` whose libraries load. Done-signal: installed `torcl`
  runs `(asdf:load-system :cl-ppcre)`.

## M2 progress + the per-object-relocation blocker (discovered while implementing)

Done and committed:
- **Tag-aware relocation** (2ee6adc): `serialize_relocation_table` now records
  TAGGED Lisp pointer fields (cons/heap-object/function), not just raw body
  pointers. Prerequisite for relocating a real object graph.

Blocker found (a symbol-registry rebuild attempt SIGSEGV'd and was reverted):
- `restore_heap` re-materializes objects by **bump-appending in dump order**, and
  the loader relocates with a **single uniform delta** (`current_base -
  original_base`). That is only correct when the restored heap reproduces the
  saved heap's EXACT layout — true for the dense `record_object` tests, but NOT
  for a real, non-compacted heap, nor when the loading process already holds
  allocations. A registry rebuild that shifts saved symbol addresses by the
  uniform delta then indexes GARBAGE → segfault.
- **Required foundation:** replace uniform-delta relocation with a **per-object
  old→new address map**. Concretely:
  1. `serialize_heap_objects`: prepend each record with the object's OLD body
     address.
  2. `restore_heap`: build `old_body → new_body` as it appends; expose it.
  3. Relocation: walk restored objects' pointer fields and remap each via the map
     (tag-aware), replacing `apply_relocations`/uniform delta.
  4. Registries (symbols, packages, macros/setf/CLOS): serialize
     `index → old_body`; on restore, map to `new_body` and rebuild — no
     re-interning, no uniform delta.
  This makes cross-process restore correct for any heap layout; it changes the
  heap section format and several §7 tests, so it is its own focused change and
  the prerequisite for M2's registry rebuild, M3, and M4.

## M2 done + the reframing that shrinks M3 (discovered while wiring the host hook)

Committed since the blocker note above:
- **Per-object relocation map** (8ee6282): `serialize_heap_objects` prepends each
  record with its OLD body address; `restore_heap` builds `old_body → new_body`
  and remaps every pointer field tag-aware (`remap_saved_pointer`). Replaces the
  uniform-delta relocation the blocker identified as unsound.
- **Symbol registry cross-process rebuild** (8ee6282): `symbols::restore_objects`
  re-indexes the RESTORED symbol objects; **value cells survive** (test:
  `symbol_value` → 42 after reload).
- **Package registry cross-process rebuild** (2985bf2): mirrors symbols;
  off-heap `lock` reset; type-checked registry guards.
- **HostRegistries section + hook** (b47dd86): `set_host_registry_hooks`; a
  section written from the serialize hook and restored LAST (after
  heap/symbols/packages) so the hook can `remap_saved_pointer` its saved
  pointers. Dummy round-trip test proves the plumbing + remap.

**The reframing.** The bug (`*OPERATIONS* is unbound` from a saved image) is a
non-serializable global *value* — a hash-table. That value is a heap object
reachable from the symbol's **value cell**. Both now round-trip: `restore_heap`
re-materializes the hash-table, `restore_symbols` rebuilds the value cell pointing
at it (remapped). **So the heap snapshot already closes the actual gap** — the
thing the source-form path structurally could not do. The remaining registries
(`GLOBAL_MACROS`, `GLOBAL_SETF_FNS`, `CLOS_STATE`) are Rust-side *code* tables,
not on the heap; they are what the host hook must carry.

**Consequence for M3 — one world, not two.** A core image restores the heap
wholesale; it must NOT also *replay source-form load actions*, because replaying
`(defmacro …)`/`(defun …)` RE-ALLOCATES bodies on the fresh heap and diverges
from the snapshot. So the core-format path is pure heap-snapshot: heap + symbols +
packages + host-registry section. The existing `build_image_from_runtime`
source-form path stays as the *legacy* format (selectable), not mixed in.

**`captured_frame` resolution (the earlier worry).** `MacroDef.captured_frame`
is a Rust-side `Rc<RefCell<EnvFrame>>`, not a heap value, so it cannot be a
remapped pointer. For **global** (top-level `defmacro`) macros the captured frame
is just the root env shell — global bindings resolve through the symbol registry,
not the frame. So the host hook serializes only `(name, params_form, body)` with
pointers remapped on restore; `bytecode = None` (recompile on demand). The
restore hook cannot see the fresh interpreter's root frame (it runs inside
`load_image`), so it **stashes the remapped records in a thread-local pending
buffer**; the CLI drains that buffer right after it builds the top-level `Env`,
constructing each `MacroDef` with `captured_frame = the fresh root frame`. Same
shape for `GLOBAL_SETF_FNS` (a `FunDef`) and for CLOS tables.

## M3 integration constraints (discovered reading the CLI save/load paths)

Two dispatch points already exist and are where M3 hooks in:
- **Save:** the `SAVE-LISP-AND-DIE` builtin (cli.rs ~14307) currently always calls
  `bytecode::build_image_from_runtime` (source-form `.bfasl`), then optionally
  `wrap_executable`. M3 adds a core path: run a compacting `major_gc`, set the
  entry continuation (+ `:toplevel`), call `torcl_rt::image::save_image(path,
  opts)`; reuse `wrap_executable` for `:executable t`. Select via `:format :core`
  (default stays `.bfasl` until the core path is proven).
- **Load:** `run()` dispatches both `embedded_image()` (~23841) and `--image`
  (~23863) on `BFASL_MAGIC`. M3 adds an `IMAGE_MAGIC` (`TORCLIMG`) branch →
  `image::load_image(path)` then `drain_pending_host_registries(&env.frame)`.

**The hard constraint: a core loads into a FRESH runtime, before bootstrap.**
By the time `run()` reaches the `--image` branch it has already built `Env` and
evaluated the bootstrap prelude, so the heap is populated. `restore_heap` calls
`clear_heap_objects` and re-materializes the snapshot — loading a core *over* a
live heap would strand every `TorclVal` the Rust-side `Env` (frame vars, funs,
macros, closures) holds, because `load_image` remaps heap objects and the
symbol/package registries but NOT the interpreter's `Env` (it is Rust state it
cannot see). So the core branch must run EARLY:

1. Detect a core image (appended `embedded_image()` or `--image`) *before*
   `Env::new` + bootstrap.
2. Ensure the heap is initialized, then `image::load_image` (restores heap +
   symbol value/function cells + packages; drains host macros/setf into the
   pending buffer).
3. `Env::new` for a fresh empty frame; `drain_pending_host_registries(&env.frame)`
   to install macros/setf bound to that frame.
4. SKIP the bootstrap prelude — the core already contains it.
5. Run `:toplevel` or the REPL against the restored world. Global functions and
   `defvar` values resolve through the restored symbol cells; packages through
   the restored registry.

Open risks specific to this integration (validate before trusting a core):
- **Whole-heap walk vs off-heap bodies.** `serialize_heap_objects` walks the real
  runtime heap (not synthetic `record_object`s). Objects with off-heap Rust
  bodies (streams, finalizers — risk #4) must be enumerated and forbidden/re-opened.
- **CLOS_STATE** (classes/generics/methods) is not yet carried by the host hook
  (only macros/setf are, 0d31d18); a core needs it too, or CLOS breaks after load.
- **Env caches.** Confirm `Env::new` does not pre-cache globals that the core
  expects to resolve lazily through the registries.

Recommended first done-signal for M3 (smaller than the ASDF goal): a
`defvar`-holding-a-`make-hash-table` world saved as a core and restored in a
fresh process, with the hash-table readable — proves the whole-heap snapshot +
symbol-value-cell restore end-to-end before tackling ASDF.

## M3/M4 LANDED + the two concrete remaining blockers (a0573b1)

The save/load/executable machinery is implemented and proven cross-process
(including under `TORCL_GC_STRESS`+`POISON`) for heap-resident, non-CLOS values:

- **`%save-core path [:executable t]`** — compacting `full_gc`, `save_image`, exit.
  `:executable t` appends the core to a runtime copy (existing `TORCLEXE` wrap).
- **`--image` / appended-image fast path** — a `TORCLIMG`-magic image is restored
  into a fresh runtime BEFORE bootstrap (`load_core_image_bytes`), then bootstrap
  and the source-form image paths are skipped.
- **`restore_heap` pins every restored object** (immortal base world; a minor GC
  must not relocate it out from under the rebuilt registries / RELOC_MAP).
- **Streams are skipped by `serialize_heap_objects`** and re-opened on load — a
  pinned nursery region is marked live wholesale, so a restored STREAM (whose GC
  trace derefs a process-local, now-freed Rust block) would fault when traced.
- **`global_macro_insert` installs the evaluator global-root scanner** so a source
  DEFMACRO body survives the save-time `full_gc`.

Verified round-trip (defvar values, lists, defun, symbols, packages, PRINT) both
non-stress and under GC stress; M4 standalone executable restores its world.

Two well-scoped blockers remain before the ASDF done-signal, each found by an
actual crash backtrace, each with a filed bead:

1. **CLOS_STATE restoration (bliss-x0f2.7).** After a core load, macro expansion
   builds a fresh `Env` → `initialize_condition_runtime_support` → `class_of` →
   null, because `torcl-stdlib::clos::CLOS_STATE` (class_registry, class_meta,
   generic_functions, method_meta, effective/short-form methods, structure_classes,
   the built-in class values, counters) is NOT serialized — bootstrap, which
   normally populates it, is skipped on a core load. This is a large structure;
   serializing it needs the same host-hook + remap treatment as macros/setf, plus
   restoring CLOS instances' wrapper/`class_of` (instances are STANDARD_OBJECT heap
   objects discriminated via a live-instance mechanism). Blocks all macro/condition/
   CLOS use in a core, hence ASDF.
2. **Off-heap-body VALUE objects (new bead).** Hash-tables (type 0x0C) box their
   body off the GC heap; the restored heap object holds a freed process-local Box
   pointer, so a *use* (gethash) faults cross-process (their GC trace visits
   nothing, so unlike streams they do NOT fault during tracing — only on use).
   ASDF's `*OPERATIONS*`/`*DEFINED-SYSTEMS*` are hash-tables, so this is required
   too. Approach: serialize off-heap bodies via their live registries; re-Box +
   re-register + remap keys/values on restore.

## Blocker 2 LANDED: off-heap hash-table bodies (518d9e0, bliss-x0f2.9)

Hash-tables ride a dedicated `OffHeap` image section (SectionType 9). Restore is
**two-phase** to break a circular dependency: ALLOCATE runs between
`restore_heap`'s two passes — it re-Boxes each table empty and folds its
(old,new) body address into the relocation map so Pass 2 / symbol / package
remaps relocate references TO tables like any heap object (safe under the heap
lock: pure Box allocation, no GC) — and POPULATE runs after `restore_heap`
returns, once Pass 2 has remapped key internals (EQUAL/EQUALP hashing recurses
into the key structure) and the heap lock is free (hashing re-enters the
collector). Entries whose key/value did not relocate into the restored heap
(e.g. keys whose own body is off-heap and not yet serialized — package-internal
symbol tables) are *skipped*, not deref'd (`TORCL_OFFHEAP_DBG=1` reports the
count). Hooks: `torcl_rt::gc::set_offheap_hooks` ←
`torcl_stdlib::hashtable::{serialize,allocate,populate}_live_tables`.

Validating it exposed a **pre-existing loader bug** (bliss-64r1, fixed in the
same commit): `restore_heap` replaced all region state without bumping
`GC_MOVE_EPOCH`, so threads kept allocating from pre-restore TLABs whose space
the reset region `alloc_top`s handed out again — the first allocating form
after a core load silently overwrote the re-opened stdio streams ("stream
error: not a stream"; reproducible with `TORCL_GC_DISABLE=1`, i.e. not a GC
bug; masked under `TORCL_GC_STRESS=1`, which retires TLABs early). Remaining
before the ASDF done-signal: CLOS_STATE restoration (blocker 1, bliss-x0f2.7)
and any other off-heap body types (streams already re-open; enumerate the rest).

## Risks / open points

- **Fixed-base availability.** MAP_FIXED_NOREPLACE can fail under ASLR; the delta
  rebase pass is the fallback and must be correct (it is the harder path to test —
  M1 tests it directly).
- **Registry coverage.** Missing any root registry = dangling refs. Enumerate
  exhaustively against the GC's own root scanners (`scan_external_roots`,
  `TraceHostRoots`, side tables) — whatever the GC roots, the dump must capture.
- **Off-heap object bodies.** Some objects hold raw pointers to off-heap Rust
  allocations (e.g. stream state boxed off-heap, finalizers). Those cannot be
  snapshotted; enumerate and either forbid in a dumped image or re-open on load.
- **GC-safety during dump.** The dump runs after a stop-the-world major GC with no
  mutator allocation; keep it allocation-free.
- **Versioning.** Stamp a format version + a build id; refuse mismatched cores.

## Milestone LANDED 2026-09-07: complete cores; save-lisp-and-die routed (710140d…61fe9b1)

The session closed the remaining coverage gaps end-to-end (bliss-x0f2.7.3,
bliss-gjey, bliss-zz6w, bliss-5ven):

- **PKGS**: the stdlib PackageStore (names/nicknames/use-lists/shadowing/table
  refs) rides HostRegistries; reader package names re-register on load.
- **Symbol registry keys** are serialized explicitly (qualified keys are not
  recoverable from bare-name cells).
- **CLSD**: interpreter `ClassDef`s — slot :initforms, :initargs,
  accessor/reader/writer names, :default-initargs, class-slot cells. (Also
  fixed: `visit_class_def_roots` never visited default-initarg forms.)
- **CLSR**: the tree-walker CLOSURE_REGISTRY with its captured `EnvFrame`
  graph serialized once by Rc identity (shared frames stay shared), plus the
  bytecode CLOSURE_ENV / CLOSURE_CONTROL tables; NEXT_CLOSURE_ID resumes.
- **BCOD**: the bytecode registry as a synthetic BYTECODE_UNIT executed by the
  ordinary BBU loader on restore — kind-3 reinstalls named functions reusing
  the restored source-free stub objects in place; new kind-10 RegisterClosure
  reinstalls closure bytecode under image-stable uninterned indices;
  source-free bytecode macro expanders re-emit as kind-4 installs.
- **OffHeap** grew two families beside hash tables, framed as
  `[len u64][bytes]` sub-blocks (`torcl_stdlib::offheap_image`): **pathnames**
  (16-byte header blocks + PathnameRecord side table) and the
  **make_lisp_string intern table** by content — each (old,new) folds into the
  reloc map, so references buried inside structures remap like heap objects.
  Allocate hooks run under the heap lock and may take no lower-order locks
  (intern-table registration defers to populate).

**Done-signals reached**: a core saved after loading ASDF answers
`find-system` and can `asdf:load-system` babel from the restored world; a core
saved *after* loading babel runs its encoder immediately (instant-startup
preloaded libraries, bliss-5uj); `save-lisp-and-die` now writes this core
(SBCL semantics — restore via `--image`/`:executable`, not LOAD), so
instance-valued globals survive it.

Known remainders (tracked as beads): `symbol-package` mis-derives the home
package of internal qualified symbols post-restore; `boundp` on qualified
symbols is NIL even without an image (pre-existing); ~32 registry functions
fail tree serialization (some UIOP); LOGICAL_TRANSLATIONS not yet carried;
`make_lisp_string_fresh` strings still cannot ride (converge stdlib strings
onto heap objects, bliss-jtc.2).
