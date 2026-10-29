// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::{Collector, HeapCollector, nil_array};

#[test]
fn nil_arrays_have_no_slots_and_raw_length_is_not_a_gc_reference() {
    egcl_rt::rooted!(other = egcl_rt::gc::alloc_double_float(42.0));
    let raw_length = other.to_raw() as usize;
    egcl_rt::rooted!(array = nil_array::make(raw_length).unwrap());
    assert_eq!(nil_array::len(*array), Some(raw_length));
    assert!(egcl_rt::types::stringp(*array));
    let size = unsafe { (*array.as_ptr().cast::<egcl_rt::ObjectHeader>()).size_units() };
    assert_eq!(
        size, 4,
        "header, tag and length words padded to 16-byte alignment regardless of length"
    );
    let before = array.to_raw();
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(array.to_raw(), before, "NIL array must really move");
    assert_ne!(
        other.to_raw() as usize,
        raw_length,
        "raw length target must move independently"
    );
    assert_eq!(nil_array::len(*array), Some(raw_length));
    egcl_rt::rooted!(zero = nil_array::make(0).unwrap());
    egcl_rt::rooted!(positive = nil_array::make(7).unwrap());
    assert_eq!(nil_array::len(*zero), Some(0));
    assert_eq!(nil_array::len(*positive), Some(7));
    assert!(!egcl_rt::types::bit_vector_p(*positive));
    assert!(nil_array::make(usize::MAX).is_err());
    assert!(nil_array::make(egcl_rt::bignum::FIXNUM_MAX as usize).is_err());
    egcl_rt::rooted!(limit = nil_array::make(egcl_rt::bignum::FIXNUM_MAX as usize - 1).unwrap());
    assert_eq!(
        nil_array::len(*limit),
        Some(egcl_rt::bignum::FIXNUM_MAX as usize - 1)
    );
}

#[test]
fn nil_storage_rejects_nonmapped_pseudoheap_handles_before_header_reads() {
    for address in [0x1000usize, 0x7fff_ffff_f000usize] {
        let value = unsafe { egcl_rt::EgclVal::from_heap_ptr(address as *mut u8) };
        assert!(value.is_heap_object());
        assert_eq!(nil_array::len(value), None);
        assert!(!nil_array::is_vector(value));
    }
}
