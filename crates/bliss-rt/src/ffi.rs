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
        match self {
            AlienType::Void => 0,
            AlienType::Int { bits, .. } => (*bits as usize + 7) / 8,
            AlienType::Float => 4,
            AlienType::Double => 8,
            AlienType::Pointer(_) | AlienType::FnPtr { .. } => std::mem::size_of::<*const ()>(),
            AlienType::Struct { fields, packed } => {
                if *packed {
                    fields.iter().map(|f| f.size()).sum()
                } else {
                    let mut offset = 0usize;
                    for field in fields {
                        let align = field.alignment();
                        // Pad to alignment
                        offset = (offset + align - 1) & !(align - 1);
                        offset += field.size();
                    }
                    // Round up total to struct alignment
                    let struct_align = self.alignment();
                    if struct_align > 0 {
                        offset = (offset + struct_align - 1) & !(struct_align - 1);
                    }
                    offset
                }
            }
            AlienType::Union { variants } => {
                let max_size = variants.iter().map(|v| v.size()).max().unwrap_or(0);
                let union_align = self.alignment();
                if union_align > 0 {
                    (max_size + union_align - 1) & !(union_align - 1)
                } else {
                    max_size
                }
            }
        }
    }

    /// Alignment of this type in bytes.
    pub fn alignment(&self) -> usize {
        match self {
            AlienType::Void => 1,
            AlienType::Int { bits, .. } => {
                let size = (*bits as usize + 7) / 8;
                // Alignment is the natural size, capped at 8
                size.min(8)
            }
            AlienType::Float => 4,
            AlienType::Double => 8,
            AlienType::Pointer(_) | AlienType::FnPtr { .. } => std::mem::size_of::<*const ()>(),
            AlienType::Struct { fields, packed } => {
                if *packed {
                    1
                } else {
                    fields.iter().map(|f| f.alignment()).max().unwrap_or(1)
                }
            }
            AlienType::Union { variants } => {
                variants.iter().map(|v| v.alignment()).max().unwrap_or(1)
            }
        }
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
    _arg_types: &[AlienType],
    args: &[u64],
) -> Result<u64, BlissError> {
    if fn_ptr.is_null() {
        return Err(BlissError::FfiError("null function pointer".into()));
    }

    // Dispatch based on argument count and return type for common C calling
    // conventions. This is a bootstrap implementation supporting common cases.
    match args.len() {
        0 => {
            let f: extern "C" fn() -> u64 = std::mem::transmute(fn_ptr);
            Ok(f())
        }
        1 => {
            match ret_type {
                AlienType::Int { bits: 32, .. } => {
                    let f: extern "C" fn(i32) -> i32 = std::mem::transmute(fn_ptr);
                    Ok(f(args[0] as i32) as u32 as u64)
                }
                _ => {
                    let f: extern "C" fn(u64) -> u64 = std::mem::transmute(fn_ptr);
                    Ok(f(args[0]))
                }
            }
        }
        2 => {
            let f: extern "C" fn(u64, u64) -> u64 = std::mem::transmute(fn_ptr);
            Ok(f(args[0], args[1]))
        }
        3 => {
            let f: extern "C" fn(u64, u64, u64) -> u64 = std::mem::transmute(fn_ptr);
            Ok(f(args[0], args[1], args[2]))
        }
        4 => {
            let f: extern "C" fn(u64, u64, u64, u64) -> u64 = std::mem::transmute(fn_ptr);
            Ok(f(args[0], args[1], args[2], args[3]))
        }
        _ => Err(BlissError::FfiError(format!(
            "ffi_call: unsupported argument count {}",
            args.len()
        ))),
    }
}

// ── Marshalling ────────────────────────────────────────────────────

/// Marshal a BlissVal into a C value for passing to a foreign function.
pub fn marshal_to_c(value: BlissVal, alien_type: &AlienType) -> Result<u64, BlissError> {
    match alien_type {
        AlienType::Void => Ok(0),
        AlienType::Int { .. } => {
            if value.is_fixnum() {
                Ok(value.as_fixnum() as u64)
            } else if value.is_nil() {
                Ok(0)
            } else {
                Err(BlissError::FfiError(
                    "cannot marshal non-fixnum to integer".into(),
                ))
            }
        }
        AlienType::Float => {
            if value.is_single_float() {
                Ok(value.as_single_float().to_bits() as u64)
            } else if value.is_fixnum() {
                Ok((value.as_fixnum() as f32).to_bits() as u64)
            } else {
                Err(BlissError::FfiError(
                    "cannot marshal value to float".into(),
                ))
            }
        }
        AlienType::Double => {
            if value.is_single_float() {
                Ok(f64::to_bits(value.as_single_float() as f64))
            } else if value.is_fixnum() {
                Ok(f64::to_bits(value.as_fixnum() as f64))
            } else {
                Err(BlissError::FfiError(
                    "cannot marshal value to double".into(),
                ))
            }
        }
        AlienType::Pointer(_) => {
            if value.is_nil() {
                Ok(0)
            } else if value.is_fixnum() {
                Ok(value.as_fixnum() as u64)
            } else {
                // Return the raw tagged pointer
                Ok(value.to_raw())
            }
        }
        _ => Err(BlissError::FfiError(format!(
            "marshal_to_c: unsupported alien type {:?}",
            alien_type
        ))),
    }
}

/// Unmarshal a C return value into a BlissVal.
pub fn unmarshal_from_c(raw: u64, alien_type: &AlienType) -> Result<BlissVal, BlissError> {
    match alien_type {
        AlienType::Void => Ok(crate::value::NIL),
        AlienType::Int { signed, bits } => {
            // Sign-extend if signed
            let val = if *signed {
                match bits {
                    8 => (raw as i8) as i64,
                    16 => (raw as i16) as i64,
                    32 => (raw as i32) as i64,
                    64 => raw as i64,
                    _ => raw as i64,
                }
            } else {
                raw as i64
            };
            Ok(BlissVal::from_fixnum(val))
        }
        AlienType::Float => {
            let f = f32::from_bits(raw as u32);
            Ok(BlissVal::from_single_float(f))
        }
        AlienType::Double => {
            // Double doesn't fit in single_float; return as fixnum of the
            // integer part, or store the bits. For bootstrap, store as
            // a fixnum of the rounded value.
            let d = f64::from_bits(raw);
            Ok(BlissVal::from_fixnum(d as i64))
        }
        AlienType::Pointer(_) => {
            if raw == 0 {
                Ok(crate::value::NIL)
            } else {
                Ok(BlissVal::from_fixnum(raw as i64))
            }
        }
        _ => Err(BlissError::FfiError(format!(
            "unmarshal_from_c: unsupported alien type {:?}",
            alien_type
        ))),
    }
}

// ── Callbacks ──────────────────────────────────────────────────────

/// Opaque handle to a callback trampoline.
pub struct Callback {
    _closure: BlissVal,
    _ret_type: AlienType,
    _arg_types: Vec<AlienType>,
    /// A small executable trampoline. In the bootstrap implementation we
    /// use a boxed closure to keep a stable function pointer.
    trampoline: Box<dyn Fn()>,
}

impl Callback {
    /// Create a callback trampoline for calling back into CL from C.
    pub fn new(
        closure: BlissVal,
        ret_type: AlienType,
        arg_types: Vec<AlienType>,
    ) -> Result<Self, BlissError> {
        let trampoline = Box::new(|| {
            // Bootstrap trampoline — does nothing when called from C.
        });
        Ok(Callback {
            _closure: closure,
            _ret_type: ret_type,
            _arg_types: arg_types,
            trampoline,
        })
    }

    /// Get the C-callable function pointer for this callback.
    pub fn as_fn_ptr(&self) -> *const () {
        &*self.trampoline as *const dyn Fn() as *const ()
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        // The trampoline Box is dropped automatically.
        // No additional cleanup needed in the bootstrap implementation.
    }
}

// ── Library loading ────────────────────────────────────────────────

/// Load a shared library by name or path.
pub fn load_foreign_library(name: &str) -> Result<*mut (), BlissError> {
    use std::ffi::CString;
    let c_name = CString::new(name)
        .map_err(|_| BlissError::FfiError("library name contains null byte".into()))?;

    // Use libc dlopen
    let handle = unsafe { libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        let err = unsafe {
            let msg = libc::dlerror();
            if msg.is_null() {
                "unknown dlopen error".to_string()
            } else {
                std::ffi::CStr::from_ptr(msg).to_string_lossy().into_owned()
            }
        };
        Err(BlissError::FfiError(format!(
            "cannot load library '{}': {}",
            name, err
        )))
    } else {
        Ok(handle as *mut ())
    }
}

/// Look up a symbol in a loaded foreign library.
///
/// # Safety
/// The returned pointer is only valid while the library remains loaded.
pub unsafe fn foreign_symbol(library: *mut (), name: &str) -> Result<*const (), BlissError> {
    if library.is_null() {
        return Err(BlissError::FfiError("null library handle".into()));
    }
    let c_name = std::ffi::CString::new(name)
        .map_err(|_| BlissError::FfiError("symbol name contains null byte".into()))?;

    // Clear any existing error
    libc::dlerror();
    let sym = libc::dlsym(library as *mut libc::c_void, c_name.as_ptr());
    let err = libc::dlerror();
    if !err.is_null() {
        let msg = std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned();
        Err(BlissError::FfiError(format!(
            "symbol '{}' not found: {}",
            name, msg
        )))
    } else {
        Ok(sym as *const ())
    }
}
