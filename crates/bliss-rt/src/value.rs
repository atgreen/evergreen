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
    pub fn from_fixnum(n: i64) -> Self {
        unimplemented!("BlissVal::from_fixnum")
    }

    /// Create a character value from a Unicode codepoint.
    pub fn from_char(c: char) -> Self {
        unimplemented!("BlissVal::from_char")
    }

    /// Create a single-float immediate from an f32.
    pub fn from_single_float(f: f32) -> Self {
        unimplemented!("BlissVal::from_single_float")
    }

    /// Create a symbol-index value.
    pub fn from_symbol_index(idx: u32) -> Self {
        unimplemented!("BlissVal::from_symbol_index")
    }

    /// Create a cons-tagged pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid cons cell.
    pub unsafe fn from_cons_ptr(ptr: *mut u8) -> Self {
        unimplemented!("BlissVal::from_cons_ptr")
    }

    /// Create a heap-object-tagged pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid `ObjectHeader`.
    pub unsafe fn from_heap_ptr(ptr: *mut u8) -> Self {
        unimplemented!("BlissVal::from_heap_ptr")
    }

    /// Create a function-tagged pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid function header.
    pub unsafe fn from_function_ptr(ptr: *mut u8) -> Self {
        unimplemented!("BlissVal::from_function_ptr")
    }
}

// ── Tag extraction ─────────────────────────────────────────────────

impl BlissVal {
    /// Extract the 3-bit tag.
    #[inline(always)]
    pub fn tag(self) -> u64 {
        unimplemented!("BlissVal::tag")
    }

    /// True if this value is a fixnum (tag `000`).
    #[inline(always)]
    pub fn is_fixnum(self) -> bool {
        unimplemented!("BlissVal::is_fixnum")
    }

    /// True if this value is a cons cell (tag `001`).
    #[inline(always)]
    pub fn is_cons(self) -> bool {
        unimplemented!("BlissVal::is_cons")
    }

    /// True if this value is a general heap object (tag `010`).
    #[inline(always)]
    pub fn is_heap_object(self) -> bool {
        unimplemented!("BlissVal::is_heap_object")
    }

    /// True if this value is a character (tag `011`).
    #[inline(always)]
    pub fn is_character(self) -> bool {
        unimplemented!("BlissVal::is_character")
    }

    /// True if this value is a single-float (tag `100`).
    #[inline(always)]
    pub fn is_single_float(self) -> bool {
        unimplemented!("BlissVal::is_single_float")
    }

    /// True if this value is a symbol (tag `101` or special NIL/T).
    #[inline(always)]
    pub fn is_symbol(self) -> bool {
        unimplemented!("BlissVal::is_symbol")
    }

    /// True if this value is a function (tag `110`).
    #[inline(always)]
    pub fn is_function(self) -> bool {
        unimplemented!("BlissVal::is_function")
    }

    /// True if this value is NIL.
    #[inline(always)]
    pub fn is_nil(self) -> bool {
        unimplemented!("BlissVal::is_nil")
    }

    /// True if this value is a list (cons or NIL).
    #[inline(always)]
    pub fn is_list(self) -> bool {
        unimplemented!("BlissVal::is_list")
    }
}

// ── Extraction ─────────────────────────────────────────────────────

impl BlissVal {
    /// Extract the fixnum value. Panics if not a fixnum.
    pub fn as_fixnum(self) -> i64 {
        unimplemented!("BlissVal::as_fixnum")
    }

    /// Extract the character value. Panics if not a character.
    pub fn as_char(self) -> char {
        unimplemented!("BlissVal::as_char")
    }

    /// Extract the single-float value. Panics if not a single-float.
    pub fn as_single_float(self) -> f32 {
        unimplemented!("BlissVal::as_single_float")
    }

    /// Extract the symbol table index. Panics if not a symbol.
    pub fn as_symbol_index(self) -> u32 {
        unimplemented!("BlissVal::as_symbol_index")
    }

    /// Extract the raw pointer (mask off tag bits).
    ///
    /// # Safety
    /// Caller must ensure the tag is a pointer tag (001, 010, or 110).
    pub unsafe fn as_ptr(self) -> *mut u8 {
        unimplemented!("BlissVal::as_ptr")
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
        unimplemented!("BlissVal::Debug")
    }
}
