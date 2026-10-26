// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{EgclVal, packed_u8};
use egcl_stdlib::sequences;

#[test]
fn octet_sequences_and_wrappers_preserve_storage_across_real_relocation() {
    // This target has one test; stress configuration is cached on allocation.
    unsafe {
        std::env::set_var("EGCL_GC_STRESS", "1");
        std::env::set_var("EGCL_GC_POISON", "1");
    }
    egcl_rt::rooted!(input = packed_u8::make(4, 0).unwrap());
    for (i, v) in [0, 1, 128, 255].into_iter().enumerate() {
        packed_u8::set(*input, i, v);
    }
    assert_eq!(sequences::length(*input).unwrap(), 4);
    for invalid in [-1, 256, i64::MAX >> 3] {
        assert!(sequences::set_aref(*input, 0, EgclVal::from_fixnum(invalid)).is_err());
    }
    assert_eq!(sequences::aref(*input, 0).unwrap(), EgclVal::from_fixnum(0));
    assert!(sequences::svref(*input, 0).is_err());
    assert!(sequences::set_svref(*input, 0, EgclVal::from_fixnum(7)).is_err());
    for operation in ["copy", "subseq", "reverse"] {
        *input = packed_u8::make(4, 3).unwrap();
        packed_u8::set(*input, 0, 255);
        let before = input.to_raw();
        egcl_rt::rooted!(
            result = match operation {
                "copy" => sequences::copy_seq(*input),
                "subseq" => sequences::subseq(*input, 0, Some(2)),
                "reverse" => sequences::reverse(*input),
                _ => unreachable!(),
            }
            .unwrap()
        );
        assert_ne!(
            input.to_raw(),
            before,
            "{operation} must really relocate the source"
        );
        assert!(packed_u8::is_vector(*result));
        assert_eq!(
            packed_u8::get(*result, if operation == "reverse" { 3 } else { 0 }),
            Some(255)
        );
        assert_eq!(packed_u8::get(*input, 0), Some(255));
    }
    assert_eq!(
        sequences::fill_vector_with_octets(*input, 1, &[0, 128, 255]),
        Some(3)
    );
    assert_eq!(
        sequences::vector_octets(*input, 0, 4),
        Some(vec![255, 0, 128, 255])
    );
    assert_eq!(sequences::vector_octets(*input, 0, 5), None);
    egcl_rt::rooted!(array = sequences::build_u8_complex_vector(2, 2, true, 7, true).unwrap());
    let before = array.to_raw();
    assert_eq!(
        sequences::vector_push_extend(*array, EgclVal::from_fixnum(255), Some(1)).unwrap(),
        EgclVal::from_fixnum(2)
    );
    assert_ne!(
        array.to_raw(),
        before,
        "growing the packed backing store must relocate its wrapper"
    );
    assert_eq!(sequences::array_total_size(*array), Some(3));
    assert_eq!(
        sequences::vector_pop(*array).unwrap(),
        EgclVal::from_fixnum(255)
    );
    assert_eq!(sequences::length(*array).unwrap(), 2);
    assert_eq!(
        sequences::aref(*array, 2).unwrap(),
        EgclVal::from_fixnum(255)
    );
    assert!(sequences::elt(*array, 2).is_err());
    egcl_rt::rooted!(
        view = sequences::build_u8_displaced_vector(*array, 1, 2, 2, false, false).unwrap()
    );
    assert_eq!(sequences::u8_array_element(*view, 1), Some(255));
    sequences::set_aref(*view, 0, EgclVal::from_fixnum(128)).unwrap();
    assert_eq!(sequences::u8_array_element(*array, 1), Some(128));
    egcl_rt::rooted!(md = sequences::build_md_array_with_storage(&[1, 2], *view).unwrap());
    assert!(sequences::is_u8_array(*md));
    assert_eq!(sequences::u8_array_element(*md, 1), Some(255));
    sequences::adjust_complex_vector(*array, 12, Some(2), EgclVal::from_fixnum(42)).unwrap();
    assert_eq!(
        sequences::aref(*array, 11).unwrap(),
        EgclVal::from_fixnum(42)
    );
    assert_eq!(
        sequences::aref(*view, 0).unwrap(),
        EgclVal::from_fixnum(128)
    );
    assert!(sequences::set_aref(*view, 0, EgclVal::from_fixnum(256)).is_err());
    sequences::adjust_complex_vector(*array, 1, Some(1), EgclVal::from_fixnum(0)).unwrap();
    assert_eq!(sequences::array_total_size(*array), Some(1));
    egcl_rt::rooted!(matrix = sequences::build_u8_md_array(&[2, 3], 255).unwrap());
    assert!(sequences::is_u8_array(*matrix));
    assert_eq!(sequences::array_total_size(*matrix), Some(6));
    assert_eq!(
        sequences::u8_array_element(*matrix, 5),
        Some(255)
    );
    assert_eq!(sequences::u8_array_element(*matrix, 6), None);
}
