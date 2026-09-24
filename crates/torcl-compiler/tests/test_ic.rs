//! Tests for inline caches — ic.rs

use torcl_compiler::ic::{
    IC_POLY_MAX, IcEntry, IcState, InlineCache, ic_generation, init_ic_registry, reset_all_caches,
};
use torcl_rt::value::{NIL, T, TorclVal};

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
fn ic_poly_max_is_eight() {
    assert_eq!(IC_POLY_MAX, 8);
}

// ── IcEntry struct ────────────────────────────────────────────────

#[test]
fn ic_entry_fields_and_clone() {
    let entry = IcEntry {
        class: NIL,
        method: T,
    };
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
    assert!(ic.lookup(TorclVal(42)).is_none());
}

// ── InlineCache::update — state transitions ───────────────────────

#[test]
fn inline_cache_single_update_to_monomorphic() {
    let ic = InlineCache::new();
    ic.update(TorclVal(10), TorclVal(20));
    assert_eq!(ic.state(), IcState::Monomorphic);
    assert_eq!(ic.lookup(TorclVal(10)), Some(TorclVal(20)));
    assert!(ic.lookup(TorclVal(99)).is_none());
}

#[test]
fn inline_cache_two_types_to_polymorphic() {
    let ic = InlineCache::new();
    ic.update(TorclVal(10), TorclVal(20));
    ic.update(TorclVal(30), TorclVal(40));
    assert_eq!(ic.state(), IcState::Polymorphic);
    assert_eq!(ic.lookup(TorclVal(10)), Some(TorclVal(20)));
    assert_eq!(ic.lookup(TorclVal(30)), Some(TorclVal(40)));
}

#[test]
fn inline_cache_exceeding_poly_max_to_megamorphic() {
    let ic = InlineCache::new();
    for i in 0..=(IC_POLY_MAX as u64) {
        ic.update(TorclVal(i * 8), TorclVal(i * 8 + 1));
    }
    assert_eq!(ic.state(), IcState::Megamorphic);
}

#[test]
fn inline_cache_entries_reflect_updates() {
    let ic = InlineCache::new();
    ic.update(TorclVal(10), TorclVal(20));
    let entries = ic.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].class, TorclVal(10));
}

#[test]
fn inline_cache_same_class_replaces_method() {
    let ic = InlineCache::new();
    let class = TorclVal(10);
    ic.update(class, TorclVal(20));
    ic.update(class, TorclVal(30));
    assert_eq!(ic.state(), IcState::Monomorphic);
    assert_eq!(ic.lookup(class), Some(TorclVal(30)));
}

// ── InlineCache::reset ────────────────────────────────────────────

#[test]
fn inline_cache_reset_clears_everything() {
    let ic = InlineCache::new();
    ic.update(TorclVal(10), TorclVal(20));
    ic.reset();
    assert_eq!(ic.state(), IcState::Uninitialized);
    assert!(ic.entries().is_empty());
    assert!(ic.lookup(TorclVal(10)).is_none());
}

// ── reset_all_caches ──────────────────────────────────────────────

#[test]
fn reset_all_caches_invalidates_existing_cache_via_generation() {
    // Ensure the IC registry is initialised (safe to call multiple times).
    init_ic_registry();

    // Create an IC and populate it before the reset.
    let ic = InlineCache::new();
    ic.update(TorclVal(10), TorclVal(20));
    assert_eq!(
        ic.state(),
        IcState::Monomorphic,
        "IC should be populated before reset"
    );
    assert_eq!(ic.lookup(TorclVal(10)), Some(TorclVal(20)));

    let gen_before = ic_generation();

    // Bump the global generation — all existing ICs become stale.
    reset_all_caches().expect("reset_all_caches");

    let gen_after = ic_generation();
    assert!(
        gen_after > gen_before,
        "global IC generation should advance after reset_all_caches (was {}, now {})",
        gen_before,
        gen_after
    );

    // The SAME ic should now be lazily invalidated on the next access:
    // its local generation is behind the global one, so state()/lookup()
    // trigger check_generation() which clears it.
    assert_eq!(
        ic.state(),
        IcState::Uninitialized,
        "IC should be lazily reset to Uninitialized after reset_all_caches"
    );
    assert!(
        ic.entries().is_empty(),
        "IC entries should be empty after reset_all_caches"
    );
    assert!(
        ic.lookup(TorclVal(10)).is_none(),
        "lookup should miss after reset_all_caches cleared the IC"
    );
}
