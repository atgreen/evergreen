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
    Int {
        signed: bool,
        bits: u8,
    },
    Float,
    Double,
    Pointer(Box<AlienType>),
    Struct {
        fields: Vec<AlienType>,
        packed: bool,
    },
    Union {
        variants: Vec<AlienType>,
    },
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
            AlienType::Int { bits, .. } => (*bits as usize).div_ceil(8),
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
                let size = (*bits as usize).div_ceil(8);
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
    arg_types: &[AlienType],
    args: &[u64],
) -> Result<u64, BlissError> {
    if fn_ptr.is_null() {
        return Err(BlissError::FfiError("null function pointer".into()));
    }

    // Issue #7: Check if the return type or any argument type involves 32-bit int
    // and dispatch appropriately. For the bootstrap, we handle the common cases
    // of all-u64 and 32-bit int signatures.
    let is_ret_i32 = matches!(ret_type, AlienType::Int { bits: 32, .. });
    let all_args_i32 = !arg_types.is_empty()
        && arg_types
            .iter()
            .all(|t| matches!(t, AlienType::Int { bits: 32, .. }));

    // Issue #6: Extended to support up to 8 arguments.
    // Issue #7: Type-aware dispatch for i32 signatures.
    if is_ret_i32 && all_args_i32 {
        // All-i32 fast path
        match args.len() {
            0 => {
                let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f() as u32 as u64)
            }
            1 => {
                let f: extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0] as i32) as u32 as u64)
            }
            2 => {
                let f: extern "C" fn(i32, i32) -> i32 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0] as i32, args[1] as i32) as u32 as u64)
            }
            3 => {
                let f: extern "C" fn(i32, i32, i32) -> i32 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0] as i32, args[1] as i32, args[2] as i32) as u32 as u64)
            }
            4 => {
                let f: extern "C" fn(i32, i32, i32, i32) -> i32 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(
                    args[0] as i32,
                    args[1] as i32,
                    args[2] as i32,
                    args[3] as i32,
                ) as u32 as u64)
            }
            5 => {
                let f: extern "C" fn(i32, i32, i32, i32, i32) -> i32 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(
                    args[0] as i32,
                    args[1] as i32,
                    args[2] as i32,
                    args[3] as i32,
                    args[4] as i32,
                ) as u32 as u64)
            }
            6 => {
                let f: extern "C" fn(i32, i32, i32, i32, i32, i32) -> i32 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(
                    args[0] as i32,
                    args[1] as i32,
                    args[2] as i32,
                    args[3] as i32,
                    args[4] as i32,
                    args[5] as i32,
                ) as u32 as u64)
            }
            7 => {
                let f: extern "C" fn(i32, i32, i32, i32, i32, i32, i32) -> i32 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(
                    args[0] as i32,
                    args[1] as i32,
                    args[2] as i32,
                    args[3] as i32,
                    args[4] as i32,
                    args[5] as i32,
                    args[6] as i32,
                ) as u32 as u64)
            }
            8 => {
                let f: extern "C" fn(i32, i32, i32, i32, i32, i32, i32, i32) -> i32 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(
                    args[0] as i32,
                    args[1] as i32,
                    args[2] as i32,
                    args[3] as i32,
                    args[4] as i32,
                    args[5] as i32,
                    args[6] as i32,
                    args[7] as i32,
                ) as u32 as u64)
            }
            _ => Err(BlissError::FfiError(format!(
                "ffi_call: unsupported argument count {} (max 8)",
                args.len()
            ))),
        }
    } else {
        // Generic u64 path (works for pointers, 64-bit ints, etc.)
        match args.len() {
            0 => {
                let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f())
            }
            1 => {
                let f: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0]))
            }
            2 => {
                let f: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0], args[1]))
            }
            3 => {
                let f: extern "C" fn(u64, u64, u64) -> u64 = unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0], args[1], args[2]))
            }
            4 => {
                let f: extern "C" fn(u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0], args[1], args[2], args[3]))
            }
            5 => {
                let f: extern "C" fn(u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0], args[1], args[2], args[3], args[4]))
            }
            6 => {
                let f: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(args[0], args[1], args[2], args[3], args[4], args[5]))
            }
            7 => {
                let f: extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(
                    args[0], args[1], args[2], args[3], args[4], args[5], args[6],
                ))
            }
            8 => {
                let f: extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(fn_ptr) };
                Ok(f(
                    args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                ))
            }
            _ => Err(BlissError::FfiError(format!(
                "ffi_call: unsupported argument count {} (max 8)",
                args.len()
            ))),
        }
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
                Err(BlissError::FfiError("cannot marshal value to float".into()))
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
            // Issue #8: Double doesn't fit in single_float. To avoid losing
            // the fractional part, check if the value fits without loss as an
            // integer; otherwise, downcast to f32 single-float (lossy but
            // preserves non-integer values for the bootstrap runtime).
            // A full implementation would use a heap-allocated double-float.
            let d = f64::from_bits(raw);
            if d.fract() == 0.0 && d >= i64::MIN as f64 && d <= i64::MAX as f64 {
                Ok(BlissVal::from_fixnum(d as i64))
            } else {
                // Store as single_float — lossy for large doubles, but preserves
                // fractional part for typical values.
                Ok(BlissVal::from_single_float(d as f32))
            }
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

/// A C-callable trampoline function type.
/// In the bootstrap implementation, callbacks invoke this static trampoline
/// which looks up the registered Lisp closure via a thread-local slot and
/// invokes it.
extern "C" fn bootstrap_trampoline() -> u64 {
    CURRENT_CALLBACK_CLOSURE.with(|cell| {
        let closure = cell.get();
        invoke_closure(closure)
    })
}

/// Invoke a Lisp closure value and return its raw result.
///
/// Dispatches based on the closure's type:
/// - NIL → returns NIL (no closure registered)
/// - Function-tagged pointer → dereferences the function header:
///   - CompiledFunctionData → calls through `entry_point`
///   - ClosureData → extracts the inner function and recurses
///   - InterpretedFunctionData → returns NIL (needs full evaluator)
/// - Fixnum → treated as a raw C function pointer (useful for testing)
/// - Any other type → returns NIL
fn invoke_closure(closure: BlissVal) -> u64 {
    use crate::object::{ClosureData, CompiledFunctionData, ObjectHeader, type_id};
    #[allow(unused_imports)]
    use crate::value::{TAG_FUNCTION, TAG_MASK};

    // NIL means no closure is registered; return NIL.
    if closure.is_nil() {
        return crate::value::NIL.to_raw();
    }

    // Function-tagged pointer: dereference the function header and dispatch
    // on the object's type_id to find the entry_point or inner function.
    if closure.is_function() {
        unsafe {
            let ptr = closure.as_ptr();
            let header = *(ptr as *const ObjectHeader);
            let tid = header.type_id();

            match tid {
                type_id::COMPILED_FUNCTION => {
                    let compiled = &*(ptr as *const CompiledFunctionData);
                    let entry = compiled.entry_point;
                    if !entry.is_null() {
                        let f: extern "C" fn() -> u64 = std::mem::transmute(entry);
                        return f();
                    }
                    // Null entry_point — fall through to NIL
                }
                type_id::CLOSURE => {
                    // A ClosureData wraps an inner function; extract and recurse.
                    let clo = &*(ptr as *const ClosureData);
                    let inner = clo.function;
                    return invoke_closure(inner);
                }
                type_id::FUNCTION_INTERPRETED => {
                    // Interpreted functions require the evaluator which lives in
                    // the compiler crate and is not accessible from the runtime.
                    // Callbacks wrapping interpreted functions must be compiled
                    // first via the tiered compilation pipeline.
                    panic!(
                        "FFI callback invoked on an interpreted (non-compiled) function. \
                         The function must be compiled before it can be used as a foreign callback."
                    );
                }
                _ => {
                    // Unknown function sub-type; return NIL.
                }
            }
        }
        return crate::value::NIL.to_raw();
    }

    // Heap-object path: the closure might be a heap-allocated closure
    // or compiled function reached via TAG_HEAP_OBJECT instead of TAG_FUNCTION.
    if closure.is_heap_object() {
        unsafe {
            let ptr = closure.as_ptr();
            let header = *(ptr as *const ObjectHeader);
            let tid = header.type_id();

            match tid {
                type_id::COMPILED_FUNCTION => {
                    let compiled = &*(ptr as *const CompiledFunctionData);
                    let entry = compiled.entry_point;
                    if !entry.is_null() {
                        let f: extern "C" fn() -> u64 = std::mem::transmute(entry);
                        return f();
                    }
                }
                type_id::CLOSURE => {
                    let clo = &*(ptr as *const ClosureData);
                    let inner = clo.function;
                    return invoke_closure(inner);
                }
                _ => {}
            }
        }
        return crate::value::NIL.to_raw();
    }

    // Fixnum path: treat the integer value as a raw C function pointer.
    // This is convenient for testing callbacks without constructing full
    // heap-allocated function objects.
    if closure.is_fixnum() {
        let raw = closure.as_fixnum() as u64;
        if raw != 0 {
            unsafe {
                let f: extern "C" fn() -> u64 = std::mem::transmute(raw as *const ());
                return f();
            }
        }
        return crate::value::NIL.to_raw();
    }

    // Anything else — return NIL.
    crate::value::NIL.to_raw()
}

std::thread_local! {
    /// Thread-local storage for the current callback's Lisp closure value.
    static CURRENT_CALLBACK_CLOSURE: std::cell::Cell<BlissVal> =
        const { std::cell::Cell::new(crate::value::NIL) };
}

/// Opaque handle to a callback trampoline.
pub struct Callback {
    closure: BlissVal,
    _ret_type: AlienType,
    _arg_types: Vec<AlienType>,
    /// The C-callable function pointer for this callback.
    fn_ptr: *const (),
}

impl Callback {
    /// Create a callback trampoline for calling back into CL from C.
    ///
    /// In the bootstrap implementation, all callbacks share a single static
    /// trampoline function. Before calling through the fn_ptr from C, the
    /// runtime sets the thread-local CURRENT_CALLBACK_CLOSURE to this
    /// callback's closure value. A full implementation would generate
    /// per-callback executable trampolines.
    pub fn new(
        closure: BlissVal,
        ret_type: AlienType,
        arg_types: Vec<AlienType>,
    ) -> Result<Self, BlissError> {
        // Store the closure so it can be set before invocation
        let fn_ptr = bootstrap_trampoline as *const ();
        Ok(Callback {
            closure,
            _ret_type: ret_type,
            _arg_types: arg_types,
            fn_ptr,
        })
    }

    /// Get the C-callable function pointer for this callback.
    ///
    /// Before calling this pointer from C code, set up the callback
    /// context by calling `prepare_call()`.
    pub fn as_fn_ptr(&self) -> *const () {
        self.fn_ptr
    }

    /// Prepare the thread-local state so that calling `as_fn_ptr()` from C
    /// will invoke this callback's closure.
    pub fn prepare_call(&self) {
        CURRENT_CALLBACK_CLOSURE.with(|cell| {
            cell.set(self.closure);
        });
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        // No additional cleanup needed — the static trampoline is shared
        // and the thread-local is per-call, not per-callback.
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
    unsafe { libc::dlerror() };
    let sym = unsafe { libc::dlsym(library as *mut libc::c_void, c_name.as_ptr()) };
    let err = unsafe { libc::dlerror() };
    if !err.is_null() {
        let msg = unsafe { std::ffi::CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned();
        Err(BlissError::FfiError(format!(
            "symbol '{}' not found: {}",
            name, msg
        )))
    } else {
        Ok(sym as *const ())
    }
}
