//! Tagged value representation — `BlissVal`.
//!
//! Every Common Lisp value in Bliss is a 64-bit tagged word.
//! See §1.2 of the spec for the encoding.

/// The universal value type — a 64-bit tagged word.
///
/// Tag encoding (low 3 bits):
/// - `000` — Fixnum (61-bit signed integer)
/// - `001` — Cons pointer (8-byte aligned)
/// - `010` — Heap object pointer (→ ObjectHeader)
/// - `011` — Character (21-bit Unicode codepoint)
/// - `100` — Single-float (IEEE 754 binary32 in bits 63:32)
/// - `101` — Symbol index (32-bit index into global symbol table)
/// - `110` — Function pointer (→ function header)
/// - `111` — Special (NIL, T, UNBOUND, MISSING, EOF)
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BlissVal(pub u64);

// ── Tag constants ──────────────────────────────────────────────────

pub const TAG_FIXNUM: u64 = 0b000;
pub const TAG_CONS: u64 = 0b001;
pub const TAG_HEAP_OBJECT: u64 = 0b010;
pub const TAG_CHARACTER: u64 = 0b011;
pub const TAG_SINGLE_FLOAT: u64 = 0b100;
pub const TAG_SYMBOL: u64 = 0b101;
pub const TAG_FUNCTION: u64 = 0b110;
pub const TAG_SPECIAL: u64 = 0b111;

pub const TAG_MASK: u64 = 0b111;

/// Payload bit marking an opaque metaobject handle (CLOS class/generic-function/
/// method id, effective-method key) under the `SPECIAL` tag. Keeps these
/// internal registry keys OFF the fixnum tag so a plain integer equal to one
/// cannot collide with it in a registry. Builtin `SPECIAL` values
/// (NIL/T/UNBOUND/MISSING/EOF) never set it. Handle ids are small positive
/// counters, so they never reach this bit. See [`BlissVal::from_meta_handle`]
/// and issue bliss-dx6.
pub const META_HANDLE_BIT: u64 = 1 << 62;

/// Payload bit marking an opaque macro-function-registry handle under the
/// `SPECIAL` tag. Same rationale as [`META_HANDLE_BIT`] (bliss-dx6) but for the
/// macro-expander registry (bliss-skx): macro keys were minted as bare fixnums
/// from a counter, so a symbol-macro expansion or any literal whose value
/// aliased a live key's integer was misinterpreted as a macro handle and the
/// wrong expander invoked (same family as bliss-6b2). Encoding macro keys off
/// the fixnum tag — and on a bit DISTINCT from `META_HANDLE_BIT` so they also
/// can't alias CLOS metaobject handles — makes the conflation structurally
/// impossible. Handle ids are small positive counters (well within 58 bits),
/// so they never reach this bit. See [`BlissVal::from_macro_handle`].
pub const MACRO_HANDLE_BIT: u64 = 1 << 61;

// ── Special-value constants ────────────────────────────────────────

pub const NIL_BITS: u64 = 0x0000_0000_0000_0007; // tag 111, payload 0
pub const T_BITS: u64 = 0x0000_0000_0000_000F; // tag 111, payload 1
pub const UNBOUND_BITS: u64 = 0x0000_0000_0000_0017; // tag 111, payload 2
pub const MISSING_BITS: u64 = 0x0000_0000_0000_001F; // tag 111, payload 3
pub const EOF_BITS: u64 = 0x0000_0000_0000_0027; // tag 111, payload 4

pub const NIL: BlissVal = BlissVal(NIL_BITS);
pub const T: BlissVal = BlissVal(T_BITS);
pub const UNBOUND: BlissVal = BlissVal(UNBOUND_BITS);
pub const MISSING: BlissVal = BlissVal(MISSING_BITS);
pub const EOF: BlissVal = BlissVal(EOF_BITS);

// ── Construction ───────────────────────────────────────────────────

impl BlissVal {
    /// Create a fixnum value from a 61-bit signed integer.
    /// The value is left-shifted by 3 bits; tag bits are 000.
    #[inline(always)]
    pub fn from_fixnum(n: i64) -> Self {
        // Left-shift by 3; tag is 000 so no OR needed.
        // The cast to u64 preserves the bit pattern for negative numbers.
        BlissVal(((n << 3) as u64) | TAG_FIXNUM)
    }

    /// Create a character value from a Unicode codepoint.
    pub fn from_char(c: char) -> Self {
        BlissVal(((c as u64) << 3) | TAG_CHARACTER)
    }

    /// Create a single-float immediate from an f32.
    /// The f32 bits are stored in bits 63:32, tag in bits 2:0.
    pub fn from_single_float(f: f32) -> Self {
        BlissVal(((f.to_bits() as u64) << 32) | TAG_SINGLE_FLOAT)
    }

    /// Create a symbol-index value.
    pub fn from_symbol_index(idx: u32) -> Self {
        BlissVal(((idx as u64) << 3) | TAG_SYMBOL)
    }

    /// Encode an opaque metaobject handle id (CLOS class/gf/method id, or an
    /// effective-method key) as a `SPECIAL`-tagged immediate with
    /// [`META_HANDLE_BIT`] set. These are internal registry keys; encoding them
    /// off the fixnum tag prevents a plain integer from colliding with one.
    /// `id` must be a small non-negative counter (well within 58 bits). See
    /// issue bliss-dx6.
    #[inline(always)]
    pub fn from_meta_handle(id: i64) -> Self {
        debug_assert!(id >= 0 && (id as u64) < (1 << 58));
        BlissVal(((id as u64) << 3) | TAG_SPECIAL | META_HANDLE_BIT)
    }

    /// True if this value is an opaque metaobject handle.
    #[inline(always)]
    pub fn is_meta_handle(self) -> bool {
        self.tag() == TAG_SPECIAL && (self.0 & META_HANDLE_BIT) != 0
    }

    /// Extract the id from a metaobject handle. Panics if not a handle.
    pub fn as_meta_handle_id(self) -> i64 {
        assert!(
            self.is_meta_handle(),
            "as_meta_handle_id called on non-handle value"
        );
        ((self.0 & !META_HANDLE_BIT) >> 3) as i64
    }

    /// Encode an opaque macro-function-registry handle id as a `SPECIAL`-tagged
    /// immediate with [`MACRO_HANDLE_BIT`] set. Keeps macro keys off the fixnum
    /// tag (and off `META_HANDLE_BIT`) so no ordinary literal — nor a CLOS
    /// metaobject handle — can alias one in `MACRO_FUNCTION_REGISTRY`
    /// (bliss-skx). `id` must be a small non-negative counter (within 58 bits).
    #[inline(always)]
    pub fn from_macro_handle(id: i64) -> Self {
        debug_assert!(id >= 0 && (id as u64) < (1 << 58));
        BlissVal(((id as u64) << 3) | TAG_SPECIAL | MACRO_HANDLE_BIT)
    }

    /// True if this value is an opaque macro-function-registry handle.
    #[inline(always)]
    pub fn is_macro_handle(self) -> bool {
        self.tag() == TAG_SPECIAL && (self.0 & MACRO_HANDLE_BIT) != 0
    }

    /// Create a cons-tagged pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid cons cell.
    pub unsafe fn from_cons_ptr(ptr: *mut u8) -> Self {
        BlissVal((ptr as u64) | TAG_CONS)
    }

    /// Create a heap-object-tagged pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid `ObjectHeader`.
    pub unsafe fn from_heap_ptr(ptr: *mut u8) -> Self {
        BlissVal((ptr as u64) | TAG_HEAP_OBJECT)
    }

    /// Create a function-tagged pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid function header.
    pub unsafe fn from_function_ptr(ptr: *mut u8) -> Self {
        BlissVal((ptr as u64) | TAG_FUNCTION)
    }
}

// ── Tag extraction ─────────────────────────────────────────────────

impl BlissVal {
    /// Extract the 3-bit tag.
    #[inline(always)]
    pub fn tag(self) -> u64 {
        self.0 & TAG_MASK
    }

    /// True if this value is a fixnum (tag `000`).
    #[inline(always)]
    pub fn is_fixnum(self) -> bool {
        self.tag() == TAG_FIXNUM
    }

    /// True if this value is a cons cell (tag `001`).
    #[inline(always)]
    pub fn is_cons(self) -> bool {
        self.tag() == TAG_CONS
    }

    /// True if this value is a general heap object (tag `010`).
    #[inline(always)]
    pub fn is_heap_object(self) -> bool {
        self.tag() == TAG_HEAP_OBJECT
    }

    /// True if this value is a character (tag `011`).
    #[inline(always)]
    pub fn is_character(self) -> bool {
        self.tag() == TAG_CHARACTER
    }

    /// True if this value is a single-float (tag `100`).
    #[inline(always)]
    pub fn is_single_float(self) -> bool {
        self.tag() == TAG_SINGLE_FLOAT
    }

    /// True if this value is a symbol (tag `101` or special NIL/T).
    #[inline(always)]
    pub fn is_symbol(self) -> bool {
        self.tag() == TAG_SYMBOL || self.0 == NIL_BITS || self.0 == T_BITS
    }

    /// True if this value is a function (tag `110`).
    #[inline(always)]
    pub fn is_function(self) -> bool {
        self.tag() == TAG_FUNCTION
    }

    /// True if this value is NIL.
    #[inline(always)]
    pub fn is_nil(self) -> bool {
        self.0 == NIL_BITS
    }

    /// True if this value is a list (cons or NIL).
    #[inline(always)]
    pub fn is_list(self) -> bool {
        self.is_cons() || self.is_nil()
    }

    /// True if this value is a string (a heap object whose type_id is
    /// `SIMPLE_BASE_STRING` or `SIMPLE_CHARACTER_STRING`).
    ///
    /// # Safety note
    /// This method dereferences the heap pointer to read the `ObjectHeader`.
    /// It is safe to call only when the underlying pointer is valid and
    /// points to a live object. Returns `false` for non-heap-object tags.
    pub fn is_string(self) -> bool {
        if self.tag() != TAG_HEAP_OBJECT {
            return false;
        }
        // SAFETY: caller guarantees the heap pointer is valid.
        unsafe {
            let ptr = self.as_ptr();
            let header = *(ptr as *const crate::object::ObjectHeader);
            let tid = header.type_id();
            tid == crate::object::type_id::SIMPLE_BASE_STRING
                || tid == crate::object::type_id::SIMPLE_CHARACTER_STRING
        }
    }

    /// True if this value is a CLOS standard-object instance (a heap object
    /// whose type_id is `STANDARD_OBJECT`). CLOS instances are heap objects
    /// laid out as `[ObjectHeader | wrapper ptr | inline slots]`.
    ///
    /// # Safety note
    /// Dereferences the heap pointer to read the `ObjectHeader`; safe only when
    /// the pointer is valid and live. Returns `false` for non-heap-object tags.
    pub fn is_standard_object(self) -> bool {
        if self.tag() != TAG_HEAP_OBJECT {
            return false;
        }
        // SAFETY: caller guarantees the heap pointer is valid.
        unsafe {
            let ptr = self.as_ptr();
            let header = *(ptr as *const crate::object::ObjectHeader);
            header.type_id() == crate::object::type_id::STANDARD_OBJECT
        }
    }
}

// ── Extraction ─────────────────────────────────────────────────────

impl BlissVal {
    /// Extract the fixnum value. Panics if not a fixnum.
    pub fn as_fixnum(self) -> i64 {
        assert!(self.is_fixnum(), "as_fixnum called on non-fixnum value");
        // Arithmetic right shift to sign-extend
        (self.0 as i64) >> 3
    }

    /// Extract the character value. Panics if not a character.
    pub fn as_char(self) -> char {
        assert!(self.is_character(), "as_char called on non-character value");
        let codepoint = (self.0 >> 3) as u32;
        char::from_u32(codepoint).expect("invalid Unicode codepoint in character value")
    }

    /// Extract the single-float value. Panics if not a single-float.
    pub fn as_single_float(self) -> f32 {
        assert!(
            self.is_single_float(),
            "as_single_float called on non-single-float value"
        );
        let bits = (self.0 >> 32) as u32;
        f32::from_bits(bits)
    }

    /// Extract the symbol table index. Panics if not a symbol.
    pub fn as_symbol_index(self) -> u32 {
        assert!(
            self.tag() == TAG_SYMBOL,
            "as_symbol_index called on non-symbol value"
        );
        (self.0 >> 3) as u32
    }

    /// The symbol table index if this is a *real* symbol (`TAG_SYMBOL`), else
    /// `None`. Unlike [`as_symbol_index`], this is safe for the special NIL/T
    /// constants — which [`is_symbol`] reports as symbols but which have no
    /// symbol-table index — so callers that may receive NIL/T (e.g. a debugger
    /// backtrace where a foreign frame is marked with `T`) don't panic.
    #[inline]
    pub fn symbol_index(self) -> Option<u32> {
        (self.tag() == TAG_SYMBOL).then(|| (self.0 >> 3) as u32)
    }

    /// Extract a heap string as a Rust `String`.
    ///
    /// Bliss currently stores simple strings as an object header, a u64 byte
    /// length, and then UTF-8 bytes starting at offset 16.
    pub fn as_string(self) -> String {
        assert!(self.is_string(), "as_string called on non-string value");
        unsafe { crate::object::read_simple_string(self.as_ptr()) }
    }

    /// Extract the raw pointer (mask off tag bits).
    ///
    /// # Safety
    /// Caller must ensure the tag is a pointer tag (001, 010, or 110).
    pub unsafe fn as_ptr(self) -> *mut u8 {
        (self.0 & !TAG_MASK) as *mut u8
    }

    /// Pass this value across FFI as a raw u64.
    #[inline(always)]
    pub fn to_raw(self) -> u64 {
        self.0
    }

    /// Construct from a raw u64 (e.g. received from FFI).
    #[inline(always)]
    pub fn from_raw(raw: u64) -> Self {
        BlissVal(raw)
    }
}

impl core::fmt::Debug for BlissVal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            NIL_BITS => write!(f, "NIL"),
            T_BITS => write!(f, "T"),
            UNBOUND_BITS => write!(f, "UNBOUND"),
            MISSING_BITS => write!(f, "MISSING"),
            EOF_BITS => write!(f, "EOF"),
            _ => match self.tag() {
                TAG_FIXNUM => write!(f, "Fixnum({})", self.as_fixnum()),
                TAG_CONS => write!(f, "Cons({:#x})", self.0 & !TAG_MASK),
                TAG_HEAP_OBJECT => write!(f, "HeapObj({:#x})", self.0 & !TAG_MASK),
                TAG_CHARACTER => write!(f, "Char({:?})", self.as_char()),
                TAG_SINGLE_FLOAT => write!(f, "SingleFloat({})", self.as_single_float()),
                TAG_SYMBOL => write!(f, "Symbol({})", self.as_symbol_index()),
                TAG_FUNCTION => write!(f, "Function({:#x})", self.0 & !TAG_MASK),
                TAG_SPECIAL => write!(f, "Special({:#x})", self.0),
                _ => write!(f, "BlissVal({:#x})", self.0),
            },
        }
    }
}

#[cfg(test)]
mod symbol_index_tests {
    use super::*;

    #[test]
    fn symbol_index_is_none_for_nil_and_t() {
        // is_symbol() reports the NIL/T constants as symbols, but they carry no
        // symbol-table index; symbol_index() must return None (bliss-hkf).
        assert!(NIL.is_symbol());
        assert!(T.is_symbol());
        assert_eq!(NIL.symbol_index(), None);
        assert_eq!(T.symbol_index(), None);
    }

    #[test]
    fn symbol_index_round_trips_real_symbols() {
        let s = BlissVal::from_symbol_index(1234);
        assert_eq!(s.symbol_index(), Some(1234));
        assert_eq!(s.as_symbol_index(), 1234);
    }
}
