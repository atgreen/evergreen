//! Object header and heap object layouts.
//!
//! Every heap-allocated object begins with an 8-byte `ObjectHeader`.
//! Cons cells are the sole exception — they are headerless (16 bytes).
//! See §1.3 of the spec.

use crate::value::TorclVal;

// ── ObjectHeader ───────────────────────────────────────────────────

/// 8-byte header at the start of every heap-allocated object (except cons cells).
///
/// Layout:
/// - bits 63:56 — `type_id` (8 bits)
/// - bits 55:48 — `gc_bits` (8 bits)
/// - bits 47:16 — `hash` (32 bits, lazily computed)
/// - bits 15:0  — `size` in 8-byte units (16 bits)
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct ObjectHeader(pub u64);

// Bit-field masks and shifts
const TYPE_ID_SHIFT: u32 = 56;
const TYPE_ID_MASK: u64 = 0xFF << TYPE_ID_SHIFT;
const GC_BITS_SHIFT: u32 = 48;
const GC_BITS_MASK: u64 = 0xFF << GC_BITS_SHIFT;
const HASH_SHIFT: u32 = 16;
const HASH_MASK: u64 = 0xFFFF_FFFF << HASH_SHIFT;
const SIZE_MASK: u64 = 0xFFFF;

impl ObjectHeader {
    /// Create a new object header.
    pub fn new(type_id: u8, size_units: u16) -> Self {
        let val = ((type_id as u64) << TYPE_ID_SHIFT) | (size_units as u64);
        ObjectHeader(val)
    }

    /// Extract the type ID (bits 63:56).
    pub fn type_id(self) -> u8 {
        ((self.0 & TYPE_ID_MASK) >> TYPE_ID_SHIFT) as u8
    }

    /// Extract the GC bits (bits 55:48).
    pub fn gc_bits(self) -> u8 {
        ((self.0 & GC_BITS_MASK) >> GC_BITS_SHIFT) as u8
    }

    /// Set GC bits (bits 55:48).
    pub fn set_gc_bits(&mut self, bits: u8) {
        self.0 = (self.0 & !GC_BITS_MASK) | ((bits as u64) << GC_BITS_SHIFT);
    }

    /// Set the FORWARDED gc-bit (bit 53). Non-atomic; callers hold the heap
    /// lock during the stop-the-world evacuation phase that installs forwarding.
    /// The type_id and size fields are left intact so a forwarded object can
    /// still be strided over by the heap walker (spec §1.3.2, R1.09).
    pub fn set_forwarded(&mut self) {
        let bits = self.gc_bits() | (1 << gc_bit::FORWARDED);
        self.set_gc_bits(bits);
    }

    /// Set the PINNED gc-bit (bit 52); the object must not be moved by the GC.
    pub fn set_pinned(&mut self) {
        let bits = self.gc_bits() | (1 << gc_bit::PINNED);
        self.set_gc_bits(bits);
    }

    /// Clear the PINNED gc-bit (bit 52); the object may be moved again.
    pub fn clear_pinned(&mut self) {
        let bits = self.gc_bits() & !(1 << gc_bit::PINNED);
        self.set_gc_bits(bits);
    }

    /// Extract the cached identity hash (bits 47:16). Zero means not yet computed.
    pub fn hash(self) -> u32 {
        ((self.0 & HASH_MASK) >> HASH_SHIFT) as u32
    }

    /// Set the cached identity hash (bits 47:16).
    pub fn set_hash(&mut self, hash: u32) {
        self.0 = (self.0 & !HASH_MASK) | ((hash as u64) << HASH_SHIFT);
    }

    /// Extract the size in 8-byte units (bits 15:0).
    /// `0xFFFF` signals the large-object extension (true size at offset 8).
    pub fn size_units(self) -> u16 {
        (self.0 & SIZE_MASK) as u16
    }

    /// Whether this is a large object (size field == 0xFFFF sentinel).
    pub fn is_large_object(self) -> bool {
        self.size_units() == 0xFFFF
    }
}

// ── GC bit accessors ───────────────────────────────────────────────

/// GC bit positions within the gc_bits byte.
pub mod gc_bit {
    pub const MARK: u8 = 7; // bit 55 — mark-white/black
    pub const GREY: u8 = 6; // bit 54 — in concurrent mark worklist
    pub const FORWARDED: u8 = 5; // bit 53 — object has been evacuated
    pub const PINNED: u8 = 4; // bit 52 — must not be moved
    pub const REMEMBERED: u8 = 3; // bit 51 — in remembered set
}

impl ObjectHeader {
    /// Get an atomic reference to the underlying u64 for CAS operations.
    /// This reinterprets `&self` as an atomic — valid because ObjectHeader
    /// is repr(transparent) over u64 and 8-byte aligned.
    fn as_atomic(&self) -> &std::sync::atomic::AtomicU64 {
        unsafe { &*((&self.0) as *const u64 as *const std::sync::atomic::AtomicU64) }
    }

    /// Atomically read mark bit.
    pub fn is_marked(&self) -> bool {
        let val = self.as_atomic().load(std::sync::atomic::Ordering::Acquire);
        let gc = ((val & GC_BITS_MASK) >> GC_BITS_SHIFT) as u8;
        (gc & (1 << gc_bit::MARK)) != 0
    }

    /// Atomically set mark bit via CAS.
    /// Returns true if the bit was successfully set (was previously unset).
    /// Returns false if the mark bit was already set.
    pub fn set_marked(&self) -> bool {
        let atomic = self.as_atomic();
        loop {
            let old = atomic.load(std::sync::atomic::Ordering::Acquire);
            let gc = ((old & GC_BITS_MASK) >> GC_BITS_SHIFT) as u8;
            if (gc & (1 << gc_bit::MARK)) != 0 {
                return false; // already marked
            }
            let new_gc = gc | (1 << gc_bit::MARK);
            let new_val = (old & !GC_BITS_MASK) | ((new_gc as u64) << GC_BITS_SHIFT);
            match atomic.compare_exchange_weak(
                old,
                new_val,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue,
            }
        }
    }

    /// Atomically check if object has been forwarded (evacuated by GC).
    pub fn is_forwarded(&self) -> bool {
        let val = self.as_atomic().load(std::sync::atomic::Ordering::Acquire);
        let gc = ((val & GC_BITS_MASK) >> GC_BITS_SHIFT) as u8;
        (gc & (1 << gc_bit::FORWARDED)) != 0
    }

    /// Atomically check if object is pinned (must not be moved by GC).
    pub fn is_pinned(&self) -> bool {
        let val = self.as_atomic().load(std::sync::atomic::Ordering::Acquire);
        let gc = ((val & GC_BITS_MASK) >> GC_BITS_SHIFT) as u8;
        (gc & (1 << gc_bit::PINNED)) != 0
    }
}

// ── Heap type IDs ──────────────────────────────────────────────────

/// Discriminator values stored in the `type_id` field of `ObjectHeader`.
pub mod type_id {
    pub const CONS: u8 = 0x01;
    pub const SYMBOL: u8 = 0x02;
    pub const SIMPLE_VECTOR: u8 = 0x03;
    pub const SIMPLE_ARRAY: u8 = 0x04;
    pub const SIMPLE_BASE_STRING: u8 = 0x05;
    pub const SIMPLE_CHARACTER_STRING: u8 = 0x06;
    pub const COMPLEX_ARRAY: u8 = 0x07;
    pub const BIGNUM: u8 = 0x08;
    pub const RATIO: u8 = 0x09;
    pub const COMPLEX: u8 = 0x0A;
    pub const DOUBLE_FLOAT: u8 = 0x0B;
    pub const HASH_TABLE: u8 = 0x0C;
    pub const STRUCTURE: u8 = 0x0D;
    pub const STANDARD_OBJECT: u8 = 0x0E;
    pub const FUNCTION_INTERPRETED: u8 = 0x0F;
    pub const COMPILED_FUNCTION: u8 = 0x10;
    pub const CLOSURE: u8 = 0x11;
    pub const PACKAGE: u8 = 0x12;
    pub const STREAM: u8 = 0x13;
    pub const PATHNAME: u8 = 0x14;
    pub const READTABLE: u8 = 0x15;
    pub const CONDITION: u8 = 0x16;
    pub const RESTART: u8 = 0x17;
    /// Multidimensional (rank ≥ 2) array. Body = [storage-ref | dims-ref |
    /// rank]; storage is a row-major SIMPLE_VECTOR of ARRAY-TOTAL-SIZE, dims is
    /// a SIMPLE_VECTOR of the fixnum dimensions. Both are heap references the GC
    /// traces; rank is an immediate fixnum. Rank-1 arrays stay plain vectors.
    pub const MD_ARRAY: u8 = 0x18;
    /// Opaque mutex handle: one untraced word owning a process-local Arc.
    pub const MUTEX: u8 = 0x19;
    /// Opaque condition-variable handle: one untraced process-local Arc.
    pub const CONDITION_VARIABLE: u8 = 0x1A;
    /// Foreign address and allocation identity: two untraced native words.
    pub const FOREIGN_POINTER: u8 = 0x1B;
    /// Foreign library registry token: one untraced process-local word.
    pub const FOREIGN_LIBRARY: u8 = 0x1C;
}

// ── Simple string on-heap encoding (SBCL model, spec §1.6.3) ───────
//
// Layout: ObjectHeader(8) | char_len: u64 @8 | data @16. The element width is
// fixed at construction and encoded by the `type_id`: SIMPLE_BASE_STRING (0x05)
// = 1 byte/char (BASE-CHAR, code points < 256), SIMPLE_CHARACTER_STRING (0x06)
// = 4 bytes/char (CHARACTER, native-endian u32, full range). CHAR/SCHAR/AREF
// are O(1). There is no coder promotion; growable strings use complex arrays.

/// Bytes per character for a simple-string `type_id` (base = 1, character = 4).
#[inline]
pub fn simple_string_width(tid: u8) -> usize {
    if tid == type_id::SIMPLE_CHARACTER_STRING {
        4
    } else {
        1
    }
}

/// Padded byte size of a simple string: `header(8) + char_len(8) + data`, where
/// `data` is `char_len * width` bytes, rounded up to an 8-byte boundary.
#[inline]
pub fn simple_string_size(char_len: usize, width: usize) -> usize {
    (16 + char_len * width + 7) & !7
}

/// Padded size to hold `s` as a 32-bit `SIMPLE_CHARACTER_STRING` — the default
/// element type for constructed strings (holds any code point, mutable).
#[inline]
pub fn character_string_alloc_size(s: &str) -> usize {
    simple_string_size(s.chars().count(), 4)
}

/// Whether every character of `s` is a `BASE-CHAR` (code point < 256), i.e.
/// whether `s` fits an 8-bit `SIMPLE_BASE_STRING`.
#[inline]
pub fn is_all_base_char(s: &str) -> bool {
    s.chars().all(|c| (c as u32) < 256)
}

/// The `type_id` and padded byte size for the NARROWEST simple string that holds
/// `s`: `SIMPLE_BASE_STRING` (1 byte/char) when every code point < 256, else
/// `SIMPLE_CHARACTER_STRING` (4 bytes/char). For strings that are safe to store
/// compactly — reader literals and other immutable strings; a MUTABLE string
/// must use [`character_string_alloc_size`] / [`write_character_string`] so that
/// `(SETF CHAR)` of any code point stays legal (a base string can only hold
/// base-chars).
#[inline]
pub fn narrowest_string_alloc(s: &str) -> (u8, usize) {
    let n = s.chars().count();
    if is_all_base_char(s) {
        (type_id::SIMPLE_BASE_STRING, simple_string_size(n, 1))
    } else {
        (type_id::SIMPLE_CHARACTER_STRING, simple_string_size(n, 4))
    }
}

/// Write `s` into a zeroed, 8-aligned allocation at the narrowest width — an
/// 8-bit `SIMPLE_BASE_STRING` when every code point < 256, else a 32-bit
/// `SIMPLE_CHARACTER_STRING`. For immutable/compactable strings only.
///
/// # Safety
/// `ptr` must point to a zeroed, 8-byte-aligned allocation of at least
/// `narrowest_string_alloc(s).1` bytes that outlives the returned object.
pub unsafe fn write_narrowest_string(ptr: *mut u8, s: &str) {
    let (tid, padded) = narrowest_string_alloc(s);
    let char_len = s.chars().count();
    unsafe {
        *(ptr as *mut ObjectHeader) = ObjectHeader::new(tid, (padded / 8) as u16);
        *((ptr as *mut u64).add(1)) = char_len as u64;
        let data = ptr.add(16);
        if tid == type_id::SIMPLE_CHARACTER_STRING {
            let d = data as *mut u32;
            for (i, c) in s.chars().enumerate() {
                *d.add(i) = c as u32;
            }
        } else {
            for (i, c) in s.chars().enumerate() {
                *data.add(i) = c as u8;
            }
        }
    }
}

/// Write a 32-bit `SIMPLE_CHARACTER_STRING` for `s` into a zeroed, 8-aligned
/// allocation of at least [`character_string_alloc_size`]`(s)` bytes. The single
/// write choke point for constructed strings; `read_simple_string` /
/// `simple_string_char_at` are the matching read choke points.
///
/// # Safety
/// `ptr` must point to a zeroed, 8-byte-aligned allocation of at least
/// `character_string_alloc_size(s)` bytes that outlives the returned object.
pub unsafe fn write_character_string(ptr: *mut u8, s: &str) {
    let char_len = s.chars().count();
    let padded = simple_string_size(char_len, 4);
    unsafe {
        *(ptr as *mut ObjectHeader) =
            ObjectHeader::new(type_id::SIMPLE_CHARACTER_STRING, (padded / 8) as u16);
        *((ptr as *mut u64).add(1)) = char_len as u64;
        let data = ptr.add(16) as *mut u32;
        for (i, c) in s.chars().enumerate() {
            *data.add(i) = c as u32;
        }
    }
}

/// The payload offset of a simple-string object: 8 for a small object, 16 for
/// a large one, whose 16-byte header carries a size-extension word at +8
/// (bliss-31x8, mirroring the bliss-tjru vector fix). The length word begins
/// the payload; character data follows it.
///
/// # Safety
/// `ptr` must point at a live object header.
#[inline]
unsafe fn simple_string_payload_offset(ptr: *const u8) -> usize {
    if unsafe { *(ptr as *const ObjectHeader) }.is_large_object() {
        16
    } else {
        8
    }
}

/// The character (not byte) count of a simple string.
///
/// # Safety
/// `ptr` must be a live `SIMPLE_BASE_STRING`/`SIMPLE_CHARACTER_STRING` object.
#[inline]
pub unsafe fn simple_string_char_count(ptr: *const u8) -> usize {
    unsafe { *(ptr.add(simple_string_payload_offset(ptr)) as *const u64) as usize }
}

/// The character at `index` of a simple string, or `None` if out of range —
/// O(1). Width is taken from the `type_id`.
///
/// # Safety
/// `ptr` must be a live simple-string object.
#[inline]
pub unsafe fn simple_string_char_at(ptr: *const u8, index: usize) -> Option<char> {
    unsafe {
        if index >= simple_string_char_count(ptr) {
            return None;
        }
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        let data = ptr.add(simple_string_payload_offset(ptr) + 8);
        let cp = if tid == type_id::SIMPLE_CHARACTER_STRING {
            *(data.add(index * 4) as *const u32)
        } else {
            *data.add(index) as u32
        };
        char::from_u32(cp)
    }
}

/// Store `ch` at `index` of a simple string, in place — O(1). Returns `false`
/// (no write) if `index` is out of range, or if `ch` ≥ 256 does not fit a
/// `SIMPLE_BASE_STRING` (the caller raises a TYPE-ERROR; a base string never
/// promotes).
///
/// # Safety
/// `ptr` must be a live simple-string object.
#[inline]
pub unsafe fn simple_string_set_char(ptr: *mut u8, index: usize, ch: char) -> bool {
    unsafe {
        if index >= simple_string_char_count(ptr) {
            return false;
        }
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        let data = ptr.add(simple_string_payload_offset(ptr) + 8);
        if tid == type_id::SIMPLE_CHARACTER_STRING {
            *(data.add(index * 4) as *mut u32) = ch as u32;
            true
        } else if (ch as u32) < 256 {
            *data.add(index) = ch as u8;
            true
        } else {
            false
        }
    }
}

/// Decode a simple string into an owned Rust `String`. Width from `type_id`.
///
/// # Safety
/// `ptr` must be a live simple-string object.
pub unsafe fn read_simple_string(ptr: *const u8) -> String {
    unsafe {
        let len = simple_string_char_count(ptr);
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        let data = ptr.add(simple_string_payload_offset(ptr) + 8);
        let mut out = String::with_capacity(len);
        if tid == type_id::SIMPLE_CHARACTER_STRING {
            for i in 0..len {
                let cp = *(data.add(i * 4) as *const u32);
                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
        } else {
            for i in 0..len {
                out.push(*data.add(i) as char);
            }
        }
        out
    }
}

// ── Package ────────────────────────────────────────────────────────

/// Heap layout for a package (56 bytes total). §1.12, D1.20.
///
/// The `internal_symbols`/`external_symbols` cells hold hash-tables (string →
/// symbol) in the full model; a package built before that machinery is wired
/// leaves them `NIL`. `lock` is a raw pointer to a heap-allocated per-package
/// reader-writer lock, or null.
#[repr(C)]
pub struct PackageData {
    pub header: ObjectHeader,
    pub name: TorclVal,
    pub internal_symbols: TorclVal,
    pub external_symbols: TorclVal,
    pub use_list: TorclVal,
    pub nicknames: TorclVal,
    pub lock: *mut core::ffi::c_void,
}

// ── Interpreted function (D1.17 + FnMeta §4.4.3) ────────────────────

/// Heap layout for an interpreted function object.
///
/// The first four fields are the §1.11.1 (D1.17) interpreted-function layout and
/// are the *only* GC references (see `trace_object`). The trailing fields are the
/// per-function tiering metadata (`FnMeta`, §4.4.3) — the substrate a HotSpot-
/// style engine hangs invocation/back-edge counters, the active entry point, the
/// current tier, and flags on. They are plain atomics, lock-free readable
/// (R4.53), and never traced/relocated, so the object is pinned for a stable
/// identity and stable metadata address across redefinition and tier changes.
#[repr(C)]
pub struct FunctionData {
    pub header: ObjectHeader,
    pub lambda_list: TorclVal,
    pub body: TorclVal,
    pub env: TorclVal,
    pub name: TorclVal,
    /// Incremented by the T0 eval loop / T1 prologue on each call.
    pub invoke_count: core::sync::atomic::AtomicU32,
    /// Incremented by T1 back-edge stubs.
    pub back_edge_count: core::sync::atomic::AtomicU32,
    /// Active entry point; updated atomically on a tier change (null at T0).
    pub entry: core::sync::atomic::AtomicPtr<u8>,
    /// Current tier: 0 (T0 interpreter), 1 (baseline), 2 (optimised).
    pub tier: core::sync::atomic::AtomicU8,
    /// FnMeta flags (QUEUED_FOR_T2, T2_FAILED, NEVER_COMPILE, …).
    pub flags: core::sync::atomic::AtomicU16,
}

// ── Cons cell ──────────────────────────────────────────────────────

/// A headerless 16-byte cons cell (car + cdr).
#[repr(C)]
pub struct ConsCell {
    pub car: TorclVal,
    pub cdr: TorclVal,
}

// ── Symbol ─────────────────────────────────────────────────────────

/// Heap layout for a symbol (56 bytes total). §1.7.
#[repr(C)]
pub struct SymbolData {
    pub header: ObjectHeader,
    pub name: TorclVal,
    pub value: TorclVal,
    pub function: TorclVal,
    pub plist: TorclVal,
    pub package: TorclVal,
    pub flags: u32,
    pub tls_index: u32,
}

/// Symbol flag bits.
pub mod symbol_flags {
    pub const CONSTANT: u32 = 1 << 0;
    pub const SPECIAL: u32 = 1 << 1;
    pub const MACRO: u32 = 1 << 2;
    pub const COMPILER_MACRO: u32 = 1 << 3;
}

// ── Arrays ─────────────────────────────────────────────────────────

/// Element type tag for specialised arrays (§1.6.2).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementTypeTag {
    General = 0,
    Bit = 1,
    U8 = 2,
    U16 = 3,
    U32 = 4,
    U64 = 5,
    I8 = 6,
    I16 = 7,
    I32 = 8,
    I64 = 9,
    SingleFloat = 10,
    DoubleFloat = 11,
    Character = 12,
    BaseChar = 13,
}

// ── Numeric heap types ─────────────────────────────────────────────

/// Bignum header (followed by limbs). §1.8.1.
#[repr(C)]
pub struct BignumHeader {
    pub header: ObjectHeader,
    pub sign: i32,
    pub n_limbs: u32,
    // followed by limbs: [u64; n_limbs]
}

/// Ratio (numerator/denominator). §1.8.2.
#[repr(C)]
pub struct RatioData {
    pub header: ObjectHeader,
    pub numerator: TorclVal,
    pub denominator: TorclVal,
}

/// Complex number. §1.8.3.
#[repr(C)]
pub struct ComplexData {
    pub header: ObjectHeader,
    pub realpart: TorclVal,
    pub imagpart: TorclVal,
}

/// Multidimensional array (rank ≥ 2). `storage` is a row-major SIMPLE_VECTOR of
/// ARRAY-TOTAL-SIZE elements; `dims` is a SIMPLE_VECTOR of the fixnum
/// dimensions; `rank` is an immediate fixnum. Both vector fields are GC-traced.
#[repr(C)]
pub struct MdArrayData {
    pub header: ObjectHeader,
    pub storage: TorclVal,
    pub dims: TorclVal,
    pub rank: TorclVal,
}

/// Heap-allocated double-float. §1.8.4.
#[repr(C)]
pub struct DoubleFloatData {
    pub header: ObjectHeader,
    pub value: f64,
}

// ── Function layouts ───────────────────────────────────────────────

/// Interpreted function. §1.11.1.
#[repr(C)]
pub struct InterpretedFunctionData {
    pub header: ObjectHeader,
    pub lambda_list: TorclVal,
    pub body: TorclVal,
    pub env: TorclVal,
    pub name: TorclVal,
}

/// Compiled function. §1.11.2.
#[repr(C)]
pub struct CompiledFunctionData {
    pub header: ObjectHeader,
    pub entry_point: *const u8,
    pub code_size: u64,
    pub name: TorclVal,
    pub lambda_list: TorclVal,
    pub min_args: u16,
    pub max_args: u16,
    pub tier: u8,
    pub _pad: [u8; 3],
    pub constants: TorclVal,
}

/// Closure. §1.11.3.
#[repr(C)]
pub struct ClosureData {
    pub header: ObjectHeader,
    pub function: TorclVal,
    // followed by closed_vars: [TorclVal; N]
}

// ── Stream ─────────────────────────────────────────────────────────

/// Stream object. §1.13.
#[repr(C)]
pub struct StreamData {
    pub header: ObjectHeader,
    pub direction: u8,
    pub element_type: u8,
    pub _pad: [u8; 6],
    pub ops: *const (),
    pub state: *mut u8,
    pub column: u64,
}

/// Stream direction values.
pub mod stream_direction {
    pub const INPUT: u8 = 0;
    pub const OUTPUT: u8 = 1;
    pub const IO: u8 = 2;
}

// ── Pathname ───────────────────────────────────────────────────────

/// Pathname object. §1.14.
#[repr(C)]
pub struct PathnameData {
    pub header: ObjectHeader,
    pub host: TorclVal,
    pub device: TorclVal,
    pub directory: TorclVal,
    pub name: TorclVal,
    pub type_field: TorclVal,
    pub version: TorclVal,
}

// ── Readtable ──────────────────────────────────────────────────────

/// Readtable object. §1.15.
#[repr(C)]
pub struct ReadtableData {
    pub header: ObjectHeader,
    pub case_mode: u8,
    pub _pad: [u8; 7],
    pub char_table: TorclVal,
    pub extended_table: TorclVal,
    pub macro_table: TorclVal,
    pub dispatch_table: TorclVal,
}

// ── Restart ────────────────────────────────────────────────────────

/// Restart object. §1.16.
#[repr(C)]
pub struct RestartData {
    pub header: ObjectHeader,
    pub name: TorclVal,
    pub function: TorclVal,
    pub report_function: TorclVal,
    pub interactive_function: TorclVal,
    pub test_function: TorclVal,
}
