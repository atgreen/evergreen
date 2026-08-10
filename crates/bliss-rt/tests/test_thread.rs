use bliss_rt::thread::*;
use bliss_rt::value::{BlissVal, NIL, T};

#[test]
fn green_thread_id_equality_and_hash() {
    use std::collections::HashSet;
    assert_eq!(GreenThreadId(42), GreenThreadId(42));
    assert_ne!(GreenThreadId(1), GreenThreadId(2));
    let mut set = HashSet::new();
    set.insert(GreenThreadId(1));
    set.insert(GreenThreadId(1));
    assert_eq!(set.len(), 1);
}

#[test]
fn thread_state_repr_values() {
    assert_eq!(ThreadState::Runnable as u8, 0);
    assert_eq!(ThreadState::Blocked as u8, 1);
    assert_eq!(ThreadState::Native as u8, 2);
    assert_eq!(ThreadState::Waiting as u8, 3);
    assert_eq!(ThreadState::Dead as u8, 4);
}

#[test]
fn make_thread_returns_unique_ids() {
    let id1 = make_thread(NIL).unwrap();
    let id2 = make_thread(NIL).unwrap();
    assert_ne!(id1, id2);
}

#[test]
fn join_thread_valid_id_succeeds() {
    let id = make_thread(T).unwrap();
    assert!(join_thread(id).is_ok());
}

#[test]
fn join_thread_invalid_id_fails() {
    assert!(join_thread(GreenThreadId(0xDEAD_BEEF)).is_err());
}

#[test]
fn current_thread_id_consistent() {
    assert_eq!(current_thread_id(), current_thread_id());
}

#[test]
fn current_thread_id_matches_thread_object() {
    let thread = current_thread();
    assert_eq!(thread.id(), current_thread_id());
}

#[test]
fn current_thread_is_runnable_with_stack() {
    let thread = current_thread();
    assert_eq!(thread.state(), ThreadState::Runnable);
    assert!(thread.stack().capacity() > 0);
}

#[test]
fn tls_roundtrip_and_independence() {
    let thread = current_thread();
    let a = BlissVal::from_raw(100 << 3);
    let b = BlissVal::from_raw(200 << 3);
    thread.tls_set(0, a);
    thread.tls_set(1, b);
    assert_eq!(thread.tls_get(0), a);
    assert_eq!(thread.tls_get(1), b);
    thread.tls_set(0, b); // overwrite
    assert_eq!(thread.tls_get(0), b);
}

#[test]
fn thread_yield_does_not_panic() {
    thread_yield();
}

#[test]
fn interrupt_thread_invalid_id_fails() {
    assert!(interrupt_thread(GreenThreadId(0xFFFF), NIL).is_err());
}

#[test]
fn all_thread_ids_includes_current() {
    let all = all_thread_ids();
    assert!(!all.is_empty());
    assert!(all.contains(&current_thread_id()));
}

#[test]
fn max_tls_is_4096() {
    assert_eq!(MAX_TLS, 4096);
}
