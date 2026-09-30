//! Symbol allocation under allocation-time GC stress.
//!
//! This lives in its own test binary so `EGCL_GC_STRESS` is set before any
//! EGCL allocation in the process. The stress stride is cached on first use.

use std::sync::{Mutex, OnceLock};
use egcl_rt::heap_stats;
use egcl_rt::value::NIL;

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn enable_gc_stress() {
    // SAFETY: tests in this binary serialize on `lock`, and every test sets the
    // same value before performing EGCL allocations. The stress stride is cached
    // on first allocation-time use.
    unsafe {
        std::env::set_var("EGCL_GC_STRESS", "1");
    }
}

#[test]
fn forced_minor_gc_does_not_promote_one_pinned_region_per_intern() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    enable_gc_stress();

    let prefix = format!("GC-STRESS-DENSE-SYMBOL-{}-", std::process::id());
    egcl_rt::symbols::intern(&format!("{prefix}WARMUP"));
    let before = heap_stats();

    for i in 0..32 {
        egcl_rt::symbols::intern(&format!("{prefix}{i}"));
    }

    let after = heap_stats();
    let regions_consumed = before.regions_free.saturating_sub(after.regions_free);
    assert!(
        regions_consumed <= 4,
        "fresh interned symbols must be allocated densely; consumed {regions_consumed} heap regions"
    );
}

#[test]
fn forced_minor_gc_does_not_promote_one_pinned_region_per_function() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    enable_gc_stress();

    egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, NIL);
    let before = heap_stats();

    for _ in 0..32 {
        egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, NIL);
    }

    let after = heap_stats();
    let regions_consumed = before.regions_free.saturating_sub(after.regions_free);
    assert!(
        regions_consumed <= 4,
        "fresh interpreted functions must be allocated densely; consumed {regions_consumed} heap regions"
    );
}

#[test]
fn forced_minor_gc_does_not_promote_one_pinned_region_per_package() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    enable_gc_stress();

    let prefix = format!("GC-STRESS-DENSE-PACKAGE-{}-", std::process::id());
    egcl_rt::packages::find_or_create(&format!("{prefix}WARMUP"));
    let before = heap_stats();

    for i in 0..32 {
        egcl_rt::packages::find_or_create(&format!("{prefix}{i}"));
    }

    let after = heap_stats();
    let regions_consumed = before.regions_free.saturating_sub(after.regions_free);
    assert!(
        regions_consumed <= 4,
        "fresh packages must be allocated densely; consumed {regions_consumed} heap regions"
    );
}
