//! Managed callback roots, runtime transitions, and errors contained inside C.
#![cfg(all(target_arch = "x86_64", unix))]
use std::sync::atomic::{AtomicUsize, Ordering};
use torcl_rt::ffi::{
    AlienType, ffi_call,
    managed_callback::{LispCallback, set_callback_runner},
};
use torcl_rt::thread::NativeThreadState;
static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PAYLOAD_DROPS: AtomicUsize = AtomicUsize::new(0);
use torcl_rt::{TorclError, TorclVal};

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

fn runner(closure: TorclVal, arguments: &[TorclVal]) -> Result<TorclVal, TorclError> {
    assert_eq!(
        torcl_rt::thread::current_thread().state(),
        NativeThreadState::Running
    );
    INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    if closure == TorclVal::from_fixnum(-4) || closure == TorclVal::from_fixnum(-5) {
        std::panic::panic_any(CallbackPanicPayload(closure == TorclVal::from_fixnum(-5)));
    }
    if closure == TorclVal::from_fixnum(-1) {
        panic!("callback panic fixture");
    }
    if closure == TorclVal::from_fixnum(-2) {
        return Err(TorclError::ProgramError("callback error fixture".into()));
    }
    if closure == TorclVal::from_fixnum(-3) {
        let nested = LispCallback::new(
            TorclVal::from_fixnum(-2),
            AlienType::Double,
            vec![AlienType::Double],
        )?;
        assert!(call(&nested).is_err());
        return Ok(torcl_rt::gc::alloc_double_float(42.0));
    }
    torcl_rt::rooted!(closure = closure);
    let mut arguments = arguments.to_vec();
    torcl_rt::rooted_ref!(_arguments = &mut arguments);
    torcl_rt::gc::full_gc()?;
    let a = torcl_rt::ffi::marshal_to_c(*closure, &AlienType::Double)?;
    let b = torcl_rt::ffi::marshal_to_c(arguments[0], &AlienType::Double)?;
    Ok(torcl_rt::gc::alloc_double_float(
        f64::from_bits(a) + f64::from_bits(b),
    ))
}

extern "C" fn invoke(callback: *const (), value: f64) -> f64 {
    assert_eq!(
        torcl_rt::thread::current_thread().state(),
        NativeThreadState::Native
    );
    let callback: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(callback) };
    let result = unsafe { callback(value) };
    assert_eq!(
        torcl_rt::thread::current_thread().state(),
        NativeThreadState::Native
    );
    result
}

fn call(callback: &LispCallback) -> Result<u64, TorclError> {
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
    set_callback_runner(runner);
    let closure = torcl_rt::gc::alloc_double_float(2.5);
    let callback = LispCallback::new(closure, AlienType::Double, vec![AlienType::Double]).unwrap();
    // The callback must now own the only precise root for the closure.
    torcl_rt::gc::full_gc().unwrap();
    assert_eq!(f64::from_bits(call(&callback).unwrap()), 3.75);
    assert_eq!(
        torcl_rt::thread::current_thread().state(),
        NativeThreadState::Running
    );
    assert!(callback.take_error().is_none());
}

#[test]
fn callback_errors_and_panics_are_reported_after_c_returns() {
    set_callback_runner(runner);
    for (closure, expected) in [
        (-1, "callback panic fixture"),
        (-2, "callback error fixture"),
    ] {
        let callback = LispCallback::new(
            TorclVal::from_fixnum(closure),
            AlienType::Double,
            vec![AlienType::Double],
        )
        .unwrap();
        let error = call(&callback).unwrap_err();
        assert!(matches!(error, TorclError::FfiError(_)));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(callback.take_error().unwrap().contains(expected));
        assert!(callback.take_error().is_none());
        assert_eq!(
            torcl_rt::thread::current_thread().state(),
            NativeThreadState::Running
        );
    }
}

#[test]
fn callback_panic_payload_destructors_cannot_unwind_into_c() {
    const CHILD: &str = "TORCL_CALLBACK_PANIC_PAYLOAD_CHILD";
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
            TorclVal::from_fixnum(closure),
            AlienType::Double,
            vec![AlienType::Double],
        )
        .unwrap();
        let error = call(&callback).unwrap_err();
        assert!(matches!(error, TorclError::FfiError(_)));
        assert!(error.to_string().contains("Rust panic in Lisp callback"));
        assert!(
            callback
                .take_error()
                .unwrap()
                .contains("Rust panic in Lisp callback")
        );
        assert_eq!(PAYLOAD_DROPS.load(Ordering::SeqCst), before + 1);
        assert_eq!(
            torcl_rt::thread::current_thread().state(),
            NativeThreadState::Running
        );
    }
    let callback = LispCallback::new(
        TorclVal::from_fixnum(-3),
        AlienType::Double,
        vec![AlienType::Double],
    )
    .unwrap();
    assert_eq!(f64::from_bits(call(&callback).unwrap()), 42.0);
    assert!(callback.take_error().is_none());
}

#[test]
fn errors_without_an_outbound_frame_are_available_on_the_callback() {
    set_callback_runner(runner);
    let callback = LispCallback::new(
        TorclVal::from_fixnum(-2),
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
    set_callback_runner(runner);
    let callback = LispCallback::new(
        TorclVal::from_fixnum(-3),
        AlienType::Double,
        vec![AlienType::Double],
    )
    .unwrap();
    assert_eq!(f64::from_bits(call(&callback).unwrap()), 42.0);
    assert!(callback.take_error().is_none());
}

#[test]
fn foreign_threads_attach_quiescent_and_wait_for_an_active_gc_pause() {
    set_callback_runner(runner);
    extern "C" fn probe(pointer: *const ()) -> u64 {
        let pointer = pointer as usize;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (enter_tx, enter_rx) = std::sync::mpsc::channel();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let entry: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(pointer) };
            let first = unsafe { entry(1.25) };
            let native = torcl_rt::thread::current_thread().state() == NativeThreadState::Native;
            ready_tx.send(()).unwrap();
            enter_rx.recv().unwrap();
            attempt_tx.send(()).unwrap();
            let second = unsafe { entry(2.25) };
            first == 3.75 && second == 4.75 && native
        });
        ready_rx.recv().unwrap();
        torcl_rt::safepoint::wait_for_all_threads().unwrap();
        let before = INVOCATIONS.load(Ordering::SeqCst);
        enter_tx.send(()).unwrap();
        attempt_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        let stayed_out = INVOCATIONS.load(Ordering::SeqCst) == before;
        torcl_rt::safepoint::resume_all_threads().unwrap();
        u64::from(worker.join().unwrap() && stayed_out)
    }
    let closure = torcl_rt::gc::alloc_double_float(2.5);
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
    set_callback_runner(runner);
    extern "C" fn probe(pointer: *const ()) -> u64 {
        let address = pointer as usize;
        let worker = std::thread::spawn(move || {
            let callback: unsafe extern "C" fn(f64) -> f64 =
                unsafe { std::mem::transmute(address) };
            (0..50).all(|_| unsafe { callback(1.25) } == 3.75)
        });
        for _ in 0..50 {
            torcl_rt::gc::full_gc().unwrap();
        }
        u64::from(worker.join().unwrap())
    }
    let closure = torcl_rt::gc::alloc_double_float(2.5);
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
