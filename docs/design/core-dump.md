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
  - `bliss_rt::symbols`: the symbol table (name → index, value cells, function
    cells, plist),
  - `bliss-stdlib` packages,
  - cli.rs thread-locals: `GLOBAL_MACROS`, `GLOBAL_SETF_FNS`, setf-expanders,
    `CLOS_STATE`, class/generic/method tables,
  - compiled-function registry (bytecode).
  A core dump must serialize **both** the heap span and these registries.
  (SBCL keeps symbols/packages/functions *on* the Lisp heap, so its core is just
  the heap; bliss's hybrid model makes the registries extra work.)

## What already exists (discovered during M1)

A spec §7.2–7.3 heap-image system is **implemented and unit-tested in-process,
but not wired to the CLI**:

- `bliss_rt::image::save_image` / `load_image` — full file format: `ImageHeader`
  (magic `BLISSIMG`), section table, sha256 checksums, optional zstd, and
  `find_appended_image` (already supports the executable-append case for M4).
- `bliss_rt::gc::serialize_heap_objects` / `restore_heap` — walk every live
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
3. **bliss-crate registries are invisible to bliss-rt.** `GLOBAL_MACROS`,
   `GLOBAL_SETF_FNS`, setf-expanders, `CLOS_STATE`, class/generic/method tables
   live in the `bliss` crate; `image.rs` (in `bliss-rt`) can't see them. Needs a
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
4. Serialize each Rust registry: its structure plus the `BlissVal`s it holds
   (which are absolute heap addresses, valid at `heap_base`).
5. Header: magic, version, `heap_base`, `heap_size`, `live_size`, registry
   sections, optional `:toplevel`.

**Load** (`--image` core format):
1. `init_heap` mmap'ing at the recorded `heap_base` via `MAP_FIXED_NOREPLACE`.
2. `read` the live span back into `[heap_base, +live_size)`; rebuild region
   metadata so the allocator continues past `live_size`.
3. If the recorded base was unavailable (ASLR/occupied): map anywhere and run a
   **rebase pass** — walk every heap object's pointer slots and every registry
   `BlissVal`, adding `new_base - old_base` (a uniform delta; reuses the precise
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
  section of `(key, BlissVal…)` records, delta-adjusted on rebase. Done-signal:
  a `defvar`/`defun`/`defmacro`/`defclass`/`make-instance`/`make-hash-table`
  world round-trips via core dump (including the hash-table + instance values).
- **M3 — wire to `save-lisp-and-die` / `--image`.** New core format selected by a
  magic header; keep the source-form path as a fallback/`:format` option.
  Done-signal: `(asdf:load-system :cl-ppcre)` succeeds from a core image; ASDF
  `*operations*` is bound.
- **M4 — executable wrapping.** Append the core to a copy of the runtime binary
  (reuse `wrap_executable` / `embedded_image`); `make image` produces a
  self-contained `bliss` whose libraries load. Done-signal: installed `bliss`
  runs `(asdf:load-system :cl-ppcre)`.

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
