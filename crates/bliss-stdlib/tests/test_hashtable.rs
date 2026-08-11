//! Tests for bliss-stdlib hashtable module.

use bliss_rt::value::{BlissVal, NIL, T};
use bliss_stdlib::hashtable::*;

// ── HashTest enum ─────────────────────────────────────────────────

#[test]
fn hashtest_eq_clone_copy_debug() {
    assert_eq!(HashTest::Eq, HashTest::Eq);
    assert_ne!(HashTest::Eq, HashTest::Eql);
    assert_ne!(HashTest::Eq, HashTest::Equal);
    assert_ne!(HashTest::Eq, HashTest::Equalp);
    let cloned = HashTest::Eq;
    assert_eq!(HashTest::Eq, cloned);
    let copied = HashTest::Eq;
    assert_eq!(HashTest::Eq, copied);
    assert!(format!("{:?}", HashTest::Eq).contains("Eq"));
}

#[test]
fn hashtest_all_variants_distinct() {
    assert_ne!(HashTest::Eql, HashTest::Equal);
    assert_ne!(HashTest::Eql, HashTest::Equalp);
    assert_ne!(HashTest::Equal, HashTest::Equalp);
    assert_eq!(HashTest::Eql, HashTest::Eql);
    assert_eq!(HashTest::Equal, HashTest::Equal);
    assert_eq!(HashTest::Equalp, HashTest::Equalp);
    assert!(format!("{:?}", HashTest::Eql).contains("Eql"));
    assert!(format!("{:?}", HashTest::Equal).contains("Equal"));
    assert!(format!("{:?}", HashTest::Equalp).contains("Equalp"));
}

// ── Weakness enum ─────────────────────────────────────────────────

#[test]
fn weakness_eq_clone_copy_debug() {
    assert_eq!(Weakness::Key, Weakness::Key);
    assert_eq!(Weakness::Value, Weakness::Value);
    assert_eq!(Weakness::KeyAndValue, Weakness::KeyAndValue);
    assert_ne!(Weakness::Key, Weakness::Value);
    assert_ne!(Weakness::Key, Weakness::KeyAndValue);
    assert_ne!(Weakness::Value, Weakness::KeyAndValue);
    let cloned = Weakness::Key;
    assert_eq!(Weakness::Key, cloned);
    let copied = Weakness::Value;
    assert_eq!(Weakness::Value, copied);
    assert!(format!("{:?}", Weakness::Key).contains("Key"));
    assert!(format!("{:?}", Weakness::Value).contains("Value"));
    assert!(format!("{:?}", Weakness::KeyAndValue).contains("KeyAndValue"));
}

// ── MakeHashTableOptions defaults ─────────────────────────────────

#[test]
fn make_hash_table_options_defaults() {
    let opts = MakeHashTableOptions::default();
    assert_eq!(opts.test, HashTest::Eql, "Default test should be Eql");
    assert_eq!(opts.size, 16, "Default size should be 16");
    assert!((opts.rehash_size - 2.0).abs() < f64::EPSILON, "Default rehash_size should be 2.0");
    assert!((opts.rehash_threshold - 0.75).abs() < f64::EPSILON, "Default rehash_threshold should be 0.75");
    assert!(!opts.synchronized, "Default should not be synchronized");
    assert!(opts.weakness.is_none(), "Default weakness should be None");
}

// ── make_hash_table with default options ──────────────────────────

#[test]
fn make_hash_table_default_succeeds_empty() {
    let opts = MakeHashTableOptions::default();
    let ht = make_hash_table(&opts).expect("should create hash table");
    assert_eq!(hash_table_count(ht).unwrap(), 0);
    assert_eq!(hash_table_test(ht).unwrap(), HashTest::Eql);
}

// ── make_hash_table with custom options ───────────────────────────

#[test]
fn make_hash_table_each_test_function() {
    for test_fn in &[HashTest::Eq, HashTest::Eql, HashTest::Equal, HashTest::Equalp] {
        let opts = MakeHashTableOptions { test: *test_fn, ..MakeHashTableOptions::default() };
        let ht = make_hash_table(&opts).expect("should create hash table");
        assert_eq!(hash_table_test(ht).unwrap(), *test_fn);
    }
}

#[test]
fn make_hash_table_custom_size() {
    let opts = MakeHashTableOptions { size: 64, ..MakeHashTableOptions::default() };
    let ht = make_hash_table(&opts).expect("should create hash table");
    let size = hash_table_size(ht).unwrap();
    assert!(size >= 64, "Size should be >= requested 64, got: {}", size);
}

#[test]
fn make_hash_table_custom_rehash_params() {
    let opts = MakeHashTableOptions {
        rehash_size: 3.0, rehash_threshold: 0.5, ..MakeHashTableOptions::default()
    };
    let ht = make_hash_table(&opts).expect("should create hash table");
    assert!((hash_table_rehash_size(ht).unwrap() - 3.0).abs() < f64::EPSILON);
    assert!((hash_table_rehash_threshold(ht).unwrap() - 0.5).abs() < f64::EPSILON);
}

#[test]
fn make_hash_table_synchronized() {
    let opts = MakeHashTableOptions { synchronized: true, ..MakeHashTableOptions::default() };
    assert!(make_hash_table(&opts).is_ok());
}

#[test]
fn make_hash_table_weakness_variants() {
    for w in &[Weakness::Key, Weakness::Value, Weakness::KeyAndValue] {
        let opts = MakeHashTableOptions { weakness: Some(*w), ..MakeHashTableOptions::default() };
        assert!(make_hash_table(&opts).is_ok(), "Should create {:?} weak table", w);
    }
}

// ── gethash ───────────────────────────────────────────────────────

#[test]
fn gethash_existing_returns_value_and_true() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let key = BlissVal::from_fixnum(42);
    let val = BlissVal::from_fixnum(100);
    set_gethash(key, ht, val).unwrap();
    let (result, present) = gethash(key, ht, NIL).unwrap();
    assert!(present, "Key should be present");
    assert_eq!(result, val);
}

#[test]
fn gethash_missing_returns_default_and_false() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let default = BlissVal::from_fixnum(-1);
    let (result, present) = gethash(BlissVal::from_fixnum(999), ht, default).unwrap();
    assert!(!present);
    assert_eq!(result, default);
}

#[test]
fn gethash_missing_returns_nil_default() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let (result, present) = gethash(BlissVal::from_fixnum(1), ht, NIL).unwrap();
    assert!(!present);
    assert_eq!(result, NIL);
}

// ── set_gethash ───────────────────────────────────────────────────

#[test]
fn set_gethash_then_retrieve() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let key = BlissVal::from_fixnum(10);
    let val = BlissVal::from_fixnum(20);
    set_gethash(key, ht, val).unwrap();
    let (result, present) = gethash(key, ht, NIL).unwrap();
    assert!(present);
    assert_eq!(result, val);
}

#[test]
fn set_gethash_overwrites() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let key = BlissVal::from_fixnum(10);
    set_gethash(key, ht, BlissVal::from_fixnum(100)).unwrap();
    set_gethash(key, ht, BlissVal::from_fixnum(200)).unwrap();
    let (result, _) = gethash(key, ht, NIL).unwrap();
    assert_eq!(result, BlissVal::from_fixnum(200));
}

#[test]
fn set_gethash_multiple_keys() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    for i in 0..10 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i * 10)).unwrap();
    }
    for i in 0..10 {
        let (val, present) = gethash(BlissVal::from_fixnum(i), ht, NIL).unwrap();
        assert!(present, "Key {} should be present", i);
        assert_eq!(val, BlissVal::from_fixnum(i * 10));
    }
}

// ── remhash ───────────────────────────────────────────────────────

#[test]
fn remhash_existing_returns_true() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    set_gethash(BlissVal::from_fixnum(5), ht, BlissVal::from_fixnum(50)).unwrap();
    assert!(remhash(BlissVal::from_fixnum(5), ht).unwrap());
}

#[test]
fn remhash_missing_returns_false() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    assert!(!remhash(BlissVal::from_fixnum(999), ht).unwrap());
}

#[test]
fn remhash_makes_key_absent() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    set_gethash(BlissVal::from_fixnum(7), ht, BlissVal::from_fixnum(70)).unwrap();
    remhash(BlissVal::from_fixnum(7), ht).unwrap();
    let (_, present) = gethash(BlissVal::from_fixnum(7), ht, NIL).unwrap();
    assert!(!present);
}

// ── maphash ───────────────────────────────────────────────────────

#[test]
fn maphash_empty_table_succeeds() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    // Use T as a no-op function placeholder for empty iteration
    assert!(maphash(T, ht).is_ok());
}

#[test]
fn maphash_visits_all_entries() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    // Insert several entries
    let entries: Vec<(i64, i64)> = vec![(1, 10), (2, 20), (3, 30), (4, 40), (5, 50)];
    for &(k, v) in &entries {
        set_gethash(BlissVal::from_fixnum(k), ht, BlissVal::from_fixnum(v)).unwrap();
    }
    // maphash must accept a callable BlissVal and iterate all entries.
    // The implementation should invoke the function with each (key, value) pair.
    // We pass a BlissVal representing a function; even though we cannot construct
    // a Rust closure as a BlissVal without runtime support, we verify that maphash
    // completes without error and does not modify the table (all entries still present).
    // A full integration test with a real Lisp function will verify collection semantics.
    //
    // For now, pass T as a function placeholder — the implementation must iterate
    // all 5 entries. After maphash, all entries must still be present.
    maphash(T, ht).expect("maphash should succeed on non-empty table");

    // Verify table is unchanged — all entries still present
    assert_eq!(hash_table_count(ht).unwrap(), 5);
    for &(k, v) in &entries {
        let (result, present) = gethash(BlissVal::from_fixnum(k), ht, NIL).unwrap();
        assert!(present, "Key {} should still be present after maphash", k);
        assert_eq!(result, BlissVal::from_fixnum(v), "Value for key {} should be unchanged", k);
    }
}

// ── clrhash ───────────────────────────────────────────────────────

#[test]
fn clrhash_empties_table() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    for i in 0..5 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i * 10)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 5);
    clrhash(ht).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 0);
}

#[test]
fn clrhash_all_keys_absent() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    for i in 0..3 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i)).unwrap();
    }
    clrhash(ht).unwrap();
    for i in 0..3 {
        let (_, present) = gethash(BlissVal::from_fixnum(i), ht, NIL).unwrap();
        assert!(!present, "Key {} should be absent after clrhash", i);
    }
}

#[test]
fn clrhash_empty_table_ok() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    assert!(clrhash(ht).is_ok());
}

// ── hash_table_count ──────────────────────────────────────────────

#[test]
fn hash_table_count_tracks_inserts_and_removes() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 0);
    for i in 0..7 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i)).unwrap();
    }
    assert_eq!(hash_table_count(ht).unwrap(), 7);
    remhash(BlissVal::from_fixnum(2), ht).unwrap();
    remhash(BlissVal::from_fixnum(4), ht).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 5);
}

#[test]
fn hash_table_count_overwrite_no_increment() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let key = BlissVal::from_fixnum(1);
    set_gethash(key, ht, BlissVal::from_fixnum(10)).unwrap();
    set_gethash(key, ht, BlissVal::from_fixnum(20)).unwrap();
    assert_eq!(hash_table_count(ht).unwrap(), 1);
}

// ── hash_table_size ───────────────────────────────────────────────

#[test]
fn hash_table_size_ge_count() {
    let ht = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    for i in 0..10 {
        set_gethash(BlissVal::from_fixnum(i), ht, BlissVal::from_fixnum(i)).unwrap();
    }
    let size = hash_table_size(ht).unwrap();
    assert!(size >= hash_table_count(ht).unwrap());
}

#[test]
fn hash_table_size_ge_requested() {
    let opts = MakeHashTableOptions { size: 30, ..MakeHashTableOptions::default() };
    let ht = make_hash_table(&opts).unwrap();
    let size = hash_table_size(ht).unwrap();
    // The spec only requires size >= requested; the implementation may use
    // power-of-two, prime, or any other sizing strategy.
    assert!(size >= 30, "Capacity {} should be >= requested 30", size);
}

#[test]
fn hash_table_size_minimum_16() {
    let opts = MakeHashTableOptions { size: 1, ..MakeHashTableOptions::default() };
    let ht = make_hash_table(&opts).unwrap();
    assert!(hash_table_size(ht).unwrap() >= 16);
}

// ── hash_table_rehash_size / threshold ────────────────────────────

#[test]
fn hash_table_rehash_size_custom_and_default() {
    let ht_default = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    assert!((hash_table_rehash_size(ht_default).unwrap() - 2.0).abs() < f64::EPSILON);
    let opts = MakeHashTableOptions { rehash_size: 4.0, ..MakeHashTableOptions::default() };
    let ht_custom = make_hash_table(&opts).unwrap();
    assert!((hash_table_rehash_size(ht_custom).unwrap() - 4.0).abs() < f64::EPSILON);
}

#[test]
fn hash_table_rehash_threshold_custom_and_default() {
    let ht_default = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    assert!((hash_table_rehash_threshold(ht_default).unwrap() - 0.75).abs() < f64::EPSILON);
    let opts = MakeHashTableOptions { rehash_threshold: 0.9, ..MakeHashTableOptions::default() };
    let ht_custom = make_hash_table(&opts).unwrap();
    assert!((hash_table_rehash_threshold(ht_custom).unwrap() - 0.9).abs() < f64::EPSILON);
}

// ── sxhash ────────────────────────────────────────────────────────

#[test]
fn sxhash_consistent() {
    let val = BlissVal::from_fixnum(42);
    assert_eq!(sxhash(val), sxhash(val));
    // Equal fixnums must produce equal hashes (R5.155)
    assert_eq!(sxhash(BlissVal::from_fixnum(100)), sxhash(BlissVal::from_fixnum(100)));
}

#[test]
fn sxhash_returns_non_negative_fixnum() {
    let hash_val = sxhash(BlissVal::from_fixnum(42));
    assert!(hash_val.is_fixnum(), "sxhash should return a fixnum");
    assert!(hash_val.as_fixnum() >= 0, "sxhash should return non-negative");
}

#[test]
fn sxhash_nil_and_t() {
    let _ = sxhash(NIL); // should not panic
    let _ = sxhash(T);   // should not panic
}

// ── Error conditions ──────────────────────────────────────────────

#[test]
fn gethash_on_non_hash_table_errors() {
    let not_a_table = BlissVal::from_fixnum(42);
    assert!(gethash(BlissVal::from_fixnum(1), not_a_table, NIL).is_err());
}

#[test]
fn set_gethash_on_non_hash_table_errors() {
    let not_a_table = BlissVal::from_fixnum(42);
    assert!(set_gethash(BlissVal::from_fixnum(1), not_a_table, BlissVal::from_fixnum(2)).is_err());
}

#[test]
fn remhash_on_non_hash_table_errors() {
    assert!(remhash(BlissVal::from_fixnum(1), BlissVal::from_fixnum(42)).is_err());
}

#[test]
fn clrhash_on_non_hash_table_errors() {
    assert!(clrhash(BlissVal::from_fixnum(42)).is_err());
}

#[test]
fn hash_table_count_on_non_hash_table_errors() {
    assert!(hash_table_count(BlissVal::from_fixnum(42)).is_err());
}

#[test]
fn maphash_on_non_hash_table_errors() {
    assert!(maphash(NIL, BlissVal::from_fixnum(42)).is_err());
}

#[test]
fn make_hash_table_invalid_rehash_size_le_one() {
    // rehash_size <= 1.0 should error per R5.160
    for bad in &[0.5, 1.0, 0.0, -1.0] {
        let opts = MakeHashTableOptions { rehash_size: *bad, ..MakeHashTableOptions::default() };
        assert!(make_hash_table(&opts).is_err(), "rehash_size={} should error", bad);
    }
}

#[test]
fn make_hash_table_invalid_rehash_threshold() {
    // threshold must be in (0, 1] per R5.160
    for bad in &[0.0, -0.5, 1.5] {
        let opts = MakeHashTableOptions { rehash_threshold: *bad, ..MakeHashTableOptions::default() };
        assert!(make_hash_table(&opts).is_err(), "rehash_threshold={} should error", bad);
    }
}
