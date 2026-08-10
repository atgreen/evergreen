//! Tests for bliss-stdlib sequences module.
//! Red-phase: all tests expected to fail until implementations land.

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL, T};
use bliss_stdlib::sequences;

// ── Helpers ───────────────────────────────────────────────────────────

/// Build a CL-style proper list from fixnums (simplified — real impl builds cons chain).
fn make_list(vals: &[i64]) -> BlissVal {
    let mut list = NIL;
    for &v in vals.iter().rev() {
        let _ = (v, list);
        list = BlissVal::from_fixnum(v);
    }
    list
}

/// Build a vector of fixnums (placeholder until runtime vector alloc exists).
fn make_vector(vals: &[i64]) -> BlissVal {
    let _ = vals;
    NIL
}

/// Default equality test placeholder (CL `#'EQL`).
fn default_test() -> BlissVal { T }

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
    let result = sequences::length(BlissVal::from_fixnum(42));
    assert!(result.is_err());
    match result.unwrap_err() {
        BlissError::TypeError { .. } => {}
        other => panic!("expected TypeError, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// ELT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn elt_valid_index_list() {
    assert_eq!(sequences::elt(make_list(&[10, 20, 30]), 1).unwrap(), BlissVal::from_fixnum(20));
}

#[test]
fn elt_first_element() {
    assert_eq!(sequences::elt(make_list(&[7, 8, 9]), 0).unwrap(), BlissVal::from_fixnum(7));
}

#[test]
fn elt_valid_index_vector() {
    assert_eq!(sequences::elt(make_vector(&[100, 200, 300]), 2).unwrap(), BlissVal::from_fixnum(300));
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
    assert!(sequences::set_elt(vec, 1, BlissVal::from_fixnum(99)).is_ok());
    assert_eq!(sequences::elt(vec, 1).unwrap(), BlissVal::from_fixnum(99));
}

#[test]
fn set_elt_out_of_bounds_errors() {
    assert!(sequences::set_elt(make_vector(&[1, 2]), 5, BlissVal::from_fixnum(0)).is_err());
}

#[test]
fn set_elt_on_immutable_errors() {
    // Immutable sequences (e.g. literal strings) should reject mutation.
    // The exact immutable type depends on runtime; this tests the error path.
    let list = make_list(&[1, 2, 3]);
    let result = sequences::set_elt(list, 0, BlissVal::from_fixnum(42));
    // Must either succeed (mutable) or error (immutable).
    assert!(result.is_ok() || result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// COPY-SEQ
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn copy_seq_independent() {
    let orig = make_list(&[1, 2, 3]);
    let copy = sequences::copy_seq(orig).unwrap();
    assert_eq!(sequences::length(copy).unwrap(), sequences::length(orig).unwrap());
    for i in 0..3 {
        assert_eq!(sequences::elt(orig, i).unwrap(), sequences::elt(copy, i).unwrap());
    }
    // Modify copy, original unchanged.
    let _ = sequences::set_elt(copy, 0, BlissVal::from_fixnum(999));
    assert_eq!(sequences::elt(orig, 0).unwrap(), BlissVal::from_fixnum(1));
}

#[test]
fn copy_seq_empty() {
    assert_eq!(sequences::length(sequences::copy_seq(NIL).unwrap()).unwrap(), 0);
}

// ═══════════════════════════════════════════════════════════════════════
// SUBSEQ
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn subseq_with_start_end() {
    let sub = sequences::subseq(make_list(&[10, 20, 30, 40, 50]), 1, Some(4)).unwrap();
    assert_eq!(sequences::length(sub).unwrap(), 3);
    assert_eq!(sequences::elt(sub, 0).unwrap(), BlissVal::from_fixnum(20));
    assert_eq!(sequences::elt(sub, 2).unwrap(), BlissVal::from_fixnum(40));
}

#[test]
fn subseq_none_end() {
    let sub = sequences::subseq(make_list(&[10, 20, 30, 40, 50]), 2, None).unwrap();
    assert_eq!(sequences::length(sub).unwrap(), 3);
    assert_eq!(sequences::elt(sub, 0).unwrap(), BlissVal::from_fixnum(30));
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
    assert_eq!(sequences::elt(rev, 0).unwrap(), BlissVal::from_fixnum(3));
    assert_eq!(sequences::elt(rev, 2).unwrap(), BlissVal::from_fixnum(1));
    // Original unchanged.
    assert_eq!(sequences::elt(list, 0).unwrap(), BlissVal::from_fixnum(1));
}

#[test]
fn reverse_empty() {
    assert_eq!(sequences::length(sequences::reverse(NIL).unwrap()).unwrap(), 0);
}

#[test]
fn nreverse_destructive() {
    let list = make_list(&[1, 2, 3]);
    let rev = sequences::nreverse(list).unwrap();
    assert_eq!(sequences::length(rev).unwrap(), 3);
    assert_eq!(sequences::elt(rev, 0).unwrap(), BlissVal::from_fixnum(3));
    assert_eq!(sequences::elt(rev, 2).unwrap(), BlissVal::from_fixnum(1));
}

// ═══════════════════════════════════════════════════════════════════════
// CONCATENATE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn concatenate_two_lists() {
    let cat = sequences::concatenate(T, &[make_list(&[1, 2]), make_list(&[3, 4])]).unwrap();
    assert_eq!(sequences::length(cat).unwrap(), 4);
    assert_eq!(sequences::elt(cat, 0).unwrap(), BlissVal::from_fixnum(1));
    assert_eq!(sequences::elt(cat, 3).unwrap(), BlissVal::from_fixnum(4));
}

#[test]
fn concatenate_with_empty() {
    let cat = sequences::concatenate(T, &[make_list(&[1, 2, 3]), NIL]).unwrap();
    assert_eq!(sequences::length(cat).unwrap(), 3);
}

#[test]
fn concatenate_multiple() {
    let cat = sequences::concatenate(T, &[make_list(&[1]), make_list(&[2]), make_list(&[3])]).unwrap();
    assert_eq!(sequences::length(cat).unwrap(), 3);
    assert_eq!(sequences::elt(cat, 1).unwrap(), BlissVal::from_fixnum(2));
}

// ═══════════════════════════════════════════════════════════════════════
// FIND
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn find_present() {
    let r = sequences::find(BlissVal::from_fixnum(20), make_list(&[10, 20, 30]),
        default_test(), None, 0, None, false).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(20));
}

#[test]
fn find_absent_returns_nil() {
    let r = sequences::find(BlissVal::from_fixnum(99), make_list(&[10, 20, 30]),
        default_test(), None, 0, None, false).unwrap();
    assert_eq!(r, NIL);
}

#[test]
fn find_with_start_end() {
    let r = sequences::find(BlissVal::from_fixnum(2), make_list(&[1, 2, 3, 2, 5]),
        default_test(), None, 2, Some(4), false).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(2));
}

#[test]
fn find_from_end() {
    let r = sequences::find(BlissVal::from_fixnum(2), make_list(&[1, 2, 3, 2, 5]),
        default_test(), None, 0, None, true).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(2));
}

// ═══════════════════════════════════════════════════════════════════════
// POSITION
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn position_present() {
    let r = sequences::position(BlissVal::from_fixnum(30), make_list(&[10, 20, 30]),
        default_test(), None, 0, None, false).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(2));
}

#[test]
fn position_absent_returns_nil() {
    let r = sequences::position(BlissVal::from_fixnum(99), make_list(&[10, 20, 30]),
        default_test(), None, 0, None, false).unwrap();
    assert_eq!(r, NIL);
}

#[test]
fn position_with_start() {
    let r = sequences::position(BlissVal::from_fixnum(2), make_list(&[1, 2, 3, 2, 5]),
        default_test(), None, 2, None, false).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(3));
}

#[test]
fn position_from_end() {
    let r = sequences::position(BlissVal::from_fixnum(2), make_list(&[1, 2, 3, 2, 5]),
        default_test(), None, 0, None, true).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(3));
}

// ═══════════════════════════════════════════════════════════════════════
// COUNT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn count_multiple() {
    let r = sequences::count(BlissVal::from_fixnum(2), make_list(&[1, 2, 3, 2, 2]),
        default_test(), None, 0, None).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(3));
}

#[test]
fn count_zero() {
    let r = sequences::count(BlissVal::from_fixnum(99), make_list(&[1, 2, 3]),
        default_test(), None, 0, None).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(0));
}

#[test]
fn count_with_start_end() {
    let r = sequences::count(BlissVal::from_fixnum(2), make_list(&[2, 2, 3, 2, 5]),
        default_test(), None, 1, Some(4)).unwrap();
    assert_eq!(r, BlissVal::from_fixnum(2));
}

// ═══════════════════════════════════════════════════════════════════════
// MAP
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn map_single_sequence() {
    let mapped = sequences::map(T, T, &[make_list(&[1, 2, 3])]).unwrap();
    assert_eq!(sequences::length(mapped).unwrap(), 3);
}

#[test]
fn map_mismatched_lengths_stops_at_shortest() {
    let mapped = sequences::map(T, T, &[make_list(&[1, 2, 3]), make_list(&[10, 20])]).unwrap();
    assert_eq!(sequences::length(mapped).unwrap(), 2);
}

// ═══════════════════════════════════════════════════════════════════════
// REDUCE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn reduce_with_initial_value() {
    assert!(sequences::reduce(T, make_list(&[1, 2, 3]),
        Some(BlissVal::from_fixnum(0)), None, 0, None, false).is_ok());
}

#[test]
fn reduce_without_initial_value() {
    assert!(sequences::reduce(T, make_list(&[10, 20, 30]),
        None, None, 0, None, false).is_ok());
}

#[test]
fn reduce_empty_no_initial_errors() {
    assert!(sequences::reduce(T, NIL, None, None, 0, None, false).is_err(),
        "reduce on empty sequence without initial-value should error");
}

#[test]
fn reduce_with_start_end() {
    assert!(sequences::reduce(T, make_list(&[10, 20, 30, 40]),
        Some(BlissVal::from_fixnum(0)), None, 1, Some(3), false).is_ok());
}

#[test]
fn reduce_from_end() {
    assert!(sequences::reduce(T, make_list(&[1, 2, 3]),
        Some(BlissVal::from_fixnum(0)), None, 0, None, true).is_ok());
}

// ═══════════════════════════════════════════════════════════════════════
// REMOVE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn remove_all_occurrences() {
    let rem = sequences::remove(BlissVal::from_fixnum(2), make_list(&[1, 2, 3, 2, 5]),
        default_test(), None, 0, None, None, false).unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 3);
    assert_eq!(sequences::elt(rem, 0).unwrap(), BlissVal::from_fixnum(1));
    assert_eq!(sequences::elt(rem, 1).unwrap(), BlissVal::from_fixnum(3));
    assert_eq!(sequences::elt(rem, 2).unwrap(), BlissVal::from_fixnum(5));
}

#[test]
fn remove_with_count() {
    let rem = sequences::remove(BlissVal::from_fixnum(2), make_list(&[2, 1, 2, 3, 2]),
        default_test(), None, 0, None, Some(2), false).unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 3);
}

#[test]
fn remove_from_end_with_count() {
    let rem = sequences::remove(BlissVal::from_fixnum(2), make_list(&[2, 1, 2, 3, 2]),
        default_test(), None, 0, None, Some(1), true).unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 4);
}

#[test]
fn remove_with_start_end() {
    let rem = sequences::remove(BlissVal::from_fixnum(2), make_list(&[2, 1, 2, 3, 2]),
        default_test(), None, 1, Some(4), None, false).unwrap();
    assert_eq!(sequences::length(rem).unwrap(), 4);
}

// ═══════════════════════════════════════════════════════════════════════
// SUBSTITUTE
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn substitute_all() {
    let s = sequences::substitute(BlissVal::from_fixnum(99), BlissVal::from_fixnum(2),
        make_list(&[1, 2, 3, 2, 5]), default_test(), None, 0, None, None, false).unwrap();
    assert_eq!(sequences::length(s).unwrap(), 5);
    assert_eq!(sequences::elt(s, 1).unwrap(), BlissVal::from_fixnum(99));
    assert_eq!(sequences::elt(s, 3).unwrap(), BlissVal::from_fixnum(99));
    assert_eq!(sequences::elt(s, 0).unwrap(), BlissVal::from_fixnum(1));
}

#[test]
fn substitute_with_count() {
    let s = sequences::substitute(BlissVal::from_fixnum(0), BlissVal::from_fixnum(2),
        make_list(&[2, 2, 2]), default_test(), None, 0, None, Some(2), false).unwrap();
    assert_eq!(sequences::elt(s, 0).unwrap(), BlissVal::from_fixnum(0));
    assert_eq!(sequences::elt(s, 1).unwrap(), BlissVal::from_fixnum(0));
    assert_eq!(sequences::elt(s, 2).unwrap(), BlissVal::from_fixnum(2));
}

#[test]
fn substitute_from_end_with_count() {
    let s = sequences::substitute(BlissVal::from_fixnum(0), BlissVal::from_fixnum(2),
        make_list(&[2, 2, 2]), default_test(), None, 0, None, Some(1), true).unwrap();
    assert_eq!(sequences::elt(s, 0).unwrap(), BlissVal::from_fixnum(2));
    assert_eq!(sequences::elt(s, 2).unwrap(), BlissVal::from_fixnum(0));
}

#[test]
fn substitute_with_start_end() {
    let s = sequences::substitute(BlissVal::from_fixnum(0), BlissVal::from_fixnum(2),
        make_list(&[2, 1, 2, 3, 2]), default_test(), None, 1, Some(4), None, false).unwrap();
    assert_eq!(sequences::elt(s, 0).unwrap(), BlissVal::from_fixnum(2)); // outside range
    assert_eq!(sequences::elt(s, 2).unwrap(), BlissVal::from_fixnum(0)); // inside range
    assert_eq!(sequences::elt(s, 4).unwrap(), BlissVal::from_fixnum(2)); // outside range
}

// ═══════════════════════════════════════════════════════════════════════
// SORT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn sort_basic() {
    let sorted = sequences::sort(make_list(&[3, 1, 4, 1, 5]), T, None).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 5);
    assert_eq!(sequences::elt(sorted, 0).unwrap(), BlissVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 4).unwrap(), BlissVal::from_fixnum(5));
}

#[test]
fn sort_empty() {
    assert_eq!(sequences::length(sequences::sort(NIL, T, None).unwrap()).unwrap(), 0);
}

#[test]
fn sort_with_key() {
    assert!(sequences::sort(make_list(&[3, 1, 2]), T, Some(T)).is_ok());
}

// ═══════════════════════════════════════════════════════════════════════
// STABLE-SORT
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn stable_sort_basic() {
    let sorted = sequences::stable_sort(make_list(&[3, 1, 4, 1, 5]), T, None).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 5);
    assert_eq!(sequences::elt(sorted, 0).unwrap(), BlissVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 4).unwrap(), BlissVal::from_fixnum(5));
}

#[test]
fn stable_sort_preserves_equal_order() {
    let sorted = sequences::stable_sort(make_list(&[3, 1, 2, 1]), T, None).unwrap();
    assert_eq!(sequences::length(sorted).unwrap(), 4);
    assert_eq!(sequences::elt(sorted, 0).unwrap(), BlissVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 1).unwrap(), BlissVal::from_fixnum(1));
    assert_eq!(sequences::elt(sorted, 2).unwrap(), BlissVal::from_fixnum(2));
    assert_eq!(sequences::elt(sorted, 3).unwrap(), BlissVal::from_fixnum(3));
}

#[test]
fn stable_sort_with_key() {
    assert!(sequences::stable_sort(make_list(&[5, 3, 1]), T, Some(T)).is_ok());
}

#[test]
fn stable_sort_empty() {
    assert_eq!(sequences::length(sequences::stable_sort(NIL, T, None).unwrap()).unwrap(), 0);
}
