// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Single-start iOS host adapter. The UI keeps its main thread; all Lisp runs
//! on an owned worker. The C boundary never transports Lisp heap values.
use std::ffi::{c_char, c_int};

#[cfg(target_os = "ios")]
mod platform;

pub const ABI_VERSION: u32 = 1;
pub const OK: c_int = 0;
pub const INVALID_ARGUMENT: c_int = -1;
pub const ALREADY_STARTED: c_int = -2;
pub const UNSUPPORTED: c_int = -3;
pub const INTERNAL_ERROR: c_int = -4;

pub const IDLE: u32 = 0;
pub const STARTING: u32 = 1;
pub const RUNNING: u32 = 2;
pub const ROOT_COMPLETED: u32 = 3;
pub const STOPPING: u32 = 4;
pub const FINISHED: u32 = 5;
pub const FAILED: u32 = 6;

#[repr(C)]
pub struct Config {
    pub abi_version: u32,
    pub struct_size: u32,
    pub source_directory: *const c_char,
    pub writable_directory: *const c_char,
    pub entry_file: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Status {
    pub state: u32,
    pub exit_code: c_int,
}

fn contain(action: impl FnOnce() -> c_int) -> c_int {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).unwrap_or(INTERNAL_ERROR)
}

/// Copy the configuration and start asynchronously; one accepted attempt per
/// process lifetime. Invalid arguments may be corrected before acceptance.
/// # Safety
/// config is null (rejected) or points at a complete aligned Config; each
/// nonnull string is valid readable
/// NUL-terminated UTF-8 for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn egcl_ios_start(config: *const Config) -> c_int {
    contain(|| {
        #[cfg(target_os = "ios")]
        {
            unsafe { platform::start(config) }
        }
        #[cfg(not(target_os = "ios"))]
        {
            let _ = config;
            UNSUPPORTED
        }
    })
}

/// Request cooperative stop; never waits for a worker or sends SIGTERM.
#[unsafe(no_mangle)]
pub extern "C" fn egcl_ios_request_stop() -> c_int {
    contain(|| {
        #[cfg(target_os = "ios")]
        {
            platform::request_stop()
        }
        #[cfg(not(target_os = "ios"))]
        {
            UNSUPPORTED
        }
    })
}

/// Snapshot state. ROOT_COMPLETED means Lisp attachments remain active; FINISHED/FAILED
/// are published only after the root and all active descendants have stopped.
/// # Safety
/// out is null (rejected) or points at writable aligned Status storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn egcl_ios_status(out: *mut Status) -> c_int {
    contain(|| {
        if out.is_null() {
            return INVALID_ARGUMENT;
        }
        #[cfg(target_os = "ios")]
        let value = platform::status();
        #[cfg(not(target_os = "ios"))]
        let value = Status::default();
        // SAFETY: caller supplies writable aligned storage, checked nonnull.
        unsafe {
            out.write(value);
        }
        #[cfg(target_os = "ios")]
        {
            OK
        }
        #[cfg(not(target_os = "ios"))]
        {
            UNSUPPORTED
        }
    })
}

/// Copy the last diagnostic as UTF-8, NUL-terminating when capacity > 0.
/// Returns the required capacity including NUL; zero means invalid arguments
/// or a contained Rust panic. No pointer into worker storage escapes.
/// # Safety
/// A nonnull destination is writable for capacity bytes. A null destination
/// with nonzero capacity is rejected; null with zero capacity queries size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn egcl_ios_copy_error(destination: *mut c_char, capacity: usize) -> usize {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if capacity != 0 && destination.is_null() {
            return 0;
        }
        #[cfg(target_os = "ios")]
        let text = platform::error_text();
        #[cfg(not(target_os = "ios"))]
        let text = "the iOS host runtime is unsupported on this target".to_owned();
        if capacity != 0 {
            let mut count = text.len().min(capacity - 1);
            while !text.is_char_boundary(count) {
                count -= 1;
            }
            // SAFETY: the caller owns destination, and count < capacity.
            unsafe {
                std::ptr::copy_nonoverlapping(text.as_ptr(), destination.cast::<u8>(), count);
                destination.add(count).write(0);
            }
        }
        text.len().saturating_add(1)
    }))
    .unwrap_or(0)
}
