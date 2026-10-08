// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FFI bridge — C-ABI foreign function calls and callbacks.
//!
//! See §2.7 of the spec.

use crate::bignum::{BigInt, bigint_from_val};
use crate::error::EgclError;
use crate::value::EgclVal;

pub mod memory;

#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
mod abi;
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
mod buffered;
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub use buffered::ffi_call_buffered;

/// Checked native layout, identical to the generated call adapter's layout.
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub fn native_layout(ty: &AlienType) -> Result<(usize, usize), EgclError> {
    let layout = abi::Layout::new(ty)?;
    Ok((layout.size as usize, layout.alignment as usize))
}

#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
mod call;
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub mod callback;
// AArch64 gets a real ABI backend rather than the legacy dispatcher's eighteen
// hardcoded shapes (spec §4.7.5.2). Everything else non-x86 still falls back.
#[cfg(all(target_arch = "aarch64", unix))]
mod aapcs64;
// riscv64 likewise: one LP64D trampoline covers every scalar signature.
#[cfg(all(target_arch = "riscv64", unix))]
mod lp64d;
/// ELFv2 foreign calls. Compiled on every host — the placement rules are pure
/// logic, and their tests are what pin the difference from AAPCS64 that would
/// otherwise be found only by running on POWER. Only the trampolines and the
/// callable entry points and re-export below are architecture-gated.
#[cfg_attr(
    not(all(target_arch = "powerpc64", target_endian = "little")),
    allow(
        dead_code,
        reason = "placement is exercised by this module's own tests"
    )
)]
mod elfv2;
#[cfg(not(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "aarch64", unix),
    all(target_arch = "riscv64", unix),
    all(target_arch = "powerpc64", target_endian = "little", unix)
)))]
mod legacy;
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub mod managed_callback;
#[cfg(all(target_arch = "x86_64", windows))]
mod win64;
#[cfg(all(target_arch = "aarch64", unix))]
pub use aapcs64::{ffi_call, ffi_call_variadic};
#[cfg(all(target_arch = "riscv64", unix))]
pub use lp64d::{ffi_call, ffi_call_variadic};
#[cfg(all(target_arch = "powerpc64", target_endian = "little", unix))]
pub use elfv2::{ffi_call, ffi_call_variadic};
#[cfg(not(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "aarch64", unix),
    all(target_arch = "riscv64", unix),
    all(target_arch = "powerpc64", target_endian = "little", unix)
)))]
pub use legacy::{ffi_call, ffi_call_variadic};

#[cfg(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "aarch64", unix),
    all(target_arch = "riscv64", unix)
))]
pub(crate) fn ffi_profile_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("EGCL_FFI_PROFILE").is_some())
}

/// Accumulate the four phases and report every 2000 calls, to stderr — which on
/// Android is the logcat tag `egcl-err`.
#[cfg(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "aarch64", unix),
    all(target_arch = "riscv64", unix)
))]
pub(crate) fn ffi_profile_record(adapter_ns: u64, enter_ns: u64, invoke_ns: u64, leave_ns: u64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    static ADAPTER: AtomicU64 = AtomicU64::new(0);
    static ENTER: AtomicU64 = AtomicU64::new(0);
    static INVOKE: AtomicU64 = AtomicU64::new(0);
    static LEAVE: AtomicU64 = AtomicU64::new(0);
    ADAPTER.fetch_add(adapter_ns, Ordering::Relaxed);
    ENTER.fetch_add(enter_ns, Ordering::Relaxed);
    INVOKE.fetch_add(invoke_ns, Ordering::Relaxed);
    LEAVE.fetch_add(leave_ns, Ordering::Relaxed);
    let n = N.fetch_add(1, Ordering::Relaxed) + 1;
    if n % 2000 == 0 {
        eprintln!(
            "[ffi-profile] {n} calls: adapter {:.1}us enter {:.1}us invoke {:.1}us leave {:.1}us",
            ADAPTER.load(Ordering::Relaxed) as f64 / 1000.0 / n as f64,
            ENTER.load(Ordering::Relaxed) as f64 / 1000.0 / n as f64,
            INVOKE.load(Ordering::Relaxed) as f64 / 1000.0 / n as f64,
            LEAVE.load(Ordering::Relaxed) as f64 / 1000.0 / n as f64,
        );
    }
}

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
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub unsafe fn ffi_call(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
) -> Result<u64, EgclError> {
    // SAFETY: forwarded caller contract; this is a non-variadic signature.
    unsafe { ffi_call_impl(fn_ptr, ret_type, arg_types, args, None) }
}

/// Call a variadic C function. `fixed_count` is the number of named parameters.
/// Types and raw slots describe values *before* C default argument promotions:
/// trailing float arguments become doubles and 8/16-bit integers become ints.
/// Named parameters retain their declared types.
///
/// # Safety
/// `fn_ptr` must have the named parameter and return types supplied here, and
/// the callee must consume the trailing arguments using their promoted types.
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub unsafe fn ffi_call_variadic(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: usize,
) -> Result<u64, EgclError> {
    if fixed_count > arg_types.len() || arg_types.len() != args.len() {
        return Err(EgclError::FfiError(
            "invalid variadic argument counts".into(),
        ));
    }
    let mut promoted_types = arg_types.to_vec();
    let mut promoted_args = args.to_vec();
    for index in fixed_count..arg_types.len() {
        match &arg_types[index] {
            AlienType::Float => {
                promoted_types[index] = AlienType::Double;
                promoted_args[index] = (f32::from_bits(args[index] as u32) as f64).to_bits();
            }
            AlienType::Int {
                bits: bits @ (8 | 16),
                signed,
            } => {
                let shift = 64 - bits;
                promoted_args[index] = if *signed {
                    (((args[index] << shift) as i64) >> shift) as u32 as u64
                } else {
                    args[index] & ((1u64 << bits) - 1)
                };
                // On the supported x86-64 ABIs, C int represents every char/short value,
                // including unsigned char and unsigned short.
                promoted_types[index] = AlienType::Int {
                    bits: 32,
                    signed: true,
                };
            }
            _ => {}
        }
    }
    // SAFETY: argument slots now match the promoted signature; other aspects
    // of the foreign target's contract remain the caller's responsibility.
    unsafe {
        ffi_call_impl(
            fn_ptr,
            ret_type,
            &promoted_types,
            &promoted_args,
            Some(fixed_count),
        )
    }
}

#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
unsafe fn ffi_call_impl(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: Option<usize>,
) -> Result<u64, EgclError> {
    if fn_ptr.is_null() {
        return Err(EgclError::FfiError("null function pointer".into()));
    }
    if arg_types.len() != args.len() {
        return Err(EgclError::FfiError(
            "foreign argument count does not match signature".into(),
        ));
    }
    // EGCL_FFI_PROFILE: where a foreign call's time actually goes. A call
    // measured 37us on x86-64 and 1311us on Android for the SAME zero-argument
    // function, which is far more than the CPU gap, so the split matters more
    // than the total (bliss-1dp).
    let profile = ffi_profile_enabled();
    let t0 = if profile { Some(std::time::Instant::now()) } else { None };

    // Compile/cache before publishing Native state. The adapter's Arc remains
    // live across foreign execution; no cache lock is held during callbacks.
    let adapter = call::CallAdapter::get(ret_type, arg_types, fixed_count)?;
    let t1 = t0.map(|_| std::time::Instant::now());

    let foreign_frame = crate::debug_stack::ForeignFrame::enter(fn_ptr)?;
    let errors = managed_callback::ForeignCallErrors::enter();
    let state_guard = crate::safepoint::ForeignStateScope::native();
    let t2 = t0.map(|_| std::time::Instant::now());

    // SAFETY: the caller supplies a matching C signature; count and supported
    // types were checked above, and the adapter stays alive through the call.
    let result = unsafe { adapter.invoke(fn_ptr, args) };
    let t3 = t0.map(|_| std::time::Instant::now());
    drop(state_guard);
    drop(foreign_frame);
    errors.finish()?;
    if let (Some(t0), Some(t1), Some(t2), Some(t3)) = (t0, t1, t2, t3) {
        ffi_profile_record(
            t1.duration_since(t0).as_nanos() as u64,
            t2.duration_since(t1).as_nanos() as u64,
            t3.duration_since(t2).as_nanos() as u64,
            std::time::Instant::now().duration_since(t3).as_nanos() as u64,
        );
    }
    Ok(result)
}

// ── Marshalling ────────────────────────────────────────────────────

fn integer_mask(bits: u8) -> Result<u64, EgclError> {
    match bits {
        8 | 16 | 32 => Ok((1u64 << bits) - 1),
        64 => Ok(u64::MAX),
        _ => Err(EgclError::FfiError(format!(
            "unsupported foreign integer width: {bits}"
        ))),
    }
}

fn marshal_integer(value: EgclVal, signed: bool, bits: u8) -> Result<u64, EgclError> {
    let mask = integer_mask(bits)?;
    // Copy the magnitude into Rust storage. This does not allocate on the Lisp
    // heap, and no raw pointer into a bignum survives an allocation.
    let integer = bigint_from_val(value)
        .ok_or_else(|| EgclError::FfiError("foreign integer argument is not an integer".into()))?;
    let magnitude = integer.mag.first().copied().unwrap_or(0);
    let limit = if signed {
        (1u64 << (bits - 1)) - u64::from(integer.sign >= 0)
    } else {
        mask
    };
    if integer.mag.len() > 1 || magnitude > limit || (!signed && integer.sign < 0) {
        return Err(EgclError::FfiError(format!(
            "integer argument is outside the {} {bits}-bit range",
            if signed { "signed" } else { "unsigned" },
        )));
    }
    Ok(if integer.sign < 0 {
        magnitude.wrapping_neg()
    } else {
        magnitude
    })
}

/// Marshal a EgclVal into a C value for passing to a foreign function.
pub fn marshal_to_c(value: EgclVal, alien_type: &AlienType) -> Result<u64, EgclError> {
    match alien_type {
        AlienType::Void => Ok(0),
        AlienType::Int { signed, bits } => marshal_integer(value, *signed, *bits),
        AlienType::Float => {
            if value.is_single_float() {
                Ok(value.as_single_float().to_bits() as u64)
            } else if value.is_fixnum() {
                Ok((value.as_fixnum() as f32).to_bits() as u64)
            } else {
                Err(EgclError::FfiError("cannot marshal value to float".into()))
            }
        }
        AlienType::Double => {
            if value.is_double_float() {
                Ok(value.as_double_float().to_bits())
            } else if value.is_single_float() {
                Ok(f64::to_bits(value.as_single_float() as f64))
            } else if value.is_fixnum() {
                Ok(f64::to_bits(value.as_fixnum() as f64))
            } else {
                Err(EgclError::FfiError(
                    "cannot marshal value to double".into(),
                ))
            }
        }
        AlienType::Pointer(_) | AlienType::FnPtr { .. } => {
            if value.is_nil() {
                Ok(0)
            } else if memory::ForeignPointer::is_pointer(value) {
                Ok(memory::ForeignPointer::from_lisp(value)?.call_address()? as u64)
            } else if value.is_fixnum() {
                Ok(value.as_fixnum() as u64)
            } else {
                Err(EgclError::FfiError(
                    "cannot marshal opaque Lisp value as foreign pointer".into(),
                ))
            }
        }
        _ => Err(EgclError::FfiError(format!(
            "marshal_to_c: unsupported alien type {:?}",
            alien_type
        ))),
    }
}

/// Unmarshal a C return value into a EgclVal.
pub fn unmarshal_from_c(raw: u64, alien_type: &AlienType) -> Result<EgclVal, EgclError> {
    match alien_type {
        AlienType::Void => Ok(crate::value::NIL),
        AlienType::Int { signed, bits } => {
            let raw = raw & integer_mask(*bits)?;
            let integer = if *signed {
                let shift = 64 - bits;
                BigInt::from_i64(((raw << shift) as i64) >> shift)
            } else {
                BigInt::from_mag(1, vec![raw])
            };
            Ok(integer.to_val())
        }
        AlienType::Float => {
            let f = f32::from_bits(raw as u32);
            Ok(EgclVal::from_single_float(f))
        }
        AlienType::Double => Ok(crate::gc::alloc_double_float(f64::from_bits(raw))),
        AlienType::Pointer(_) | AlienType::FnPtr { .. } => {
            memory::ForeignPointer::from_address(raw as usize).into_lisp()
        }
        _ => Err(EgclError::FfiError(format!(
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
fn invoke_closure(closure: EgclVal) -> u64 {
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
    static CURRENT_CALLBACK_CLOSURE: std::cell::Cell<EgclVal> =
        const { std::cell::Cell::new(crate::value::NIL) };
}

/// Opaque handle to a callback trampoline.
pub struct Callback {
    closure: EgclVal,
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
        closure: EgclVal,
        ret_type: AlienType,
        arg_types: Vec<AlienType>,
    ) -> Result<Self, EgclError> {
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
//
// Default (static-capable) build: load libraries at runtime with `elf_loader` —
// a pure-Rust ELF loader that needs no `ld.so`/`dlopen` and works in a fully
// static musl binary (bliss-bca.5). A loaded library's own undefined symbols
// (malloc, memcpy, …) resolve against THIS executable's statically-linked musl
// symbols via a host `SyntheticModule`. `DT_INIT_ARRAY` constructors run during
// relocation. `ffi_call`/marshalling (which need no loader) are unchanged.
//
// The `c-ffi` build instead uses libc `dlopen` (below) — for dynamically-linked
// targets that load system (glibc) libraries. POWER and IBM Z also use this
// backend by default because elf_loader has no native relocation support there.

struct ForeignSymbolName {
    address: usize,
    name: Box<str>,
    next: *mut ForeignSymbolName,
}

static FOREIGN_SYMBOL_NAMES: std::sync::atomic::AtomicPtr<ForeignSymbolName> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn cached_foreign_symbol_name(address: usize) -> Option<&'static str> {
    use std::sync::atomic::Ordering;

    let mut node = FOREIGN_SYMBOL_NAMES.load(Ordering::Acquire);
    // SAFETY: successful insertion publishes fully initialized nodes with
    // Release ordering, and published nodes are never mutated or reclaimed.
    while let Some(entry) = unsafe { node.as_ref() } {
        if entry.address == address {
            return Some(&entry.name);
        }
        node = entry.next;
    }
    None
}

pub(super) fn remember_foreign_symbol(address: *const (), name: &str) {
    use std::sync::atomic::Ordering;

    let address = address as usize;
    if address == 0 {
        return;
    }
    let entry = Box::into_raw(Box::new(ForeignSymbolName {
        address,
        name: name.into(),
        next: std::ptr::null_mut(),
    }));
    loop {
        let head = FOREIGN_SYMBOL_NAMES.load(Ordering::Acquire);
        let mut node = head;
        // SAFETY: the Acquire load observes initialized, permanently retained
        // nodes; the new entry is still private to this thread.
        while let Some(existing) = unsafe { node.as_ref() } {
            if existing.address == address && existing.name.as_ref() == name {
                // SAFETY: `entry` has not been published by a successful CAS.
                unsafe { drop(Box::from_raw(entry)) };
                return;
            }
            node = existing.next;
        }
        // SAFETY: `entry` remains unpublished, so updating its link is private.
        unsafe { (*entry).next = head };
        if FOREIGN_SYMBOL_NAMES
            .compare_exchange_weak(head, entry, Ordering::Release, Ordering::Acquire)
            .is_ok()
        {
            // Names are durable debug metadata. Nodes are deliberately retained:
            // active and historical backtraces may outlive a closed provider.
            return;
        }
    }
}

#[cfg(all(
    unix,
    not(feature = "c-ffi"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod elf_backend {
    use crate::error::EgclError;
    use elf_loader::{
        Loader, Relocator,
        image::{LoadedCore, SyntheticModule, SyntheticSymbol},
        memory::RegionAccess,
        relocation::RelocationArch,
        tls::TlsResolver,
    };
    use std::sync::{Arc, Mutex};

    /// A loaded foreign library, type-erased so the registry need not carry
    /// elf_loader's generic parameters.
    trait ForeignLib: Send + Sync {
        fn symbol(&self, name: &str) -> Option<*const ()>;
    }

    impl<D, Arch, R, Tls> ForeignLib for LoadedCore<D, Arch, R, Tls>
    where
        D: 'static + Send + Sync,
        Arch: RelocationArch,
        R: RegionAccess,
        Tls: TlsResolver<Arch> + 'static,
        Self: Send + Sync,
    {
        fn symbol(&self, name: &str) -> Option<*const ()> {
            // SAFETY: a plain symbol lookup; the returned address stays valid as
            // long as the library lives in the registry (which owns it).
            unsafe { self.get::<extern "C" fn()>(name).map(|s| s.into_raw()) }
        }
    }

    /// Non-reused 1-based tokens. Closed entries remain empty, so a stale token
    /// cannot accidentally resolve into a later library.
    static LIBS: Mutex<Vec<Option<Arc<dyn ForeignLib>>>> = Mutex::new(Vec::new());

    /// Host symbols a loaded library may import from us — statically linked into
    /// this binary from musl. Deliberately minimal (the libc surface plugins
    /// commonly need); grow this list on demand. An unresolved import produces a
    /// clear "cannot link" error naming the missing symbol.
    fn host_symbols() -> Vec<SyntheticSymbol> {
        host_symbol_addresses()
            .into_iter()
            .map(|(name, address)| SyntheticSymbol::function(name, address))
            .collect()
    }

    fn host_symbol_addresses() -> Vec<(&'static str, *const ())> {
        macro_rules! host_syms {
            ($($name:ident),* $(,)?) => {{
                // Opaque decls: we only take addresses, so signatures are moot.
                unsafe extern "C" { $( fn $name(); )* }
                vec![ $( (stringify!($name), $name as *const ()) ),* ]
            }};
        }
        // The handful of symbols the standard library itself relies on cannot be
        // declared opaquely: since rustc 1.98 a declaration whose signature
        // disagrees with the one std expects is a hard error ("invalid definition
        // of the runtime `memcpy` symbol used by the standard library"), so the
        // opaque `fn memcpy();` above broke the build outright (bliss-cice).
        // Their real signatures go here; everything else stays opaque, because
        // only the ADDRESS is ever used.
        //
        // The break hid from `cargo test --workspace`, which unifies features and
        // so turns `c-ffi` on and cfg's this whole module out. Only a build that
        // resolves features for this crate alone compiles it.
        // The ALLOCATOR trio joined this list for the same reason, one build
        // configuration later (bliss-0g677). A normal build links a prebuilt
        // std and the check never fires; a build that compiles std FROM SOURCE
        // -- `-Z build-std`, which every sanitizer job needs, and `cargo miri
        // setup` -- puts std's own malloc/realloc/free in the same crate graph
        // and rejects the opaque declarations. That is why sanitizers.yml had
        // never once passed: both of its jobs died here, in compilation, before
        // running a single check. `calloc` is moved with them because it is the
        // same allocator family and its true signature is no less correct; only
        // malloc, realloc and free actually error today.
        use core::ffi::c_void;
        unsafe extern "C" {
            fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
            fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
            fn memset(dest: *mut c_void, c: i32, n: usize) -> *mut c_void;
            fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32;
            fn strlen(s: *const i8) -> usize;
            fn malloc(size: usize) -> *mut c_void;
            fn calloc(count: usize, size: usize) -> *mut c_void;
            fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
            fn free(ptr: *mut c_void);
        }
        let mut syms = host_syms![
            strcmp, strncmp, strcpy, strncpy, strncat, strcat,
            abort, abs, sqrt, qsort,
        ];
        syms.extend_from_slice(&[
            ("memcpy", memcpy as *const ()),
            ("memmove", memmove as *const ()),
            ("memset", memset as *const ()),
            ("memcmp", memcmp as *const ()),
            ("strlen", strlen as *const ()),
            ("malloc", malloc as *const ()),
            ("calloc", calloc as *const ()),
            ("realloc", realloc as *const ()),
            ("free", free as *const ()),
        ]);
        syms
    }

    /// Load a shared library by path with `elf_loader`, resolving its undefined
    /// symbols against the host and running its constructors.
    pub fn load_foreign_library(name: &str) -> Result<*mut (), EgclError> {
        let host = SyntheticModule::new("__egcl_host", host_symbols());
        let lib = Relocator::new()
            .run(
                Loader::new()
                    .load_dylib(name)
                    .map_err(|e| EgclError::FfiError(format!("cannot load '{name}': {e}")))?,
            )
            .scope([host])
            .relocate::<()>()
            .map_err(|e| EgclError::FfiError(format!("cannot link '{name}': {e}")))?;
        // relocate() already ran the library's DT_INIT_ARRAY constructors.
        let mut libs = LIBS.lock().unwrap();
        libs.push(Some(Arc::new(lib)));
        Ok(libs.len() as *mut ()) // 1-based handle
    }

    /// Look up a symbol in a previously loaded library.
    ///
    /// # Safety
    /// The library must remain loaded while any returned pointer is used.
    pub unsafe fn foreign_symbol(library: *mut (), name: &str) -> Result<*const (), EgclError> {
        let handle = library as usize;
        if handle == 0 {
            return Err(EgclError::FfiError("null library handle".into()));
        }
        if name.contains('\0') {
            return Err(EgclError::FfiError(
                "symbol name contains null byte".into(),
            ));
        }
        let lib = LIBS
            .lock()
            .unwrap()
            .get(handle - 1)
            .and_then(Option::as_ref)
            .cloned()
            .ok_or_else(|| EgclError::FfiError("invalid or closed library handle".into()))?;
        let symbol = lib
            .symbol(name)
            .ok_or_else(|| EgclError::FfiError(format!("symbol '{name}' not found")))?;
        super::remember_foreign_symbol(symbol, name);
        Ok(symbol)
    }

    /// # Safety
    /// No foreign call, callback or retained symbol may use this library after close.
    pub unsafe fn close_foreign_library(library: *mut ()) -> Result<(), EgclError> {
        let lib = {
            let mut libs = LIBS.lock().unwrap();
            (library as usize)
                .checked_sub(1)
                .and_then(|i| libs.get_mut(i))
                .and_then(Option::take)
                .ok_or_else(|| EgclError::FfiError("invalid or closed library handle".into()))?
        };
        // Dropping LoadedCore runs DT_FINI_ARRAY and releases its mappings.
        // Never hold the registry lock while arbitrary C destructors execute.
        drop(lib);
        Ok(())
    }

    /// Search open libraries, then the statically linked host export surface.
    /// # Safety
    /// The provider must remain loaded while the returned pointer is used.
    pub unsafe fn foreign_symbol_global(name: &str) -> Result<*const (), EgclError> {
        if name.contains('\0') {
            return Err(EgclError::FfiError(
                "symbol name contains null byte".into(),
            ));
        }
        let libs: Vec<_> = LIBS.lock().unwrap().iter().flatten().cloned().collect();
        for lib in libs {
            if let Some(symbol) = lib.symbol(name) {
                super::remember_foreign_symbol(symbol, name);
                return Ok(symbol);
            }
        }
        let address = host_symbol_addresses()
            .into_iter()
            .find(|(symbol, _)| *symbol == name)
            .map(|(_, address)| address)
            .ok_or_else(|| EgclError::FfiError(format!("symbol '{name}' not found")))?;
        super::remember_foreign_symbol(address, name);
        Ok(address)
    }
}

#[cfg(all(
    unix,
    not(feature = "c-ffi"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use elf_backend::{
    close_foreign_library, foreign_symbol, foreign_symbol_global, load_foreign_library,
};

#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
struct DynamicLibrary(usize);

#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        // SAFETY: each instance owns exactly one successful dlopen reference.
        unsafe {
            libc::dlclose(self.0 as *mut libc::c_void);
        }
    }
}

#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
static DYNAMIC_LIBRARIES: std::sync::Mutex<Vec<Option<std::sync::Arc<DynamicLibrary>>>> =
    std::sync::Mutex::new(Vec::new());

/// Load a shared library by name or path.
#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
pub fn load_foreign_library(name: &str) -> Result<*mut (), EgclError> {
    use std::ffi::CString;
    let c_name = CString::new(name)
        .map_err(|_| EgclError::FfiError("library name contains null byte".into()))?;

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
        Err(EgclError::FfiError(format!(
            "cannot load library '{}': {}",
            name, err
        )))
    } else {
        let mut libraries = DYNAMIC_LIBRARIES.lock().unwrap();
        libraries.push(Some(std::sync::Arc::new(DynamicLibrary(handle as usize))));
        Ok(libraries.len() as *mut ())
    }
}

/// Look up a symbol in a loaded foreign library.
///
/// # Safety
/// The returned pointer is only valid while the library remains loaded.
#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
pub unsafe fn foreign_symbol(library: *mut (), name: &str) -> Result<*const (), EgclError> {
    if library.is_null() {
        return Err(EgclError::FfiError("null library handle".into()));
    }
    let library = DYNAMIC_LIBRARIES
        .lock()
        .unwrap()
        .get(library as usize - 1)
        .and_then(Option::as_ref)
        .cloned()
        .ok_or_else(|| EgclError::FfiError("invalid or closed library handle".into()))?;
    // SAFETY: the Arc retains a live dlopen reference through lookup.
    unsafe { dynamic_symbol(library.0 as *mut libc::c_void, name) }
}

#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
unsafe fn dynamic_symbol(library: *mut libc::c_void, name: &str) -> Result<*const (), EgclError> {
    let c_name = std::ffi::CString::new(name)
        .map_err(|_| EgclError::FfiError("symbol name contains null byte".into()))?;

    // Clear any existing error
    unsafe { libc::dlerror() };
    let sym = unsafe { libc::dlsym(library, c_name.as_ptr()) };
    let err = unsafe { libc::dlerror() };
    if !err.is_null() {
        let msg = unsafe { std::ffi::CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned();
        Err(EgclError::FfiError(format!(
            "symbol '{}' not found: {}",
            name, msg
        )))
    } else {
        let symbol = sym as *const ();
        remember_foreign_symbol(symbol, name);
        Ok(symbol)
    }
}

/// Close one owned loader reference and invalidate its token.
/// # Safety
/// No foreign call, callback or retained symbol may use this library after close.
#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
pub unsafe fn close_foreign_library(library: *mut ()) -> Result<(), EgclError> {
    let library = {
        let mut libraries = DYNAMIC_LIBRARIES.lock().unwrap();
        (library as usize)
            .checked_sub(1)
            .and_then(|i| libraries.get_mut(i))
            .and_then(Option::take)
            .ok_or_else(|| EgclError::FfiError("invalid or closed library handle".into()))?
    };
    // Destructors can reenter the loader, so release the registry lock first.
    drop(library);
    Ok(())
}

/// Look up a symbol in the loader's global namespace.
/// # Safety
/// The provider must remain loaded while the returned pointer is used.
#[cfg(all(
    unix,
    any(
        feature = "c-ffi",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
pub unsafe fn foreign_symbol_global(name: &str) -> Result<*const (), EgclError> {
    // RTLD_DEFAULT makes glibc add a lookup dependency from this executable to
    // the provider (elf/dl-sym.c: DL_LOOKUP_ADD_DEPENDENCY), preventing explicit
    // unload after a global lookup. A process handle searches the same global
    // scope without silently acquiring that process-lifetime dependency.
    let handle = unsafe { libc::dlopen(std::ptr::null(), libc::RTLD_NOW) };
    if handle.is_null() {
        return Err(EgclError::FfiError(
            "cannot open process symbol scope".into(),
        ));
    }
    let process = DynamicLibrary(handle as usize);
    unsafe { dynamic_symbol(process.0 as *mut libc::c_void, name) }
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    close_foreign_library, foreign_symbol, foreign_symbol_global, load_foreign_library,
};

/// Cold symbolization of an active outbound call. The lookup itself is lock-free
/// and loader-independent; returned text is owned so historical snapshots never
/// retain pointers into a provider's symbol table.
pub(crate) fn foreign_frame_name(address: usize) -> String {
    if let Some(name) = cached_foreign_symbol_name(address) {
        return name.to_owned();
    }
    format!("<unknown at 0x{address:x}>")
}

#[cfg(all(test, target_os = "linux", target_env = "gnu", feature = "c-ffi"))]
mod foreign_frame_name_tests {
    use super::{foreign_frame_name, foreign_symbol_global};
    use std::ffi::CString;
    use std::fs;
    use std::process::Command;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn symbolization_does_not_wait_for_the_dynamic_loader() {
        let malloc = unsafe { foreign_symbol_global("malloc") }.expect("resolve malloc");

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("egcl-loader-lock-{}-{nonce}", std::process::id()));
        fs::create_dir(&dir).expect("create test directory");
        let source = dir.join("loader-lock.c");
        let library = dir.join("loader-lock.so");
        let ready = dir.join("ready");
        let release = dir.join("release");
        fs::write(
            &source,
            format!(
                r#"
#include <fcntl.h>
#include <unistd.h>

__attribute__((constructor)) static void hold_loader_lock(void) {{
    int fd = open("{}", O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd >= 0) close(fd);
    while (access("{}", F_OK) != 0) usleep(1000);
}}
"#,
                ready.display(),
                release.display()
            ),
        )
        .expect("write test library source");
        let compile = Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .output()
            .expect("run cc");
        assert!(
            compile.status.success(),
            "cc failed: {}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let release_after_probe = release.clone();
        let ready_for_releaser = ready.clone();
        let probe_done = Arc::new(AtomicBool::new(false));
        let probe_done_for_releaser = Arc::clone(&probe_done);
        let releaser_started = Arc::new(Barrier::new(2));
        let releaser_barrier = Arc::clone(&releaser_started);
        let releaser = std::thread::spawn(move || {
            releaser_barrier.wait();
            while !ready_for_releaser.exists() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !probe_done_for_releaser.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            fs::write(release_after_probe, b"").expect("release constructor");
        });
        releaser_started.wait();

        let loader = std::thread::spawn(move || {
            let path = CString::new(library.as_os_str().as_encoded_bytes()).unwrap();
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW) };
            assert!(!handle.is_null(), "test dlopen failed");
            assert_eq!(unsafe { libc::dlclose(handle) }, 0);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(Instant::now() < deadline, "constructor did not start");
            std::thread::sleep(Duration::from_millis(1));
        }

        let started = Instant::now();
        let name = foreign_frame_name(malloc as usize);
        let elapsed = started.elapsed();
        probe_done.store(true, Ordering::Release);

        releaser.join().unwrap();
        loader.join().unwrap();
        assert_eq!(name, "malloc");
        assert!(
            elapsed < Duration::from_secs(1),
            "symbolization waited {elapsed:?} for the loader lock"
        );
        fs::remove_dir_all(dir).expect("remove test directory");
    }
}
