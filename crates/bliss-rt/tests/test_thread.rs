use bliss_rt::gc::TraceHostRoots;
use bliss_rt::thread::*;
use bliss_rt::value::{BlissVal, NIL, T};
use std::sync::atomic::{AtomicBool, Ordering};

static FIBER_FOREGROUND_READY: AtomicBool = AtomicBool::new(false);
static FIBER_FOREGROUND_CAN_FINISH: AtomicBool = AtomicBool::new(false);
static NATIVE_INTERRUPT_READY: AtomicBool = AtomicBool::new(false);
static NATIVE_INTERRUPT_CAN_FINISH: AtomicBool = AtomicBool::new(false);

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

fn foreground_fiber_entry() -> BlissVal {
    set_current_execution_foreground();
    FIBER_FOREGROUND_READY.store(true, Ordering::Release);
    while !FIBER_FOREGROUND_CAN_FINISH.load(Ordering::Acquire) {
        thread_yield();
    }
    match take_current_pending_signal() {
        Some(PendingSignal::Interrupt) => T,
        _ => NIL,
    }
}

fn wait_for_native_interrupt_entry() -> BlissVal {
    NATIVE_INTERRUPT_READY.store(true, Ordering::Release);
    while !NATIVE_INTERRUPT_CAN_FINISH.load(Ordering::Acquire) {
        thread_yield();
    }
    NIL
}

fn condition_state_thread_entry() -> BlissVal {
    let thread = current_thread();
    if thread.condition_state_snapshot().handler_depth == 0
        && thread.condition_state_snapshot().restart_depth == 0
    {
        T
    } else {
        NIL
    }
}

fn condition_state_fiber_entry() -> BlissVal {
    let Some(fiber) = current_fiber() else {
        return NIL;
    };
    let snapshot = fiber.condition_state_snapshot();
    if snapshot.handler_depth == 0 && snapshot.restart_depth == 0 {
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
    NATIVE_INTERRUPT_READY.store(false, Ordering::Release);
    NATIVE_INTERRUPT_CAN_FINISH.store(false, Ordering::Release);
    let entry = unsafe {
        BlissVal::from_function_ptr(wait_for_native_interrupt_entry as *const () as *mut u8)
    };
    let id = make_thread(entry).unwrap();
    while !NATIVE_INTERRUPT_READY.load(Ordering::Acquire) {
        thread_yield();
    }
    interrupt_thread(id, T).unwrap();
    NATIVE_INTERRUPT_CAN_FINISH.store(true, Ordering::Release);
    assert_eq!(join_thread(id).unwrap(), T);
}

#[test]
fn pending_signal_bits_are_execution_local_and_one_shot() {
    assert_eq!(take_current_pending_signal(), None);
    post_current_pending_signal(PendingSignal::Interrupt);
    assert_eq!(
        take_current_pending_signal(),
        Some(PendingSignal::Interrupt)
    );
    assert_eq!(take_current_pending_signal(), None);
}

#[test]
fn condition_state_is_execution_local_empty_and_runtime_owned() {
    let current = current_thread();
    current.with_condition_state_mut(|state| {
        state
            .handler_stack
            .push(bliss_rt::thread::ConditionHandlerCluster { frame: 0, count: 1 });
        state
            .restart_stack
            .push(bliss_rt::thread::ConditionRestartCluster { frame: 0, count: 1 });
    });

    let snapshot = current.condition_state_snapshot();
    assert_eq!(snapshot.handler_depth, 1);
    assert_eq!(snapshot.restart_depth, 1);

    let native = make_thread(unsafe {
        BlissVal::from_function_ptr(condition_state_thread_entry as *const () as *mut u8)
    })
    .unwrap();
    assert_eq!(join_thread(native).unwrap(), T);

    let fiber = make_fiber(unsafe {
        BlissVal::from_function_ptr(condition_state_fiber_entry as *const () as *mut u8)
    })
    .unwrap();
    submit_fiber(fiber).unwrap();
    assert_eq!(join_fiber(fiber).unwrap(), T);

    current.with_condition_state_mut(|state| {
        state.restart_stack.clear();
        state.handler_stack.clear();
    });
}

#[test]
fn condition_state_traces_debugger_and_handler_case_roots() {
    let current = current_thread();
    let mut state = bliss_rt::thread::ThreadConditionState::new();
    state.debugger_hook = Some(BlissVal::from_fixnum(31));
    state.break_on_signals = Some(BlissVal::from_fixnum(32));
    state
        .handler_case_clauses
        .insert(41, BlissVal::from_fixnum(42));
    state.pending_handler_case = Some((BlissVal::from_fixnum(51), BlissVal::from_fixnum(52)));

    let mut seen = Vec::new();
    state.trace_host_roots(&mut |slot| {
        seen.push(unsafe { (*slot).as_fixnum() });
    });

    assert_eq!(seen, vec![31, 32, 42, 51, 52]);

    current.with_condition_state_mut(|current_state| {
        assert!(current_state.handler_stack.is_empty());
        assert!(current_state.restart_stack.is_empty());
    });
}

#[test]
fn foreground_pending_signal_targets_registered_execution() {
    set_current_execution_foreground();

    let worker = std::thread::spawn(|| {
        post_foreground_pending_signal(PendingSignal::Interrupt).unwrap();
        take_current_pending_signal()
    });

    assert_eq!(worker.join().unwrap(), None);
    assert_eq!(
        take_current_pending_signal(),
        Some(PendingSignal::Interrupt)
    );
    assert_eq!(take_current_pending_signal(), None);
}

#[test]
fn foreground_pending_signal_can_target_mounted_fiber() {
    FIBER_FOREGROUND_READY.store(false, Ordering::Release);
    FIBER_FOREGROUND_CAN_FINISH.store(false, Ordering::Release);

    let entry =
        unsafe { BlissVal::from_function_ptr(foreground_fiber_entry as *const () as *mut u8) };
    let id = make_fiber(entry).unwrap();
    submit_fiber(id).unwrap();
    while !FIBER_FOREGROUND_READY.load(Ordering::Acquire) {
        thread_yield();
    }

    post_foreground_pending_signal(PendingSignal::Interrupt).unwrap();
    FIBER_FOREGROUND_CAN_FINISH.store(true, Ordering::Release);

    assert_eq!(join_fiber(id).unwrap(), T);
    set_current_execution_foreground();
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
