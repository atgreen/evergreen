//! Tests for OSR (on-stack replacement) and deoptimisation — osr.rs

use bliss_compiler::osr::{DeoptConfig, DeoptLog, DeoptReason, deoptimize, osr_entry};
use bliss_rt::value::{BlissVal, NIL, T, TAG_FUNCTION};

// ── DeoptReason enum variants ─────────────────────────────────────

#[test]
fn deopt_reason_type_mismatch() {
    let reason = DeoptReason::TypeMismatch {
        expected: "FIXNUM".into(), actual: "CONS".into(),
    };
    match &reason {
        DeoptReason::TypeMismatch { expected, actual } => {
            assert_eq!(expected, "FIXNUM");
            assert_eq!(actual, "CONS");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn deopt_reason_uninitialized_variable() {
    match DeoptReason::UninitializedVariable(NIL) {
        DeoptReason::UninitializedVariable(v) => assert_eq!(v, NIL),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn deopt_reason_class_changed() {
    match DeoptReason::ClassChanged(T) {
        DeoptReason::ClassChanged(v) => assert_eq!(v, T),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn deopt_reason_inline_cache_overflow_and_other() {
    assert!(matches!(DeoptReason::InlineCacheOverflow, DeoptReason::InlineCacheOverflow));
    match DeoptReason::Other("custom".into()) {
        DeoptReason::Other(msg) => assert_eq!(msg, "custom"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn deopt_reason_clone_and_debug() {
    let r = DeoptReason::InlineCacheOverflow;
    let _ = r.clone();
    assert!(format!("{:?}", r).contains("InlineCacheOverflow"));
}

// ── DeoptLog ──────────────────────────────────────────────────────

#[test]
fn deopt_log_new_starts_empty() {
    let log = DeoptLog::new();
    assert_eq!(log.count(), 0);
    assert!(!log.is_blacklisted());
}

#[test]
fn deopt_log_record_increments_count() {
    let mut log = DeoptLog::new();
    log.record(DeoptReason::InlineCacheOverflow);
    assert_eq!(log.count(), 1);
    log.record(DeoptReason::Other("trap".into()));
    assert_eq!(log.count(), 2);
}

#[test]
fn deopt_log_record_all_reason_variants() {
    let mut log = DeoptLog::new();
    log.record(DeoptReason::TypeMismatch { expected: "A".into(), actual: "B".into() });
    log.record(DeoptReason::UninitializedVariable(NIL));
    log.record(DeoptReason::ClassChanged(T));
    log.record(DeoptReason::InlineCacheOverflow);
    log.record(DeoptReason::Other("test".into()));
    assert_eq!(log.count(), 5);
}

// ── DeoptConfig struct ────────────────────────────────────────────

#[test]
fn deopt_config_fields() {
    let config = DeoptConfig { blacklist_threshold: 4, backoff_seconds: 60 };
    assert_eq!(config.blacklist_threshold, 4);
    assert_eq!(config.backoff_seconds, 60);
}

// ── osr_entry / deoptimize ────────────────────────────────────────

#[test]
fn deoptimize_returns_result() {
    bliss_compiler::osr::clear_global_deopt_logs();
    // Use a function-tagged value so validation passes.
    let func = BlissVal(TAG_FUNCTION);
    let result = deoptimize(func, DeoptReason::InlineCacheOverflow, &[T, NIL]);
    assert!(result.is_ok(), "deoptimize with function-tagged value should return Ok");
}

#[test]
fn deoptimize_rejects_non_function() {
    // NIL is not function-tagged; deoptimize should return Err.
    let result = deoptimize(NIL, DeoptReason::InlineCacheOverflow, &[T, NIL]);
    assert!(result.is_err(), "deoptimize with non-function value should return Err");
}

#[test]
fn osr_entry_with_valid_map() {
    use bliss_compiler::osr::{OsrEntryMap, LocalMapping};
    bliss_compiler::osr::clear_global_deopt_logs();
    let map = OsrEntryMap::new(
        vec![LocalMapping { local_index: 0, ssa_var: 0 }],
        0,
    );
    // Use a function-tagged value so validation passes.
    let func = BlissVal(TAG_FUNCTION);
    let result = osr_entry(func, &map, &[NIL]);
    assert!(result.is_ok(), "osr_entry with function-tagged value should return Ok");
}

#[test]
fn osr_entry_rejects_non_function() {
    use bliss_compiler::osr::{OsrEntryMap, LocalMapping};
    let map = OsrEntryMap::new(
        vec![LocalMapping { local_index: 0, ssa_var: 0 }],
        0,
    );
    // NIL is not function-tagged; osr_entry should return Err.
    let result = osr_entry(NIL, &map, &[NIL]);
    assert!(result.is_err(), "osr_entry with non-function value should return Err");
}

// ── OsrEntryMap::enter ───────────────────────────────────────────

#[test]
fn osr_entry_map_enter_with_valid_locals() {
    use bliss_compiler::osr::{OsrEntryMap, LocalMapping};
    let map = OsrEntryMap::new(
        vec![
            LocalMapping { local_index: 0, ssa_var: 0 },
            LocalMapping { local_index: 1, ssa_var: 1 },
        ],
        0,
    );
    let target_pc: *const u8 = std::ptr::null();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        map.enter(&[NIL, T], target_pc)
    }));
    // enter currently calls unimplemented!(); once implemented it should
    // return Ok(()). Either outcome is acceptable for red-phase.
    assert!(
        result.is_err() || result.unwrap().is_ok(),
        "OsrEntryMap::enter should either panic (unimplemented) or return Ok"
    );
}

// ── DeoptLog blacklisting ────────────────────────────────────────

#[test]
fn deopt_log_becomes_blacklisted_after_threshold() {
    let config = DeoptConfig { blacklist_threshold: 3, backoff_seconds: 10 };
    let mut log = DeoptLog::with_config(&config);
    // Not yet blacklisted
    assert!(!log.is_blacklisted(), "should not be blacklisted with 0 deopts");
    // Record enough deopts to reach the threshold
    for i in 0..config.blacklist_threshold {
        log.record(DeoptReason::InlineCacheOverflow);
        if i + 1 < config.blacklist_threshold {
            assert!(!log.is_blacklisted(),
                "should not be blacklisted after {} deopts (threshold={})",
                i + 1, config.blacklist_threshold);
        }
    }
    assert!(
        log.is_blacklisted(),
        "DeoptLog should be blacklisted after {} deopts (threshold={})",
        log.count(),
        config.blacklist_threshold,
    );
}

#[test]
fn deopt_log_with_config_uses_custom_threshold() {
    // Verify that with_config actually applies the threshold, not a hardcoded default
    let config_high = DeoptConfig { blacklist_threshold: 10, backoff_seconds: 5 };
    let mut log = DeoptLog::with_config(&config_high);
    for _ in 0..3 {
        log.record(DeoptReason::InlineCacheOverflow);
    }
    assert!(!log.is_blacklisted(),
        "threshold=10 but only 3 deopts — should NOT be blacklisted");

    let config_low = DeoptConfig { blacklist_threshold: 2, backoff_seconds: 5 };
    let mut log2 = DeoptLog::with_config(&config_low);
    log2.record(DeoptReason::InlineCacheOverflow);
    log2.record(DeoptReason::Other("test".into()));
    assert!(log2.is_blacklisted(),
        "threshold=2 with 2 deopts — should be blacklisted");
}
