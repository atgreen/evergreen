//! TorCL as an Android NativeActivity (bliss-w2vp spike).
//!
//! Android hands a drawable surface only to code in the app's own process, so a
//! spawned `torcl` binary — the shape of the REPL app — can never draw. This
//! library is the other shape: the runtime linked INTO the activity, handed the
//! `ANativeWindow*`, with every EGL call made from Lisp through TorCL's FFI.
//!
//! Deliberately thin. Rust does three things: export the entry point Android
//! looks for, remember the window pointer, and start the interpreter on a form
//! that passes that pointer to Lisp. It contains no EGL whatsoever.

// NativeActivity and libandroid are available only on Android. Keep this
// workspace member empty on host targets so workspace tests can link there.
#![cfg(target_os = "android")]

use std::ffi::{c_char, c_int, c_void};
use std::os::raw::c_ulong;
use std::sync::atomic::{AtomicI32, Ordering};

/// Non-zero while the surface is alive. Lisp reads this through its own FFI
/// every frame and stops when it clears, which is what makes an endless render
/// loop safe: drawing into a destroyed surface is a crash, and Android destroys
/// it on rotate, background, or exit. Rust owns the flag because Android's
/// callback is the only thing that knows.
static RUNNING: AtomicI32 = AtomicI32::new(0);

/// `ANativeActivityCallbacks` (android/native_activity.h). Only the field
/// ORDER matters — Android writes function pointers into this struct, so the
/// layout has to match exactly even though this spike sets just one.
#[repr(C)]
pub struct ANativeActivityCallbacks {
    pub on_start: Option<extern "C" fn(*mut ANativeActivity)>,
    pub on_resume: Option<extern "C" fn(*mut ANativeActivity)>,
    pub on_save_instance_state:
        Option<extern "C" fn(*mut ANativeActivity, *mut usize) -> *mut c_void>,
    pub on_pause: Option<extern "C" fn(*mut ANativeActivity)>,
    pub on_stop: Option<extern "C" fn(*mut ANativeActivity)>,
    pub on_destroy: Option<extern "C" fn(*mut ANativeActivity)>,
    pub on_window_focus_changed: Option<extern "C" fn(*mut ANativeActivity, c_int)>,
    pub on_native_window_created: Option<extern "C" fn(*mut ANativeActivity, *mut c_void)>,
    pub on_native_window_resized: Option<extern "C" fn(*mut ANativeActivity, *mut c_void)>,
    pub on_native_window_redraw_needed: Option<extern "C" fn(*mut ANativeActivity, *mut c_void)>,
    pub on_native_window_destroyed: Option<extern "C" fn(*mut ANativeActivity, *mut c_void)>,
    pub on_input_queue_created: Option<extern "C" fn(*mut ANativeActivity, *mut c_void)>,
    pub on_input_queue_destroyed: Option<extern "C" fn(*mut ANativeActivity, *mut c_void)>,
    pub on_content_rect_changed: Option<extern "C" fn(*mut ANativeActivity, *const c_void)>,
    pub on_configuration_changed: Option<extern "C" fn(*mut ANativeActivity)>,
    pub on_low_memory: Option<extern "C" fn(*mut ANativeActivity)>,
}

/// `ANativeActivity`. Same story: the prefix through `internal_data_path` is
/// what this spike reads, but every field up to it must line up.
#[repr(C)]
pub struct ANativeActivity {
    pub callbacks: *mut ANativeActivityCallbacks,
    pub vm: *mut c_void,
    pub env: *mut c_void,
    pub clazz: *mut c_void,
    pub internal_data_path: *const c_char,
    pub external_data_path: *const c_char,
    pub sdk_version: i32,
    pub instance: *mut c_void,
    pub asset_manager: *mut c_void,
    pub obb_path: *const c_char,
}

/// The Lisp half. Embedded rather than read from disk: a file would reintroduce
/// exactly the baked-path problem that stopped `(require :asdf)` on a device
/// (bliss-bp4q).
const EGL_LISP: &str = include_str!("egl.lisp");

/// The scene — a Lisp list — and the walker that turns it into GLSL. Separate
/// from the plumbing because this is the interesting half: change the list and
/// the shader changes with it.
const SCENE_LISP: &str = include_str!("scene.lisp");

/// Android calls this when the activity starts; the symbol name is fixed.
///
/// # Safety
/// `activity` is Android's, valid for the call, and its `callbacks` struct is
/// ours to populate — that is the documented contract of this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ANativeActivity_onCreate(
    activity: *mut ANativeActivity,
    _saved_state: *mut c_void,
    _saved_state_size: usize,
) {
    if activity.is_null() {
        return;
    }
    let callbacks = unsafe { (*activity).callbacks };
    if !callbacks.is_null() {
        unsafe {
            (*callbacks).on_native_window_created = Some(on_native_window_created);
            (*callbacks).on_native_window_destroyed = Some(on_native_window_destroyed);
            (*callbacks).on_input_queue_created = Some(on_input_queue_created);
            (*callbacks).on_input_queue_destroyed = Some(on_input_queue_destroyed);
        };
    }
}

/// The surface exists. Start the interpreter on a thread of its own and give
/// Lisp the window address; everything after this is Lisp calling EGL.
extern "C" fn on_native_window_created(_activity: *mut ANativeActivity, window: *mut c_void) {
    let address = window as c_ulong;
    let running = (&RUNNING as *const AtomicI32) as c_ulong;
    RUNNING.store(1, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("torcl-egl".into())
        // The runtime publishes a 6 MiB self-call guard from near the base of
        // its thread's stack, so give it a stack that can hold one.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let form =
                format!("(progn {SCENE_LISP}\n{EGL_LISP}\n(torcl-egl-demo {address} {running}))");
            let args = vec![
                "torcl".to_string(),
                "--no-init".to_string(),
                "--eval".to_string(),
                form,
            ];
            match torcl::run(&args) {
                Ok(code) => log(&format!("torcl exited {code}")),
                Err(e) => log(&format!("torcl error: {e}")),
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| log(&format!("cannot spawn torcl thread: {e}")));
}

// libandroid's looper and input queue. NativeActivity REQUIRES the native side
// to service the input queue: events that are never consumed make the input
// dispatcher time out and Android kills the app with "isn't responding", even
// though the render thread is perfectly healthy.
#[link(name = "android")]
unsafe extern "C" {
    fn ALooper_forThread() -> *mut c_void;
    fn AInputQueue_attachLooper(
        queue: *mut c_void,
        looper: *mut c_void,
        ident: c_int,
        callback: Option<extern "C" fn(c_int, c_int, *mut c_void) -> c_int>,
        data: *mut c_void,
    );
    fn AInputQueue_detachLooper(queue: *mut c_void);
    fn AInputQueue_getEvent(queue: *mut c_void, event: *mut *mut c_void) -> i32;
    fn AInputQueue_preDispatchEvent(queue: *mut c_void, event: *mut c_void) -> i32;
    fn AInputQueue_finishEvent(queue: *mut c_void, event: *mut c_void, handled: c_int);
}

/// Drain the queue. The spike does nothing with the events yet — it only has to
/// consume them, which is what keeps Android from declaring the app hung.
extern "C" fn drain_input(_fd: c_int, _events: c_int, data: *mut c_void) -> c_int {
    let queue = data;
    loop {
        let mut event: *mut c_void = std::ptr::null_mut();
        // SAFETY: `queue` is the one Android handed to onInputQueueCreated and
        // is live until onInputQueueDestroyed detaches it.
        if unsafe { AInputQueue_getEvent(queue, &mut event) } < 0 {
            break;
        }
        if unsafe { AInputQueue_preDispatchEvent(queue, event) } != 0 {
            continue;
        }
        unsafe { AInputQueue_finishEvent(queue, event, 0) };
    }
    1 // keep the callback registered
}

extern "C" fn on_input_queue_created(_activity: *mut ANativeActivity, queue: *mut c_void) {
    // This callback runs on the main thread, so its looper is the one to attach.
    unsafe { AInputQueue_attachLooper(queue, ALooper_forThread(), 1, Some(drain_input), queue) };
}

extern "C" fn on_input_queue_destroyed(_activity: *mut ANativeActivity, queue: *mut c_void) {
    unsafe { AInputQueue_detachLooper(queue) };
}

/// The surface is going away. Clear the flag and let the Lisp loop notice on
/// its next frame; returning from this callback while Lisp still held the
/// surface would tear it down underneath a live EGL context.
extern "C" fn on_native_window_destroyed(_activity: *mut ANativeActivity, _window: *mut c_void) {
    RUNNING.store(0, Ordering::SeqCst);
}

/// Android's log, so output survives having no stdout.
fn log(message: &str) {
    unsafe extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
    if let (Ok(tag), Ok(text)) = (
        std::ffi::CString::new("torcl"),
        std::ffi::CString::new(message),
    ) {
        // SAFETY: both pointers are NUL-terminated and live for the call.
        unsafe { __android_log_write(4, tag.as_ptr(), text.as_ptr()) };
    }
}
