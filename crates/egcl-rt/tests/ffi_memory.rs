// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native memory semantics underlying EGCL-FFI and the CFFI backend.
//! Per R8.04, tracked storage rejects invalid lifetimes, bounds and null access.
use egcl_rt::ffi::{AlienType, memory::ForeignPointer};

fn integer(bits: u8) -> AlienType {
    AlienType::Int {
        signed: false,
        bits,
    }
}

#[test]
fn native_buffer_copies_check_exact_ranges_and_revalidate_after_free() {
    use std::mem::MaybeUninit;
    let pointer = ForeignPointer::allocate(3).unwrap();
    let bytes = [
        MaybeUninit::new(7),
        MaybeUninit::new(8),
        MaybeUninit::new(9),
    ];
    unsafe {
        pointer.write_buffer(&bytes).unwrap();
        let read = pointer.read_buffer(3).unwrap();
        assert_eq!(
            read.iter().map(|b| b.assume_init()).collect::<Vec<_>>(),
            [7, 8, 9]
        );
        assert!(pointer.read_buffer(4).is_err());
        assert!(pointer.offset(1).unwrap().write_buffer(&bytes).is_err());
    }
    pointer.check_range(3).unwrap();
    assert!(pointer.check_range(4).is_err());
    pointer.free().unwrap();
    assert!(pointer.check_range(3).is_err());
    unsafe {
        assert!(pointer.read_buffer(3).is_err());
        assert!(pointer.write_buffer(&bytes).is_err());
    }
}

#[test]
fn owned_memory_preserves_scalar_bits_even_at_unaligned_offsets() {
    let allocation = ForeignPointer::allocate(32).unwrap();
    assert_ne!(allocation.address(), 0);
    assert_eq!(allocation.address() % 16, 0);
    for (ty, bits) in [
        (integer(8), 0xff),
        (integer(16), 0xabcd),
        (integer(32), 0x89abcdef),
        (integer(64), u64::MAX),
        (AlienType::Float, (-0.0f32).to_bits() as u64),
        (AlienType::Double, 0x7ff8000000000123),
        (AlienType::Pointer(Box::new(AlienType::Void)), 0x12345678),
    ] {
        let pointer = allocation.offset(1).unwrap();
        unsafe {
            pointer.write_scalar(&ty, bits).unwrap();
            assert_eq!(pointer.read_scalar(&ty).unwrap(), bits);
        }
    }
    allocation.free().unwrap();
}

#[test]
fn bounds_and_freed_aliases_are_checked_before_access() {
    let allocation = ForeignPointer::allocate(8).unwrap();
    let interior = allocation.offset(1).unwrap();
    let end = allocation.offset(8).unwrap();
    let before = allocation.offset(-1).unwrap();
    unsafe {
        assert_eq!(allocation.read_scalar(&integer(64)).unwrap(), 0);
        assert!(interior.read_scalar(&integer(64)).is_err());
        assert!(end.read_scalar(&integer(8)).is_err());
        assert!(before.write_scalar(&integer(8), 1).is_err());
    }
    assert!(interior.free().is_err());
    allocation.free().unwrap();
    assert!(allocation.free().is_err());
    unsafe {
        assert!(allocation.read_scalar(&integer(8)).is_err());
        assert!(interior.write_scalar(&integer(8), 0).is_err());
    }
    // A new allocation must never revive an old alias, even if its address is reused.
    let replacement = ForeignPointer::allocate(8).unwrap();
    unsafe {
        assert!(allocation.read_scalar(&integer(8)).is_err());
    }
    replacement.free().unwrap();
}

#[test]
fn borrowed_addresses_are_explicit_and_do_not_acquire_ownership() {
    let mut word = 0x0123456789abcdefu64;
    let pointer = ForeignPointer::from_address((&mut word as *mut u64) as usize);
    unsafe {
        assert_eq!(pointer.read_scalar(&integer(64)).unwrap(), word);
        pointer.write_scalar(&integer(64), 99).unwrap();
    }
    assert_eq!(word, 99);
    assert!(pointer.free().is_err());
    let high = ForeignPointer::from_address(usize::MAX);
    assert_eq!(high.address(), usize::MAX);
    assert!(high.offset(1).is_err());
    assert!(ForeignPointer::from_address(0).offset(-1).is_err());
}

#[test]
fn null_empty_and_unsupported_accesses_are_errors() {
    let null = ForeignPointer::from_address(0);
    null.free().unwrap();
    unsafe {
        assert!(null.read_scalar(&integer(8)).is_err());
    }
    let empty = ForeignPointer::allocate(0).unwrap();
    assert_ne!(empty.address(), 0);
    unsafe {
        assert!(empty.read_scalar(&integer(8)).is_err());
    }
    empty.free().unwrap();
    assert!(ForeignPointer::allocate(usize::MAX).is_err());
    let pointer = ForeignPointer::allocate(8).unwrap();
    for ty in [
        AlienType::Void,
        integer(0),
        integer(24),
        AlienType::Struct {
            fields: vec![integer(8)],
            packed: false,
        },
    ] {
        unsafe {
            assert!(pointer.read_scalar(&ty).is_err());
            assert!(pointer.write_scalar(&ty, 0).is_err());
        }
    }
    pointer.free().unwrap();
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn allocated_memory_is_usable_through_the_jit_call_bridge() {
    unsafe extern "C" fn store(pointer: *mut u64, value: u64) {
        unsafe {
            pointer.write(value);
        }
    }
    let pointer = ForeignPointer::allocate(8).unwrap();
    unsafe {
        egcl_rt::ffi::ffi_call(
            store as *const (),
            &AlienType::Void,
            &[AlienType::Pointer(Box::new(integer(64))), integer(64)],
            &[pointer.address() as u64, u64::MAX],
        )
        .unwrap();
        assert_eq!(pointer.read_scalar(&integer(64)).unwrap(), u64::MAX);
    }
    pointer.free().unwrap();
}
