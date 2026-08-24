# §5 Standard Library

**Scope:** This chapter specifies the Bliss standard library — the CL-side code
and Rust glue that implements ANSI Common Lisp's standard packages, types, and
functions. It covers package layout (§5.1), bootstrap ordering (§5.2), CLOS
(§5.3), the condition system (§5.4), streams (§5.5), sequences (§5.6), hash
tables (§5.7), pathnames (§5.8), and the format/pretty-printer subsystem (§5.9).

Detailed specifications for each subsystem are in companion files
`spec/05-01-packages-bootstrap.md` through `spec/05-07-pathnames.md`. The
package/bootstrap companion covers both §5.1 and §5.2; the standalone CLOS
companion is authoritative for §5.3.

---

## 5.1  Package Layout

### 5.1.1  Standard Packages

| Package | Nicknames | Use-list | Purpose |
|---------|-----------|----------|---------|
| `COMMON-LISP` | `CL` | — | All 978 ANSI-specified external symbols |
| `COMMON-LISP-USER` | `CL-USER` | `(CL)` | Default interactive package |
| `KEYWORD` | — | — | Self-evaluating keyword symbols |
| `BLISS-EXT` | — | `(CL)` | Canonical Bliss-specific public extensions (§9) |
| `BLISS` | `BL` | `(CL BLISS-EXT)` | Deprecated compatibility nickname/package for older extension spelling |
| `BLISS-INTERNALS` | `BL-INT` | `(CL BLISS-EXT)` | Runtime internals, GC hooks, compiler intrinsics |
| `BLISS-THREAD` | — | `(CL BLISS-EXT)` | OS-backed native threads and synchronization API (§13) |
| `BLISS-THREADS` | `BL-THR` | `(CL BLISS-THREAD)` | Deprecated compatibility spelling for `BLISS-THREAD` |
| `BLISS-FIBER` | — | `(CL BLISS-EXT)` | Lightweight managed fibers and scheduler groups (§13) |
| `BLISS-FFI` | `BL-FFI` | `(CL BLISS-EXT)` | Foreign function interface (§2, §8) |

**R5.01** The `COMMON-LISP` package MUST export exactly the 978 external
symbols specified by ANSI X3.226-1994. No Bliss-specific symbols may leak
into `CL`.

**R5.02** The `KEYWORD` package MUST intern symbols as self-evaluating
constants with their name as the value. `(symbol-package :foo)` → `KEYWORD`.

**R5.03** `BLISS-INTERNALS` symbols MUST NOT be exported into `BLISS-EXT`,
`BLISS`, or `CL` without explicit review — they constitute an unstable API.

**R5.03a** New Bliss extension APIs MUST use `BLISS-EXT` for general runtime
extensions, `BLISS-THREAD` for native thread/synchronization objects,
`BLISS-FIBER` for lightweight fibers, and `BLISS-FFI` for foreign-function
interfaces. `BLISS` and `BLISS-THREADS` are compatibility spellings only and
MUST NOT be used as the primary package in new specification text.

**R5.04** Package objects MUST be protected by per-package reader-writer
locks (see §2). Read operations (`find-symbol`, `do-symbols`) acquire a
read lock; mutations (`intern`, `unintern`, `export`) acquire the write lock.

### 5.1.2  Package Registry

```rust
// crates/bliss-rt/src/package.rs
pub struct PackageRegistry {
    /// Name → Package, protected by a global RwLock for registry-level ops.
    packages: RwLock<HashMap<Box<str>, Arc<Package>>>,
}

pub struct Package {
    name: Box<str>,
    nicknames: Vec<Box<str>>,
    use_list: RwLock<Vec<Arc<Package>>>,
    internal_symbols: RwLock<SymbolTable>,
    external_symbols: RwLock<SymbolTable>,
}
```

**R5.05** `FIND-PACKAGE` MUST complete in O(1) amortised time via the
registry hash map.

---

## 5.2  Bootstrap Order

The bootstrap sequence defines which functionality is implemented in Rust
(Phase 1) vs. loaded from CL source (Phase 2+). The Rust core provides the
*minimum viable runtime* needed to load, compile, and execute CL files.

### 5.2.1  Rust-Implemented Primitives (Phase 1)

These MUST exist before any CL source file can be loaded:

| Category | Functions / Forms |
|----------|-------------------|
| Object core | `cons`, `car`, `cdr`, `rplaca`, `rplacd`, `consp`, `atom` |
| Type predicates | `typep` (primitive cases), `null`, `symbolp`, `numberp`, `characterp`, `stringp`, `vectorp`, `functionp` |
| Arithmetic | `+`, `-`, `*`, `/`, `=`, `<`, `>`, `<=`, `>=`, `truncate`, `rem`, `mod`, `ash`, `logand`, `logior`, `logxor` |
| Comparison | `eq`, `eql`, `equal`, `equalp` |
| Symbols | `symbol-name`, `symbol-value`, `symbol-function`, `symbol-plist`, `boundp`, `fboundp`, `set`, `makunbound` |
| Control | `if`, `progn`, `block`, `return-from`, `tagbody`, `go`, `catch`, `throw`, `unwind-protect`, `multiple-value-call`, `multiple-value-prog1`, `values` |
| Functions | `funcall`, `apply`, `lambda` (special operator) |
| Binding | `let`, `let*`, `setq`, `flet`, `labels`, `macrolet`, `symbol-macrolet` |
| Arrays | `make-array`, `aref`, `(setf aref)`, `array-dimensions`, `array-element-type`, `vector-push-extend` |
| Strings | `char`, `schar`, `string=`, `string<`, `make-string` |
| I/O (minimal) | `read`, `read-char`, `write-char`, `terpri`, `print`, `princ`, `prin1`, `write`, `open`, `close`, `*standard-input*`, `*standard-output*`, `*error-output*` |
| Reader core | `read`, `read-from-string`, `*readtable*`, `set-macro-character`, `get-macro-character`, dispatch macros |
| Eval/Apply | `eval`, `apply` (source-to-bytecode lowering plus T0 bytecode interpreter) |
| Packages | `make-package`, `find-package`, `in-package`, `intern`, `export`, `use-package`, `find-symbol` |
| Definitions | `defun` (macro), `defvar`, `defparameter`, `defconstant`, `defmacro`, `defstruct` (basic) |
| Macros (core) | `and`, `or`, `when`, `unless`, `cond`, `case`, `do`, `do*`, `dolist`, `dotimes`, `loop` (simple) |
| Hash tables | `make-hash-table`, `gethash`, `(setf gethash)`, `remhash`, `maphash`, `hash-table-count` |
| Misc | `error`, `format` (minimal ~A ~S ~% ~D), `gensym`, `values-list` |

**R5.06** All Phase 1 primitives MUST be callable before `lib/boot.lisp` is
loaded. They are registered in the `CL` package during `startup.rs` init.

**R5.07** Phase 1 `DEFSTRUCT` MUST support basic slot definitions with
`:type`, `:initform`, `:read-only`, `:conc-name`, `:constructor`,
`:predicate`, and `:copier`. `:include` inheritance and BOA constructors are
Phase 2.

### 5.2.2  CL-Defined Standard Library (Phase 2)

Once the Rust core is running, `lib/boot.lisp` loads CL source files in
dependency order:

```text
Boot Load Order:
 1. lib/boot.lisp            — early macros, setf expansion, backquote
 2. crates/bliss-stdlib/src/defstruct-full.lisp   — full defstruct with :include, printing
 3. crates/bliss-stdlib/src/type-system.lisp      — subtypep, typep full, type specifiers
 4. crates/bliss-stdlib/src/setf.lisp             — define-setf-expander, full setf
 5. crates/bliss-stdlib/src/list-ops.lisp         — mapcar, mapc, mapcan, assoc, member, etc.
 6. crates/bliss-stdlib/src/sequences.lisp        — generic sequence operations
 7. crates/bliss-stdlib/src/hash-table-ext.lisp   — with-hash-table-iterator, hash-table-rehash-*
 8. crates/bliss-stdlib/src/clos/boot.lisp        — standard-class, standard-object bootstrap
 9. crates/bliss-stdlib/src/clos/generic.lisp     — defgeneric, defmethod, method dispatch
10. crates/bliss-stdlib/src/clos/slots.lisp       — slot-value, slot protocol
11. crates/bliss-stdlib/src/clos/combination.lisp — method combination
12. crates/bliss-stdlib/src/clos/change.lisp      — change-class, class redefinition
13. crates/bliss-stdlib/src/conditions.lisp       — condition types, handler-bind, restarts
14. crates/bliss-stdlib/src/streams.lisp          — Gray streams, standard stream types
15. crates/bliss-stdlib/src/pathnames.lisp        — pathname parsing, logical pathnames
16. crates/bliss-stdlib/src/format-full.lisp      — full FORMAT, ~{, ~[, ~<, ~/, etc.
17. crates/bliss-stdlib/src/printer.lisp          — pretty-printer, print-object methods
18. crates/bliss-stdlib/src/loop-full.lisp        — extended LOOP macro
19. crates/bliss-stdlib/src/environment.lisp      — describe, inspect, documentation
```

**R5.08** Each file in the boot sequence MUST only depend on symbols defined
by files loaded before it. Circular dependencies are forbidden.

**R5.09** The boot sequence MUST complete in under 200 ms on the reference
platform (see §7) when loading from the image cache.

### 5.2.3  Bootstrap Circularity Resolution

CLOS bootstrap presents a chicken-and-egg problem: `standard-class` is an
instance of itself; `standard-object` is a superclass of `standard-class`
but also an instance of it.

**R5.10 [S4]** The CLOS bootstrap MUST use a three-phase protocol:
1. **Proto-classes** — Rust allocates raw structures for `T`,
   `standard-object`, `standard-class`, `built-in-class` with placeholder
   metaclass pointers.
2. **Wire-up** — Once all proto-classes exist, patch metaclass slots to
   close the circularity. `(class-of (find-class 'standard-class))` →
   `#<standard-class STANDARD-CLASS>`.
3. **CL takeover** — Load `clos/boot.lisp`, which defines `defclass`,
   `make-instance`, and the slot-access protocol using the now-complete
   class graph.

---

## 5.3  CLOS — Common Lisp Object System

Detailed specification in `spec/05-02-clos.md` (§5.3). Summary of key design
points:

### 5.3.1  Class Hierarchy (Core)

```text
t
 ├── standard-object
 │    ├── standard-class        (metaclass of most user classes)
 │    ├── built-in-class        (metaclass of fixnum, cons, etc.)
 │    ├── structure-class       (metaclass of defstruct classes)
 │    ├── funcallable-standard-class (metaclass for generic functions)
 │    └── (user classes)
 ├── function
 │    ├── compiled-function
 │    └── generic-function
 │         └── standard-generic-function
 ├── method
 │    └── standard-method
 ├── slot-definition
 │    ├── standard-direct-slot-definition
 │    └── standard-effective-slot-definition
 └── method-combination
      └── standard-method-combination
```

**R5.11 [S4]** `STANDARD-CLASS` MUST support single and multiple inheritance with
C3 linearization for the class precedence list (CPL).

**R5.12 [S4]** `MAKE-INSTANCE` MUST follow the ANSI initialization protocol:
`allocate-instance` → `initialize-instance` → `shared-initialize`.

**R5.13 [S4]** Slot access via `SLOT-VALUE` MUST be optimised by the T1/T2
compilers to a direct memory offset load when the class is sealed or the
slot position is monomorphic (§4).

**R5.14 [S4]** Generic function dispatch MUST use a **discriminating function**
compiled to native code. The dispatch strategy MUST support:
- Single-dispatch fast path (vtable-like index lookup).
- Multi-method dispatch via method caching (hash on class tuple).

**R5.15 [S4]** Method combination MUST support `STANDARD`, `+`, `AND`, `OR`,
`LIST`, `APPEND`, `NCONC`, `MIN`, `MAX`, and `PROGN`, plus
`DEFINE-METHOD-COMBINATION` (short and long forms).

**R5.16 [S4]** `CHANGE-CLASS` MUST call `UPDATE-INSTANCE-FOR-DIFFERENT-CLASS`
and preserve slot values for slots with the same name.

**R5.17 [S4]** Class redefinition MUST lazily update existing instances via
`UPDATE-INSTANCE-FOR-REDEFINED-CLASS` on next slot access (stamp-check
protocol).

---

## 5.4  Condition System

Detailed specification in `spec/05-03-conditions.md`. Summary:

**R5.18 [S4]** The condition type hierarchy MUST include at least:

```text
condition
 ├── serious-condition
 │    ├── error
 │    │    ├── type-error
 │    │    ├── cell-error (unbound-variable, undefined-function, unbound-slot)
 │    │    ├── arithmetic-error (division-by-zero, floating-point-overflow, ...)
 │    │    ├── package-error
 │    │    ├── stream-error (end-of-file)
 │    │    ├── file-error
 │    │    ├── parse-error (reader-error)
 │    │    ├── control-error
 │    │    ├── program-error
 │    │    └── print-not-readable
 │    └── storage-condition
 └── warning
      ├── style-warning
      └── simple-warning
 simple-condition (mixin)
 simple-error, simple-type-error, simple-warning (via multiple inheritance)
```

**R5.19 [S4]** `HANDLER-BIND` MUST establish handlers without unwinding the
stack. `HANDLER-CASE` MUST unwind before running the handler clause.

**R5.20 [S4]** `RESTART-BIND` / `RESTART-CASE` MUST support `:interactive`,
`:report`, and `:test` options.

**R5.21 [S4]** Default restarts MUST be established:
- `ABORT` — available in all `ERROR` calls (exit to nearest REPL).
- `CONTINUE` — available in `CERROR` calls.
- `MUFFLE-WARNING` — available in `WARN` calls.

**R5.22 [S4]** `*DEBUGGER-HOOK*` MUST be called before the default debugger
when an unhandled condition is signalled. Signature:
`(lambda (condition hook) ...)`.

---

## 5.5  Streams

Detailed specification in `spec/05-04-streams.md`. Summary:

**R5.23** Bliss MUST implement the **Gray streams** protocol
(trivial-gray-streams compatible) as the primary extensibility mechanism.
All built-in stream classes derive from Gray stream base classes.

**R5.24** Built-in stream classes:

| Stream type | Key slots | Notes |
|-------------|-----------|-------|
| `file-stream` | fd, buffer, direction, element-type, external-format | OS file descriptor backed |
| `string-input-stream` | string, index, end | From `make-string-input-stream` |
| `string-output-stream` | buffer (adjustable string) | From `make-string-output-stream` |
| `broadcast-stream` | component-streams list | Output to multiple streams |
| `concatenated-stream` | streams list | Read sequentially from multiple |
| `two-way-stream` | input-stream, output-stream | Bidirectional |
| `echo-stream` | input-stream, output-stream | Echoes input to output |
| `synonym-stream` | symbol | Delegates to symbol-value at call time |

**R5.25** External format handling MUST support at least `:utf-8`,
`:ascii`, `:latin-1`, `:utf-16`, and `:utf-32`. The default external
format MUST be `:utf-8`.

**R5.26** Stream I/O operations MUST be thread-safe. Each stream object
holds a mutex; bulk operations (e.g., `write-sequence`) hold the lock for
the entire call to ensure atomicity.

---

## 5.6  Sequences

Detailed specification in `spec/05-05-sequences.md`. Summary:

**R5.27** Sequence functions MUST dispatch on argument type and use
specialised paths:
- **Lists:** CDR-chaining traversal; destructive operations recycle cons
  cells.
- **Vectors:** Direct indexed access; bounds-checked in safe code,
  unchecked when the compiler proves safety (§4).

**R5.28** Sort algorithms:
- `SORT` on vectors: **Introsort** (quicksort → heapsort fallback on
  depth limit). NEED NOT be stable.
- `STABLE-SORT` on vectors: **Timsort** (merge sort variant). MUST be
  stable.
- `SORT` / `STABLE-SORT` on lists: **Merge sort**. MUST be stable. MUST
  NOT cons (destructive merge).

**R5.29** The compiler SHOULD generate specialised code for `MAP`, `REDUCE`,
`FIND`, `POSITION`, `COUNT`, `REMOVE`, `SUBSTITUTE` via compiler macros
when the sequence type is known at compile time (§4).

**R5.30** `CONCATENATE` with result-type `STRING` MUST pre-calculate total
length and allocate once.

---

## 5.7  Hash Tables

Detailed specification in `spec/05-05-sequences-hashtables.md` §5.6–§5.7. Summary:

**R5.31** Hash table implementation MUST use **Robin Hood hashing** with
open addressing and backward-shift deletion.

**D5.01** Hash table layout:

```rust
pub struct BlissHashTable {
    test: HashTestFn,        // eq, eql, equal, equalp, or custom
    entries: Box<[Entry]>,   // flat array of key-value-hash triples
    count: usize,
    capacity: usize,
    rehash_size: f64,        // growth factor (default 2.0)
    rehash_threshold: f64,   // load factor trigger (default 0.75)
    lock: Option<Mutex<()>>, // present if :synchronized t
}
```

**R5.32** `SXHASH` MUST satisfy: `(equal x y)` → `(= (sxhash x) (sxhash y))`.
SXHASH values MUST be non-negative fixnums. SXHASH MUST be deterministic
within a session; it MAY differ across sessions (ASLR-seeded).

**R5.33** `MAKE-HASH-TABLE` MUST accept a `:synchronized` keyword
(Bliss extension, §9). When `:synchronized t`, all operations acquire the
internal mutex.

**R5.34** `WITH-HASH-TABLE-ITERATOR` MUST provide a local macro that
returns `(values found-p key value)` on each call. Iteration order is
unspecified. Modifying the table during iteration (other than `(setf gethash)`
on the current key) has undefined consequences.

---

## 5.8  Pathnames and Logical Pathnames

Detailed specification in `spec/05-07-pathnames.md`. Summary:

**R5.35** Pathname components MUST be: `host`, `device`, `directory`,
`name`, `type`, `version` per ANSI spec.

**R5.36** Physical pathname parsing MUST follow POSIX conventions on
Linux/macOS: `/` directory separator, no host/device/version.

**R5.37** Logical pathnames MUST be supported via
`LOGICAL-PATHNAME-TRANSLATIONS`. Translation rules map logical components
to physical paths.

**R5.38** `MERGE-PATHNAMES` and `TRANSLATE-LOGICAL-PATHNAME` MUST follow
ANSI semantics exactly — these are common sources of portability bugs.

**R5.39** Namestring parsing MUST handle: relative paths, `~` expansion
(Bliss extension), `.` and `..` components, and trailing `/` to distinguish
directory from file.

---

## 5.9  Format and Pretty-Printer

Detailed specification in `spec/05-06-format-printer.md`. Summary:

**R5.40** `FORMAT` MUST implement all ANSI directives:

| Category | Directives |
|----------|------------|
| Basic | `~A`, `~S`, `~W`, `~C`, `~%`, `~&`, `~~`, `~\|`, `~T` |
| Numeric | `~D`, `~B`, `~O`, `~X`, `~R`, `~E`, `~F`, `~G`, `~$` |
| Conditional | `~[...~;...~]`, `~{...~}`, `~<...~>`, `~*`, `~?` |
| Flow | `~^`, `~P`, `~(...)`, `~newline` |

**R5.41** The pretty-printer MUST implement `PPRINT-DISPATCH`,
`PPRINT-LOGICAL-BLOCK`, `PPRINT-NEWLINE`, `PPRINT-INDENT`, and
`PPRINT-TAB` per ANSI spec.

**R5.42** `*PRINT-PPRINT-DISPATCH*` MUST contain default entries for all
standard types (lists, arrays, structures, CLOS objects).

**R5.43** Pretty-printer output MUST respect `*PRINT-RIGHT-MARGIN*`
(default 80) and `*PRINT-MISER-WIDTH*` (default 40).

---

## 5.10  Module Map

Standard library source layout within `crates/bliss-stdlib/`:

```text
crates/bliss-stdlib/
├── bliss-stdlib.asd
└── src/
    ├── boot-macros.lisp       — early macros needed by everything
    ├── setf.lisp              — setf expansion framework
    ├── type-system.lisp       — subtypep, typep, type specifiers
    ├── defstruct-full.lisp    — full defstruct
    ├── list-ops.lisp          — list utilities
    ├── sequences.lisp         — generic sequence operations
    ├── hash-table-ext.lisp    — hash-table extensions
    ├── clos/
    │   ├── boot.lisp          — proto-class creation, circularity fix
    │   ├── generic.lisp       — defgeneric, defmethod, dispatch
    │   ├── slots.lisp         — slot-value, slot protocol
    │   ├── combination.lisp   — method combination types
    │   └── change.lisp        — change-class, redefinition
    ├── conditions.lisp        — condition system
    ├── streams.lisp           — Gray streams + built-in streams
    ├── pathnames.lisp         — pathname + logical pathname
    ├── format-full.lisp       — full FORMAT implementation
    ├── printer.lisp           — pretty-printer
    ├── loop-full.lisp         — extended LOOP macro
    └── environment.lisp       — describe, inspect, documentation
```

---

## 5.11  Error Handling

**R5.44** All standard library functions MUST signal appropriate ANSI
condition types for erroneous inputs rather than returning undefined values
or crashing. Examples:
- `(elt '(1 2 3) 10)` → `type-error` (not segfault).
- `(/ 1 0)` → `division-by-zero`.
- `(open "/no/such/file")` → `file-error`.

**R5.45** Condition objects MUST be heap-allocated CLOS instances. Condition
signalling (SIGNAL, ERROR, WARN, CERROR) MUST NOT cons beyond the condition
object itself and the handler-search stack walk.

---

## 5.12  Concurrency

**R5.46** Standard library functions operating on shared mutable state
(packages, readtables, `*gensym-counter*`, pprint dispatch tables) MUST
be thread-safe.

**R5.47** Sequence functions, FORMAT, and arithmetic MUST be safe to call
concurrently on disjoint data without synchronisation.

---

## 5.13  Configuration

| Parameter | Default | Notes |
|-----------|---------|-------|
| `BLISS-EXT:*DEFAULT-EXTERNAL-FORMAT*` | `:UTF-8` | Used by `OPEN`, stream creation; `BLISS:*DEFAULT-EXTERNAL-FORMAT*` is a deprecated compatibility spelling |
| `BLISS-EXT:*HASH-TABLE-DEFAULT-SIZE*` | `16` | Initial capacity; `BLISS:*HASH-TABLE-DEFAULT-SIZE*` is a deprecated compatibility spelling |
| `BLISS-EXT:*HASH-TABLE-SYNCHRONIZED-DEFAULT*` | `NIL` | Extension: default :synchronized; `BLISS:*HASH-TABLE-SYNCHRONIZED-DEFAULT*` is a deprecated compatibility spelling |
| `BLISS-EXT:*SORT-PARALLEL-THRESHOLD*` | `10000` | Vectors larger than this MAY use parallel sort; `BLISS:*SORT-PARALLEL-THRESHOLD*` is a deprecated compatibility spelling |

---

## 5.14  Test Strategy

**R5.48** The ANSI test suite (`ansi-test`) MUST pass for all standard
library chapters. Target: 100% of applicable tests.

**R5.49** Bliss-specific tests MUST cover:
- CLOS bootstrap circularity.
- Thread safety of packages, hash tables, and streams.
- Gray streams protocol conformance.
- Sorted output verification for all sort algorithm variants.
- FORMAT edge cases (nested `~{`, recursive `~?`, miser mode).
- Pathname parsing across Linux and macOS.

**R5.50** Performance benchmarks (part of `cl-bench`) MUST include:
- Generic function dispatch (1-arg, 2-arg, N-arg).
- Hash table insert/lookup throughput.
- Sequence sort on 10k, 100k, 1M element vectors.
- FORMAT throughput for log-line generation.
