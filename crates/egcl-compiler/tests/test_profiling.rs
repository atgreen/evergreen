//! Tests for profiling infrastructure — profiling.rs

use egcl_compiler::profiling::{
    BackEdgeCounter, InvocationCounter, TYPE_PROFILE_MAX_ENTRIES, TypeProfile,
};
use egcl_rt::value::EgclVal;

// ── InvocationCounter ─────────────────────────────────────────────

#[test]
fn invocation_counter_new_starts_at_zero() {
    let counter = InvocationCounter::new();
    assert_eq!(counter.count(), 0);
}

#[test]
fn invocation_counter_increment_increases_count() {
    let counter = InvocationCounter::new();
    counter.increment(100);
    assert_eq!(counter.count(), 1);
}

#[test]
fn invocation_counter_increment_multiple_times() {
    let counter = InvocationCounter::new();
    for _ in 0..10 {
        counter.increment(100);
    }
    assert_eq!(counter.count(), 10);
}

#[test]
fn invocation_counter_returns_false_below_threshold() {
    let counter = InvocationCounter::new();
    assert!(!counter.increment(5));
}

#[test]
fn invocation_counter_returns_true_at_threshold() {
    let counter = InvocationCounter::new();
    let mut reached = false;
    for _ in 0..5 {
        if counter.increment(5) {
            reached = true;
            break;
        }
    }
    assert!(reached);
}

#[test]
fn invocation_counter_reset_clears_count() {
    let counter = InvocationCounter::new();
    counter.increment(100);
    counter.increment(100);
    counter.reset();
    assert_eq!(counter.count(), 0);
}

#[test]
fn invocation_counter_threshold_one() {
    let counter = InvocationCounter::new();
    assert!(counter.increment(1));
}

// ── BackEdgeCounter ───────────────────────────────────────────────

#[test]
fn back_edge_counter_new_starts_at_zero() {
    let counter = BackEdgeCounter::new();
    assert_eq!(counter.count(), 0);
}

#[test]
fn back_edge_counter_increment_and_threshold() {
    let counter = BackEdgeCounter::new();
    assert!(!counter.increment(10));
    assert_eq!(counter.count(), 1);
}

#[test]
fn back_edge_counter_reaches_threshold() {
    let counter = BackEdgeCounter::new();
    let mut reached = false;
    for _ in 0..10 {
        if counter.increment(10) {
            reached = true;
            break;
        }
    }
    assert!(reached);
}

#[test]
fn back_edge_counter_reset() {
    let counter = BackEdgeCounter::new();
    for _ in 0..5 {
        counter.increment(100);
    }
    counter.reset();
    assert_eq!(counter.count(), 0);
}

// ── TYPE_PROFILE_MAX_ENTRIES constant ─────────────────────────────

#[test]
fn type_profile_max_entries_is_four() {
    assert_eq!(TYPE_PROFILE_MAX_ENTRIES, 4);
}

// ── TypeProfile ───────────────────────────────────────────────────

#[test]
fn type_profile_new_is_empty() {
    let profile = TypeProfile::new();
    assert!(profile.entries().is_empty());
    assert!(profile.dominant_type().is_none());
    assert!(!profile.is_monomorphic());
}

#[test]
fn type_profile_single_type_is_monomorphic() {
    let profile = TypeProfile::new();
    let class = EgclVal(42);
    profile.record(class);
    assert!(profile.is_monomorphic());
    assert_eq!(profile.dominant_type(), Some(class));
    assert!(profile.entries().contains(&class));
}

#[test]
fn type_profile_multiple_same_type_still_monomorphic() {
    let profile = TypeProfile::new();
    let class = EgclVal(42);
    for _ in 0..5 {
        profile.record(class);
    }
    assert!(profile.is_monomorphic());
}

#[test]
fn type_profile_two_types_not_monomorphic() {
    let profile = TypeProfile::new();
    profile.record(EgclVal(10));
    profile.record(EgclVal(20));
    assert!(!profile.is_monomorphic());
}

#[test]
fn type_profile_dominant_type_is_most_frequent() {
    let profile = TypeProfile::new();
    let frequent = EgclVal(10);
    for _ in 0..3 {
        profile.record(frequent);
    }
    profile.record(EgclVal(20));
    assert_eq!(profile.dominant_type(), Some(frequent));
}

#[test]
fn type_profile_respects_max_entries() {
    let profile = TypeProfile::new();
    for i in 0..(TYPE_PROFILE_MAX_ENTRIES + 4) {
        profile.record(EgclVal(i as u64));
    }
    assert!(profile.entries().len() <= TYPE_PROFILE_MAX_ENTRIES);
}

#[test]
fn type_profile_reset_clears_all() {
    let profile = TypeProfile::new();
    profile.record(EgclVal(42));
    profile.reset();
    assert!(profile.entries().is_empty());
    assert!(profile.dominant_type().is_none());
}

#[test]
fn type_profile_reset_then_record_works() {
    let profile = TypeProfile::new();
    profile.record(EgclVal(10));
    profile.reset();
    profile.record(EgclVal(20));
    assert!(profile.is_monomorphic());
    assert_eq!(profile.dominant_type(), Some(EgclVal(20)));
}

// ── FunctionProfile ───────────────────────────────────────────────

use egcl_compiler::profiling::FunctionProfile;

#[test]
#[should_panic(expected = "FunctionProfile")]
fn function_profile_invocation_counter() {
    // FunctionProfile has private fields — construction will panic with unimplemented,
    // but this test validates the invocation_counter() method signature.
    // We zero-initialize to ensure `initialized` is false, triggering the panic
    // in check_init() before any invalid field (HashMap) is accessed.
    #[allow(invalid_value)]
    let fp = unsafe { std::mem::MaybeUninit::<FunctionProfile>::zeroed().assume_init() };
    let _counter: &InvocationCounter = fp.invocation_counter();
}

#[test]
#[should_panic(expected = "FunctionProfile")]
fn function_profile_type_profile() {
    #[allow(invalid_value)]
    let fp = unsafe { std::mem::MaybeUninit::<FunctionProfile>::zeroed().assume_init() };
    let _tp: Option<&TypeProfile> = fp.type_profile(0);
}
