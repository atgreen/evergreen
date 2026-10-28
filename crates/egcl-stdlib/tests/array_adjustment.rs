// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{
    EgclVal,
    object::{ConsCell, type_id},
    value::NIL,
};
use egcl_stdlib::sequences;

#[test]
fn adjustable_md_commit_relocates_new_backing_and_dimensions_and_rejects_partial_updates() {
    // This integration binary has one test, and owns explicit minor collections.
    let _ = egcl_rt::packed_u8::make(0, 0).unwrap();
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        storage = sequences::build_complex_vector(
            &[EgclVal::from_fixnum(1)],
            1,
            1,
            true,
            false,
            false,
            false
        )
    );
    egcl_rt::rooted!(array = sequences::build_md_array_with_storage(&[1, 1], *storage).unwrap());
    egcl_rt::gc::collect_t0_minor().unwrap();
    let body = egcl_rt::alloc_typed(16, type_id::CONS).unwrap();
    egcl_rt::rooted!(
        marker = unsafe {
            *body.cast::<EgclVal>() = EgclVal::from_fixnum(71);
            *body.add(8).cast::<EgclVal>() = NIL;
            EgclVal::from_cons_ptr(body)
        }
    );
    egcl_rt::rooted!(
        replacement_storage =
            sequences::build_complex_vector(&[*marker; 4], 4, 4, true, false, false, false)
    );
    egcl_rt::rooted!(
        replacement =
            sequences::build_md_array_with_storage(&[2, 2], *replacement_storage).unwrap()
    );
    let storage_before = egcl_rt::types::md_array_storage(*replacement)
        .unwrap()
        .to_raw();
    let dims_before = egcl_rt::types::md_array_dims(*replacement)
        .unwrap()
        .to_raw();
    let marker_before = marker.to_raw();
    assert_eq!(
        sequences::adopt_adjusted_array(*array, *replacement).unwrap(),
        *array
    );
    assert!(sequences::array_adjustable(*array));
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(updated_storage = egcl_rt::types::md_array_storage(*array).unwrap());
    egcl_rt::rooted!(updated_dims = egcl_rt::types::md_array_dims(*array).unwrap());
    assert_ne!(
        updated_storage.to_raw(),
        storage_before,
        "young replacement storage must actually move after adoption"
    );
    assert_ne!(
        updated_dims.to_raw(),
        dims_before,
        "young replacement dimensions must actually move after adoption"
    );
    assert_ne!(
        marker.to_raw(),
        marker_before,
        "stored heap value must actually move"
    );
    assert_eq!(
        Some(*updated_storage),
        egcl_rt::types::md_array_storage(*replacement)
    );
    assert_eq!(
        Some(*updated_dims),
        egcl_rt::types::md_array_dims(*replacement)
    );
    assert_eq!(sequences::array_total_size(*array), Some(4));
    assert_eq!(
        sequences::elt(*updated_dims, 0).unwrap(),
        EgclVal::from_fixnum(2)
    );
    for index in 0..4 {
        let value = sequences::aref(*updated_storage, index).unwrap();
        assert_eq!(value, *marker);
        assert_eq!(
            unsafe { (*value.as_ptr().cast::<ConsCell>()).car },
            EgclVal::from_fixnum(71)
        );
    }
    // Wrong rank and a newly introduced displacement cycle both fail before
    // changing any of the old object's references or shape.
    assert!(sequences::adopt_adjusted_array(*array, *replacement_storage).is_err());
    egcl_rt::rooted!(
        view = sequences::build_displaced_vector(*array, 0, 4, 4, true, false, false, false)
    );
    egcl_rt::rooted!(cyclic = sequences::build_md_array_with_storage(&[2, 2], *view).unwrap());
    assert!(sequences::adopt_adjusted_array(*array, *cyclic).is_err());
    assert_eq!(
        egcl_rt::types::md_array_storage(*array),
        Some(*updated_storage)
    );
    assert_eq!(egcl_rt::types::md_array_dims(*array), Some(*updated_dims));
}
