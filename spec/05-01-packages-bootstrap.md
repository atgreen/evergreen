# §5.1  Package System

## §5.1.0  Scope

This section specifies the Bliss package registry, symbol tables, locking
protocol, iteration protocol, package-local nicknames, and conduit packages.
It covers data structures D5.02–D5.12, requirements R5.51–R5.58, and
algorithms A5.01–A5.04.

---

## §5.1.1  Requirements

| ID | Req | Level |
|----|-----|-------|
| R5.51 | The package registry MUST support atomic lookup, creation, deletion, and rename of packages by name and by nickname. | MUST |
| R5.52 | Every package MUST maintain an internal symbol table for present symbols and a separate shadow set. `INTERN`, `FIND-SYMBOL`, `UNINTERN` MUST satisfy ANSI §11.1 semantics. | MUST |
| R5.53 | Package operations MUST be safe under concurrent access from multiple OS threads without a global interpreter lock. A reader-writer lock per package is used; the global registry uses a separate RwLock. | MUST |
| R5.54 | Lock acquisition MUST follow a total ordering (§5.1.5) to prevent deadlock when operations span multiple packages (e.g., `USE-PACKAGE`, `IMPORT`). | MUST |
| R5.55 | Symbol table lookup MUST complete in amortised O(1) time using open-addressing hashing (§5.1.4). | MUST |
| R5.56 | `DO-SYMBOLS`, `DO-EXTERNAL-SYMBOLS`, `DO-ALL-SYMBOLS` MUST iterate without holding write locks and MUST tolerate concurrent insertions (may or may not see them) but MUST NOT crash or return garbage. | MUST |
| R5.57 | Bliss MUST implement package-local nicknames (PLN) per SBCL `sb-ext:add-package-local-nickname` semantics (§5.1.6). | MUST |
| R5.58 | Bliss SHOULD support conduit packages — packages that re-export symbols from other packages without owning them — as a Bliss extension. | SHOULD |

---

## §5.1.2  Package Registry (D5.02)

The **global package registry** is a process-wide singleton mapping
canonical package names and global nicknames to `Package` handles.

### D5.02 — PackageRegistry

```rust
/// Process-global package registry.  Accessed via `PackageRegistry::global()`.
pub struct PackageRegistry {
    /// Primary map: canonical name → Arc<Package>.
    by_name: HashMap<Box<str>, Arc<Package>>,

    /// Nickname map: nickname → canonical name.
    /// Invariant: every nickname key maps to a key present in `by_name`.
    nicknames: HashMap<Box<str>, Box<str>>,

    /// Monotonic package-id counter for lock-ordering (§5.1.5).
    next_id: AtomicU64,
}
```

**Locking:** Guarded by a single `RwLock<PackageRegistry>`:

| Operation | Lock |
|-----------|------|
| `FIND-PACKAGE` | Read |
| `MAKE-PACKAGE`, `DELETE-PACKAGE`, `RENAME-PACKAGE` | Write |

**Invariants:** No duplicate keys (case-sensitive).  `DELETE-PACKAGE`
removes entries and sets `pkg.deleted = true`; `Arc` prevents deallocation
until all references are dropped.

### D5.03 — Package

```rust
pub struct Package {
    /// Immutable after creation.
    id: u64,                         // assigned by PackageRegistry::next_id

    /// Package canonical name.  Mutable only under registry write lock.
    name: RwLock<Box<str>>,

    /// Internal (present) symbols.
    internal: RwLock<SymbolTable>,    // D5.04

    /// External (exported) symbols.
    external: RwLock<SymbolTable>,    // D5.04

    /// Shadowing symbols set.
    shadowing: RwLock<HashSet<SymbolId>>,

    /// Use-list: packages whose external symbols are inherited.
    use_list: RwLock<Vec<Arc<Package>>>,

    /// Used-by-list: inverse of use_list for fast cascade updates.
    used_by: RwLock<Vec<Weak<Package>>>,

    /// Package-local nicknames (§5.1.6).
    local_nicknames: RwLock<HashMap<Box<str>, Arc<Package>>>,

    /// Deleted flag.
    deleted: AtomicBool,
}
```

`Package.id` is the total-order key for deadlock prevention (§5.1.5).

---

## §5.1.3  Symbol Identity (D5.05)

Symbols are heap-allocated objects (tag `010`, §1).  Fields: `name: Box<str>`
(immutable), `package: AtomicPtr<Package>` (home package, NULL if uninterned),
`value: AtomicU64`, `function: AtomicU64`, `plist: RwLock<BlissVal>`,
`flags: AtomicU8` (CONSTANT_P, SPECIAL_P, MACRO_P bits).  The `package`
pointer is mutated only by `INTERN`/`UNINTERN` under the owning package's
write lock.

---

## §5.1.4  Symbol Table — Open-Addressing Hash (D5.04, A5.01)

Each package owns two `SymbolTable` instances (internal, external).

### D5.04 — SymbolTable

```rust
pub struct SymbolTable {
    /// Power-of-two sized flat array of slots.
    slots: Box<[Slot]>,

    /// Number of live entries.
    count: usize,

    /// Number of live + tombstone entries (for load-factor calculation).
    used: usize,

    /// Capacity (slots.len()), always a power of two.
    capacity: usize,
}

#[derive(Clone, Copy)]
enum Slot {
    Empty,
    Tombstone,
    Occupied { hash: u64, symbol: SymbolId },
}
```

### A5.01 — Lookup / Insert

Hash via `siphash-2-4(name)` (HashDoS-resistant, §8).  Index =
`hash & (capacity - 1)`.  Robin Hood linear probing: on collision,
compare probe distances; if occupant has shorter distance, key is absent.
Tombstones are skipped during lookup, reused during insert.  Load factor
≥ 75% triggers rehash (double capacity, discard tombstones).  Delete
replaces slot with `Tombstone`; tombstone ratio > 25% triggers compacting
rehash.  **Complexity:** amortised O(1), worst-case O(log n) probes.

---

## §5.1.5  Lock Protocol & Ordering (R5.54)

### Lock Hierarchy

Deadlock-free locking requires the following total order:

```
Level 0:  PackageRegistry (global RwLock)
Level 1:  Package (per-package RwLock fields)
Level 2:  SymbolTable (embedded in Package RwLock)
```

**Rule L1:** The registry lock MUST be acquired before any per-package
lock when both are needed (e.g., `MAKE-PACKAGE` creates the package,
then interns default symbols).

**Rule L2:** When an operation requires write locks on multiple packages
(e.g., `USE-PACKAGE A B` or `IMPORT` from A into B), locks MUST be
acquired in ascending `Package.id` order.

**Rule L3:** A thread MUST NOT hold a package write lock while acquiring
the registry write lock (violates L1).

**Rule L4:** Within a single package, the `internal` and `external`
RwLock fields MUST be acquired in the order `external` before `internal`
when both are needed (e.g., `EXPORT` moves a symbol from internal to
external).

### A5.03 — Multi-Package Lock Acquisition

Sort packages by `id`, dedup, then acquire write locks in order.

**Downgrade:** `FIND-SYMBOL` and `DO-*-SYMBOLS` use read locks only.
`INTERN` takes a read lock first; upgrades to write only if insertion is
needed (re-checks for races after upgrade).

---

## §5.1.6  Package-Local Nicknames (R5.57)

Each package carries a `local_nicknames` map (D5.03) that provides
private aliases visible only when that package is `*PACKAGE*`.

### API (Bliss extension, exported from `BLISS-EXT`)

| Function | Signature | Behaviour |
|----------|-----------|-----------|
| `ADD-PACKAGE-LOCAL-NICKNAME` | `(local-nickname actual-package &optional (package *package*))` | Adds mapping `local-nickname → actual-package` in `package.local_nicknames`. Signals `PACKAGE-ERROR` if `local-nickname` clashes with a global name/nickname of a **different** package. |
| `REMOVE-PACKAGE-LOCAL-NICKNAME` | `(local-nickname &optional (package *package*))` | Removes the local nickname. Returns the previously-associated package or `NIL`. |
| `PACKAGE-LOCAL-NICKNAMES` | `(&optional (package *package*))` | Returns an alist of `(nickname . package)`. |
| `PACKAGE-LOCALLY-NICKNAMED-BY-LIST` | `(package)` | Returns list of packages that have a local nickname for `package`. |

### Interaction with `FIND-PACKAGE`

`FIND-PACKAGE` checks `(*package*).local_nicknames` first (read lock),
then the global registry.  Per CDR-10, local nicknames shadow both
global canonical names and global nicknames.

---

## §5.1.7  Conduit Packages (R5.58, Bliss Extension)

A **conduit package** is a convenience that re-exports all external
symbols of designated source packages.  It owns no symbols itself.

### `DEFCONDUIT` Macro

```lisp
(bliss-ext:defconduit :my-api
  (:use)
  (:extends :package-a :package-b))
```

Expands to `DEFPACKAGE` with empty `:use`, then imports and re-exports
all external symbols from each `:extends` package.  The conduit records
its source packages (`conduit_sources: Option<Vec<Arc<Package>>>` in
D5.03) so `BLISS-EXT:REFRESH-CONDUIT` can re-synchronise.

---

## §5.1.8  Package Iteration Protocol (R5.56)

### A5.04 — Snapshot Iteration

`DO-SYMBOLS`, `DO-EXTERNAL-SYMBOLS`, `DO-ALL-SYMBOLS` collect a
`Vec<SymbolId>` snapshot under a read lock, then release the lock.
The macro body iterates the snapshot without holding any lock.
O(n) memory (≈8 KB for CL's 978 symbols).  Concurrent inserts are
invisible (ANSI-permitted); stale entries from concurrent deletes
SHOULD be tolerated.

---

## §5.1.9  Error Handling

| Condition | Type | When |
|-----------|------|------|
| `PACKAGE-ERROR` | Error | Name clash in `INTERN`, `EXPORT`, `USE-PACKAGE`; package not found; operation on deleted package. |
| `PACKAGE-DOES-NOT-EXIST` | `PACKAGE-ERROR` subtype | `FIND-PACKAGE` returns `NIL`; `IN-PACKAGE` with unknown name. |
| `SYMBOL-CONFLICT` | `PACKAGE-ERROR` subtype | `USE-PACKAGE` or `EXPORT` would introduce a name conflict. Restarts: `SHADOWING-IMPORT`, `UNINTERN`, `SKIP`. |

---

## §5.1.10  Configuration

| Knob | Default | Description |
|------|---------|-------------|
| `BLISS_PKG_INITIAL_CAPACITY` | 64 | Initial slot count for new `SymbolTable`. |
| `BLISS_PKG_LOAD_FACTOR` | 75 | Percent load factor triggering rehash. |
| `BLISS_PKG_TOMBSTONE_RATIO` | 25 | Percent tombstone ratio triggering compaction. |

---

## §5.1.11  Test Strategy

- **Unit tests (Rust):** Insert/lookup/delete cycles on `SymbolTable`,
  verify probe distances, tombstone compaction, rehash correctness.
- **Concurrency stress:** N threads concurrently intern random symbols
  into the same package; verify no lost inserts, no crashes.
- **Lock-order checker:** Debug build inserts assertions that locks are
  acquired in `Package.id` order (panic on violation).
- **ANSI test suite:** All `ansi-test` tests under `packages/` subtree
  MUST pass.

---

# §5.2  Bootstrap Sequence

## §5.2.0  Scope

This section specifies the Rust-to-CL handoff protocol, Phase 1 function
signatures, the `boot.lisp` load algorithm, cold-start vs warm-start
paths, and bootstrap self-test assertions.  Requirements R5.59–R5.65.

---

## §5.2.1  Requirements

| ID | Req | Level |
|----|-----|-------|
| R5.59 | The Rust runtime MUST provide a minimal set of primitive functions (§5.2.3) sufficient to load and execute `lib/boot.lisp`. | MUST |
| R5.60 | `boot.lisp` MUST be loadable by the Rust reader and evaluable by the tree-walk interpreter (Tier 0) without requiring any CL code to be pre-loaded. | MUST |
| R5.61 | The bootstrap sequence MUST create the `COMMON-LISP`, `COMMON-LISP-USER`, `KEYWORD`, and `BLISS-INTERNAL` packages before loading any CL source. | MUST |
| R5.62 | Cold start (no image) MUST complete in < 100 ms on a 2020-era x86-64 machine with SSD. | MUST |
| R5.63 | Warm start (image resume) MUST complete in < 20 ms by memory-mapping the `.bimg` file and performing pointer relocation without re-executing `boot.lisp`. | MUST |
| R5.64 | Bootstrap MUST run a self-test assertion suite (§5.2.7) before entering the REPL or executing user code.  Failure MUST abort with a diagnostic message and exit code 70 (`EX_SOFTWARE`). | MUST |
| R5.65 | The Rust-to-CL handoff MUST be idempotent: calling the bootstrap sequence on an already-initialised runtime MUST be a no-op. | MUST |

---

## §5.2.2  Cold-Start Sequence Overview

1. Parse CLI arguments.
2. Init platform (signals, TLS, TLAB — §2) and GC (§3).
3. Check for `.bimg` → if found, goto warm-start (§5.2.6).
4. **Cold start:** create `PackageRegistry` → bootstrap packages (§5.2.2.1)
   → register Phase 1 primitives (§5.2.3) → load `boot.lisp` (§5.2.4)
   → run self-tests (§5.2.7) → set `RUNTIME_STATE = Ready`.
5. Enter REPL or execute `--script` / `--eval`.
6. Shutdown (finalisers, thread join, exit).

### §5.2.2.1  Bootstrap Packages

Created in step 5b, in this order:

| Order | Package | Nicknames | Use-list | Notes |
|-------|---------|-----------|----------|-------|
| 1 | `KEYWORD` | — | — | Home for keyword symbols. |
| 2 | `COMMON-LISP` | `CL` | — | ANSI symbols interned by Rust. |
| 3 | `BLISS-INTERNAL` | `BI` | `CL` | Runtime internals, not exported to users. |
| 4 | `BLISS-EXT` | — | `CL` | Bliss extensions (PLN, conduits, etc.). |
| 5 | `COMMON-LISP-USER` | `CL-USER` | `CL`, `BLISS-EXT` | Default user package. |

`COMMON-LISP` MUST have all 978 ANSI external symbols interned and
exported before any CL loads.  Unimplemented symbols are created unbound.

---

## §5.2.3  Phase 1 Primitive Functions

Phase 1 primitives are Rust functions exposed as CL function objects in
the `COMMON-LISP` or `BLISS-INTERNAL` package.  They are the minimal
set needed to execute `boot.lisp`.

### D5.10 — PrimitiveFn

```rust
/// A Phase 1 primitive: a Rust function callable as a CL function.
pub struct PrimitiveFn {
    name: SymbolId,
    /// Min and max accepted argument count.  max == u16::MAX means &rest.
    min_args: u16,
    max_args: u16,
    /// The Rust implementation.
    func: fn(args: &[BlissVal], env: &Environment) -> Result<BlissVal, BlissError>,
}
```

### Phase 1 Function Table

All reside in `CL` unless noted.  MUST be registered before `boot.lisp` loads.

| Category | Symbols | Notes |
|----------|---------|-------|
| Eval | `EVAL`, `APPLY`, `FUNCALL` | Standard ANSI semantics. |
| Cons/list | `CONS`, `CAR`, `CDR`, `RPLACA`, `RPLACD`, `LIST`, `LIST*` | Signal `TYPE-ERROR` on non-cons for CAR/CDR. |
| Symbols | `INTERN`, `FIND-SYMBOL`, `EXPORT`, `MAKE-PACKAGE`, `FIND-PACKAGE`, `MAKE-SYMBOL`, `SYMBOL-NAME`, `SYMBOL-PACKAGE`, `SYMBOL-VALUE`, `SYMBOL-FUNCTION` | `FIND-PACKAGE` includes PLN lookup. |
| Arithmetic | `+`, `-`, `*`, `/`, `=`, `<`, `>`, `<=`, `>=`, `INTEGERP`, `NUMBERP` | Fixnum-only; overflow signals error in Phase 1. |
| Predicates | `CONSP`, `ATOM`, `SYMBOLP`, `FUNCTIONP`, `STRINGP`, `CHARACTERP`, `LISTP`, `NULL`, `EQ`, `EQL`, `EQUAL` | Standard ANSI. |
| I/O | `READ`, `PRINT`, `WRITE`, `TERPRI`, `FORMAT`, `OPEN`, `CLOSE`, `READ-CHAR`, `UNREAD-CHAR`, `PEEK-CHAR` | Phase 1 FORMAT: `~A ~S ~% ~D` only. OPEN: `:direction :input` only. |
| Control | `ERROR`, `SIGNAL`, `VALUES` | Standard ANSI. |

**Special forms** (handled by T0 interpreter, not `PrimitiveFn`):
`QUOTE`, `IF`, `LAMBDA`, `LET`, `LET*`, `SETQ`, `PROGN`, `BLOCK`,
`RETURN-FROM`, `TAGBODY`, `GO`, `CATCH`, `THROW`, `UNWIND-PROTECT`,
`MULTIPLE-VALUE-CALL`, `MULTIPLE-VALUE-PROG1`, `THE`, `LOCALLY`,
`FLET`, `LABELS`, `MACROLET`, `SYMBOL-MACROLET`, `LOAD-TIME-VALUE`,
`EVAL-WHEN`, `FUNCTION`, `DEFMACRO`.

**BLISS-INTERNAL primitives** (package `BI`):

| Symbol | Contract |
|--------|----------|
| `%SET-SYMBOL-FUNCTION` | Low-level write to function cell. |
| `%SET-SYMBOL-VALUE` | Low-level write to value cell. |
| `%MAKE-CLOSURE` | Wrap code pointer + environment vector into closure. |
| `%ALLOCATE-VECTOR` | Raw vector allocation (element-type, length). |
| `%TYPEP-TAG` | Return tag bits (0–7) of a `BlissVal`. |
| `%GC-COLLECT` | Force GC (`:full t` for full collection). |

---

## §5.2.4  boot.lisp Load Algorithm (A5.05)

### A5.05 — Boot Load Algorithm

Locate `lib/boot.lisp` relative to the executable; exit 72 if missing.
Open stream, bind `*PACKAGE*` to `BLISS-INTERNAL`.  Loop: `READ` a form
(with EOF sentinel); `EVAL` it.  On error: report with stream position,
invoke `SKIP-FORM` restart in dev mode or exit 70 in strict mode.
Close stream on completion or fatal error.

### Error Recovery

`boot.lisp` sections are marked with `;;; §N.N` headers.  On error the
loader prints condition, section number, and line:col to `*ERROR-OUTPUT*`.
In dev mode it invokes the `SKIP-FORM` restart if available; in strict
mode (`--strict-boot`, default for release) any error is fatal (exit 70).

---

## §5.2.5  Rust-to-CL Handoff Protocol

The handoff is the moment when control transfers from Rust-driven
initialisation to CL-driven execution.

### Handoff Sequence

1. **Rust phase complete:** All Phase 1 primitives registered, 978 ANSI
   symbols present, bootstrap packages exist, `boot.lisp` loaded.
2. **Handoff gate:** Rust sets `RUNTIME_STATE` (`AtomicU8`) from
   `Booting`(1) to `Ready`(2).  Other states: `Uninitialised`(0),
   `ShuttingDown`(3).  R5.65 — subsequent bootstrap calls check this
   flag and return immediately.
3. **CL entry:** Rust calls `(funcall *boot-complete-hook*)` if bound,
   then enters REPL or script execution (CL-side, defined by boot.lisp).

**Contract:** Before handoff only Rust + T0-interpreted CL execute.
After handoff the full runtime is available; tier promotion is active.
T0 remains available for `EVAL`.

---

## §5.2.6  Warm Start — Image Resume (R5.63)

When a `.bimg` image file is found (step 4 in §5.2.2):

### Warm-Start Sequence

1. `mmap` the `.bimg` file (`MAP_PRIVATE`).
2. Validate header: magic `"BLIS"`, version, arch tag.
3. Relocate pointers: `delta = mapped_base - original_base`; walk
   relocation table, add delta to each absolute-pointer offset.
4. Restore `PackageRegistry` from serialised section.
5. Restore `*PACKAGE*` = `CL-USER`, `*READTABLE*` = standard.
6. Set `RUNTIME_STATE` = `Ready`.
7. Call saved continuation (the point at which `SAVE-IMAGE` was invoked).

**boot.lisp is NOT re-evaluated** during warm start.

### Cold vs Warm Comparison

| Aspect | Cold Start | Warm Start |
|--------|-----------|------------|
| boot.lisp | Loaded & evaluated | Skipped |
| Package creation | From scratch | Restored from image |
| Symbol table | Built incrementally | mmap'd |
| Time budget | < 100 ms (R5.62) | < 20 ms (R5.63) |
| GC state | Empty heap | Restored regions |
| Compiled code cache | Empty | Restored |

---

## §5.2.7  Bootstrap Self-Test Assertions (R5.64)

After `boot.lisp` loads (cold start) or after image resume (warm start),
the runtime executes a battery of self-test assertions.

### Assertion Categories

| # | Category | Example assertions |
|---|----------|--------------------|
| A1 | Package integrity | `(find-package "COMMON-LISP")` returns non-NIL; `(find-symbol "CONS" "CL")` → `(:external)`; all 978 ANSI symbols present. |
| A2 | Arithmetic | `(+ 1 2)` → 3; `(- most-positive-fixnum most-positive-fixnum)` → 0; `(* 0 42)` → 0. |
| A3 | Cons / list | `(car (cons 1 2))` → 1; `(length '(a b c))` → 3; `(null nil)` → T. |
| A4 | Symbol identity | `(eq 'foo 'foo)` → T; `(eq (intern "X") (intern "X"))` → T. |
| A5 | Multiple values | `(multiple-value-list (values 1 2 3))` → `(1 2 3)`. |
| A6 | Control flow | `(block b (return-from b 42))` → 42; `(catch 'tag (throw 'tag 7))` → 7. |
| A7 | Condition system | `(handler-case (error "boom") (error () :caught))` → `:CAUGHT`. |
| A8 | Type predicates | `(consp '(1))` → T; `(symbolp 'x)` → T; `(integerp 42)` → T. |

### Implementation

Self-tests are a table of `(expression-string, expected-printed-result)`
pairs (~30 entries).  `rust_read_from_string` → `eval` → `print_to_string`;
compare against expected.  On mismatch: print expression, expected, and
actual to stderr, return `Err(BlissError::BootstrapFailure)` → exit 70.

---

## §5.2.8  boot.lisp Structure

`lib/boot.lisp` is loaded during cold start.  It defines the CL-level
infrastructure that cannot (or should not) be written in Rust.

### Sections (in load order)

§B.1 `DEFUN`/`DEFVAR`/`DEFPARAMETER`/`DEFCONSTANT` bootstrap macros →
§B.2 List utilities (`APPEND`, `MAPCAR`, `MEMBER`, etc.) →
§B.3 Control macros (`WHEN`, `UNLESS`, `COND`, `CASE`, `AND`, `OR`) →
§B.4 Sequence basics (`LENGTH`, `FIND`, `REDUCE`, etc.) →
§B.5 Strings/characters →  §B.6 Hash tables →
§B.7 Condition system (`HANDLER-CASE`, `RESTART-CASE`, etc.) →
§B.8 `DEFPACKAGE`/`IN-PACKAGE` →  §B.9 Extended FORMAT →
§B.10 `LOOP` (simplified) →  §B.11 `SETF` framework →
§B.12 Remaining ANSI functions.

---

## §5.2.9  Error Handling & Concurrency

| Failure mode | Behaviour |
|-------------|-----------|
| `boot.lisp` not found | Exit 72 (`EX_OSFILE`). |
| Read/eval error (strict) | Exit 70 with diagnostics. |
| Read/eval error (dev) | Warn, invoke `SKIP-FORM`, continue. |
| Self-test failure | Exit 70 with expression + actual vs expected. |
| Image header invalid | Fall through to cold start with warning. |
| Image arch mismatch | Exit 76 (`EX_PROTOCOL`). |

Bootstrap is **single-threaded** — worker pool starts only after
`RUNTIME_STATE` = `Ready`.  No locks needed during cold start.

---

## §5.2.10  Configuration

| Knob | Default | Description |
|------|---------|-------------|
| `--image PATH` | `bliss.bimg` | Image file for warm start. |
| `--no-image` | — | Force cold start. |
| `--strict-boot` | On (release) | Fatal on any boot error. |
| `--boot-file PATH` | `lib/boot.lisp` | Override boot.lisp path. |
| `BLISS_BOOT_TRACE` | `0` | Print each form before eval if `1`. |

## §5.2.11  Test Strategy

- **Cold-start:** no image → self-tests pass, REPL reachable, < 100 ms.
- **Warm-start:** save image → restart → self-tests pass, < 20 ms.
- **Idempotency:** second bootstrap call is a no-op (R5.65).
- **Corruption:** truncated `.bimg` → cold-start fallback with warning.
- **Strict-mode:** injected error → exit 70; dev mode → continuation.
- **Completeness:** every symbol in `boot.lisp` exists in Phase 1 table
  or is defined earlier in boot.lisp itself.
