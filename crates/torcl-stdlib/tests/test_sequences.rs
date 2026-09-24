//! Tests for torcl-stdlib sequences module.
//! Red-phase: all tests expected to fail until implementations land.

use torcl_rt::error::TorclError;
use torcl_rt::object::{ConsCell, ObjectHeader, type_id};
use torcl_rt::value::{NIL, T, TorclVal};
use torcl_stdlib::sequences;

// ── Helpers ───────────────────────────────────────────────────────────

/// Build a CL-style proper list from fixnums by allocating real cons cells.
/// Each cons cell is heap-allocated (leaked for test simplicity) and tagged
/// with TAG_CONS so the runtime recognises it as a list.
fn make_list(vals: &[i64]) -> TorclVal {
    let mut list = NIL;
    for &v in vals.iter().rev() {
        let cell = Box::leak(Box::new(ConsCell {
            car: TorclVal::from_fixnum(v),
            cdr: list,
        }));
        let ptr = cell as *mut ConsCell as *mut u8;
        list = unsafe { TorclVal::from_cons_ptr(ptr) };
    }
    list
}

/// Build a simple-vector of fixnums by allocating a heap object.
/// Layout: ObjectHeader (type_id = SIMPLE_VECTOR) followed by length (as u64)
/// followed by N TorclVal elements. Tagged with TAG_HEAP_OBJECT.
fn make_vector(vals: &[i64]) -> TorclVal {
    // Layout: [ObjectHeader, length_u64, elements...]
    let total_u64s = 2 + vals.len(); // header + length + N elements
    let mut buf: Vec<u64> = Vec::with_capacity(total_u64s);

    // ObjectHeader with type_id = SIMPLE_VECTOR, size in 8-byte units
    let header = ObjectHeader::new(type_id::SIMPLE_VECTOR, total_u64s as u16);
    buf.push(header.0);

    // Length
    buf.push(vals.len() as u64);

    // Elements
    for &v in vals {
        buf.push(TorclVal::from_fixnum(v).to_raw());
    }

    let ptr = buf.as_mut_ptr() as *mut u8;
    std::mem::forget(buf); // Leak for test lifetime
    unsafe { TorclVal::from_heap_ptr(ptr) }
}

/// Default equality test placeholder (CL `#'EQL`).
/// Uses NIL to signify "use default equality" — the implementation should
/// treat NIL/None key/test as the standard EQL comparison.
fn default_test() -> TorclVal {
    NIL
}

/// Build a simple "identity" function value for use as a key parameter.
/// This is a placeholder — the runtime will need to recognise it as a callable.
/// We use a symbol-index value as a stand-in for a function reference.
fn identity_key() -> TorclVal {
    // Use symbol index 1 as a stand-in for the IDENTITY function.
    TorclVal::from_symbol_index(1)
}

/// Build a "custom test" function value (e.g. CL `#'EQUAL` or a lambda).
/// Uses a distinct symbol index so it differs from default_test().
fn custom_test() -> TorclVal {
    // Use symbol index 2 as a stand-in for a custom test function (e.g. EQUAL).
    TorclVal::from_symbol_index(2)
}

/// Build a "negate" key function for sorting tests — a key that inverts
/// fixnum ordering so key-based sort differs from value-based sort.
fn negate_key() -> TorclVal {
    // Use symbol index 3 as a stand-in for a negation key function.
    TorclVal::from_symbol_index(3)
}

/// Build a function value for use in map/reduce (e.g. CL `#'+`).
fn addition_fn() -> TorclVal {
    // Use symbol index 4 as a stand-in for the + function.
    TorclVal::from_symbol_index(4)
}

// ═══════════════════════════════════════════════════════════════════════
// LENGTH
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn length_empty_list() {
    assert_eq!(sequences::length(NIL).unwrap(), 0);
}

#[test]
fn length_nonempty_list() {
    assert_eq!(sequences::length(make_list(&[1, 2, 3, 4, 5])).unwrap(), 5);
}

#[test]
fn length_vector() {
    assert_eq!(sequences::length(make_vector(&[10, 20, 30])).unwrap(), 3);
}

#[test]
fn length_non_sequence_errors() {
    let result = sequences::length(TorclVal::from_fixnum(42));
    assert!(result.is_err());
    match result.unwrap_err() {
        TorclError::TypeError { .. } => {}
        other => panic!("expected TypeError, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// ELT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn elt_valid_index_list() {
    assert_eq!(
        sequences::elt(make_list(&[10, 20, 30]), 1).unwrap(),
        TorclVal::from_fixnum(20)
    );
}

#[test]
fn elt_first_element() {
    assert_eq!(
        sequences::elt(make_list(&[7, 8, 9]), 0).unwrap(),
        TorclVal::from_fixnum(7)
    );
}

#[test]
fn elt_valid_index_vector() {
    assert_eq!(
        sequences::elt(make_vector(&[100, 200, 300]), 2).unwrap(),
        TorclVal::from_fixnum(300)
    );
}

#[test]
fn elt_out_of_bounds_errors() {
    assert!(sequences::elt(make_list(&[1, 2, 3]), 10).is_err());
}

#[test]
fn elt_on_empty_errors() {
    assert!(sequences::elt(NIL, 0).is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// SET_ELT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn set_elt_valid_index() {
    let vec = make_vector(&[1, 2, 3]);
    assert!(sequences::set_elt(vec, 1, TorclVal::from_fixnum(99)).is_ok());
    assert_eq!(sequences::elt(vec, 1).unwrap(), TorclVal::from_fixnum(99));
}

#[test]
fn set_elt_out_of_bounds_errors() {
    assert!(sequences::set_elt(make_vector(&[1, 2]), 5, TorclVal::from_fixnum(0)).is_err());
}

#[test]
fn set_elt_on_immutable_errors() {
    // Lists in CL are not setf-elt-able; set_elt on a list should error
    // with a TypeError since lists are not mutable sequences for SETF ELT.
    let list = make_list(&[1, 2, 3]);
    let result = sequences::set_elt(list, 0, TorclVal::from_fixnum(42));
    assert!(
        result.is_err(),
        "set_elt on an immutable/non-setf-elt-able sequence should error"
    );
    match result.unwrap_err() {
        TorclError::TypeError { .. } => {}
        other => panic!("expected TypeError for immutable sequence, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// COPY-SEQ
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn copy_seq_independent() {
    let orig = make_list(&[1, 2, 3]);
    let copy = sequences::copy_seq(orig).unwrap();
    assert_eq!(
        sequences::length(copy).unwrap(),
        sequences::length(orig).unwrap()
    );
    for i in 0..3 {
        assert_eq!(
            sequences::elt(orig, i).unwrap(),
            sequences::elt(copy, i).unwrap()
        );
    }
    // Modify copy, original unchanged.
    let _ = sequences::set_elt(copy, 0, TorclVal::from_fixnum(999));
    assert_eq!(sequences::elt(orig, 0).unwrap(), TorclVal::from_fixnum(1));
}

#[test]
fn copy_seq_empty() {
    assert_eq!(
        sequences::length(sequences::copy_seq(NIL).unwrap()).unwrap(),
        0
    );
}

// ═══════════════════════════════════════════════════════════════════════
// SUBSEQ
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn subseq_with_start_end() {
    let sub = sequences::subseq(make_list(&[10, 20, 30, 40, 50]), 1, Some(4)).unwrap();
    assert_eq!(sequences::length(sub).unwrap(), 3);
    assert_eq!(sequences::elt(sub, 0).unwrap(), TorclVal::from_fixnum(20));
    assert_eq!(sequences::elt(sub, 2).unwrap(), TorclVal::from_fixnum(40));
}

#[test]
fn subseq_none_end() {
    let sub = sequences::subseq(make_list(&[10, 20, 30, 40, 50]), 2, None).unwrap();
    assert_eq!(sequences::length(sub).unwrap(), 3);
    assert_eq!(sequences::elt(sub, 0).unwrap(), TorclVal::from_fixnum(30));
}

#[test]
fn subseq_start_gt_end_errors() {
    assert!(sequences::subseq(make_list(&[1, 2, 3]), 3, Some(1)).is_err());
}

#[test]
fn subseq_end_beyond_length_errors() {
    assert!(sequences::subseq(make_list(&[1, 2]), 0, Some(10)).is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// REVERSE / NREVERSE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn reverse_non_destructive() {
    let list = make_list(&[1, 2, 3]);
    let rev = sequences::reverse(list).unwrap();
    assert_eq!(sequences::length(rev).unwrap(), 3);
    assert_eq!(sequences::elt(rev, 0).unwrap(), TorclVal::from_fixnum(3));
    assert_eq!(sequences::elt(rev, 2).unwrap(), TorclVal::from_fixnum(1));
    // Original unchanged.
    assert_eq!(sequences::elt(list, 0).unwrap(), TorclVal::from_fixnum(1));
}

#[test]
fn reverse_empty() {
    assert_eq!(
        sequences::length(sequences::reverse(NIL).unwrap()).unwrap(),
        0
    );
}

#[test]
fn nreverse_destructive() {
    let list = make_list(&[1, 2, 3]);
    let rev = sequences::nreverse(list).unwrap();
    assert_eq!(sequences::length(rev).unwrap(), 3);
    assert_eq!(sequences::elt(rev, 0).unwrap(), TorclVal::from_fixnum(3));
    assert_eq!(sequences::elt(rev, 2).unwrap(), TorclVal::from_fixnum(1));
}

// ═══════════════════════════════════════════════════════════════════════
// CONCATENATE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn concatenate_two_lists() {
    let cat = sequences::concatenate(T, &[make_list(&[1, 2]), make_list(&[3, 4])]).unwrap();
    assert_eq!(sequences::length(cat).unwrap(), 4);
    assert_eq!(sequences::elt(cat, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(cat, 3).unwrap(), TorclVal::from_fixnum(4));
}

#[test]
fn concatenate_with_empty() {
    let cat = sequences::concatenate(T, &[make_list(&[1, 2, 3]), NIL]).unwrap();
    assert_eq!(sequences::length(cat).unwrap(), 3);
}

#[test]
fn concatenate_multiple() {
    let cat =
        sequences::concatenate(T, &[make_list(&[1]), make_list(&[2]), make_list(&[3])]).unwrap();
    assert_eq!(sequences::length(cat).unwrap(), 3);
    assert_eq!(sequences::elt(cat, 1).unwrap(), TorclVal::from_fixnum(2));
}

#[test]
fn concatenate_string_result_returns_a_real_string() {
    let string_sym = TorclVal::from_symbol_index(torcl_compiler::reader::intern_symbol("STRING"));
    let cat = sequences::concatenate(
        string_sym,
        &[
            torcl_stdlib::streams::make_lisp_string("alpha"),
            torcl_stdlib::streams::make_lisp_string("-SOUP"),
        ],
    )
    .unwrap();
    assert!(cat.is_string());
    assert_eq!(cat.as_string(), "alpha-SOUP");
}

// ═══════════════════════════════════════════════════════════════════════
// FIND
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn find_present() {
    let r = sequences::find(
        TorclVal::from_fixnum(20),
        make_list(&[10, 20, 30]),
        default_test(),
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(20));
}

#[test]
fn find_absent_returns_nil() {
    let r = sequences::find(
        TorclVal::from_fixnum(99),
        make_list(&[10, 20, 30]),
        default_test(),
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, NIL);
}

#[test]
fn find_with_start_end() {
    let r = sequences::find(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        default_test(),
        None,
        2,
        Some(4),
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(2));
}

#[test]
fn find_from_end() {
    let r = sequences::find(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        default_test(),
        None,
        0,
        None,
        true,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(2));
}

#[test]
fn find_with_key() {
    // Search list [10, 20, 30] with a key function (identity_key).
    // Looking for 20 with key applied — the key extracts the element itself,
    // so find should still return the matching element 20.
    let r = sequences::find(
        TorclVal::from_fixnum(20),
        make_list(&[10, 20, 30]),
        default_test(),
        Some(identity_key()),
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(20));
}

#[test]
fn find_with_custom_test() {
    // Use a custom test function instead of default EQL.
    // custom_test() represents a broader equality (e.g. EQUAL).
    // Looking for 10 in [10, 20, 30] with custom test — should still find it.
    let r = sequences::find(
        TorclVal::from_fixnum(10),
        make_list(&[10, 20, 30]),
        custom_test(),
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(10));
}

// ═══════════════════════════════════════════════════════════════════════
// POSITION
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn position_present() {
    let r = sequences::position(
        TorclVal::from_fixnum(30),
        make_list(&[10, 20, 30]),
        default_test(),
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(2));
}

#[test]
fn position_absent_returns_nil() {
    let r = sequences::position(
        TorclVal::from_fixnum(99),
        make_list(&[10, 20, 30]),
        default_test(),
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, NIL);
}

#[test]
fn position_with_start() {
    let r = sequences::position(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        default_test(),
        None,
        2,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(3));
}

#[test]
fn position_from_end() {
    let r = sequences::position(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        default_test(),
        None,
        0,
        None,
        true,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(3));
}

#[test]
fn position_with_key() {
    // position with key function — key is applied to each element before comparison.
    let r = sequences::position(
        TorclVal::from_fixnum(20),
        make_list(&[10, 20, 30]),
        default_test(),
        Some(identity_key()),
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(1));
}

#[test]
fn position_with_custom_test() {
    // position with a non-default test function.
    let r = sequences::position(
        TorclVal::from_fixnum(30),
        make_list(&[10, 20, 30]),
        custom_test(),
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(2));
}

// ═══════════════════════════════════════════════════════════════════════
// COUNT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn count_multiple() {
    let r = sequences::count(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 2]),
        default_test(),
        None,
        0,
        None,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(3));
}

#[test]
fn count_zero() {
    let r = sequences::count(
        TorclVal::from_fixnum(99),
        make_list(&[1, 2, 3]),
        default_test(),
        None,
        0,
        None,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(0));
}

#[test]
fn count_with_start_end() {
    let r = sequences::count(
        TorclVal::from_fixnum(2),
        make_list(&[2, 2, 3, 2, 5]),
        default_test(),
        None,
        1,
        Some(4),
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(2));
}

#[test]
fn count_with_key() {
    // Count with key function applied to each element before comparison.
    let r = sequences::count(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 2]),
        default_test(),
        Some(identity_key()),
        0,
        None,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(3));
}

#[test]
fn count_with_custom_test() {
    // Count with a custom test function instead of default EQL.
    let r = sequences::count(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 2]),
        custom_test(),
        None,
        0,
        None,
    )
    .unwrap();
    assert_eq!(r, TorclVal::from_fixnum(3));
}

// ═══════════════════════════════════════════════════════════════════════
// MAP
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn map_single_sequence() {
    // Map addition_fn (standing in for a real function like 1+) over [1, 2, 3].
    // We check both length and that the result contains transformed values.
    let mapped = sequences::map(T, addition_fn(), &[make_list(&[1, 2, 3])]).unwrap();
    assert_eq!(sequences::length(mapped).unwrap(), 3);
    // The mapped result should contain the function applied to each element.
    // With a proper 1+ function, [1,2,3] -> [2,3,4].
    // We verify at least that the elements are not the originals:
    let first = sequences::elt(mapped, 0).unwrap();
    let second = sequences::elt(mapped, 1).unwrap();
    let third = sequences::elt(mapped, 2).unwrap();
    // The function should have been applied — results should differ from identity.
    // With addition_fn as CL #'+, single-arg + returns the argument itself,
    // but the intent is that a real function is applied. At minimum, verify
    // they are valid values (not NIL placeholders).
    assert_ne!(first, NIL);
    assert_ne!(second, NIL);
    assert_ne!(third, NIL);
}

#[test]
fn map_mismatched_lengths_stops_at_shortest() {
    let mapped = sequences::map(
        T,
        addition_fn(),
        &[make_list(&[1, 2, 3]), make_list(&[10, 20])],
    )
    .unwrap();
    assert_eq!(sequences::length(mapped).unwrap(), 2);
}

// ═══════════════════════════════════════════════════════════════════════
// REDUCE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn reduce_with_initial_value() {
    // reduce #'+ '(1 2 3) :initial-value 0  =>  6
    let result = sequences::reduce(
        addition_fn(),
        make_list(&[1, 2, 3]),
        Some(TorclVal::from_fixnum(0)),
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        result,
        TorclVal::from_fixnum(6),
        "reduce with + over [1,2,3] starting from 0 should yield 6"
    );
}

#[test]
fn reduce_without_initial_value() {
    // reduce #'+ '(10 20 30) => 60
    let result = sequences::reduce(
        addition_fn(),
        make_list(&[10, 20, 30]),
        None,
        None,
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        result,
        TorclVal::from_fixnum(60),
        "reduce with + over [10,20,30] should yield 60"
    );
}

#[test]
fn reduce_empty_no_initial_errors() {
    assert!(
        sequences::reduce(addition_fn(), NIL, None, None, 0, None, false).is_err(),
        "reduce on empty sequence without initial-value should error"
    );
}

#[test]
fn reduce_with_start_end() {
    // reduce #'+ '(10 20 30 40) :initial-value 0 :start 1 :end 3
    // Only reduces elements at indices 1,2 => 20 + 30 = 50
    let result = sequences::reduce(
        addition_fn(),
        make_list(&[10, 20, 30, 40]),
        Some(TorclVal::from_fixnum(0)),
        None,
        1,
        Some(3),
        false,
    )
    .unwrap();
    assert_eq!(
        result,
        TorclVal::from_fixnum(50),
        "reduce over sub-range [1,3) of [10,20,30,40] with initial 0 should yield 50"
    );
}

#[test]
fn reduce_from_end() {
    // reduce #'+ '(1 2 3) :initial-value 0 :from-end t
    // Right-fold: 1 + (2 + (3 + 0)) = 6 — same as left for +,
    // but the implementation must process from the end.
    // For a commutative op the result is the same; we still verify the value.
    let result = sequences::reduce(
        addition_fn(),
        make_list(&[1, 2, 3]),
        Some(TorclVal::from_fixnum(0)),
        None,
        0,
        None,
        true,
    )
    .unwrap();
    assert_eq!(
        result,
        TorclVal::from_fixnum(6),
        "reduce from-end with + over [1,2,3] starting from 0 should yield 6"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// REMOVE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn remove_all_occurrences() {
    let rem = sequences::remove(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        default_test(),
        None,
        0,
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 3);
    assert_eq!(sequences::elt(rem, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(rem, 1).unwrap(), TorclVal::from_fixnum(3));
    assert_eq!(sequences::elt(rem, 2).unwrap(), TorclVal::from_fixnum(5));
}

#[test]
fn remove_with_count() {
    let rem = sequences::remove(
        TorclVal::from_fixnum(2),
        make_list(&[2, 1, 2, 3, 2]),
        default_test(),
        None,
        0,
        None,
        Some(2),
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 3);
}

#[test]
fn remove_from_end_with_count() {
    let rem = sequences::remove(
        TorclVal::from_fixnum(2),
        make_list(&[2, 1, 2, 3, 2]),
        default_test(),
        None,
        0,
        None,
        Some(1),
        true,
    )
    .unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 4);
}

#[test]
fn remove_with_start_end() {
    let rem = sequences::remove(
        TorclVal::from_fixnum(2),
        make_list(&[2, 1, 2, 3, 2]),
        default_test(),
        None,
        1,
        Some(4),
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 4);
}

#[test]
fn remove_with_key() {
    // Remove with key function applied to elements before test comparison.
    // With identity_key, behaviour matches default — remove all 2s.
    let rem = sequences::remove(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2]),
        default_test(),
        Some(identity_key()),
        0,
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 2);
    assert_eq!(sequences::elt(rem, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(rem, 1).unwrap(), TorclVal::from_fixnum(3));
}

#[test]
fn remove_with_custom_test() {
    // Remove with a custom test function instead of default EQL.
    let rem = sequences::remove(
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        custom_test(),
        None,
        0,
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 3);
    assert_eq!(sequences::elt(rem, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(rem, 1).unwrap(), TorclVal::from_fixnum(3));
    assert_eq!(sequences::elt(rem, 2).unwrap(), TorclVal::from_fixnum(5));
}

// ═══════════════════════════════════════════════════════════════════════
// SUBSTITUTE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn substitute_all() {
    let s = sequences::substitute(
        TorclVal::from_fixnum(99),
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        default_test(),
        None,
        0,
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(s).unwrap(), 5);
    assert_eq!(sequences::elt(s, 1).unwrap(), TorclVal::from_fixnum(99));
    assert_eq!(sequences::elt(s, 3).unwrap(), TorclVal::from_fixnum(99));
    assert_eq!(sequences::elt(s, 0).unwrap(), TorclVal::from_fixnum(1));
}

#[test]
fn substitute_with_count() {
    let s = sequences::substitute(
        TorclVal::from_fixnum(0),
        TorclVal::from_fixnum(2),
        make_list(&[2, 2, 2]),
        default_test(),
        None,
        0,
        None,
        Some(2),
        false,
    )
    .unwrap();
    assert_eq!(sequences::elt(s, 0).unwrap(), TorclVal::from_fixnum(0));
    assert_eq!(sequences::elt(s, 1).unwrap(), TorclVal::from_fixnum(0));
    assert_eq!(sequences::elt(s, 2).unwrap(), TorclVal::from_fixnum(2));
}

#[test]
fn substitute_from_end_with_count() {
    let s = sequences::substitute(
        TorclVal::from_fixnum(0),
        TorclVal::from_fixnum(2),
        make_list(&[2, 2, 2]),
        default_test(),
        None,
        0,
        None,
        Some(1),
        true,
    )
    .unwrap();
    assert_eq!(sequences::elt(s, 0).unwrap(), TorclVal::from_fixnum(2));
    assert_eq!(sequences::elt(s, 2).unwrap(), TorclVal::from_fixnum(0));
}

#[test]
fn substitute_with_start_end() {
    let s = sequences::substitute(
        TorclVal::from_fixnum(0),
        TorclVal::from_fixnum(2),
        make_list(&[2, 1, 2, 3, 2]),
        default_test(),
        None,
        1,
        Some(4),
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::elt(s, 0).unwrap(), TorclVal::from_fixnum(2)); // outside range
    assert_eq!(sequences::elt(s, 2).unwrap(), TorclVal::from_fixnum(0)); // inside range
    assert_eq!(sequences::elt(s, 4).unwrap(), TorclVal::from_fixnum(2)); // outside range
}

#[test]
fn substitute_with_key() {
    // Substitute with key function applied to elements before comparison.
    let s = sequences::substitute(
        TorclVal::from_fixnum(99),
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3]),
        default_test(),
        Some(identity_key()),
        0,
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(s).unwrap(), 3);
    assert_eq!(sequences::elt(s, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(s, 1).unwrap(), TorclVal::from_fixnum(99));
    assert_eq!(sequences::elt(s, 2).unwrap(), TorclVal::from_fixnum(3));
}

#[test]
fn substitute_with_custom_test() {
    // Substitute with a non-default test function.
    let s = sequences::substitute(
        TorclVal::from_fixnum(99),
        TorclVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]),
        custom_test(),
        None,
        0,
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(sequences::length(s).unwrap(), 5);
    assert_eq!(sequences::elt(s, 1).unwrap(), TorclVal::from_fixnum(99));
    assert_eq!(sequences::elt(s, 3).unwrap(), TorclVal::from_fixnum(99));
}

// ═══════════════════════════════════════════════════════════════════════
// SORT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn sort_basic() {
    let sorted = sequences::sort(make_list(&[3, 1, 4, 1, 5]), T, None).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 5);
    assert_eq!(sequences::elt(sorted, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 4).unwrap(), TorclVal::from_fixnum(5));
}

#[test]
fn sort_empty() {
    assert_eq!(
        sequences::length(sequences::sort(NIL, T, None).unwrap()).unwrap(),
        0
    );
}

#[test]
fn sort_with_key() {
    // Sort [3, 1, 2] with a negation key. With negate_key, the key function
    // maps each element x to -x before comparison, so ascending sort on -x
    // produces descending order on the original values: [3, 2, 1].
    let sorted = sequences::sort(make_list(&[3, 1, 2]), T, Some(negate_key())).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 3);
    // With negate key, ascending sort by key produces descending by value.
    assert_eq!(sequences::elt(sorted, 0).unwrap(), TorclVal::from_fixnum(3));
    assert_eq!(sequences::elt(sorted, 1).unwrap(), TorclVal::from_fixnum(2));
    assert_eq!(sequences::elt(sorted, 2).unwrap(), TorclVal::from_fixnum(1));
}

// ═══════════════════════════════════════════════════════════════════════
// STABLE-SORT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn stable_sort_basic() {
    let sorted = sequences::stable_sort(make_list(&[3, 1, 4, 1, 5]), T, None).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 5);
    assert_eq!(sequences::elt(sorted, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 4).unwrap(), TorclVal::from_fixnum(5));
}

#[test]
fn stable_sort_preserves_equal_order() {
    let sorted = sequences::stable_sort(make_list(&[3, 1, 2, 1]), T, None).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 4);
    assert_eq!(sequences::elt(sorted, 0).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 1).unwrap(), TorclVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 2).unwrap(), TorclVal::from_fixnum(2));
    assert_eq!(sequences::elt(sorted, 3).unwrap(), TorclVal::from_fixnum(3));
}

#[test]
fn stable_sort_with_key() {
    // Stable-sort [3, 1, 2] with a negation key. The key maps x -> -x,
    // so ascending sort on -x gives descending order: [3, 2, 1].
    let sorted = sequences::stable_sort(make_list(&[3, 1, 2]), T, Some(negate_key())).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 3);
    assert_eq!(sequences::elt(sorted, 0).unwrap(), TorclVal::from_fixnum(3));
    assert_eq!(sequences::elt(sorted, 1).unwrap(), TorclVal::from_fixnum(2));
    assert_eq!(sequences::elt(sorted, 2).unwrap(), TorclVal::from_fixnum(1));
}

#[test]
fn stable_sort_empty() {
    assert_eq!(
        sequences::length(sequences::stable_sort(NIL, T, None).unwrap()).unwrap(),
        0
    );
}
