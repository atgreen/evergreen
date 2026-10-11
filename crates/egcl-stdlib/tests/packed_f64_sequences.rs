// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{EgclVal, packed_f64};
use egcl_stdlib::sequences;

#[test]
fn packed_sequences_preserve_storage_check_types_and_box_under_collection() {
    // One test owns this process; stress is cached on first allocation.
    unsafe { std::env::set_var("EGCL_GC_STRESS", "1") };
    egcl_rt::rooted!(input = packed_f64::make(3, 1.25).unwrap());
    assert_eq!(sequences::length(*input).unwrap(), 3);
    assert_eq!(sequences::array_total_size(*input).unwrap(), 3);
    assert!(sequences::set_aref(*input, 0, EgclVal::from_fixnum(1)).is_err());
    assert!(sequences::aref(*input, 3).is_err());
    let fill = egcl_rt::gc::alloc_double_float(-3.5);
    sequences::set_aref(*input, 0, fill).unwrap();
    assert_eq!(sequences::aref(*input, 0).unwrap().as_double_float(), -3.5);
    for operation in ["copy", "subseq", "reverse"] {
        *input = packed_f64::make(3, 1.25).unwrap();
        packed_f64::set(*input, 0, -3.5);
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
            "{operation} must relocate its source"
        );
        assert!(packed_f64::is_vector(*result));
        let index = if operation == "reverse" { 2 } else { 0 };
        assert_eq!(packed_f64::get(*result, index), Some(-3.5));
        assert_eq!(packed_f64::get(*input, 0), Some(-3.5));
    }
    let vector_type = EgclVal::from_symbol_index(egcl_compiler::reader::intern_symbol("VECTOR"));
    // A fresh vector must relocate inside boxing, not before the operation.
    *input = packed_f64::make(3, 1.25).unwrap();
    packed_f64::set(*input, 0, -3.5);
    let before = input.to_raw();
    egcl_rt::rooted!(boxed = sequences::concatenate(vector_type, &[*input, *input]).unwrap());
    assert_ne!(
        input.to_raw(),
        before,
        "boxing must exercise a moving collection"
    );
    assert_eq!(sequences::length(*boxed).unwrap(), 6);
    for index in [0, 1, 2, 3, 5] {
        let expected = if index % 3 == 0 { -3.5 } else { 1.25 };
        assert_eq!(
            sequences::elt(*boxed, index).unwrap().as_double_float(),
            expected
        );
    }
    egcl_rt::rooted!(needle = egcl_rt::gc::alloc_double_float(-3.5));
    let found =
        sequences::find(*needle, *input, egcl_rt::value::NIL, None, 0, None, false).unwrap();
    assert_eq!(found.as_double_float(), -3.5);
    egcl_rt::rooted!(
        removed = sequences::remove(
            *needle,
            *input,
            egcl_rt::value::NIL,
            None,
            0,
            None,
            None,
            false
        )
        .unwrap()
    );
    assert_eq!(packed_f64::len(*removed), Some(2));
    assert_eq!(packed_f64::get(*removed, 0), Some(1.25));
    sequences::sort(*input, egcl_rt::value::NIL, None).unwrap();
    assert_eq!(packed_f64::get(*input, 2), Some(-3.5));

    // Registered pathname component strings still obey the ordinary heap
    // string representation; packed predicates must reject them safely.
    egcl_rt::rooted!(text = egcl_stdlib::make_lisp_string_fresh("array"));
    egcl_stdlib::register_string(*text, "array");
    assert!(!packed_f64::is_vector(*text));
    assert_eq!(sequences::length(*text).unwrap(), 5);
    egcl_rt::rooted!(copy = sequences::copy_seq(*text).unwrap());
    assert_eq!(copy.as_string(), "array");
    assert_eq!(sequences::elt(*text, 2).unwrap().as_char(), 'r');
    packed_array_wrappers_preserve_roots_and_storage();
}

fn packed_array_wrappers_preserve_roots_and_storage() {
    egcl_rt::rooted!(array = sequences::build_double_float_complex_vector(2, 2, true, 1.5, true).unwrap());
    let before = array.to_raw();
    assert_eq!(sequences::aref(*array, 1).unwrap().as_double_float(), 1.5);
    assert_ne!(before, array.to_raw(), "boxing must relocate the wrapper");
    egcl_rt::rooted!(value = egcl_rt::gc::alloc_double_float(7.25));
    assert_eq!(sequences::vector_push_extend(*array, *value, Some(1)).unwrap().as_fixnum(), 2);
    assert_eq!(sequences::cvec_capacity(*array), 3);
    assert_eq!(sequences::vector_pop(*array).unwrap().as_double_float(), 7.25);
    assert_eq!(sequences::length(*array).unwrap(), 2);
    egcl_rt::rooted!(view = sequences::build_double_float_displaced_vector(*array, 1, 2, 2, false, false).unwrap());
    assert_eq!(sequences::double_float_array_element(*view, 1), Some(7.25));
    sequences::set_aref(*view, 0, *value).unwrap();
    assert_eq!(sequences::double_float_array_element(*array, 1), Some(7.25));
    egcl_rt::rooted!(copy = sequences::copy_seq(*view).unwrap());
    assert!(packed_f64::is_vector(*copy));
    assert_eq!(packed_f64::get(*copy, 0), Some(7.25));
    sequences::adjust_complex_vector(*array, 12, Some(2), *value).unwrap();
    // Boxing after backing replacement forces another collection; an old
    // wrapper must remember the newly allocated backing array.
    assert_eq!(sequences::aref(*array, 11).unwrap().as_double_float(), 7.25);
    assert_eq!(sequences::aref(*view, 0).unwrap().as_double_float(), 7.25);
    assert!(sequences::set_aref(*view, 0, egcl_rt::value::NIL).is_err());
    assert!(sequences::aref(*view, 2).is_err());
    use egcl_stdlib::hashtable::{self, HashTest, MakeHashTableOptions};
    egcl_rt::rooted!(table = hashtable::make_hash_table(&MakeHashTableOptions {
        test: HashTest::Equalp, ..Default::default()
    }).unwrap());
    egcl_rt::rooted!(zero = packed_f64::make(2, -0.0).unwrap());
    hashtable::set_gethash(*zero, *table, EgclVal::from_fixnum(91)).unwrap();
    egcl_rt::rooted!(same = packed_f64::make(2, 0.0).unwrap());
    assert_eq!(hashtable::gethash(*same, *table, egcl_rt::value::NIL).unwrap(), (EgclVal::from_fixnum(91), true));
    egcl_rt::rooted!(general = egcl_stdlib::build_simple_vector(&[EgclVal::from_fixnum(0); 2]));
    assert_eq!(hashtable::gethash(*general, *table, egcl_rt::value::NIL).unwrap(), (EgclVal::from_fixnum(91), true));
    egcl_rt::rooted!(double = egcl_rt::gc::alloc_double_float(0.0));
    *general = egcl_stdlib::build_simple_vector(&[*double; 2]);
    assert_eq!(hashtable::gethash(*general, *table, egcl_rt::value::NIL).unwrap(), (EgclVal::from_fixnum(91), true));
    for dims in [vec![], vec![2, 3]] {
        egcl_rt::rooted!(matrix = sequences::build_double_float_md_array(&dims, -4.5).unwrap());
        assert!(sequences::is_double_float_array(*matrix));
        let size = dims.iter().product::<usize>();
        assert_eq!(sequences::array_total_size(*matrix), Some(size));
        assert_eq!(sequences::double_float_array_element(*matrix, size - 1), Some(-4.5));
        let printed = egcl_stdlib::format::format(egcl_rt::value::NIL, "~S", &[*matrix]).unwrap();
        assert!(printed.as_string().contains("-4.5"));
    }
}
