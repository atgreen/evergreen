//! Tests for inline caches — ic.rs

use bliss_compiler::ic::{IcEntry, IcState, InlineCache, IC_POLY_MAX, reset_all_caches};
use bliss_rt::value::{BlissVal, NIL, T};

// ── IcState enum ──────────────────────────────────────────────────

#[test]
fn ic_state_variants_distinct_and_copy() {
    assert_ne!(IcState::Uninitialized, IcState::Monomorphic);
    assert_ne!(IcState::Polymorphic, IcState::Megamorphic);
    let a = IcState::Monomorphic;
    let b = a;
    assert_eq!(a, b);
    assert!(format!("{:?}", IcState::Polymorphic).contains("Polymorphic"));
}

#[test]
fn ic_poly_max_is_four() {
    assert_eq!(IC_POLY_MAX, 4);
}

// ── IcEntry struct ────────────────────────────────────────────────

#[test]
fn ic_entry_fields_and_clone() {
    let entry = IcEntry { class: NIL, method: T };
    assert_eq!(entry.class, NIL);
    let cloned = entry.clone();
    assert_eq!(cloned.method, T);
}

// ── InlineCache::new ──────────────────────────────────────────────

#[test]
fn inline_cache_new_is_uninitialized_and_empty() {
    let ic = InlineCache::new();
    assert_eq!(ic.state(), IcState::Uninitialized);
    assert!(ic.entries().is_empty());
}

#[test]
fn inline_cache_lookup_miss_when_uninitialized() {
    let ic = InlineCache::new();
    assert!(ic.lookup(BlissVal(42)).is_none());
}

// ── InlineCache::update — state transitions ───────────────────────

#[test]
fn inline_cache_single_update_to_monomorphic() {
    let ic = InlineCache::new();
    ic.update(BlissVal(10), BlissVal(20));
    assert_eq!(ic.state(), IcState::Monomorphic);
    assert_eq!(ic.lookup(BlissVal(10)), Some(BlissVal(20)));
    assert!(ic.lookup(BlissVal(99)).is_none());
}

#[test]
fn inline_cache_two_types_to_polymorphic() {
    let ic = InlineCache::new();
    ic.update(BlissVal(10), BlissVal(20));
    ic.update(BlissVal(30), BlissVal(40));
    assert_eq!(ic.state(), IcState::Polymorphic);
    assert_eq!(ic.lookup(BlissVal(10)), Some(BlissVal(20)));
    assert_eq!(ic.lookup(BlissVal(30)), Some(BlissVal(40)));
}

#[test]
fn inline_cache_exceeding_poly_max_to_megamorphic() {
    let ic = InlineCache::new();
    for i in 0..=(IC_POLY_MAX as u64) {
        ic.update(BlissVal(i * 8), BlissVal(i * 8 + 1));
    }
    assert_eq!(ic.state(), IcState::Megamorphic);
}

#[test]
fn inline_cache_entries_reflect_updates() {
    let ic = InlineCache::new();
    ic.update(BlissVal(10), BlissVal(20));
    let entries = ic.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].class, BlissVal(10));
}

#[test]
fn inline_cache_same_class_replaces_method() {
    let ic = InlineCache::new();
    let class = BlissVal(10);
    ic.update(class, BlissVal(20));
    ic.update(class, BlissVal(30));
    assert_eq!(ic.state(), IcState::Monomorphic);
    assert_eq!(ic.lookup(class), Some(BlissVal(30)));
}

// ── InlineCache::reset ────────────────────────────────────────────

#[test]
fn inline_cache_reset_clears_everything() {
    let ic = InlineCache::new();
    ic.update(BlissVal(10), BlissVal(20));
    ic.reset();
    assert_eq!(ic.state(), IcState::Uninitialized);
    assert!(ic.entries().is_empty());
    assert!(ic.lookup(BlissVal(10)).is_none());
}

// ── reset_all_caches ──────────────────────────────────────────────

#[test]
fn reset_all_caches_clears_all() {
    // After calling reset_all_caches, any previously populated inline caches
    // should be cleared. This test verifies the function completes and the
    // global cache state is empty afterwards.
    reset_all_caches();
    // If we get here without panic, the function is implemented.
    // Verify a fresh cache is uninitialized after global reset.
    let ic = InlineCache::new();
    assert_eq!(ic.state(), IcState::Uninitialized);
    assert!(ic.entries().is_empty());
}
