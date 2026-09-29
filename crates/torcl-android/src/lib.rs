//! Reusable Android NativeActivity host. Application Lisp is supplied as APK assets.
#![cfg(target_os = "android")]

mod lifecycle;
mod main_thread;
use lifecycle::ActivityState;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The live activity, for the few platform calls that need it rather than the
/// worker's ActivityState. Only one may exist at a time -- ACTIVE enforces that
/// above -- so a single slot is the whole story rather than a simplification.
static ACTIVITY: std::sync::atomic::AtomicPtr<ANativeActivity> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
struct Activity {
    state: Arc<ActivityState>,
    worker: Option<JoinHandle<()>>,
    window: *mut c_void,
}

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

#[link(name = "android")]
unsafe extern "C" {
    fn ANativeActivity_finish(activity: *mut ANativeActivity);
    fn ANativeWindow_acquire(window: *mut c_void);
    fn ANativeWindow_release(window: *mut c_void);
    fn AAssetManager_open(manager: *mut c_void, name: *const c_char, mode: c_int) -> *mut c_void;
    fn AAsset_getLength64(asset: *mut c_void) -> i64;
    fn AAsset_read(asset: *mut c_void, buffer: *mut c_void, count: usize) -> c_int;
    fn AAsset_close(asset: *mut c_void);
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
    fn AInputEvent_getType(event: *const c_void) -> i32;
    fn AMotionEvent_getAction(event: *const c_void) -> i32;
    fn AMotionEvent_getX(event: *const c_void, index: usize) -> f32;
    fn AMotionEvent_getY(event: *const c_void, index: usize) -> f32;
    fn AKeyEvent_getAction(event: *const c_void) -> i32;
    fn AKeyEvent_getKeyCode(event: *const c_void) -> i32;
    fn AKeyEvent_getMetaState(event: *const c_void) -> i32;
}

fn read_asset(manager: *mut c_void, name: &str) -> Result<Vec<u8>, String> {
    let name = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
    let asset = unsafe { AAssetManager_open(manager, name.as_ptr(), 2) };
    if asset.is_null() {
        return Err(format!("missing APK asset {name:?}"));
    }
    let result = (|| {
        let size = unsafe { AAsset_getLength64(asset) };
        if !(0..=64 * 1024 * 1024).contains(&size) {
            return Err("asset exceeds 64 MiB".into());
        }
        let mut bytes = vec![0; size as usize];
        let mut offset = 0;
        while offset < bytes.len() {
            let count = unsafe {
                AAsset_read(
                    asset,
                    bytes[offset..].as_mut_ptr().cast(),
                    bytes.len() - offset,
                )
            };
            if count <= 0 {
                return Err(format!("cannot read APK asset {name:?}"));
            }
            offset += count as usize;
        }
        Ok(bytes)
    })();
    unsafe { AAsset_close(asset) };
    result
}

// Extract the explicitly indexed assets into this app's private directory so
// ordinary Lisp LOAD and file APIs work for nested application resources.
fn unpack_assets(activity: &ANativeActivity) -> Result<PathBuf, String> {
    let index = read_asset(activity.asset_manager, "torcl-assets.txt")?;
    let index = std::str::from_utf8(&index).map_err(|e| e.to_string())?;
    let mut lines = index.lines();
    if lines.next() != Some("torcl-android-assets-v1") {
        return Err("incompatible asset protocol".into());
    }
    let root = PathBuf::from(
        unsafe { CStr::from_ptr(activity.internal_data_path) }
            .to_string_lossy()
            .as_ref(),
    )
    .join("torcl-assets");
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    for name in lines {
        let path = Path::new(name);
        if name.is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!("invalid asset path {name:?}"));
        }
        let bytes = read_asset(activity.asset_manager, name)?;
        let destination = root.join(path);
        std::fs::create_dir_all(destination.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(destination, bytes).map_err(|e| e.to_string())?;
    }
    Ok(root)
}

/// # Safety
/// Android supplies a valid NativeActivity and callback table for its lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ANativeActivity_onCreate(
    activity: *mut ANativeActivity,
    _saved_state: *mut c_void,
    _saved_state_size: usize,
) {
    if activity.is_null() {
        return;
    }
    if ACTIVE.swap(true, Ordering::SeqCst) {
        log("only one TorCL Activity may run in a process");
        unsafe { ANativeActivity_finish(activity) };
        return;
    }
    ACTIVITY.store(activity, Ordering::SeqCst);
    capture_standard_streams();
    // On the main thread, which is the only place the gate can be opened, and
    // before the worker starts so that its very first frame may use it.
    main_thread::install();
    let result = (|| -> Result<(), String> {
        let root = unpack_assets(unsafe { &*activity })?;
        let state = Arc::new(ActivityState::default());
        state.set_paused(true);
        let worker_state = state.clone();
        let worker = std::thread::Builder::new().name("torcl-android".into())
            .stack_size(16 * 1024 * 1024).spawn(move || {
                let result = std::panic::catch_unwind(|| {
                    std::env::set_current_dir(root).map_err(|e| e.to_string())?;
                    // Debug knobs, on a device where there is otherwise no way
                    // to set one. An optional `torcl.env` asset, KEY=VALUE per
                    // line, applied before the interpreter starts -- which is
                    // when TORCL_GC_STRESS, TORCL_DISABLE_T2 and the rest are
                    // read. `setprop wrap.<package>` is the usual route and is
                    // denied to the shell user on a production build, so
                    // without this an Android-only bug cannot be bisected with
                    // the tools the rest of TorCL is debugged with. Read from
                    // the unpacked asset directory, so it ships in the APK and
                    // no app can be reconfigured from outside itself.
                    if let Ok(text) = std::fs::read_to_string("torcl.env") {
                        for line in text.lines() {
                            let line = line.trim();
                            if line.is_empty() || line.starts_with('#') {
                                continue;
                            }
                            if let Some((name, value)) = line.split_once('=') {
                                let (name, value) = (name.trim(), value.trim());
                                if name.is_empty() || name.contains('\0') {
                                    continue;
                                }
                                log(&format!("torcl.env: {name}={value}"));
                                // SAFETY: the interpreter has not started and
                                // no other thread in this process reads the
                                // environment; the main thread is inside the
                                // platform looper.
                                unsafe { std::env::set_var(name, value) };
                            }
                        }
                    }
                    let form = format!("(progn (load \"android.lisp\") (load \"app.lisp\") (funcall (find-symbol \"RUN\" \"TORCL-ANDROID\") {}))", Arc::as_ptr(&worker_state) as usize);
                    torcl::run(&["torcl".into(), "--no-init".into(), "--eval".into(), form])
                        .map_err(|e| e.to_string())
                });
                log(&format!("Lisp worker finished: {result:?}"));
                worker_state.shutdown();
                worker_state.finish_window();
            }).map_err(|e| e.to_string())?;
        let context = Box::new(Activity {
            state,
            worker: Some(worker),
            window: std::ptr::null_mut(),
        });
        unsafe {
            (*activity).instance = Box::into_raw(context).cast();
            let callbacks = &mut *(*activity).callbacks;
            callbacks.on_native_window_created = Some(window_created);
            callbacks.on_native_window_destroyed = Some(window_destroyed);
            callbacks.on_input_queue_created = Some(input_created);
            callbacks.on_input_queue_destroyed = Some(input_destroyed);
            callbacks.on_pause = Some(paused);
            callbacks.on_resume = Some(resumed);
            callbacks.on_destroy = Some(destroyed);
        }
        Ok(())
    })();
    if let Err(error) = result {
        log(&format!("Activity startup failed: {error}"));
        main_thread::shutdown();
        ACTIVITY.store(std::ptr::null_mut(), Ordering::SeqCst);
        ACTIVE.store(false, Ordering::SeqCst);
        unsafe { ANativeActivity_finish(activity) };
    }
}

fn context(activity: *mut ANativeActivity) -> &'static mut Activity {
    // Only Android's serialized main-thread callbacks access the Activity box.
    unsafe { &mut *((*activity).instance as *mut Activity) }
}
extern "C" fn window_created(activity: *mut ANativeActivity, window: *mut c_void) {
    let context = context(activity);
    unsafe { ANativeWindow_acquire(window) };
    context.window = window;
    context.state.set_window(window as usize);
}
extern "C" fn window_destroyed(activity: *mut ANativeActivity, _window: *mut c_void) {
    let context = context(activity);
    context.state.destroy_window(); // Lisp has released EGL before this returns.
    if !context.window.is_null() {
        unsafe { ANativeWindow_release(context.window) };
        context.window = std::ptr::null_mut();
    }
}
extern "C" fn paused(activity: *mut ANativeActivity) {
    context(activity).state.set_paused(true);
}
extern "C" fn resumed(activity: *mut ANativeActivity) {
    context(activity).state.set_paused(false);
}
extern "C" fn destroyed(activity: *mut ANativeActivity) {
    let mut context = unsafe { Box::from_raw((*activity).instance as *mut Activity) };
    context.state.shutdown();
    // Before the join below, not after: a worker parked on a main-thread call
    // would otherwise be waiting for this very thread, which is waiting for it.
    main_thread::shutdown();
    if let Some(worker) = context.worker.take() {
        let _ = worker.join();
    }
    if !context.window.is_null() {
        unsafe { ANativeWindow_release(context.window) };
    }
    unsafe { (*activity).instance = std::ptr::null_mut() };
    ACTIVITY.store(std::ptr::null_mut(), Ordering::SeqCst);
    ACTIVE.store(false, Ordering::SeqCst);
}

extern "C" fn drain_input(_fd: c_int, _events: c_int, data: *mut c_void) -> c_int {
    let activity = data as *mut ANativeActivity;
    // Input queue is passed through the Activity's main-thread callback state.
    let queue = INPUT_QUEUE.with(|q| q.get());
    loop {
        let mut event = std::ptr::null_mut();
        if unsafe { AInputQueue_getEvent(queue, &mut event) } < 0 {
            break;
        }
        if unsafe { AInputQueue_preDispatchEvent(queue, event) } != 0 {
            continue;
        }
        // AINPUT_EVENT_TYPE_KEY is 1 and AINPUT_EVENT_TYPE_MOTION is 2. Both
        // arrive on this one queue; keys used to fall into the `else` and be
        // discarded, which made the runtime look as though it had no key path
        // at all when it simply had no branch.
        let handled = match unsafe { AInputEvent_getType(event) } {
            2 => {
                context(activity).state.touch(
                    unsafe { AMotionEvent_getAction(event) } & 255,
                    unsafe { AMotionEvent_getX(event, 0) },
                    unsafe { AMotionEvent_getY(event, 0) },
                );
                1
            }
            1 => {
                let code = unsafe { AKeyEvent_getKeyCode(event) };
                context(activity).state.key(
                    unsafe { AKeyEvent_getAction(event) },
                    code,
                    unsafe { AKeyEvent_getMetaState(event) },
                );
                // BACK stays the system's. Reporting it handled would trap the
                // user in the application with no way out, which is a far worse
                // failure than an application not seeing the key.
                i32::from(code != 4)
            }
            _ => 0,
        };
        unsafe { AInputQueue_finishEvent(queue, event, handled) };
    }
    1
}
thread_local! { static INPUT_QUEUE: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(std::ptr::null_mut()) }; }
extern "C" fn input_created(activity: *mut ANativeActivity, queue: *mut c_void) {
    INPUT_QUEUE.with(|q| q.set(queue));
    unsafe {
        AInputQueue_attachLooper(
            queue,
            ALooper_forThread(),
            1,
            Some(drain_input),
            activity.cast(),
        )
    };
}
extern "C" fn input_destroyed(_activity: *mut ANativeActivity, queue: *mut c_void) {
    unsafe { AInputQueue_detachLooper(queue) };
    INPUT_QUEUE.with(|q| q.set(std::ptr::null_mut()));
}

// Private ABI v1, consumed by the bundled android.lisp. All pointers are valid
// only while the interpreter worker is running and the Activity owns its Arc.
#[unsafe(no_mangle)]
pub extern "C" fn torcl_android_api_version() -> i32 {
    // 2 adds torcl_android_key; 3 adds torcl_android_call_on_main. Callers
    // should test for AT LEAST the version they need rather than for equality,
    // so that a later addition does not break an application that never uses it.
    3
}

/// Call `function` with `count` word-sized arguments on the Android main thread
/// and wait for it, writing its result to `out`.
///
/// 0 on success; -1 when there is no gate (before the Activity exists, or after
/// it is gone); -2 when the main thread did not answer within five seconds; -3
/// for more than six arguments.
///
/// The arguments and the result are words. That is exactly enough for the JNI
/// calls a view hierarchy needs -- `CallObjectMethodA(env, object, method,
/// args)` and `NewObjectA` -- and the caller supplies the function pointer out
/// of the JNI table it is already walking, so this stays out of the business of
/// knowing which Java call an application wants to make. A Java method
/// returning `float` or `double` is the one shape it cannot carry.
///
/// `promote` and `release`, when given, run before the visit ends:
/// `promote(args[0], result)` replaces the result and `release(args[0], result)`
/// disposes of what it replaced. They exist because a handle the main thread
/// returns may be valid only while the call that made it is still on the stack
/// -- a JNI local reference is exactly that -- and a later visit is too late.
/// Both are skipped for a zero result.
///
/// # Safety
/// `function` must be a C function of `count` word-sized parameters, `args`
/// must point to `count` readable words, and `out` to one writable word.
/// `promote` and `release`, if not null, must take two words.
#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_call_on_main(
    function: *mut c_void,
    args: *const i64,
    count: i32,
    promote: *mut c_void,
    release: *mut c_void,
    out: *mut i64,
) -> i32 {
    if !(0..=6).contains(&count) || (count > 0 && args.is_null()) {
        return -3;
    }
    // from_raw_parts refuses a null base even for an empty slice.
    let args = match count {
        0 => &[][..],
        count => unsafe { std::slice::from_raw_parts(args, count as usize) },
    };
    match main_thread::call_on_main(
        function as usize,
        args,
        promote as usize,
        release as usize,
    ) {
        Ok(result) => {
            if !out.is_null() {
                unsafe { *out = result };
            }
            0
        }
        Err(code) => code,
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_wait_window(state: *const ActivityState) -> usize {
    unsafe { &*state }.wait_window()
}
#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_finish_window(state: *const ActivityState) {
    unsafe { &*state }.finish_window();
}
#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_running(state: *const ActivityState) -> i32 {
    unsafe { &*state }.running() as i32
}
#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_paused(state: *const ActivityState) -> i32 {
    unsafe { &*state }.paused() as i32
}
/// The live ANativeActivity, or null when there is none.
///
/// Its `clazz` field -- the fourth pointer -- is a global reference to the Java
/// NativeActivity itself, so handing this pointer out gives Lisp the Context,
/// the Window, the View hierarchy and getSystemService through JNI, and keeps
/// this runtime out of the business of deciding which of those an application
/// may reach.
///
/// That generality is not a preference. Measured on Android 16, the NDK's own
/// ANativeActivity_showSoftInput leaves mInputShown false and raises no
/// keyboard, while InputMethodManager.showSoftInput on the decor view -- the
/// same request, made through JNI -- returns true and raises it. Wrapping the
/// NDK call here would have shipped a function that reliably does nothing and
/// whose void return looks like success.
#[unsafe(no_mangle)]
extern "C" fn torcl_android_activity() -> *mut c_void {
    ACTIVITY.load(Ordering::SeqCst).cast()
}

/// The next queued key event, or -1 when there is none.
///
/// Returns the action and writes the key code and meta state to `output`, which
/// mirrors `torcl_android_touch` so the two poll the same way. A key CODE, not a
/// character: turning one into text needs the keyboard layout and the meta
/// state, which is a decision for the caller and not for this queue.
#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_key(state: *const ActivityState, output: *mut i32) -> i32 {
    if let Some((action, code, meta)) = unsafe { &*state }.poll_key() {
        unsafe {
            *output = code;
            *output.add(1) = meta;
        }
        action
    } else {
        -1
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_touch(state: *const ActivityState, output: *mut f32) -> i32 {
    if let Some((action, x, y)) = unsafe { &*state }.poll_touch() {
        unsafe {
            *output = x;
            *output.add(1) = y;
        }
        action
    } else {
        -1
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn torcl_android_log(text: *const c_char) {
    log(&unsafe { CStr::from_ptr(text) }.to_string_lossy());
}

/// Send this process's stderr and stdout to logcat, under the tag `torcl-err`.
///
/// Android discards both unless `setprop log.redirect-stdio true` is set, which
/// is denied to the shell user on a production build -- so a Rust panic, an
/// assertion, and every `eprintln!` diagnostic the runtime has (the GC stress
/// bisector's allocation backtrace among them) vanished silently on a phone.
/// The reader thread lives for the life of the process on purpose: the writer
/// end is dup'd onto fd 1 and 2, which nothing closes.
fn capture_standard_streams() {
    unsafe extern "C" {
        fn pipe(fds: *mut c_int) -> c_int;
        fn dup2(from: c_int, to: c_int) -> c_int;
        fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    }
    let mut fds = [0 as c_int; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let (reader, writer) = (fds[0], fds[1]);
    unsafe {
        dup2(writer, 1);
        dup2(writer, 2);
    }
    let _ = std::thread::Builder::new()
        .name("torcl-stderr".into())
        .spawn(move || {
            let mut pending = Vec::<u8>::new();
            let mut buffer = [0u8; 1024];
            loop {
                let n = unsafe { read(reader, buffer.as_mut_ptr().cast(), buffer.len()) };
                if n <= 0 {
                    return;
                }
                pending.extend_from_slice(&buffer[..n as usize]);
                // One logcat line per output line; a line longer than the log
                // buffer is truncated by liblog, not by us.
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
                    log_tagged("torcl-err", text.replace('\0', " ").trim_end());
                }
            }
        });
}

fn log_tagged(tag: &str, message: &str) {
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
    if let (Ok(tag), Ok(text)) = (
        std::ffi::CString::new(tag),
        std::ffi::CString::new(message),
    ) {
        unsafe { __android_log_write(4, tag.as_ptr(), text.as_ptr()) };
    }
}

fn log(message: &str) {
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
    if let Ok(text) = std::ffi::CString::new(message) {
        unsafe { __android_log_write(4, c"torcl".as_ptr(), text.as_ptr()) };
    }
}
