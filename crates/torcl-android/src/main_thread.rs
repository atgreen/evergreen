//! Running a C call on the Android main thread, and waiting for the answer.
//!
//! Android's view hierarchy has thread affinity: touch a View from anywhere but
//! the thread that created it and `ViewRootImpl.checkThread` throws
//! `CalledFromWrongThreadException`. The Lisp interpreter runs on a worker
//! thread -- it has to, since the main thread belongs to the platform's message
//! loop -- so without a way back to the main thread, an application written in
//! Lisp can never attach a View, and therefore can never have a focused editor,
//! an InputConnection, or real text input.
//!
//! Every other Android framework has this same gate and every one of them
//! writes it in Java: Qt has `runOnAndroidMainThread`, SDL posts a
//! `ShowTextInputTask` to `runOnUiThread`, React Native queues onto the UI
//! thread, Flutter marshals platform-channel work to the platform thread. All of
//! them can, because all of them ship Java classes and can therefore write a
//! `Runnable`. TorCL applications are Lisp and a `.so` with no DEX at all, so
//! there is no `Runnable` to post and the gate has to be built from the parts
//! Android offers natively: a pipe, and the main thread's own `ALooper`.
//!
//! Which is, in fact, exactly what the platform does for itself -- AOSP's
//! `NativeActivity` drives its own main-thread commands down a pipe registered
//! on that looper. This is that, opened up to callers.
//!
//! The gate deliberately knows nothing about JNI. It calls a function pointer
//! with up to six word-sized arguments, which is all a `CallObjectMethodA` or a
//! `NewObjectA` needs, and leaves the caller to pull those pointers out of the
//! JNI table it is already walking. A runtime that understood JNI here would
//! have to be extended for every new call shape; this one does not.
//!
//! Two things it will not carry, both from the same cause -- a return value
//! comes back in an integer register: a Java method returning `float` or
//! `double`, and any argument that is not word-sized. Neither has come up; both
//! would want their own entry point rather than a wider `i64`.
//!
//! ## Why a call may convert its own result
//!
//! A handle the main thread hands back can be valid only for as long as the
//! main thread stays inside the call that produced it. That is not a corner
//! case here, it is the ordinary one: the main thread reaches this callback
//! from inside `MessageQueue.nativePollOnce`, a JNI native method, so every
//! local reference made here belongs to THAT frame and is popped when the
//! looper returns to Java. Promoting one on a later visit is too late --
//!
//!   JNI DETECTED ERROR IN APPLICATION: jobject is an invalid local reference
//!   (popped reference at index 11 in a table of size 7) in call to NewGlobalRef
//!
//! -- which is a crash, not an error, because CheckJNI aborts the process.
//!
//! So a caller may name a `promote` function, applied to the result before the
//! visit ends, and a `release` for the temporary it replaces. This stays out of
//! JNI: the runtime knows only that a result may need converting while the call
//! that made it is still on the stack, and the caller supplies both functions.

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[link(name = "android")]
unsafe extern "C" {
    fn ALooper_forThread() -> *mut c_void;
    fn ALooper_addFd(
        looper: *mut c_void,
        fd: c_int,
        ident: c_int,
        events: c_int,
        callback: Option<extern "C" fn(c_int, c_int, *mut c_void) -> c_int>,
        data: *mut c_void,
    ) -> c_int;
    fn ALooper_removeFd(looper: *mut c_void, fd: c_int) -> c_int;
}
unsafe extern "C" {
    fn pipe(fds: *mut c_int) -> c_int;
    fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn gettid() -> c_int;
}

/// Enough for `CallObjectMethodA(env, object, method, args)` and for
/// `NewObjectA`, with two to spare.
const MAX_ARGS: usize = 6;

const IDLE: u8 = 0;
const QUEUED: u8 = 1;
const DONE_OK: u8 = 2;
const ABANDONED: u8 = 3;

#[derive(Clone, Copy)]
struct Slot {
    function: usize,
    args: [i64; MAX_ARGS],
    count: usize,
    /// Applied as `promote(args[0], result)` before the visit ends, when the
    /// result would otherwise not outlive it. Zero to return the result as-is.
    promote: usize,
    /// Applied as `release(args[0], result)` to the value `promote` replaced.
    release: usize,
    result: i64,
    state: u8,
}

/// One slot, not a queue: a caller holds TURN for the whole exchange, so there
/// is never a second request in flight. A queue would buy nothing -- the caller
/// blocks until its own call returns either way -- and would have to answer what
/// happens to the backlog when the Activity goes away.
static SLOT: Mutex<Slot> = Mutex::new(Slot {
    function: 0,
    args: [0; MAX_ARGS],
    count: 0,
    promote: 0,
    release: 0,
    result: 0,
    state: IDLE,
});
static FINISHED: Condvar = Condvar::new();
static TURN: Mutex<()> = Mutex::new(());
static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static READ_FD: AtomicI32 = AtomicI32::new(-1);
static MAIN_THREAD: AtomicI32 = AtomicI32::new(0);

/// Open the gate. MUST be called on the main thread, where `ALooper_forThread`
/// is the main message loop's own looper.
pub fn install() {
    let mut fds = [0 as c_int; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let looper = unsafe { ALooper_forThread() };
    // ALOOPER_EVENT_INPUT. The identifier is ignored when a callback is given.
    if looper.is_null()
        || unsafe { ALooper_addFd(looper, fds[0], 2, 1, Some(run_queued), std::ptr::null_mut()) }
            != 1
    {
        unsafe {
            close(fds[0]);
            close(fds[1]);
        }
        return;
    }
    MAIN_THREAD.store(unsafe { gettid() }, Ordering::SeqCst);
    READ_FD.store(fds[0], Ordering::SeqCst);
    // Published last: a non-negative write end is what makes the gate usable,
    // so nothing may see it before the looper is listening.
    WRITE_FD.store(fds[1], Ordering::SeqCst);
}

/// Close the gate and release anyone waiting on it.
///
/// Called before the worker is joined, and the order matters: a worker parked on
/// a call whose main thread is inside `onDestroy` would otherwise wait out the
/// whole timeout, with the main thread blocked in `join` behind it.
pub fn shutdown() {
    let write_fd = WRITE_FD.swap(-1, Ordering::SeqCst);
    let read_fd = READ_FD.swap(-1, Ordering::SeqCst);
    {
        let mut slot = SLOT.lock().unwrap_or_else(|e| e.into_inner());
        if slot.state == QUEUED {
            slot.state = ABANDONED;
        }
    }
    FINISHED.notify_all();
    if read_fd >= 0 {
        let looper = unsafe { ALooper_forThread() };
        if !looper.is_null() {
            unsafe { ALooper_removeFd(looper, read_fd) };
        }
        unsafe { close(read_fd) };
    }
    if write_fd >= 0 {
        unsafe { close(write_fd) };
    }
}

extern "C" fn run_queued(fd: c_int, _events: c_int, _data: *mut c_void) -> c_int {
    let mut byte = 0u8;
    unsafe { read(fd, (&raw mut byte).cast(), 1) };
    let call = {
        let slot = SLOT.lock().unwrap_or_else(|e| e.into_inner());
        if slot.state != QUEUED {
            return 1;
        }
        *slot
    };
    // Called with no lock held. It re-enters Java and may run for as long as
    // Java takes; holding SLOT across it would block the very wakeup it ends in.
    let mut result = unsafe { invoke(call.function, &call.args[..call.count]) };
    // Still inside the call, which for a local reference is the only moment it
    // is legal. A zero result is nothing to convert, and is what a call that
    // threw leaves behind.
    if result != 0 && call.promote != 0 && call.count > 0 {
        let durable = unsafe { invoke(call.promote, &[call.args[0], result]) };
        if call.release != 0 {
            unsafe { invoke(call.release, &[call.args[0], result]) };
        }
        result = durable;
    }
    let mut slot = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.state == QUEUED {
        slot.result = result;
        slot.state = DONE_OK;
    }
    drop(slot);
    FINISHED.notify_all();
    1
}

/// # Safety
/// `function` must be a C function of `args.len()` word-sized parameters
/// returning a word-sized value, valid for this call.
unsafe fn invoke(function: usize, args: &[i64]) -> i64 {
    unsafe {
        match *args {
            [] => std::mem::transmute::<usize, extern "C" fn() -> i64>(function)(),
            [a] => std::mem::transmute::<usize, extern "C" fn(i64) -> i64>(function)(a),
            [a, b] => std::mem::transmute::<usize, extern "C" fn(i64, i64) -> i64>(function)(a, b),
            [a, b, c] => {
                std::mem::transmute::<usize, extern "C" fn(i64, i64, i64) -> i64>(function)(a, b, c)
            }
            [a, b, c, d] => std::mem::transmute::<usize, extern "C" fn(i64, i64, i64, i64) -> i64>(
                function,
            )(a, b, c, d),
            [a, b, c, d, e] => std::mem::transmute::<
                usize,
                extern "C" fn(i64, i64, i64, i64, i64) -> i64,
            >(function)(a, b, c, d, e),
            _ => std::mem::transmute::<usize, extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64>(
                function,
            )(args[0], args[1], args[2], args[3], args[4], args[5]),
        }
    }
}

/// 0 and the result on success; -1 with no gate; -2 on timeout; -3 for too many
/// arguments.
pub fn call_on_main(
    function: usize,
    args: &[i64],
    promote: usize,
    release: usize,
) -> Result<i64, i32> {
    if function == 0 || WRITE_FD.load(Ordering::SeqCst) < 0 {
        return Err(-1);
    }
    if args.len() > MAX_ARGS {
        return Err(-3);
    }
    // Already there. Not an optimisation: posting to our own looper and then
    // waiting for it is the one case that cannot finish.
    if unsafe { gettid() } == MAIN_THREAD.load(Ordering::SeqCst) {
        let result = unsafe { invoke(function, args) };
        // The frame is the caller's own and does not end here, so the result
        // needs no promotion -- but a caller that asked for one must get the
        // same kind of reference back either way.
        if result != 0 && promote != 0 && !args.is_empty() {
            let durable = unsafe { invoke(promote, &[args[0], result]) };
            if release != 0 {
                unsafe { invoke(release, &[args[0], result]) };
            }
            return Ok(durable);
        }
        return Ok(result);
    }
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    {
        let mut slot = SLOT.lock().unwrap_or_else(|e| e.into_inner());
        slot.function = function;
        slot.count = args.len();
        slot.args[..args.len()].copy_from_slice(args);
        slot.promote = promote;
        slot.release = release;
        slot.state = QUEUED;
    }
    let byte = 1u8;
    let fd = WRITE_FD.load(Ordering::SeqCst);
    if fd < 0 || unsafe { write(fd, (&raw const byte).cast(), 1) } != 1 {
        SLOT.lock().unwrap_or_else(|e| e.into_inner()).state = IDLE;
        return Err(-1);
    }
    // Bounded. A main thread that never reaches its looper again -- destroyed,
    // or wedged in an ANR -- must not park the interpreter for ever, because a
    // Lisp worker that never returns cannot even report why.
    let (mut slot, wait) = FINISHED
        .wait_timeout_while(
            SLOT.lock().unwrap_or_else(|e| e.into_inner()),
            Duration::from_secs(5),
            |slot| slot.state == QUEUED,
        )
        .unwrap_or_else(|e| e.into_inner());
    let outcome = match slot.state {
        DONE_OK => Ok(slot.result),
        _ if wait.timed_out() => Err(-2),
        _ => Err(-1),
    };
    slot.state = IDLE;
    outcome
}
