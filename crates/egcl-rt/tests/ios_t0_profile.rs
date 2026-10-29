// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Initial iOS profile boundaries; source fixtures, not a device-support claim.
#![cfg(all(target_os = "ios", target_arch = "aarch64"))]

use egcl_rt::{ffi, image, jit::JitBuffer, syscall};

#[test]
fn ios_image_identity_distinguishes_device_and_simulator_from_other_platforms() {
    let expected = if cfg!(egcl_ios_simulator) {
        image::Os::IosSimulator
    } else {
        image::Os::Ios
    };
    let current = image::current_platform_tag();
    assert_eq!(current, image::platform_tag(image::Arch::Aarch64, expected));
    for other in [image::Os::Linux, image::Os::MacOS, image::Os::Android] {
        assert_ne!(current, image::platform_tag(image::Arch::Aarch64, other));
    }
    assert_ne!(
        image::platform_tag(image::Arch::Aarch64, image::Os::Ios),
        image::platform_tag(image::Arch::Aarch64, image::Os::IosSimulator)
    );
}

#[test]
fn executable_mapping_requests_are_rejected_before_the_os_call() {
    assert!(JitBuffer::new(&[0; 4]).is_none());
    // SAFETY: mmap owns no input pointers. The invalid mprotect address is
    // intentional: profile rejection must return EPERM before asking the OS
    // to inspect an unmapped range (which would normally report EINVAL).
    unsafe {
        assert_eq!(
            syscall::mmap(
                std::ptr::null_mut(),
                syscall::page_size(),
                syscall::PROT_READ | syscall::PROT_EXEC,
                syscall::MAP_PRIVATE | syscall::MAP_ANONYMOUS,
                -1,
                0,
            ),
            Err(libc::EPERM)
        );
        assert_eq!(
            syscall::mprotect(
                std::ptr::null_mut(),
                syscall::page_size(),
                syscall::PROT_EXEC
            ),
            Err(libc::EPERM)
        );
    }
}

#[test]
fn nonexecutable_memory_still_supports_runtime_storage() {
    let length = syscall::page_size();
    // SAFETY: the test owns the complete mapping, initializes before reading,
    // and unmaps on exit, including an assertion or protection failure.
    unsafe {
        let mapping = syscall::mmap(
            std::ptr::null_mut(),
            length,
            syscall::PROT_READ | syscall::PROT_WRITE,
            syscall::MAP_PRIVATE | syscall::MAP_ANONYMOUS,
            -1,
            0,
        )
        .expect("runtime storage mapping");
        struct Mapping(*mut u8, usize);
        impl Drop for Mapping {
            fn drop(&mut self) {
                // SAFETY: uniquely owns the anonymous mapping.
                let _ = unsafe { syscall::munmap(self.0, self.1) };
            }
        }
        let owned = Mapping(mapping, length);
        owned.0.cast::<u64>().write(42);
        syscall::mprotect(owned.0, owned.1, syscall::PROT_READ).expect("read-only storage");
        assert_eq!(owned.0.cast::<u64>().read(), 42);
        syscall::mprotect(owned.0, owned.1, syscall::PROT_READ | syscall::PROT_WRITE)
            .expect("writable storage");
    }
}

#[test]
fn linked_scalar_calls_work_without_generated_executable_adapters() {
    extern "C" fn sum(a: u64, b: u64) -> u64 {
        a + b
    }
    let word = ffi::AlienType::Int {
        bits: 64,
        signed: false,
    };
    // SAFETY: the static target takes and returns the advertised u64 scalars.
    let result = unsafe {
        ffi::ffi_call(
            sum as *const (),
            &word,
            &[word.clone(), word.clone()],
            &[20, 22],
        )
    }
    .expect("linked scalar call");
    assert_eq!(result, 42);
    assert!(JitBuffer::new(&[0; 4]).is_none());
}

#[test]
fn linked_static_library_symbols_remain_available_without_the_elf_loader() {
    extern "C" fn linked_marker() -> u64 {
        42
    }
    let symbol = linked_marker as *const ();
    // SAFETY: this statically compiled symbol remains valid for process life.
    unsafe {
        ffi::register_static_library("ios-profile-fixture", &[], &[("linked_marker", symbol)])
            .expect("register linked provider");
        let handle =
            ffi::load_foreign_library("ios-profile-fixture").expect("open linked provider");
        assert_eq!(
            ffi::foreign_symbol(handle, "linked_marker").expect("linked symbol"),
            symbol
        );
        ffi::close_foreign_library(handle).expect("close provider handle");
    }
}
