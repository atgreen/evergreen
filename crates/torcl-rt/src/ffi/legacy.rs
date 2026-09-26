//! Existing bootstrap dispatcher, retained on targets awaiting JIT ABI adapters.
//! Do not extend this path; new FFI support belongs in the generated adapters.

use super::{
    AlienType, FiberState, NativeThreadState, current_fiber, current_stack, current_thread,
};
use crate::error::TorclError;

/// Variadic calls require the target's generated adapter implementation.
///
/// # Safety
/// The foreign function and values must match the supplied C signature.
pub unsafe fn ffi_call_variadic(
    _fn_ptr: *const (),
    _ret_type: &AlienType,
    _arg_types: &[AlienType],
    _args: &[u64],
    _fixed_count: usize,
) -> Result<u64, TorclError> {
    Err(TorclError::FfiError(
        "variadic calls are not implemented for this target ABI".into(),
    ))
}

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
) -> Result<u64, TorclError> {
    enum NativeStateGuard {
        Fiber(&'static crate::thread::Fiber, FiberState),
        Thread(&'static crate::thread::NativeThread, NativeThreadState),
    }

    impl Drop for NativeStateGuard {
        fn drop(&mut self) {
            match self {
                NativeStateGuard::Fiber(fiber, previous) => fiber.set_state(*previous),
                NativeStateGuard::Thread(thread, previous) => thread.set_state(*previous),
            }
        }
    }

    if fn_ptr.is_null() {
        return Err(TorclError::FfiError("null function pointer".into()));
    }

    current_stack().publish_top();
    let _state_guard = if let Some(fiber) = current_fiber() {
        let guard = NativeStateGuard::Fiber(fiber, fiber.state());
        fiber.set_state(FiberState::Native);
        guard
    } else {
        let thread = current_thread();
        let guard = NativeStateGuard::Thread(thread, thread.state());
        thread.set_state(NativeThreadState::Native);
        guard
    };

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
            _ => Err(TorclError::FfiError(format!(
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
            _ => Err(TorclError::FfiError(format!(
                "ffi_call: unsupported argument count {} (max 8)",
                args.len()
            ))),
        }
    }
}
