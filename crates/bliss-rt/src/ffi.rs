//! FFI bridge — C-ABI foreign function calls and callbacks.
//!
//! See §2.7 of the spec.

use crate::error::BlissError;
use crate::value::BlissVal;

// ── Alien type system ──────────────────────────────────────────────

/// Descriptor for a C/foreign type. D2.03.
#[derive(Clone, Debug, PartialEq)]
pub enum AlienType {
    Void,
    Int { signed: bool, bits: u8 },
    Float,
    Double,
    Pointer(Box<AlienType>),
    Struct { fields: Vec<AlienType>, packed: bool },
    Union { variants: Vec<AlienType> },
    FnPtr {
        ret: Box<AlienType>,
        args: Vec<AlienType>,
        variadic: bool,
    },
}

impl AlienType {
    /// Size of this type in bytes.
    pub fn size(&self) -> usize {
        unimplemented!("AlienType::size")
    }

    /// Alignment of this type in bytes.
    pub fn alignment(&self) -> usize {
        unimplemented!("AlienType::alignment")
    }
}

// ── FFI call interface ─────────────────────────────────────────────

/// Call a foreign function via pointer.
///
/// The green thread transitions to `Native` state during the call
/// so that GC safepoints are not blocked.
///
/// # Safety
/// `fn_ptr` must point to a valid function with the given signature.
pub unsafe fn ffi_call(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
) -> Result<u64, BlissError> {
    unimplemented!("ffi_call")
}

// ── Marshalling ────────────────────────────────────────────────────

/// Marshal a BlissVal into a C value for passing to a foreign function.
pub fn marshal_to_c(value: BlissVal, alien_type: &AlienType) -> Result<u64, BlissError> {
    unimplemented!("marshal_to_c")
}

/// Unmarshal a C return value into a BlissVal.
pub fn unmarshal_from_c(raw: u64, alien_type: &AlienType) -> Result<BlissVal, BlissError> {
    unimplemented!("unmarshal_from_c")
}

// ── Callbacks ──────────────────────────────────────────────────────

/// Opaque handle to a callback trampoline.
pub struct Callback {
    _private: (),
}

impl Callback {
    /// Create a callback trampoline for calling back into CL from C.
    ///
    /// Returns a function pointer that C code can call. When called,
    /// the trampoline transitions the thread from Native to Runnable,
    /// calls the CL closure, and returns the result.
    pub fn new(
        closure: BlissVal,
        ret_type: AlienType,
        arg_types: Vec<AlienType>,
    ) -> Result<Self, BlissError> {
        unimplemented!("Callback::new")
    }

    /// Get the C-callable function pointer for this callback.
    pub fn as_fn_ptr(&self) -> *const () {
        unimplemented!("Callback::as_fn_ptr")
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        unimplemented!("Callback::drop")
    }
}

// ── Library loading ────────────────────────────────────────────────

/// Load a shared library by name or path.
pub fn load_foreign_library(name: &str) -> Result<*mut (), BlissError> {
    unimplemented!("load_foreign_library")
}

/// Look up a symbol in a loaded foreign library.
///
/// # Safety
/// The returned pointer is only valid while the library remains loaded.
pub unsafe fn foreign_symbol(library: *mut (), name: &str) -> Result<*const (), BlissError> {
    unimplemented!("foreign_symbol")
}
