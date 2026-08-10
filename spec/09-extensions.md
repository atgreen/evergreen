# §9  SBCL-Compatible Extensions

**Scope:** Bliss adopts a curated set of SBCL-compatible extensions where
ANSI X3.226-1994 is silent and the extension is widely depended upon by the
portable Common Lisp ecosystem. Each extension MUST have a rationale,
MUST NOT conflict with ANSI semantics, and MUST be documented here.

## 9.1  Adoption Criteria

An SBCL extension is adopted when ALL of the following hold:

1. **Ecosystem demand** — used by ≥3 popular libraries (Quicklisp top-100)
   or essential for practical CL development.
2. **No ANSI conflict** — the extension operates in an area the standard
   leaves unspecified or explicitly implementation-defined.
3. **Clean namespace** — exported from `BLISS-EXT` (not `CL`), with
   `SB-EXT`-compatible symbol names available via a compatibility package.
4. **Documented divergence** — any behavioural difference from SBCL's
   version is listed explicitly.

**R9.01** Bliss MUST provide a `BLISS-EXT` package exporting all adopted
extensions.
**R9.02** Bliss MUST provide `SB-EXT`, `SB-THREAD`, `SB-MOP` compatibility
packages that re-export the corresponding Bliss symbols, enabling existing
SBCL-targeting code to load unchanged.
**R9.03** Each adopted extension MUST include a test verifying SBCL-compatible
behaviour.

## 9.2  Threading Extensions (`SB-THREAD` Compatibility)

**R9.04** Bliss MUST provide the following threading API in `BLISS-THREADS`,
re-exported via `SB-THREAD`:

| Symbol | Type | Notes |
|--------|------|-------|
| `MAKE-THREAD` | function | `(make-thread fn &key name)` |
| `JOIN-THREAD` | function | Blocks until thread completes |
| `THREAD-ALIVE-P` | function | |
| `INTERRUPT-THREAD` | function | Async signal to target thread |
| `*CURRENT-THREAD*` | special var | |
| `MAKE-MUTEX` / `WITH-MUTEX` | function/macro | Recursive by default |
| `MAKE-WAITQUEUE` / `CONDITION-WAIT` / `CONDITION-NOTIFY` | functions | POSIX-style condvar |
| `MAKE-SEMAPHORE` / `SIGNAL-SEMAPHORE` / `WAIT-ON-SEMAPHORE` | functions | Counting semaphore |
| `BARRIER` | macro | Memory barrier |

**R9.05** `BORDEAUX-THREADS` MUST load and pass its test suite on Bliss
without modification.

## 9.3  Global Variables

**R9.06** `DEFGLOBAL` — like `DEFVAR` but declares the variable globally
special with a fixed value that is the same across all threads (no
per-thread binding). Maps to an atomic cell in the runtime.

**R9.07** `DEFINE-LOAD-TIME-GLOBAL` — evaluated at load time, thereafter
treated as a global constant for optimisation purposes.

## 9.4  Weak References and Finalizers

**R9.08** `MAKE-WEAK-POINTER` / `WEAK-POINTER-VALUE` — wraps an object;
the GC may clear the pointer if the object is otherwise unreachable.

**R9.09** `FINALIZE` / `CANCEL-FINALIZATION` — attach/detach a finalizer
function to an object. Finalizers run in a dedicated thread, never
during GC (§3).

## 9.5  Hash Table Extensions

**R9.10** `:SYNCHRONIZED` keyword to `MAKE-HASH-TABLE` — creates a
thread-safe hash table with internal locking.

**R9.11** `:WEAKNESS` keyword — supports `:KEY`, `:VALUE`,
`:KEY-AND-VALUE`, `:KEY-OR-VALUE` weak hash tables.

**R9.12** `WITH-LOCKED-HASH-TABLE` — macro for multi-operation
atomicity on synchronized hash tables.

## 9.6  Sequence Extensions

**R9.13** `SB-SEQUENCE:DEFINE-SEQUENCE-CLASS` — user-defined sequence
types that integrate with standard sequence functions. MAY be deferred
to v2 if ecosystem demand is low.

## 9.7  MOP Extensions (`SB-MOP` / Closer-MOP Compatibility)

**R9.14** Bliss MUST export enough MOP symbols to allow `CLOSER-MOP` to
load and expose a portable MOP interface. Minimum set:

- `CLASS-DIRECT-SUBCLASSES`, `CLASS-DIRECT-SUPERCLASSES`
- `CLASS-SLOTS`, `CLASS-DIRECT-SLOTS`
- `SLOT-DEFINITION-NAME`, `SLOT-DEFINITION-INITFORM`, etc.
- `COMPUTE-APPLICABLE-METHODS-USING-CLASSES`
- `MAKE-METHOD-LAMBDA` (SBCL-compatible signature)

Full AMOP is a v2 goal; v1 provides the Closer-MOP minimum.

## 9.8  Miscellaneous

**R9.15** `SAVE-LISP-AND-DIE` — dump a heap image and exit (§7).
Bliss name: `SAVE-IMAGE`; compatibility alias provided.

**R9.16** `QUIT` / `EXIT` — process exit with status code.

**R9.17** `WITH-TIMEOUT` — execute body with a wall-clock time limit;
signal `TIMEOUT` condition on expiry.

**R9.18** `NATIVE-NAMESTRING` — return the OS-native path string for a
pathname object (§5.7).

**R9.19** Source-location tracking: `DEFINITION-SOURCE` returns file,
line, and form-number for any named definition. Used by SLIME/SLY
`M-.` (§6).

## 9.9  Extension Versioning

**R9.20** The `BLISS-EXT` package exports a feature keyword
`:BLISS-EXT-VERSION` whose value is a `(major minor)` list. Libraries
can conditionalize on extension availability.

**R9.21** Removed or incompatibly changed extensions MUST bump the major
version and be announced in release notes.
