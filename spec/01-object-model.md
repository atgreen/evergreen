# §1 Object Model

All Common Lisp values in EGCL are represented as 64-bit tagged words
(`EgclVal`). This chapter specifies the tagged-pointer encoding,
object header format, memory layouts for every built-in type, the CL
type lattice mapping, and the NIL representation.

Source: `crates/egcl-rt/src/object.rs`, `crates/egcl-rt/src/types/`.

---

## 1.1  Requirements

| ID | Requirement |
|----|-------------|
| R1.01 | Every Lisp value MUST be representable as a single 64-bit `EgclVal`. |
| R1.02 | The low 3 bits of a `EgclVal` MUST encode the primary type tag per §4.1 of §0. |
| R1.03 | Fixnums MUST represent at least the range [−2⁶⁰, 2⁶⁰−1] (61-bit signed). |
| R1.04 | Characters MUST support the full Unicode range (U+0000 – U+10FFFF). |
| R1.05 | Single-float immediates MUST use IEEE 754 binary32 representation. |
| R1.06 | Heap pointers (tags `001`, `010`, `110`) MUST be 8-byte aligned; the runtime MUST recover the raw pointer by masking the low 3 bits. |
| R1.07 | Every heap-allocated object MUST begin with an `ObjectHeader` (D1.02) that is exactly 8 bytes. |
| R1.08 | The object header MUST contain a type-ID field sufficient to distinguish all built-in types (≥ 8 bits). |
| R1.09 | The GC MUST be able to read and write mark/forward bits in the header without a lock in the common (non-forwarding) case. |
| R1.10 | `NIL` MUST satisfy `SYMBOLP`, `LISTP`, and `NULL` simultaneously. |
| R1.11 | `NIL` MUST be encoded as the `EgclVal` bit pattern `0b111` (tag `111`, payload zero). |
| R1.12 | `T` MUST be encoded as `EgclVal` with tag `111` and payload `1` (bit pattern `0b1_111` = `0x0F`). |
| R1.13 | Cons cells MUST be exactly 16 bytes (two `EgclVal` fields: CAR, CDR), with no header. |
| R1.14 | Simple strings MUST use fixed-width specialised-vector internal storage (§1.6.3), with the element width fixed at construction and encoded by the `type_id`: `SIMPLE_BASE_STRING` = 1 byte/char (`BASE-CHAR`, code points < 256), `SIMPLE_CHARACTER_STRING` = 4 bytes/char (`CHARACTER`, full range), so `CHAR`/`SCHAR`/`AREF` are O(1) by character index. There is no runtime coder promotion; growable strings use complex arrays (§1.6.4). UTF-8 is an external-format encoding only, applied at I/O boundaries — NOT the internal representation. |
| R1.15 | Arrays MUST support all element-type specialisations required by ANSI CL §15.1. |
| R1.16 | Symbols MUST contain at least: name, value, function, plist, and package cells. |
| R1.17 | The `UNBOUND` marker MUST be a unique `EgclVal` with tag `111` that is distinct from NIL, T, and every other value. |
| R1.18 | The runtime MUST provide O(1) type-tag extraction from any `EgclVal`. |
| R1.19 | Hash codes stored in object headers MUST be lazily computed on first call to `SXHASH` and cached. |
| R1.20 | All heap object layouts MUST be naturally aligned (fields aligned to their size, overall object aligned to 8 bytes). |
| R1.21 | Every `EgclVal` MUST be passable across FFI boundaries as a `u64` / `uint64_t`. |

---

## 1.2  EgclVal Tagged Pointer — D1.01

```text
63                              3  2  1  0
┌──────────────────────────────┬──┬──┬──┐
│          payload (61 bits)   │ t2│t1│t0│
└──────────────────────────────┴──┴──┴──┘
```

`EgclVal` is a `#[repr(transparent)]` wrapper around `u64` in Rust.

### 1.2.1  Tag Table

| Tag (`t2 t1 t0`) | Binary | Type class | Payload interpretation |
|-------------------|--------|------------|------------------------|
| `000` | `0b000` | **Fixnum** | Arithmetic-right-shift 3 → signed 61-bit integer |
| `001` | `0b001` | **Cons** | `(payload & !0x7)` → pointer to 16-byte cons cell |
| `010` | `0b010` | **Heap object** | `(payload & !0x7)` → pointer to `ObjectHeader` |
| `011` | `0b011` | **Character** | Bits 3–23 → 21-bit Unicode codepoint; bits 24–63 reserved (zero) |
| `100` | `0b100` | **Single-float** | Bits 32–63 → IEEE 754 binary32; bits 3–31 zero |
| `101` | `0b101` | **Symbol (indexed)** | Bits 3–34 → 32-bit index into global symbol table |
| `110` | `0b110` | **Function** | `(payload & !0x7)` → pointer to function header |
| `111` | `0b111` | **Special** | Sub-discriminated by payload (see §1.2.2) |

### 1.2.2  Special Values (tag `111`)

| Payload (bits 63:3) | Constant | Bit pattern (hex) | Purpose |
|----------------------|----------|--------------------|---------|
| `0` | `NIL` | `0x0000_0000_0000_0007` | Boolean false, empty list, symbol `CL:NIL` |
| `1` | `T` | `0x0000_0000_0000_000F` | Boolean true, symbol `CL:T` |
| `2` | `UNBOUND` | `0x0000_0000_0000_0017` | Unbound variable / function marker |
| `3` | `MISSING` | `0x0000_0000_0000_001F` | Missing `&optional`/`&key` sentinel (internal) |
| `4` | `EOF` | `0x0000_0000_0000_0027` | Reader EOF marker (internal) |

`NIL` and `T` also appear in the symbol table (see §1.7) but their
`EgclVal` encoding is always the special-tag form, never the symbol-index
form. Runtime code MUST compare with the canonical bit patterns (R1.11, R1.12).

---

## 1.3  Object Header — D1.02

Every heap-allocated object (tag `010`, `110`, and structures behind
cons cells) begins with an 8-byte header. Cons cells are the sole
exception — they carry no header to save space (R1.13).

```text
63          56 55    48 47                 16 15             0
┌────────────┬────────┬─────────────────────┬───────────────┐
│  type_id   │ gc_bits│       hash          │    size        │
│  (8 bits)  │(8 bits)│     (32 bits)       │  (16 bits)    │
└────────────┴────────┴─────────────────────┴───────────────┘
```

| Field | Bits | Description |
|-------|------|-------------|
| `type_id` | 63:56 | Discriminator for the heap type (see §1.3.1). Up to 256 types. |
| `gc_bits` | 55:48 | Structural GC flags: forwarding (1), pinned (1), remembered-set (1), reserved (5). Written only during STW phases. **Note:** marking state (mark-white/black, mark-grey) is tracked in the global mark bitmap (§3.4, R3.17), not in these header bits. See §3. |
| `hash` | 47:16 | 32-bit identity-hash cache. Zero means "not yet computed." Lazily filled on first `SXHASH` call (R1.19). 32 bits provides ~4 billion distinct hashes, sufficient to avoid excessive collisions in EQ-based hash tables even with millions of live objects. |
| `size` | 15:0 | Object size in 8-byte units including header. Max inline size = 65 535 × 8 = 524 280 bytes. Objects exceeding this use a **large-object extension**: the `size` field is set to `0xFFFF` (sentinel), and the true 64-bit byte size is stored as a `u64` immediately following the header at offset 8, with the object's payload beginning at offset 16 instead of offset 8. Objects exceeding `region_size / 2` (default 1 MB with a 2 MB region) are allocated directly in large-object regions (§3.2.5). |

### 1.3.1  GC Bits Layout

```text
Bits 55:54: reserved (MUST be zero; mark state lives in side-table bitmap, §3.4)
Bit 53: forwarded (object has been evacuated; rest of object = forwarding address)
Bit 52: pinned (object must not be moved)
Bit 51: remembered (in remembered-set for generational barrier)
Bits 50:48: reserved for future use (MUST be zero)
```

> **Design note:** Marking bits (white/black/grey) are intentionally
> excluded from the object header to avoid data races between concurrent
> marking threads and mutator field writes (R3.17).  The global mark
> bitmap (§3.4) provides the authoritative marking state.

### 1.3.2  Heap Type IDs

| type_id | CL Type | Heap layout section |
|---------|---------|---------------------|
| `0x01` | `CONS` | §1.5.1 (headerless — ID used only in type checks on forwarded objects) |
| `0x02` | `SYMBOL` | §1.7 |
| `0x03` | `SIMPLE-VECTOR` | §1.6.1 |
| `0x04` | `SIMPLE-ARRAY` (general) | §1.6.2 |
| `0x05` | `SIMPLE-BASE-STRING` | §1.6.3 |
| `0x06` | `SIMPLE-CHARACTER-STRING` | §1.6.3 |
| `0x07` | `COMPLEX-ARRAY` | §1.6.4 |
| `0x08` | `BIGNUM` | §1.8.1 |
| `0x09` | `RATIO` | §1.8.2 |
| `0x0A` | `COMPLEX` (number) | §1.8.3 |
| `0x0B` | `DOUBLE-FLOAT` | §1.8.4 |
| `0x0C` | `HASH-TABLE` | §1.9 |
| `0x0D` | `STRUCTURE` | §1.10.1 |
| `0x0E` | `STANDARD-OBJECT` | §1.10.2 |
| `0x0F` | `FUNCTION` (interpreted) | §1.11.1 |
| `0x10` | `COMPILED-FUNCTION` | §1.11.2 |
| `0x11` | `CLOSURE` | §1.11.3 |
| `0x12` | `PACKAGE` | §1.12 |
| `0x13` | `STREAM` | §1.13 |
| `0x14` | `PATHNAME` | §1.14 |
| `0x15` | `READTABLE` | §1.15 |
| `0x16` | `CONDITION` | §1.10.3 |
| `0x17` | `RESTART` | §1.16 |
| `0x18` | `MD-ARRAY` | Multidimensional array: storage, dimensions, rank |
| `0x19` | `MUTEX` | §13.9; opaque native mutex handle |
| `0x1A` – `0xFF` | — | Reserved for user-defined / future types |

---

## 1.4  Immediate Types

### 1.4.1  Fixnum (tag `000`)

```rust
// Encoding
fn fixnum(n: i64) -> EgclVal { EgclVal((n << 3) as u64) }
// Decoding
fn as_fixnum(v: EgclVal) -> i64 { (v.0 as i64) >> 3 }
```

Range: [−2⁶⁰, 2⁶⁰ − 1]. Overflow to `BIGNUM` MUST be handled by
arithmetic operations (§5 sequences / numbers).

Because the tag is `000`, fixnum addition can use raw machine `ADD` and
check overflow, then strip/re-tag only when needed. (See §4 baseline
compiler fast paths.)

### 1.4.2  Character (tag `011`)

```text
63       24 23          3 2  0
┌──────────┬─────────────┬────┐
│  zeros   │  codepoint  │011 │
└──────────┴─────────────┴────┘
```

21-bit codepoint supports U+0000–U+10FFFF (R1.04). Upper bits MUST be
zero. `CHAR-CODE` / `CODE-CHAR` extract/insert the codepoint field.

### 1.4.3  Single-Float (tag `100`)

```text
63       32 31          3 2  0
┌──────────┬─────────────┬────┐
│ IEEE f32 │   zeros     │100 │
└──────────┴─────────────┴────┘
```

The 32-bit float occupies bits 63:32. `DOUBLE-FLOAT` is heap-allocated
(§1.8.4). NaN boxing is NOT used; the low 32 bits are always zero + tag.

### 1.4.4  Symbol Index (tag `101`)

```text
63       35 34          3 2  0
┌──────────┬─────────────┬────┐
│  zeros   │  sym_index  │101 │
└──────────┴─────────────┴────┘
```

32-bit index into the global symbol table (max ~4 billion symbols).
The symbol table itself is a heap-resident `Vec<*mut SymbolData>`.
Index 0 is reserved (maps to `NIL`'s symbol data); index 1 maps to
`T`'s symbol data. However, the `EgclVal` for `NIL`/`T` always uses
tag `111` — the symbol-index encoding is used only for other symbols.

---

## 1.5  Cons Cells

### 1.5.1  Layout — D1.03

Cons cells are **headerless** to minimise memory: exactly 16 bytes.

```text
Offset  Size   Field
  0       8    car: EgclVal
  8       8    cdr: EgclVal
```

Allocation: bump-pointer in the nursery TLAB (§3). Pointer tag `001`
points directly at byte offset 0 of the cons cell; untag by
`ptr & !0x7`.

Headerless cons cells mean the GC must identify cons cells by their
allocation region (nursery cons pages vs. object pages) or by the
`EgclVal` tag of the referring pointer.

**Cons forwarding protocol.** When a cons is evacuated during GC, it is
copied to another cons page (remaining headerless at the destination —
it does NOT gain an `ObjectHeader`). The original cell is then
overwritten in place as follows:

- `car` ← a **forwarding sentinel**: the special `EgclVal` bit pattern
  `0x0000_0000_0000_0017` (`UNBOUND`). Because `UNBOUND` can never
  legitimately appear as a `car` value, its presence signals that the
  cell has been forwarded.
- `cdr` ← the new address of the evacuated cons, encoded as a raw
  `EgclVal` with tag `001` (cons pointer to the destination cell).

During GC pointer-fix-up, any cons reference is checked by loading the
`car` field of the target cell: if it equals the `UNBOUND` sentinel,
the reference is updated to the forwarding address stored in `cdr`.

The type_id `0x01` in the Heap Type ID table (§1.3.2) is reserved for
GC metadata and internal type-dispatch tables; it is never stored in an
`ObjectHeader` for cons cells (since they remain headerless throughout
their lifetime, including after evacuation).

---

## 1.6  Arrays and Strings

### 1.6.1  Simple-Vector — D1.04

```text
Offset  Size       Field
  0       8        ObjectHeader { type_id=0x03, ..., size }
  8       8        length: u64 (number of elements)
 16       8×N      data[0..N]: EgclVal[]
```

Element type is `T` (general). Total size = 16 + 8×N, rounded up to
8-byte alignment (always satisfied since 8×N is aligned).

### 1.6.2  Simple Specialised Array — D1.05

```text
Offset  Size       Field
  0       8        ObjectHeader { type_id=0x04, ..., size }
  8       8        length: u64
 16       1        rank: u8
 17       1        element_type_tag: u8 (enum, see below)
 18       6        padding (align to 8)
 24       8×rank   dimensions[0..rank]: u64
 24+8R    varies   data[] — packed per element_type_tag
```

Element type tags:

| Tag | CL Type | Bits per element |
|-----|---------|-----------------|
| `0` | `T` | 64 (EgclVal) |
| `1` | `BIT` | 1 |
| `2` | `(UNSIGNED-BYTE 8)` | 8 |
| `3` | `(UNSIGNED-BYTE 16)` | 16 |
| `4` | `(UNSIGNED-BYTE 32)` | 32 |
| `5` | `(UNSIGNED-BYTE 64)` | 64 |
| `6` | `(SIGNED-BYTE 8)` | 8 |
| `7` | `(SIGNED-BYTE 16)` | 16 |
| `8` | `(SIGNED-BYTE 32)` | 32 |
| `9` | `(SIGNED-BYTE 64)` | 64 |
| `10` | `SINGLE-FLOAT` | 32 |
| `11` | `DOUBLE-FLOAT` | 64 |
| `12` | `CHARACTER` | 32 (UCS-4) |
| `13` | `BASE-CHAR` | 8 (ASCII/Latin-1) |

Data region is packed and padded to the next 8-byte boundary (R1.20).

### 1.6.3  Strings — D1.06

**Decision: fixed-width specialised simple-string vectors, modelled on SBCL
(R1.14).** (Supersedes the original UTF-8-only decision; see D1.06-history.)

A **simple string** is a fixed-width array of characters. Its element width
is fixed at construction and encoded by the object's `type_id` — the width
*is* the type, and there is no runtime re-encoding:

| `type_id` | CL type | Width | Range |
|-----------|---------|-------|-------|
| `0x05` `SIMPLE_BASE_STRING` | `(simple-array base-char (*))` | 1 byte/char | code points 0–255 |
| `0x06` `SIMPLE_CHARACTER_STRING` | `(simple-array character (*))` | 4 bytes/char (`u32`, native-endian) | 0–`#x10FFFF` |

`length` is the **character** count. `CHAR`/`SCHAR`/`AREF` and
`(SETF CHAR)`/`(SETF SCHAR)` are **O(1)** by character index.

```text
Simple string (type_id 0x05 base / 0x06 character):
  Offset  Size     Field
    0       8      ObjectHeader
    8       8      length: u64  (CHARACTER count)
   16       W·L    data: L elements of W bytes (W = 1 base, 4 character)
   16+W·L   pad    zero-padding to the next 8-byte boundary
```

**Element type is fixed at construction — no promotion.** `MAKE-STRING` (and
general string construction) default to element type `CHARACTER`, i.e. a
32-bit `SIMPLE-CHARACTER-STRING` that holds any code point and so is freely
mutable by `(SETF CHAR)`. A `SIMPLE-BASE-STRING` (8-bit) is produced only
when `BASE-CHAR` element type is requested or when the source is known to be
all-`BASE-CHAR` and immutable (e.g. an interned reader literal, a compactness
optimisation); storing a code point ≥ 256 into one is a `TYPE-ERROR`, never a
promotion. This is SBCL's model: the two widths are distinct specialised
vector types, chosen once, rather than a single string with a mutable coder
flag (the JVM Compact-Strings scheme — appropriate for *immutable* Java
strings, but a poor fit for CL's mutable char arrays).

**Growable strings.** Adjustable, fill-pointer, and displaced strings are NOT
simple strings: they are complex arrays (§1.6.4, `type_id 0x07`) whose
underlying store is a char-typed simple string. Growth (`ADJUST-ARRAY`,
`VECTOR-PUSH-EXTEND`) reallocates that underlying store with identity
preserved by the complex-array header, so a string only ever "grows" through
this path — a simple string's length is immutable.

**Equality.** `EQUAL`/`STRING=`/hashing compare by *character* (code point),
so a base-char and a character with the same code point are equal and a base
string and a character string with the same characters are `EQUAL` and hash
alike, independent of storage width.

**Rationale.** Fixed-width storage gives constant-time character indexing (the
UTF-8 layout was O(n) per `CHAR`, so an index loop was O(n²)); the base-char
width keeps the common ASCII/Latin-1 case at 1 byte/char. `CHARACTER` uses a
fixed-width 32-bit `UCS4` element (not UTF-16) so that `CHAR` — which returns
a whole code point up to `#x10FFFF`, unlike Java `charAt`'s 16-bit code unit —
stays O(1).

**External format.** UTF-8 remains the default external format: streams encode
on write and decode on read at the I/O boundary (§05-04). It is never the
in-memory representation.

**D1.06-history.** The original decision stored both string types as UTF-8
bytes with an O(n) `CHAR`; a first revision adopted the JVM Compact-Strings
coder-flag scheme. Both were superseded (bliss-pd0) by the SBCL fixed-width
model above: dynamic coder promotion does not fit CL's mutable, fixed-element-
type strings, and growable strings already have a home in complex arrays.

### 1.6.4  Complex Arrays — D1.07

Complex arrays add displacement, fill-pointer, and adjustability.

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x07 }
  8       8      underlying: EgclVal  — points to a simple array (the data store)
 16       8      displacement: u64     — element offset into underlying
 24       8      fill_pointer: u64     — MOST-POSITIVE-FIXNUM if none
 32       1      flags: u8            — bit 0: adjustable, bit 1: has-fill-pointer
 33       1      rank: u8
 34       6      padding
 40       8×rank  dimensions[0..rank]: u64
```

`ADJUST-ARRAY` on an adjustable array replaces `underlying` and updates
`displacement` / `dimensions` atomically (pointer-swap + fence).

---

## 1.7  Symbols — D1.08

```text
Offset  Size  Field
  0       8   ObjectHeader { type_id=0x02 }
  8       8   name: EgclVal        — string (the symbol name)
 16       8   value: EgclVal       — global value cell (UNBOUND if unbound)
 24       8   function: EgclVal    — global function cell (UNBOUND if undefined)
 32       8   plist: EgclVal       — property list (NIL or cons)
 40       8   package: EgclVal     — home package (NIL for uninterned)
 48       4   flags: u32            — bit 0: constant, bit 1: special, bit 2: macro, bit 3: compiler-macro
 52       4   tls_index: u32        — thread-local-storage slot index (0 = no TLS binding)
```

Total: 56 bytes per symbol.

The **global symbol table** is a `Vec<*mut SymbolData>` guarded by a
read-write lock. Interning inserts into both the table and the owning
package's hash-maps (§1.12). The symbol-index in a `EgclVal` (tag
`101`) is the index into this vector.

`NIL` and `T` have symbol table entries at indices 0 and 1, but their
`EgclVal` representation is always the special-tag encoding. Type
checks for `SYMBOLP` MUST accept both tag `101` and the special-tag
NIL/T bit patterns.

Thread-local bindings use the `tls_index` field. If non-zero, the
runtime checks `thread.tls_slots[tls_index]` before the global `value`
cell. Special variable binding (`LET` on a proclaimed-special var)
pushes/pops onto the TLS slot stack (§2).

---

## 1.8  Numeric Heap Types

### 1.8.1  Bignum — D1.09

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x08 }
  8       4      sign: i32 (1 or -1)
 12       4      n_limbs: u32
 16       8×N    limbs[0..N]: u64 (little-endian order, least significant first)
```

Limbs are unsigned 64-bit words. Arithmetic delegates to a port of
GMP-style algorithms (see §5 numbers).

### 1.8.2  Ratio — D1.10

```text
Offset  Size  Field
  0       8   ObjectHeader { type_id=0x09 }
  8       8   numerator: EgclVal   — fixnum or bignum
 16       8   denominator: EgclVal — fixnum or bignum, always positive
```

Ratios MUST be stored in lowest terms (GCD = 1). The denominator MUST
NOT be 1 (use fixnum/bignum instead).

### 1.8.3  Complex — D1.11

```text
Offset  Size  Field
  0       8   ObjectHeader { type_id=0x0A }
  8       8   realpart: EgclVal
 16       8   imagpart: EgclVal
```

If both parts are rational, the type is `(COMPLEX RATIONAL)`. If either
is a float, both are coerced to the same float type per ANSI rules.

### 1.8.4  Double-Float — D1.12

```text
Offset  Size  Field
  0       8   ObjectHeader { type_id=0x0B }
  8       8   value: f64 (IEEE 754 binary64)
```

Boxed because 64-bit payload + 3-bit tag doesn't fit in 64 bits.

---

## 1.9  Hash-Table — D1.13

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x0C }
  8       8      count: u64          — number of live entries
 16       8      capacity: u64       — number of bucket slots
 24       8      buckets: *mut Bucket — pointer to external bucket array
 32       8      rehash_threshold: f64
 40       1      test: u8            — 0=EQ 1=EQL 2=EQUAL 3=EQUALP
 41       1      flags: u8           — bit 0: synchronized (thread-safe)
 42       6      padding
```

Buckets use Robin Hood open addressing. Each `Bucket` is 24 bytes:
`{ hash: u64, key: EgclVal, value: EgclVal }`.

Synchronized hash-tables (bit 0 of flags) use a per-table reader-writer
lock; unsynchronized tables require external synchronisation.

---

## 1.10  Structures and CLOS Objects

### 1.10.1  Structure Instance — D1.14

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x0D }
  8       8      layout: EgclVal    — pointer to structure-class descriptor
 16       8×N    slots[0..N]: EgclVal
```

Slot count N is determined by the `layout` descriptor at structure
definition time. Slot access is direct indexed — no hash lookup.

### 1.10.2  Standard-Object Instance — D1.15

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x0E }
  8       8      class: EgclVal     — pointer to standard-class metaobject
 16       8      slot_vector: EgclVal — pointer to simple-vector of slot values
```

Indirection through `slot_vector` enables class redefinition (CLOS
`UPDATE-INSTANCE-FOR-REDEFINED-CLASS`): the vector is replaced
atomically while the instance identity is preserved.

### 1.10.3  Condition — D1.16

Same physical layout as standard-object (type_id `0x16`). Distinguished
type_id enables fast `CONDITIONP` checks.

---

## 1.11  Functions and Closures

### 1.11.1  Interpreted Function — D1.17

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x0F }
  8       8      lambda_list: EgclVal  — parsed lambda list
 16       8      body: EgclVal         — cons-tree of body forms
 24       8      env: EgclVal          — captured lexical environment
 32       8      name: EgclVal         — function name (symbol or list) or NIL
```

### 1.11.2  Compiled Function — D1.18

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x10 }
  8       8      entry_point: *const u8  — native code address
 16       8      code_size: u64         — size of native code in bytes
 24       8      name: EgclVal
 32       8      lambda_list: EgclVal  — for introspection
 40       2      min_args: u16
 42       2      max_args: u16 (0xFFFF = &rest)
 44       1      tier: u8 (1=baseline, 2=optimised)
 45       3      padding
 48       8      constants: EgclVal    — simple-vector of referenced constants
```

### 1.11.3  Closure — D1.19

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x11 }
  8       8      function: EgclVal   — compiled-function or interpreted-function
 16       8×N    closed_vars[0..N]: EgclVal  — captured variable values
```

Closures share the underlying function and add a flat array of captured
variable values. Mutable captured variables use a heap-allocated cell
(a single-element simple-vector) so multiple closures sharing the same
mutable binding see the same cell (standard CL semantics).

---

## 1.12  Package — D1.20

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x12 }
  8       8      name: EgclVal          — string
 16       8      internal_symbols: EgclVal — hash-table (string → symbol)
 24       8      external_symbols: EgclVal — hash-table (string → symbol)
 32       8      use_list: EgclVal       — list of used packages
 40       8      nicknames: EgclVal      — list of strings
 48       8      lock: *mut RwLock<()>    — pointer to heap-allocated per-package reader-writer lock
```

---

## 1.13  Stream — D1.21

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x13 }
  8       1      direction: u8       — :input=0 :output=1 :io=2
  9       1      element_type: u8    — 0=character 1=byte
 10       6      padding
 16       8      ops: *const StreamOps — vtable: read_char, write_char, read_byte, ...
 24       8      state: *mut u8       — opaque backend state (file descriptor, buffer, etc.)
 32       8      column: u64          — current column for pretty-printer
```

`StreamOps` is a Rust trait-object vtable. Concrete stream types
(file-stream, string-stream, broadcast-stream, etc.) provide their
own `StreamOps` implementations.

---

## 1.14  Pathname — D1.22

```text
Offset  Size   Field
  0       8    ObjectHeader { type_id=0x14 }
  8       8    host: EgclVal
 16       8    device: EgclVal
 24       8    directory: EgclVal   — list
 32       8    name: EgclVal
 40       8    type_field: EgclVal
 48       8    version: EgclVal
```

All components are either strings, symbols (`:WILD`, `:UNSPECIFIC`,
`:NEWEST`), `NIL`, or lists (directory component). Logical pathnames
use the same structure with `host` being a logical-host string.

---

## 1.15  Readtable — D1.23

```text
Offset  Size     Field
  0       8      ObjectHeader { type_id=0x15 }
  8       8      case_mode: u8  — 0=:upcase 1=:downcase 2=:preserve 3=:invert
  9       7      padding
 16       8      char_table: EgclVal — simple-vector of 128 entries (syntax types for ASCII)
 24       8      extended_table: EgclVal — hash-table for non-ASCII chars
 32       8      macro_table: EgclVal — hash-table char→function for reader macros
 40       8      dispatch_table: EgclVal — hash-table char→(hash-table sub-char→function)
```

---

## 1.16  Type Lattice

The CL type hierarchy maps onto `EgclVal` tag + `type_id` as follows.
Type tests proceed from tag check (O(1)) to header type_id check (one
memory load). The lattice below shows the `SUBTYPEP` relationships
that the type system must implement.

```text
T
├── NUMBER
│   ├── REAL
│   │   ├── RATIONAL
│   │   │   ├── INTEGER
│   │   │   │   ├── FIXNUM          [tag 000]
│   │   │   │   └── BIGNUM          [type_id 0x08]
│   │   │   └── RATIO               [type_id 0x09]
│   │   └── FLOAT
│   │       ├── SINGLE-FLOAT        [tag 100]
│   │       └── DOUBLE-FLOAT        [type_id 0x0B]
│   └── COMPLEX                     [type_id 0x0A]
├── CHARACTER                        [tag 011]
├── SYMBOL                           [tag 101 ∪ special NIL/T]
├── CONS                             [tag 001]
├── ARRAY
│   ├── VECTOR
│   │   ├── SIMPLE-VECTOR            [type_id 0x03]
│   │   ├── STRING
│   │   │   ├── SIMPLE-BASE-STRING   [type_id 0x05]
│   │   │   └── SIMPLE-CHARACTER-STRING [type_id 0x06]
│   │   └── BIT-VECTOR               [type_id 0x04, elt=BIT]
│   └── COMPLEX-ARRAY                [type_id 0x07]
├── HASH-TABLE                       [type_id 0x0C]
├── FUNCTION
│   ├── INTERPRETED-FUNCTION         [type_id 0x0F]
│   ├── COMPILED-FUNCTION            [type_id 0x10]
│   └── CLOSURE                      [type_id 0x11]
├── STRUCTURE-OBJECT                 [type_id 0x0D]
├── STANDARD-OBJECT                  [type_id 0x0E]
├── CONDITION                        [type_id 0x16]
├── PACKAGE                          [type_id 0x12]
├── STREAM                           [type_id 0x13]
├── PATHNAME                         [type_id 0x14]
├── READTABLE                        [type_id 0x15]
└── RESTART                          [type_id 0x17]
```

`NULL` = `(EQL NIL)` = `(MEMBER NIL)`. `LIST` = `(OR CONS NULL)`.
`ATOM` = `(NOT CONS)`. `BOOLEAN` = `(MEMBER T NIL)`.

---

## 1.17  Type-Tag Check Sequences — A1.01

All type predicates compile to inline checks. The sequences below show
the logic; the compiler (§4) SHOULD emit these as branchless or
minimal-branch instruction sequences.

```rust
fn fixnump(v: EgclVal)   -> bool { v.0 & 0x7 == 0b000 }
fn consp(v: EgclVal)     -> bool { v.0 & 0x7 == 0b001 }
fn characterp(v: EgclVal)-> bool { v.0 & 0x7 == 0b011 }
fn single_float_p(v: EgclVal) -> bool { v.0 & 0x7 == 0b100 }
fn symbolp(v: EgclVal)   -> bool {
    let tag = v.0 & 0x7;
    tag == 0b101 || v.0 == NIL_BITS || v.0 == T_BITS
}
fn functionp(v: EgclVal) -> bool { v.0 & 0x7 == 0b110 }
fn nullp(v: EgclVal)     -> bool { v.0 == NIL_BITS }
fn listp(v: EgclVal)     -> bool { v.0 & 0x7 == 0b001 || v.0 == NIL_BITS }
fn heap_object_p(v: EgclVal) -> bool { v.0 & 0x7 == 0b010 }

// For heap objects, secondary dispatch on type_id:
fn type_id_of(v: EgclVal) -> u8 {
    let ptr = (v.0 & !0x7) as *const u64;
    (unsafe { *ptr } >> 56) as u8
}
fn stringp(v: EgclVal) -> bool {
    if !heap_object_p(v) { return false; }
    let tid = type_id_of(v);
    // Simple strings: direct type_id check
    if tid == 0x05 || tid == 0x06 { return true; }
    // Complex (adjustable/displaced) strings: a COMPLEX-ARRAY whose
    // element_type_tag indicates character or base-char elements.
    if tid == 0x07 {
        let ptr = (v.0 & !0x7) as *const u8;
        let elt_tag = unsafe { *ptr.add(33) }; // rank is at offset 33
        // Actually: element_type_tag is stored on the underlying simple
        // array. Dereference the `underlying` field (offset 8) and read
        // its element_type_tag (offset 17).
        let underlying = unsafe { *((ptr.add(8)) as *const u64) };
        let und_ptr = (underlying & !0x7) as *const u8;
        let elt = unsafe { *und_ptr.add(17) };  // element_type_tag at offset 17
        return elt == 12 || elt == 13; // CHARACTER or BASE-CHAR
    }
    false
}

// VECTORP: rank-1 arrays (simple-vector, simple strings, simple
// specialised arrays with rank=1, or complex arrays with rank=1).
fn vectorp(v: EgclVal) -> bool {
    if !heap_object_p(v) { return false; }
    let tid = type_id_of(v);
    // Simple-vector and simple strings are always vectors (rank 1)
    if matches!(tid, 0x03 | 0x05 | 0x06) { return true; }
    // Simple specialised array: check rank == 1
    if tid == 0x04 {
        let ptr = (v.0 & !0x7) as *const u8;
        return unsafe { *ptr.add(16) } == 1; // rank at offset 16
    }
    // Complex array: check rank == 1
    if tid == 0x07 {
        let ptr = (v.0 & !0x7) as *const u8;
        return unsafe { *ptr.add(33) } == 1; // rank at offset 33
    }
    false
}

// ARRAYP: any array type
fn arrayp(v: EgclVal) -> bool {
    heap_object_p(v) && matches!(type_id_of(v), 0x03 | 0x04 | 0x05 | 0x06 | 0x07)
}

// BIT-VECTOR-P: rank-1 array with BIT element type
fn bit_vector_p(v: EgclVal) -> bool {
    if !heap_object_p(v) { return false; }
    let tid = type_id_of(v);
    if tid == 0x04 {
        let ptr = (v.0 & !0x7) as *const u8;
        let rank = unsafe { *ptr.add(16) };
        let elt = unsafe { *ptr.add(17) };
        return rank == 1 && elt == 1; // rank 1, element_type_tag BIT
    }
    if tid == 0x07 {
        // Complex array: must be rank 1 with BIT underlying
        let ptr = (v.0 & !0x7) as *const u8;
        let rank = unsafe { *ptr.add(33) };
        if rank != 1 { return false; }
        let underlying = unsafe { *((ptr.add(8)) as *const u64) };
        let und_ptr = (underlying & !0x7) as *const u8;
        let elt = unsafe { *und_ptr.add(17) };
        return elt == 1; // BIT
    }
    false
}
```

**Note on compound type predicates.** ANSI CL requires `STRINGP`,
`VECTORP`, `BIT-VECTOR-P`, and `ARRAYP` to return `T` for both simple
and complex (adjustable/displaced) variants. The predicates above
handle this by checking type_id `0x07` (COMPLEX-ARRAY) and inspecting
the `rank` and/or the underlying array's `element_type_tag`. The
compiler (§4) SHOULD emit specialised inline sequences for these common
predicates rather than falling through to generic `TYPEP`.

`TYPEP` for compound types (`(AND ...)`, `(OR ...)`, `(SATISFIES ...)`)
is expanded by the compiler into compositions of these primitives (§4).

---

## 1.18  NIL Representation

`NIL` is the most polymorphic value in CL. EGCL encodes it as a
special-tag immediate (R1.11: bit pattern `0x07`) but also maintains a
full `SymbolData` entry at symbol-table index 0.

This means:
- `SYMBOLP` checks accept `NIL` via the special-tag fast path.
- `LISTP` checks accept `NIL` via explicit comparison (`v == NIL_BITS`).
- `(CAR NIL)` → `NIL` and `(CDR NIL)` → `NIL` are handled as special
  cases in the `CAR`/`CDR` implementation (check for NIL before
  dereferencing; the cost is one comparison, which branch prediction
  handles well on real workloads).
- `(SYMBOL-NAME NIL)` → `"NIL"` fetches from the symbol table entry.

---

## 1.19  Error Handling

All type-mismatch errors (e.g., `(CAR 42)`) MUST signal a `TYPE-ERROR`
condition with `:datum` and `:expected-type` slots filled. In Rust
bootstrap code this is `Err(EgclError::TypeError { datum, expected })`,
which the condition-system bridge converts to a CL condition (§5).

Header-corruption detected during GC or type dispatch (e.g., `type_id`
out of range) MUST trigger an internal panic with a diagnostic message
including the corrupt header value and the object address. This is a
non-recoverable runtime error.

---

## 1.20  Concurrency

- **Object headers:** GC bits are modified via atomic compare-and-swap
  (CAS). Mark bits use relaxed ordering during concurrent mark; the
  forwarding bit uses acquire/release (§3).
- **Symbol value cells:** Reads use `Acquire` load; writes use `Release`
  store. Special-variable push/pop on TLS is thread-local (no
  synchronisation needed).
- **Hash-table buckets:** Synchronized tables acquire a per-table
  read-write lock. Unsynchronized tables have undefined behaviour
  under concurrent mutation (matching SBCL).

---

## 1.21  Configuration

| Knob | Default | Description |
|------|---------|-------------|
| `EGCL_SYMBOL_TABLE_INIT` | 8192 | Initial symbol table capacity |
| `EGCL_HASH_TABLE_DEFAULT_SIZE` | 16 | Default bucket count for `MAKE-HASH-TABLE` |
| `EGCL_CONS_PAGE_SIZE` | 2 MiB | Size of cons-only allocation pages |
| `EGCL_LARGE_OBJECT_THRESHOLD` | `region_size / 2` (1 MB) | Objects above this go to large-object regions (§3.2.5). Derived from region size; not directly settable. |

---

## 1.22  Test Strategy

1. **Unit tests** (`tests/unit/object_test.rs`): Encode/decode round-trip
   for every immediate type. Construct and inspect every heap layout.
   Verify header field packing/unpacking.
2. **Property tests** (proptest): Random `EgclVal` → encode → decode
   preserves value. Random fixnum arithmetic detects overflow to bignum.
3. **Type-predicate exhaustiveness**: For each type predicate, test true
   and false cases against every other type tag and type_id.
4. **NIL polymorphism**: `(SYMBOLP NIL)`, `(LISTP NIL)`, `(CAR NIL)`,
   `(CDR NIL)`, `(NULL NIL)` all return expected values.
5. **ANSI conformance**: `ansi-test` section 4 (types) and section 14
   (conses) provide regression coverage.
