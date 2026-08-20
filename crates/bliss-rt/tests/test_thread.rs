use bliss_rt::thread::*;
use bliss_rt::value::{BlissVal, NIL, T};

fn inspect_mounted_fiber() -> BlissVal {
    let Some(fiber) = current_fiber() else {
        return NIL;
    };
    if !current_thread().is_carrier() || fiber.carrier_id() != Some(current_thread_id()) {
        return NIL;
    }
    fiber.pin();
    if fiber.can_yield() || fiber_yield().is_ok() {
        return NIL;
    }
    fiber.unpin().unwrap();
    fiber.continuation().save(10, 20, 30);
    if fiber.can_yield() && fiber.continuation().snapshot() == (10, 20, 30) {
        T
    } else {
        NIL
    }
}

#[test]
fn native_thread_and_fiber_ids_are_distinct_types() {
    assert_ne!(
        std::any::TypeId::of::<NativeThreadId>(),
        std::any::TypeId::of::<FiberId>()
    );
    assert_eq!(NativeThreadId(42), NativeThreadId(42));
    assert_eq!(FiberId(42), FiberId(42));
}

#[test]
fn native_and_fiber_state_spaces_are_distinct() {
    assert_eq!(NativeThreadState::Born as u8, 0);
    assert_eq!(NativeThreadState::Running as u8, 1);
    assert_eq!(FiberState::Created as u8, 0);
    assert_eq!(FiberState::Running as u8, 2);
    assert_eq!(FiberState::Dead as u8, 7);
}

#[test]
fn make_thread_creates_and_joins_a_native_os_thread() {
    let id1 = make_thread(NIL).unwrap();
    let id2 = make_thread(T).unwrap();
    assert_ne!(id1, id2);
    assert_eq!(join_thread(id1).unwrap(), NIL);
    assert_eq!(join_thread(id2).unwrap(), T);
}

#[test]
fn join_thread_invalid_native_id_fails() {
    assert!(join_thread(NativeThreadId(0xDEAD_BEEF)).is_err());
}

#[test]
fn make_fiber_is_unscheduled_until_explicit_submission() {
    let id = make_fiber(T).unwrap();
    assert_eq!(fiber_state(id), Some(FiberState::Created));
    submit_fiber(id).unwrap();
    assert_eq!(join_fiber(id).unwrap(), T);
}

#[test]
fn mounted_fiber_tracks_carrier_continuation_and_pin_state() {
    let entry =
        unsafe { BlissVal::from_function_ptr(inspect_mounted_fiber as *const () as *mut u8) };
    let id = make_fiber(entry).unwrap();
    submit_fiber(id).unwrap();
    assert_eq!(join_fiber(id).unwrap(), T);
}

#[test]
fn current_native_thread_identity_and_stack_are_stable() {
    assert_eq!(current_thread_id(), current_thread_id());
    let thread = current_thread();
    assert_eq!(thread.id(), current_thread_id());
    assert_eq!(thread.state(), NativeThreadState::Running);
    assert!(thread.stack().capacity() > 0);
    assert!(current_fiber().is_none());
}

#[test]
fn native_thread_tls_roundtrip() {
    let thread = current_thread();
    let a = BlissVal::from_raw(100 << 3);
    let b = BlissVal::from_raw(200 << 3);
    thread.tls_set(0, a);
    thread.tls_set(1, b);
    assert_eq!(thread.tls_get(0), a);
    assert_eq!(thread.tls_get(1), b);
}

#[test]
fn yielding_a_native_thread_does_not_require_a_fiber() {
    thread_yield();
    assert!(fiber_yield().is_err());
}

#[test]
fn interrupt_thread_uses_native_thread_identity() {
    assert!(interrupt_thread(NativeThreadId(0xFFFF), NIL).is_err());
    let id = make_thread(NIL).unwrap();
    interrupt_thread(id, T).unwrap();
    assert_eq!(join_thread(id).unwrap(), T);
}

#[test]
fn native_thread_registry_includes_current_and_carriers() {
    assert!(all_thread_ids().contains(&current_thread_id()));
    let carriers = carrier_thread_ids();
    assert!(!carriers.is_empty());
    let all = all_thread_ids();
    assert!(carriers.iter().all(|id| all.contains(id)));
}

#[test]
fn max_tls_is_4096() {
    assert_eq!(MAX_TLS, 4096);
}
