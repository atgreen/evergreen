// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Managed callback roots, runtime transitions, and errors contained inside C.
#![cfg(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "s390x", unix)
))]
use egcl_rt::ffi::{
    AlienType, ffi_call,
    managed_callback::{LispCallback, set_callback_runner},
};
use egcl_rt::thread::NativeThreadState;
use std::sync::atomic::{AtomicUsize, Ordering};
static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PAYLOAD_DROPS: AtomicUsize = AtomicUsize::new(0);
use egcl_rt::{EgclError, EgclVal};

// These fixtures deliberately manipulate the process-global GC coordinator.
// Keep Cargo's parallel harness out of that protocol, while retaining each
// fixture's intentional callback/collector concurrency inside its child.
fn run_isolated(name: &str) -> bool {
    const CHILD: &str = "EGCL_CALLBACK_RUNTIME_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

struct CallbackPanicPayload(bool);
impl Drop for CallbackPanicPayload {
    fn drop(&mut self) {
        PAYLOAD_DROPS.fetch_add(1, Ordering::SeqCst);
        if self.0 {
            // A second payload with the same destructor prevents a fix that
            // merely moves the uncontained panic to dropping the second one.
            std::panic::panic_any(CallbackPanicPayload(true));
        }
    }
}

fn runner(closure: EgclVal, arguments: &[EgclVal]) -> Result<EgclVal, EgclError> {
    assert_eq!(
        egcl_rt::thread::current_thread().state(),
        NativeThreadState::Running
    );
    INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    if closure == EgclVal::from_fixnum(-4) || closure == EgclVal::from_fixnum(-5) {
        std::panic::panic_any(CallbackPanicPayload(closure == EgclVal::from_fixnum(-5)));
    }
    if closure == EgclVal::from_fixnum(-1) {
        panic!("callback panic fixture");
    }
    if closure == EgclVal::from_fixnum(-2) {
        return Err(EgclError::ProgramError("callback error fixture".into()));
    }
    if closure == EgclVal::from_fixnum(-3) {
        let nested = LispCallback::new(
            EgclVal::from_fixnum(-2),
            AlienType::Double,
            vec![AlienType::Double],
        )?;
        assert!(call(&nested).is_err());
        return Ok(egcl_rt::gc::alloc_double_float(42.0));
    }
    egcl_rt::rooted!(closure = closure);
    let mut arguments = arguments.to_vec();
    egcl_rt::rooted_ref!(_arguments = &mut arguments);
    egcl_rt::gc::full_gc()?;
    let a = egcl_rt::ffi::marshal_to_c(*closure, &AlienType::Double)?;
    let b = egcl_rt::ffi::marshal_to_c(arguments[0], &AlienType::Double)?;
    Ok(egcl_rt::gc::alloc_double_float(
        f64::from_bits(a) + f64::from_bits(b),
    ))
}

extern "C" fn invoke(callback: *const (), value: f64) -> f64 {
    assert_eq!(
        egcl_rt::thread::current_thread().state(),
        NativeThreadState::Native
    );
    let callback: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(callback) };
    let result = unsafe { callback(value) };
    assert_eq!(
        egcl_rt::thread::current_thread().state(),
        NativeThreadState::Native
    );
    result
}

fn call(callback: &LispCallback) -> Result<u64, EgclError> {
    unsafe {
        ffi_call(
            invoke as *const (),
            &AlienType::Double,
            &[
                AlienType::Pointer(Box::new(AlienType::Void)),
                AlienType::Double,
            ],
            &[callback.as_fn_ptr() as usize as u64, 1.25f64.to_bits()],
        )
    }
}

#[test]
fn retained_closure_and_arguments_survive_gc_with_native_state_restored() {
    if run_isolated("retained_closure_and_arguments_survive_gc_with_native_state_restored") {
        return;
    }
    set_callback_runner(runner);
    let closure = egcl_rt::gc::alloc_double_float(2.5);
    let callback = LispCallback::new(closure, AlienType::Double, vec![AlienType::Double]).unwrap();
    // The callback must now own the only precise root for the closure.
    egcl_rt::gc::full_gc().unwrap();
    assert_eq!(f64::from_bits(call(&callback).unwrap()), 3.75);
    assert_eq!(
        egcl_rt::thread::current_thread().state(),
        NativeThreadState::Running
    );
    assert!(callback.take_error().is_none());
}

#[test]
fn callback_errors_and_panics_are_reported_after_c_returns() {
    if run_isolated("callback_errors_and_panics_are_reported_after_c_returns") {
        return;
    }
    set_callback_runner(runner);
    for (closure, expected) in [
        (-1, "callback panic fixture"),
        (-2, "callback error fixture"),
    ] {
        let callback = LispCallback::new(
            EgclVal::from_fixnum(closure),
            AlienType::Double,
            vec![AlienType::Double],
        )
        .unwrap();
        let error = call(&callback).unwrap_err();
        assert!(matches!(error, EgclError::FfiError(_)));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(callback.take_error().unwrap().contains(expected));
        assert!(callback.take_error().is_none());
        assert_eq!(
            egcl_rt::thread::current_thread().state(),
            NativeThreadState::Running
        );
    }
}

#[test]
fn callback_panic_payload_destructors_cannot_unwind_into_c() {
    const CHILD: &str = "EGCL_CALLBACK_PANIC_PAYLOAD_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "callback_panic_payload_destructors_cannot_unwind_into_c",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    set_callback_runner(runner);
    for closure in [-4, -5] {
        let before = PAYLOAD_DROPS.load(Ordering::SeqCst);
        let callback = LispCallback::new(
            EgclVal::from_fixnum(closure),
            AlienType::Double,
            vec![AlienType::Double],
        )
        .unwrap();
        let error = call(&callback).unwrap_err();
        assert!(matches!(error, EgclError::FfiError(_)));
        assert!(error.to_string().contains("Rust panic in Lisp callback"));
        assert!(
            callback
                .take_error()
                .unwrap()
                .contains("Rust panic in Lisp callback")
        );
        assert_eq!(PAYLOAD_DROPS.load(Ordering::SeqCst), before + 1);
        assert_eq!(
            egcl_rt::thread::current_thread().state(),
            NativeThreadState::Running
        );
    }
    let callback = LispCallback::new(
        EgclVal::from_fixnum(-3),
        AlienType::Double,
        vec![AlienType::Double],
    )
    .unwrap();
    assert_eq!(f64::from_bits(call(&callback).unwrap()), 42.0);
    assert!(callback.take_error().is_none());
}

#[test]
fn errors_without_an_outbound_frame_are_available_on_the_callback() {
    if run_isolated("errors_without_an_outbound_frame_are_available_on_the_callback") {
        return;
    }
    set_callback_runner(runner);
    let callback = LispCallback::new(
        EgclVal::from_fixnum(-2),
        AlienType::Double,
        vec![AlienType::Double],
    )
    .unwrap();
    let entry: unsafe extern "C" fn(f64) -> f64 =
        unsafe { std::mem::transmute(callback.as_fn_ptr()) };
    assert_eq!(unsafe { entry(1.0) }, 0.0);
    assert!(
        callback
            .take_error()
            .unwrap()
            .contains("callback error fixture")
    );
}

#[test]
fn a_handled_nested_callback_error_does_not_poison_the_outer_call() {
    if run_isolated("a_handled_nested_callback_error_does_not_poison_the_outer_call") {
        return;
    }
    set_callback_runner(runner);
    let callback = LispCallback::new(
        EgclVal::from_fixnum(-3),
        AlienType::Double,
        vec![AlienType::Double],
    )
    .unwrap();
    assert_eq!(f64::from_bits(call(&callback).unwrap()), 42.0);
    assert!(callback.take_error().is_none());
}

#[test]
fn foreign_threads_attach_quiescent_and_wait_for_an_active_gc_pause() {
    if run_isolated("foreign_threads_attach_quiescent_and_wait_for_an_active_gc_pause") {
        return;
    }
    set_callback_runner(runner);
    extern "C" fn probe(pointer: *const ()) -> u64 {
        let pointer = pointer as usize;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (enter_tx, enter_rx) = std::sync::mpsc::channel();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let entry: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(pointer) };
            let first = unsafe { entry(1.25) };
            let native = egcl_rt::thread::current_thread().state() == NativeThreadState::Native;
            ready_tx.send(()).unwrap();
            enter_rx.recv().unwrap();
            attempt_tx.send(()).unwrap();
            let second = unsafe { entry(2.25) };
            first == 3.75 && second == 4.75 && native
        });
        ready_rx.recv().unwrap();
        egcl_rt::safepoint::wait_for_all_threads().unwrap();
        let before = INVOCATIONS.load(Ordering::SeqCst);
        enter_tx.send(()).unwrap();
        attempt_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        let stayed_out = INVOCATIONS.load(Ordering::SeqCst) == before;
        egcl_rt::safepoint::resume_all_threads().unwrap();
        u64::from(worker.join().unwrap() && stayed_out)
    }
    let closure = egcl_rt::gc::alloc_double_float(2.5);
    let callback = LispCallback::new(closure, AlienType::Double, vec![AlienType::Double]).unwrap();
    let result = unsafe {
        ffi_call(
            probe as *const (),
            &AlienType::Int {
                signed: false,
                bits: 64,
            },
            &[AlienType::Pointer(Box::new(AlienType::Void))],
            &[callback.as_fn_ptr() as usize as u64],
        )
    }
    .unwrap();
    assert_eq!(
        result, 1,
        "foreign thread must wait for GC before accessing rooted Lisp values"
    );
}

#[test]
fn callback_root_reads_cooperate_with_a_competing_collector() {
    if run_isolated("callback_root_reads_cooperate_with_a_competing_collector") {
        return;
    }
    set_callback_runner(runner);
    extern "C" fn probe(pointer: *const ()) -> u64 {
        let address = pointer as usize;
        let worker = std::thread::spawn(move || {
            let callback: unsafe extern "C" fn(f64) -> f64 =
                unsafe { std::mem::transmute(address) };
            (0..50).all(|_| unsafe { callback(1.25) } == 3.75)
        });
        for _ in 0..50 {
            egcl_rt::gc::full_gc().unwrap();
        }
        u64::from(worker.join().unwrap())
    }
    let closure = egcl_rt::gc::alloc_double_float(2.5);
    let callback = LispCallback::new(closure, AlienType::Double, vec![AlienType::Double]).unwrap();
    let result = unsafe {
        ffi_call(
            probe as *const (),
            &AlienType::Int {
                signed: false,
                bits: 64,
            },
            &[AlienType::Pointer(Box::new(AlienType::Void))],
            &[callback.as_fn_ptr() as usize as u64],
        )
    }
    .unwrap();
    assert_eq!(result, 1);
}

#[test]
fn first_callback_trace_survives_a_later_callback_collection() {
    if run_isolated("first_callback_trace_survives_a_later_callback_collection") {
        return;
    }
    callback_trace_survives_collection(false);
}

// Buffered (aggregate-capable) calls exist only on x86-64; the scalar half above
// runs on every target with callbacks.
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
#[test]
fn buffered_callback_trace_survives_a_later_callback_collection() {
    if run_isolated("buffered_callback_trace_survives_a_later_callback_collection") {
        return;
    }
    callback_trace_survives_collection(true);
}

/// `target(callback)` through the scalar path, or through the buffered path
/// where it exists.
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
unsafe fn invoke_through_c(
    buffered: bool,
    target: *const (),
    pointer: *const (),
) -> Result<u64, EgclError> {
    let pointer_type = AlienType::Pointer(Box::new(AlienType::Void));
    if buffered {
        let mut result = 0.0_f64;
        unsafe {
            egcl_rt::ffi::ffi_call_buffered(
                target,
                &AlienType::Double,
                &[pointer_type],
                &[(&pointer as *const *const ()).cast()],
                (&mut result as *mut f64).cast(),
                None,
            )
        }
        .map(|()| result.to_bits())
    } else {
        unsafe {
            ffi_call(
                target,
                &AlienType::Double,
                &[pointer_type],
                &[pointer as usize as u64],
            )
        }
    }
}

#[cfg(not(all(target_arch = "x86_64", any(unix, windows))))]
unsafe fn invoke_through_c(
    buffered: bool,
    target: *const (),
    pointer: *const (),
) -> Result<u64, EgclError> {
    assert!(!buffered, "buffered calls are x86-64 only");
    let pointer_type = AlienType::Pointer(Box::new(AlienType::Void));
    unsafe {
        ffi_call(
            target,
            &AlienType::Double,
            &[pointer_type],
            &[pointer as usize as u64],
        )
    }
}

fn callback_trace_survives_collection(buffered: bool) {
    static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    static RETURNED: AtomicUsize = AtomicUsize::new(0);
    fn trace_runner(_closure: EgclVal, arguments: &[EgclVal]) -> Result<EgclVal, EgclError> {
        if arguments[0].as_double_float() == 1.25 {
            ORIGINAL.store(arguments[0].to_raw() as usize, Ordering::SeqCst);
            let _call =
                egcl_rt::debug_stack::CallFrame::enter_with_args("ORIGINAL-CALLBACK", arguments);
            Err(EgclError::ProgramError("original callback failure".into()).capture_backtrace())
        } else {
            // The first invocation has returned through C. Its argument is now
            // reachable only from the pending historical trace, not a live frame.
            egcl_rt::collect_t0_minor()?;
            let _call = egcl_rt::debug_stack::CallFrame::enter("LATER-CALLBACK");
            Err(EgclError::ProgramError("later callback failure".into()).capture_backtrace())
        }
    }
    extern "C" fn invoke_twice(pointer: *const ()) -> f64 {
        let callback: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(pointer) };
        for value in [1.25, 2.5] {
            assert_eq!(unsafe { callback(value) }, 0.0);
            RETURNED.fetch_add(1, Ordering::SeqCst);
        }
        17.0
    }
    set_callback_runner(trace_runner);
    let callback = LispCallback::new(
        EgclVal::from_fixnum(0),
        AlienType::Double,
        vec![AlienType::Double],
    )
    .unwrap();
    egcl_rt::rooted!(
        error =
            unsafe { invoke_through_c(buffered, invoke_twice as *const (), callback.as_fn_ptr()) }
                .unwrap_err()
    );
    assert_eq!(
        RETURNED.load(Ordering::SeqCst),
        2,
        "both C calls must return before signalling"
    );
    assert!(matches!(error.without_backtrace(), EgclError::FfiError(_)));
    assert!(error.to_string().contains("original callback failure"));
    let frames = error
        .backtrace()
        .expect("retain the first callback's historical trace");
    let foreign: Vec<_> = frames
        .iter()
        .filter(|frame| frame.origin == egcl_rt::debug_stack::FrameOrigin::Foreign)
        .collect();
    assert_eq!(foreign.len(), 1);
    assert_eq!(
        foreign[0].function.as_deref(),
        Some(format!("<unknown at 0x{:x}>", invoke_twice as *const () as usize).as_str())
    );
    assert_eq!(foreign[0].arguments, None);
    assert!(
        egcl_rt::debug_stack::capture_current(100).is_empty(),
        "returned C boundary must leave the execution stack"
    );
    let frame = frames
        .iter()
        .find(|frame| frame.function.as_deref() == Some("ORIGINAL-CALLBACK"))
        .expect("original callback frame");
    let arguments = frame.arguments.as_ref().unwrap();
    assert_eq!(arguments.len(), 1);
    assert_ne!(
        arguments[0].to_raw() as usize,
        ORIGINAL.load(Ordering::SeqCst),
        "the pending trace's only argument must actually relocate"
    );
    assert_eq!(arguments[0].as_double_float(), 1.25);
    assert!(
        !frames
            .iter()
            .any(|frame| frame.function.as_deref() == Some("LATER-CALLBACK"))
    );
    // The explicit diagnostic API still reports the latest text, independently
    // of the first error propagated by the enclosing outbound call.
    assert!(
        callback
            .take_error()
            .unwrap()
            .contains("later callback failure")
    );
    assert!(callback.take_error().is_none());
}
