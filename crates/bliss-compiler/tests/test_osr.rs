//! Tests for OSR (on-stack replacement) and deoptimisation — osr.rs

use bliss_compiler::osr::{DeoptConfig, DeoptLog, DeoptReason, deoptimize, osr_entry};
use bliss_rt::value::{NIL, T};

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
#[should_panic(expected = "deoptimize")]
fn deoptimize_not_yet_implemented() {
    let _ = deoptimize(NIL, DeoptReason::InlineCacheOverflow, &[T, NIL]);
}

#[test]
#[should_panic(expected = "osr_entry")]
fn osr_entry_not_yet_implemented() {
    use bliss_compiler::osr::OsrEntryMap;
    let map = std::mem::MaybeUninit::<OsrEntryMap>::uninit();
    let map_ref = unsafe { &*map.as_ptr() };
    let _ = osr_entry(NIL, map_ref, &[NIL]);
}
