//! Windows owns the native stack and its TEB bounds for every managed fiber.
//!
//! The scheduler serializes mounts, including migration between carriers.
//! Handles are deleted only after the last owner releases an unmounted fiber.

use super::{Ordering, TorclError};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use windows_sys::Win32::System::Threading::{
    ConvertFiberToThread, ConvertThreadToFiberEx, CreateFiberEx, DeleteFiber, SwitchToFiber,
};

// WinBase.h: preserve floating-point state as well as the integer context.
const FIBER_FLAG_FLOAT_SWITCH: u32 = 1;

pub(super) struct FiberExecutionContext {
    handle: NonNull<c_void>,
}

impl FiberExecutionContext {
    pub(super) fn new(stack_size: usize) -> Result<Self, TorclError> {
        // Commit a small initial stack; Windows grows it using guard pages up
        // to the requested reservation (the default is 512 KiB).
        let handle = unsafe {
            CreateFiberEx(
                16 * 1024,
                stack_size,
                FIBER_FLAG_FLOAT_SWITCH,
                Some(start),
                std::ptr::null(),
            )
        };
        NonNull::new(handle)
            .map(|handle| Self { handle })
            .ok_or_else(|| os_error("CreateFiberEx"))
    }

    /// The caller must exclusively own the mount and keep this context alive
    /// until control returns to the carrier.
    pub(super) unsafe fn resume(&self, scheduler_return: &AtomicUsize) -> Result<(), TorclError> {
        // Scheduler workers are ordinary Rust threads between mounts. Convert
        // only for this mount, and undo that conversion when the fiber leaves.
        let carrier = unsafe { ConvertThreadToFiberEx(std::ptr::null(), FIBER_FLAG_FLOAT_SWITCH) };
        if carrier.is_null() {
            return Err(os_error("ConvertThreadToFiberEx"));
        }
        scheduler_return.store(carrier as usize, Ordering::Release);
        unsafe { SwitchToFiber(self.handle.as_ptr()) };
        scheduler_return.store(0, Ordering::Release);
        if unsafe { ConvertFiberToThread() } == 0 {
            // Continuing as an unexpectedly converted carrier would invalidate
            // later mount ownership. This is an internal invariant failure.
            crate::syscall::abort();
        }
        Ok(())
    }
}

impl Drop for FiberExecutionContext {
    fn drop(&mut self) {
        // A WorkerTask retains an Arc<Fiber> until SwitchToFiber has returned,
        // even when another thread has consumed JOIN and removed the registry
        // entry. Thus deletion can never target the currently executing fiber.
        unsafe { DeleteFiber(self.handle.as_ptr()) };
    }
}

unsafe extern "system" fn start(_: *mut c_void) {
    super::fiber_context_trampoline();
}

/// Called only from the mounted fiber; its carrier remains suspended in resume.
pub(super) unsafe fn suspend(scheduler_return: &AtomicUsize) -> Result<(), TorclError> {
    let carrier = scheduler_return.load(Ordering::Acquire) as *mut c_void;
    if carrier.is_null() {
        return Err(TorclError::Internal(
            "fiber has no mounted scheduler context".into(),
        ));
    }
    unsafe { SwitchToFiber(carrier) };
    Ok(())
}

fn os_error(operation: &str) -> TorclError {
    TorclError::Internal(format!("{operation}: {}", std::io::Error::last_os_error()))
}
