// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(not(target_os = "ios"))]

#[test]
fn non_ios_workspace_library_refuses_start_without_dereferencing_config() {
    // SAFETY: unsupported target rejects before dereferencing the null input.
    assert_eq!(
        unsafe { egcl_ios::egcl_ios_start(std::ptr::null()) },
        egcl_ios::UNSUPPORTED
    );
    let mut status = egcl_ios::Status {
        state: 999,
        exit_code: 999,
    };
    // SAFETY: owns complete writable status storage.
    assert_eq!(
        unsafe { egcl_ios::egcl_ios_status(&mut status) },
        egcl_ios::UNSUPPORTED
    );
    assert_eq!(status.state, egcl_ios::IDLE);
    assert_eq!(status.exit_code, 0);
    assert_eq!(egcl_ios::egcl_ios_request_stop(), egcl_ios::UNSUPPORTED);
}

#[test]
fn diagnostics_use_owned_bounded_nul_terminated_storage() {
    // SAFETY: zero capacity requires no destination pointer.
    let needed = unsafe { egcl_ios::egcl_ios_copy_error(std::ptr::null_mut(), 0) };
    assert!(needed > 1);
    let mut buffer = [0x55i8; 5];
    // SAFETY: complete five-byte writable buffer, deliberately truncated.
    let actual = unsafe { egcl_ios::egcl_ios_copy_error(buffer.as_mut_ptr().cast(), buffer.len()) };
    assert_eq!(actual, needed);
    assert_eq!(buffer[4], 0);
    // SAFETY: previous call guaranteed a NUL in the owned buffer.
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr().cast()) }.to_bytes(),
        b"the "
    );
    // SAFETY: invalid null destination is rejected before any write.
    assert_eq!(
        unsafe { egcl_ios::egcl_ios_copy_error(std::ptr::null_mut(), 1) },
        0
    );
}
