# §5.6 Sequences & §5.7 Hash Tables

**Scope:** Generic sequence protocol with type-dispatched specializations,
sorting algorithms, parallel sort, and the full hash-table subsystem
including Robin Hood hashing, growth policy, hash functions, synchronization,
and iterator protocol.

---

## 5.6  Sequences

### 5.6.1  Type Dispatch Architecture

**R5.131** — The sequence subsystem MUST support a *generic sequence
protocol* that dispatches to type-specialized fast paths for `LIST`,
`VECTOR`, `SIMPLE-VECTOR`, `SIMPLE-STRING`, and `SIMPLE-BIT-VECTOR`.

**R5.132** — Each sequence function MUST accept any object satisfying
`TYPEP x 'SEQUENCE`. User-defined sequence types (via protocol extension,
§5.6.1.3) SHOULD be supported in a future version; the dispatch table MUST
be designed to accommodate them.

**R5.133** — Type dispatch MUST occur at most once per top-level call
(not once per element).

#### 5.6.1.1  Generic Sequence Protocol

Every ANSI sequence function is implemented as a thin entry point that:

1. Resolves the concrete sequence type via `SEQ-TYPE-TAG` (a 4-bit tag
   derived from the object header; see D1.01 in §1).
2. Indexes into a dispatch table (`SEQ-DISPATCH-TABLE`, D5.21) keyed by
   `(function-id × type-tag)`.
3. Tail-calls the specialized implementation.

```text
seq-dispatch(fn-id, seq, args…):
    tag ← seq-type-tag(seq)
    impl ← SEQ-DISPATCH-TABLE[fn-id][tag]
    if impl = NULL → signal TYPE-ERROR
    tail-call impl(seq, args…)
```

#### 5.6.1.2  List-Specialized Paths

List-specialized routines operate via CDR traversal.  Key specializations:

| Function | Optimization |
|----------|-------------|
| `LENGTH` | CDR-walk counter; detect circularity via tortoise-and-hare (signals `TYPE-ERROR` on cycle) |
| `ELT`    | CDR-walk to index; cache last-accessed cons for sequential patterns |
| `MAP`    | Inline cons-allocation for result list; spine-consing |
| `FIND` / `POSITION` | Linear scan with early exit |
| `REDUCE` | Unboxed accumulator when `:KEY` is `#'IDENTITY` and type is fixnum |

#### 5.6.1.3  Vector-Specialized Paths

Vector paths use direct indexing (`O(1)` element access).  Key optimizations:

| Function | Optimization |
|----------|-------------|
| `LENGTH` | Read fill-pointer or header length field; O(1) |
| `ELT` / `(SETF ELT)` | Bounds check + direct memory load/store; no dispatch per element |
| `MAP` | Allocate result vector up-front with known length |
| `FIND` / `POSITION` | Unrolled loop ×4 for `SIMPLE-VECTOR`; SIMD scan for `(SIMPLE-ARRAY (UNSIGNED-BYTE 8))` |
| `REDUCE` | Loop with unboxed accumulator for numeric element types |
| `COPY-SEQ` | `memcpy` for simple specialized arrays |
| `FILL` | `memset` for `(UNSIGNED-BYTE 8)`; word-fill for fixnum vectors |

### 5.6.2  Compiler Macro Strategy

**R5.134** — Compiler macros MUST be defined for `MAP`, `FIND`, `POSITION`,
`REMOVE`, `REMOVE-IF`, `COUNT`, `REDUCE`, `SORT`, `EVERY`, `SOME`,
`NOTANY`, `NOTEVERY`, `SUBSTITUTE`, and `SUBSTITUTE-IF`.

**R5.135** — When the compiler can determine the sequence type at compile
time (via declared types or type inference from §4), the compiler macro
MUST emit a direct call to the specialized implementation, bypassing the
dispatch table.

**R5.136** — Compiler macros MUST preserve correct semantics for all
keyword argument combinations; if the keyword set is too complex for the
fast path, the macro MUST decline (return the original form).

Strategy:

```lisp
(define-compiler-macro find (&whole form item seq &rest keys
                              &key key test test-not start end from-end)
  (let ((seq-type (compiler-known-type seq)))
    (cond
      ((subtypep seq-type 'simple-vector)
       `(%%find-simple-vector ,item ,seq ,@keys))
      ((subtypep seq-type 'list)
       `(%%find-list ,item ,seq ,@keys))
      (t form))))  ; decline — use generic dispatch
```

### 5.6.3  Sorting Algorithms

#### 5.6.3.1  Vector Sort — Introsort (A5.05)

**R5.137** — `SORT` on vectors MUST use introsort: quicksort with fallback
to heapsort when recursion depth exceeds `2 × floor(log₂(n))`.

**R5.138** — For sub-arrays of length ≤ 24, introsort MUST switch to
insertion sort.

```text
Algorithm A5.05 — Introsort(vec, lo, hi, predicate, key, depth-limit):
  if hi - lo ≤ 24:
      insertion-sort(vec, lo, hi, predicate, key)
      return
  if depth-limit = 0:
      heapsort(vec, lo, hi, predicate, key)
      return
  pivot ← median-of-three(vec, lo, (lo+hi)/2, hi)
  mid   ← partition(vec, lo, hi, pivot, predicate, key)
  Introsort(vec, lo, mid-1, predicate, key, depth-limit - 1)
  Introsort(vec, mid+1, hi, predicate, key, depth-limit - 1)

Initial call: Introsort(vec, 0, n-1, pred, key, 2*floor(log2(n)))
```

**Pivot selection:** Median-of-three from `vec[lo]`, `vec[(lo+hi)/2]`,
`vec[hi]`.  For n > 1000, ninther (median of three medians) SHOULD be used.

#### 5.6.3.2  Vector Sort — Timsort (A5.06)

**R5.139** — `STABLE-SORT` on vectors MUST use Timsort.

**R5.140** — Timsort MUST detect pre-existing ascending and descending
runs, reversing descending runs in-place.

**R5.141** — Minimum run length MUST be computed as the most significant
6 bits of n, plus 1 if any remaining bits are set (yielding 32 ≤ minrun ≤ 64).

```text
Algorithm A5.06 — Timsort(vec, n, predicate, key):
  minrun ← compute-minrun(n)
  runs   ← []
  i ← 0
  while i < n:
      run-start ← i
      run-end   ← detect-run(vec, i, n, predicate, key)
      if run is descending:
          reverse-in-place(vec, run-start, run-end)
      run-len ← run-end - run-start + 1
      if run-len < minrun:
          extend ← min(minrun, n - run-start)
          insertion-sort(vec, run-start, run-start + extend - 1, predicate, key)
          run-len ← extend
      push (run-start, run-len) onto runs
      i ← run-start + run-len
      merge-collapse(runs, vec, predicate, key)
  merge-force-collapse(runs, vec, predicate, key)

Merge uses galloping mode:
  gallop-merge(a, b, predicate, key):
    — Linear compare until one side wins MIN-GALLOP (default 7) consecutive times
    — Switch to galloping: binary-search for insertion point in the winning run
    — Adjust MIN-GALLOP: decrement on successful gallop, increment on failure
    — Temporary buffer allocated from thread-local merge buffer (D5.22)
```

#### 5.6.3.3  List Sort — Merge Sort (A5.07)

**R5.142** — `SORT` and `STABLE-SORT` on lists MUST use bottom-up iterative
merge sort (no recursion, O(1) extra space beyond list cells).

```text
Algorithm A5.07 — List-Merge-Sort(list, predicate, key):
  if list is NIL or (cdr list) is NIL: return list
  width ← 1
  while width < length:
      head ← list
      list ← NIL
      tail ← NIL
      while head ≠ NIL:
          left ← head
          right ← split-at(head, width)
          next  ← split-at(right, width)
          merged ← merge-lists(left, right, predicate, key)
          if tail = NIL: list ← merged
          else: (setf (cdr tail) merged)
          tail ← last-cons(merged)
          head ← next
      width ← width × 2
  return list
```

**R5.143** — List merge sort MUST be stable.

#### 5.6.3.4  Parallel Sort for Large Vectors

**R5.144** — When `n ≥ 100,000` and more than one worker thread is
available, `SORT` and `STABLE-SORT` on vectors SHOULD use parallel
partitioning.

Strategy (work-stealing):

1. **Sample:** Draw `√n` random pivots; sort samples to choose `P−1`
   splitters (where `P` = number of workers, capped at 8).
2. **Classify:** Each worker scans its slice, classifying elements into
   `P` buckets by splitter comparison.
3. **Redistribute:** Prefix-sum on bucket counts; scatter elements into
   partitions.
4. **Sort partitions:** Each worker sorts its partition via A5.05 or
   A5.06 independently.
5. **Concatenate:** Partitions are already in global order; copy back.

Thread coordination uses the runtime work-stealing pool (§2).

### 5.6.4  SORT Destructiveness Contract

**R5.145** — `SORT` MAY destructively modify the input sequence.  Callers
MUST use the return value, not the original binding.

**R5.146** — For vectors, `SORT` MUST sort in-place (no allocation except
for parallel sort scratch buffers).

**R5.147** — For lists, `SORT` MUST reuse existing cons cells (CDR
mutation); no new conses are allocated.

**R5.148** — `STABLE-SORT` follows the same destructiveness contract.
Its temporary merge buffer (D5.22) is thread-local and reused across calls.

### 5.6.5  Sequence Bounding — Start/End Keyword Handling

**R5.149** — All sequence functions accepting `:START` and `:END` keyword
arguments MUST validate bounds before processing.

Shared bounds-check utility:

```rust
/// D5.23 — Bounds-check for sequence operations.
/// Returns validated (start, effective_end).
fn check_seq_bounds(
    seq_len: usize,
    start: Option<usize>,   // default 0
    end: Option<usize>,     // default seq_len (NIL in CL)
) -> Result<(usize, usize), TorclError> {
    let s = start.unwrap_or(0);
    let e = end.unwrap_or(seq_len);
    if s > e || e > seq_len {
        Err(TorclError::type_error(
            "Bounding indices", format!("START={} END={} LENGTH={}", s, e, seq_len)))
    } else {
        Ok((s, e))
    }
}
```

**R5.150** — If `:START` > `:END`, or `:END` > `(LENGTH sequence)`, a
`TYPE-ERROR` MUST be signalled.

**R5.151** — When `:END` is `NIL` (the default), it MUST be treated as
`(LENGTH sequence)`.

---

## 5.7  Hash Tables

### 5.7.1  Robin Hood Hashing (A5.08)

**R5.152** — Hash tables MUST use open addressing with Robin Hood hashing.

Robin Hood hashing minimizes variance in probe sequence length (PSL) by
displacing entries with shorter PSL during insertion.

#### Data Structure D5.20 — Hash Table Layout

```rust
struct TorclHashTable {
    entries:      *mut Entry,      // contiguous array of Entry
    capacity:     usize,           // always a power of two
    count:        usize,           // number of live entries
    mask:         usize,           // capacity - 1
    max_psl:      u8,              // current maximum probe sequence length
    test:         HashTestTag,     // EQ | EQL | EQUAL | EQUALP
    hash_fn:      fn(TorclVal) -> u64,
    eq_fn:        fn(TorclVal, TorclVal) -> bool,
    rehash_size:  f32,             // growth factor (default 2.0)
    rehash_threshold: f32,         // load factor trigger (default 0.75)
    lock:         Option<RwLock>,  // present only for synchronized tables
}

struct Entry {
    key:   TorclVal,               // UNBOUND sentinel for empty
    value: TorclVal,
    hash:  u64,                    // cached full hash
    psl:   u8,                     // probe sequence length from home slot
}
```

#### Algorithm A5.08 — Robin Hood Hash Operations

**Insertion:**

```text
insert(table, key, value):
    if load-factor(table) ≥ rehash-threshold:
        rehash(table)
    h     ← hash(key)
    idx   ← h & mask
    entry ← (key, value, h, psl=0)
    loop:
        slot ← table.entries[idx]
        if slot is EMPTY:
            table.entries[idx] ← entry
            table.count ← table.count + 1
            table.max_psl ← max(table.max_psl, entry.psl)
            return
        if slot.hash = h AND eq(slot.key, key):
            slot.value ← value      // update existing
            return
        if entry.psl > slot.psl:
            swap(entry, slot)        // Robin Hood displacement
            table.entries[idx] ← entry
        entry.psl ← entry.psl + 1
        idx ← (idx + 1) & mask
        if entry.psl > PSL_CAP (= 128):
            rehash(table)            // probe sequence too long
            restart insert
```

**Lookup:**

```text
lookup(table, key):
    h   ← hash(key)
    idx ← h & mask
    psl ← 0
    loop:
        slot ← table.entries[idx]
        if slot is EMPTY OR psl > slot.psl:
            return NOT-FOUND         // Robin Hood invariant: early exit
        if slot.hash = h AND eq(slot.key, key):
            return slot.value
        psl ← psl + 1
        idx ← (idx + 1) & mask
```

**Deletion (backward-shift):**

```text
delete(table, key):
    idx ← find-slot(table, key)
    if idx = NOT-FOUND: return NIL
    table.entries[idx] ← EMPTY
    table.count ← table.count - 1
    // backward shift: pull subsequent entries back
    j ← (idx + 1) & mask
    loop:
        slot ← table.entries[j]
        if slot is EMPTY OR slot.psl = 0:
            break
        slot.psl ← slot.psl - 1
        table.entries[(j - 1) & mask] ← slot
        table.entries[j] ← EMPTY
        j ← (j + 1) & mask
```

### 5.7.2  Growth Policy

**R5.153** — On rehash, capacity MUST double.  The table MUST shrink
(halve) when load drops below 25%, but MUST NOT shrink below the
initial capacity (minimum 16 entries).

| Event | Trigger | Action |
|-------|---------|--------|
| Grow  | `count / capacity ≥ 0.75` OR `max_psl > PSL_CAP` | Allocate 2× array, re-insert all |
| Shrink | `count / capacity < 0.25` AND `capacity > initial-capacity` | Allocate ½ array, re-insert all |

Rehash is always a full rebuild: allocate new array, re-insert every
live entry using A5.08 insertion. The old array is freed after rehash
completes (GC-managed or explicit deallocation for Rust-side tables).

### 5.7.3  Hash Functions

**R5.154** — Hash function selection MUST match the test function:

| Test | Hash Function | Description |
|------|--------------|-------------|
| `EQ` | `tagged-pointer-hash` | XOR-fold of the tagged pointer value with a per-boot random seed; multiply-shift mixing (Murmur3 finalizer) |
| `EQL` | `eql-hash` | `eq-hash` for non-numeric; numeric types: extract raw bits, mix with Murmur3 finalizer; ensures `(eql 1 1.0)` is false → different hashes allowed |
| `EQUAL` | `equal-hash` | Recursive structural hash: strings→FNV-1a on chars; conses→combine car/cdr hashes with rotation+XOR; arrays→hash elements with depth limit of 4; all others→`eql-hash` |
| `EQUALP` | `equalp-hash` | Like `equal-hash` but: strings→FNV-1a on `CHAR-DOWNCASE`'d chars; numbers→coerce to `DOUBLE-FLOAT` bits then hash; hash tables→hash entries recursively |

**Depth limit:** Recursive hashing (`equal-hash`, `equalp-hash`) MUST
enforce a depth limit (default 4) to avoid unbounded traversal of
circular structures.  Beyond the depth limit, return a constant
(e.g., 0) — correctness is preserved since the hash table falls back
to the equality test for collision resolution.

#### SXHASH Implementation

**R5.155** — `SXHASH` MUST satisfy the ANSI contract:
- `(equal x y)` implies `(= (sxhash x) (sxhash y))`.
- The return value is a non-negative fixnum.
- The value is consistent within a session (MAY differ across sessions).

Implementation: `sxhash(x) = equal-hash(x) & MOST-POSITIVE-FIXNUM`.

The per-boot random seed means SXHASH values are NOT stable across
image saves/loads unless the seed is serialized in the image.  TorCL
chooses NOT to serialize the seed (§7 image format) — rehash on load.

### 5.7.4  Synchronized Hash Tables

TorCL offers two levels of thread safety for hash tables:

#### Mutex-Based Synchronized Table

A synchronized hash table wraps a standard Robin Hood table with an
`RwLock` (D5.20, `lock` field).  Granularity: **table-level lock**.

- Every public operation (`GETHASH`, `(SETF GETHASH)`, `REMHASH`,
  `CLRHASH`, `MAPHASH`, `WITH-HASH-TABLE-ITERATOR`) acquires the mutex.
- `GETHASH` acquires a read lock; mutating operations acquire a write lock.
  Implementation uses a `RwLock` rather than plain `Mutex` for read
  parallelism.

Creation via `:SYNCHRONIZED T` keyword to `MAKE-HASH-TABLE` (SBCL extension,
§9).

#### Lock-Free Read Path (Future)

For high-read workloads, a future version MAY implement:
- Atomic entry reads (requires 128-bit CAS or epoch-based reclamation).
- Writers still take the table lock.
- Readers never block, seeing either the old or new value (linearizable
  per-slot).

This is deferred; the data structure layout (D5.20) reserves space for
an epoch counter to enable future migration.

### 5.7.5  WITH-HASH-TABLE-ITERATOR Implementation

```lisp
(defmacro with-hash-table-iterator ((name hash-table) &body body)
  (let ((ht      (gensym "HT"))
        (index   (gensym "INDEX"))
        (cap     (gensym "CAP")))
    `(let* ((,ht     ,hash-table)
            (,index  0)
            (,cap    (%%ht-capacity ,ht)))
       (macrolet ((,name ()
                    `(%%with-ht-read-lock ,',ht    ;; acquire read lock per call
                       (loop
                         (when (>= ,',index ,',cap)
                           (return (values nil nil nil)))
                         (let ((entry (%%entry-ref (%%ht-entries ,',ht) ,',index)))
                           (incf ,',index)
                           (unless (%%entry-empty-p entry)
                             (return (values t
                                             (%%entry-key entry)
                                             (%%entry-value entry)))))))))
         ,@body))))
```

**R5.156** — `WITH-HASH-TABLE-ITERATOR` MUST NOT hold the hash-table
lock for the entire body; it MUST acquire the read lock only during each
`(name)` call to advance the iterator.  Consequence: concurrent
mutations between iterator calls MAY cause skipped or duplicated entries
(ANSI allows this — the standard says consequences are undefined when
modifying during iteration except via `(SETF GETHASH)` on the current key).

### 5.7.6  MAPHASH Specification

**R5.157** — `MAPHASH` MUST call the given function on every key/value
pair in the hash table.  Order of traversal is unspecified.

**R5.158** — For synchronized hash tables, `MAPHASH` MUST acquire the
read lock for the **entire** traversal (unlike `WITH-HASH-TABLE-ITERATOR`,
which acquires per-call).  Rationale: `MAPHASH` guarantees a consistent
snapshot — each entry is visited exactly once, with no skips or
duplicates from concurrent mutations.

**R5.159** — During `MAPHASH`, calling `(SETF GETHASH)` on the current
key is permitted (ANSI §18.1.1).  Calling `REMHASH` on any key or
`(SETF GETHASH)` on a key other than the current one has undefined
consequences.  The implementation MAY (but is not required to) detect
such violations and signal a `PROGRAM-ERROR`.

Implementation:

```text
maphash(function, table):
    acquire read-lock(table)   // entire traversal under lock
    for idx from 0 below table.capacity:
        slot ← table.entries[idx]
        if slot is not EMPTY:
            funcall(function, slot.key, slot.value)
    release read-lock(table)
```

### 5.7.7  MAKE-HASH-TABLE Parameter Handling

**R5.160** — `MAKE-HASH-TABLE` MUST accept the following keyword arguments
per ANSI CL and handle them as specified:

| Keyword | ANSI Type | Internal Handling |
|---------|-----------|-------------------|
| `:TEST` | `{EQ, EQL, EQUAL, EQUALP}` or designator | Resolve to `HashTestTag` enum; signal `TYPE-ERROR` for unsupported tests.  Default: `EQL`. |
| `:SIZE` | Non-negative integer | Advisory initial capacity hint.  Actual capacity = next power of two ≥ `max(size, 16)`.  Default: 16. |
| `:REHASH-SIZE` | Integer ≥ 1, or float > 1.0 | If integer: convert to float (`(float rehash-size)`).  If float ≤ 1.0: signal `TYPE-ERROR`.  Stored as `rehash_size: f32` in D5.20.  Default: `2.0`. |
| `:REHASH-THRESHOLD` | Real in `(0, 1]` | Clamp to `[0.1, 1.0]` (values below 0.1 waste too much memory).  If ≤ 0 or > 1: signal `TYPE-ERROR`.  Stored as `rehash_threshold: f32` in D5.20.  Default: `0.75`. |
| `:SYNCHRONIZED` | Boolean (SBCL extension, §9) | If true, allocate `RwLock` in the `lock` field of D5.20.  Default: `NIL`. |

**R5.161** — Capacity rounding to a power of two MUST be transparent to
the user: `HASH-TABLE-SIZE` returns the actual (rounded) capacity, while
`HASH-TABLE-REHASH-SIZE` and `HASH-TABLE-REHASH-THRESHOLD` return the
values as supplied by the user (or defaults).

---

## Data Structures Summary

| ID | Name | Location | Description |
|----|------|----------|-------------|
| D5.20 | `TorclHashTable` | `crates/torcl-rt/src/hashtable.rs` | Robin Hood open-addressing hash table (§5.7.1) |
| D5.21 | `SEQ-DISPATCH-TABLE` | `crates/torcl-stdlib/src/sequences.lisp` | 2D dispatch table `[fn-id][type-tag]` mapping to specialized impls |
| D5.22 | `TimsortMergeBuffer` | `crates/torcl-rt/src/sort.rs` | Thread-local scratch buffer for Timsort galloping merge; grown to `n/2`, reused |
| D5.23 | `check_seq_bounds` | `crates/torcl-rt/src/sequence.rs` | Shared bounds-validation utility for `:START`/`:END` keywords |

## Algorithms Summary

| ID | Name | Section | Complexity |
|----|------|---------|------------|
| A5.05 | Introsort | §5.6.3.1 | O(n log n) worst-case, O(n log n) average |
| A5.06 | Timsort | §5.6.3.2 | O(n log n) worst-case, O(n) best-case (presorted) |
| A5.07 | List-Merge-Sort | §5.6.3.3 | O(n log n) time, O(1) extra space |
| A5.08 | Robin Hood Hash | §5.7.1 | O(1) amortized lookup/insert, O(n) worst-case rehash |

## Error Handling

| Condition | When | Action |
|-----------|------|--------|
| `TYPE-ERROR` | Non-sequence passed to sequence function | Signal immediately |
| `TYPE-ERROR` | Invalid `:START`/`:END` bounds | Signal via `check_seq_bounds` (D5.23) |
| `TYPE-ERROR` | Circular list detected in `LENGTH` | Signal after tortoise-and-hare detection |
| `TYPE-ERROR` | Wrong-type key for `EQ`/`EQL`/`EQUAL`/`EQUALP` test | Signal from test function |
| `STORAGE-CONDITION` | Hash table rehash allocation failure | Signal; table state unchanged (old array retained) |

## Concurrency

- **Sequence operations** are not synchronized; operating on a shared
  sequence from multiple threads without external locking has undefined
  behavior (matching SBCL semantics).
- **Synchronized hash tables** use `RwLock` (§5.7.4); lock ordering: hash
  table locks are leaf locks — no other runtime lock may be acquired while
  holding a hash-table lock.
- **Parallel sort** coordinates via the runtime work-stealing pool (§2);
  no additional locks — each worker writes to its own partition.

## Configuration

| Knob | Default | Env Var | Description |
|------|---------|---------|-------------|
| `*sort-parallel-threshold*` | 100,000 | `TORCL_SORT_PAR_THRESHOLD` | Min vector length for parallel sort |
| `*sort-parallel-max-workers*` | 8 | `TORCL_SORT_PAR_WORKERS` | Max threads for parallel sort |
| `*hash-table-initial-capacity*` | 16 | — | Minimum capacity for new hash tables |
| `*hash-table-psl-cap*` | 128 | — | Max probe sequence length before forced rehash |
| `*hash-table-rehash-threshold*` | 0.75 | — | Load factor triggering growth |
| `*hash-table-shrink-threshold*` | 0.25 | — | Load factor triggering shrink |

## Test Strategy

1. **ANSI test suite:** Run `ansi-test` sections for `SORT`, `STABLE-SORT`,
   all sequence functions, `MAKE-HASH-TABLE`, `GETHASH`, `REMHASH`,
   `MAPHASH`, `CLRHASH`, `SXHASH`, `WITH-HASH-TABLE-ITERATOR`.
2. **Property-based tests:** QuickCheck-style tests for sort stability,
   sort correctness (permutation check), hash-table invariants (Robin Hood
   PSL monotonicity after each operation).
3. **Stress tests:** Concurrent `GETHASH`/`(SETF GETHASH)` on synchronized
   tables from 16 threads; verify no lost updates and no crashes.
4. **Performance benchmarks:** Compare sort throughput against SBCL's
   `SORT`/`STABLE-SORT`; measure hash-table lookup latency (p50/p99) under
   varying load factors.
5. **Edge cases:** Empty sequences, single-element, all-equal elements,
   reverse-sorted, circular list detection, hash collisions with identical
   PSL, zero-capacity hash table, boundary load factors.
