//! GC-managed handles for stable, Rust-owned synchronization primitives.
use std::sync::Arc;
use std::time::Duration;
use torcl_rt::TorclError;
use torcl_rt::object::{ObjectHeader, type_id};
use torcl_rt::sync::TorclMutex;
use torcl_rt::value::{NIL, T, TorclVal};

pub fn mutex_p(value: TorclVal) -> bool {
    value.is_heap_object()
        && unsafe { (*(value.as_ptr() as *const ObjectHeader)).type_id() == type_id::MUTEX }
}

pub fn make_mutex(name: Option<String>, recursive: bool) -> Result<TorclVal, TorclError> {
    crate::streams::install_gc_hooks();
    let mutex = Arc::new(TorclMutex::new(name, recursive));
    let body = torcl_rt::alloc_typed(8, type_id::MUTEX).ok_or(TorclError::Oom)?;
    unsafe { *(body as *mut *const TorclMutex) = Arc::into_raw(mutex) };
    // No Lisp allocation occurs between allocating the handle and registering
    // its finalizer. The GC finalizer registry keys objects by body address.
    torcl_rt::gc::register_finalizer(TorclVal::from_raw(body as u64), NIL)?;
    Ok(unsafe { TorclVal::from_heap_ptr(body.sub(8)) })
}

fn native_mutex(value: TorclVal) -> Result<Arc<TorclMutex>, TorclError> {
    if !mutex_p(value) {
        return Err(TorclError::TypeError {
            datum: value,
            expected: "TORCL-THREAD:MUTEX".into(),
        });
    }
    unsafe {
        let pointer = *(value.as_ptr().add(8) as *const *const TorclMutex);
        if pointer.is_null() {
            return Err(TorclError::ProgramError(
                "mutex is unavailable after image restore".into(),
            ));
        }
        // Retain stable native storage before entering a blocking region. The
        // handle may move while blocked; never read it again after the wait.
        Arc::increment_strong_count(pointer);
        Ok(Arc::from_raw(pointer))
    }
}

pub fn grab_mutex(
    value: TorclVal,
    wait: bool,
    timeout: Option<Duration>,
) -> Result<bool, TorclError> {
    native_mutex(value)?.grab(wait, timeout)
}

pub fn release_mutex(value: TorclVal, if_owned: bool) -> Result<bool, TorclError> {
    match native_mutex(value)?.release() {
        Ok(()) => Ok(true),
        Err(TorclError::ProgramError(_)) if if_owned => Ok(false),
        Err(error) => Err(error),
    }
}

/// Drop the handle's Arc, without acquiring a native wait-queue lock in GC.
///
/// # Safety
/// `body` must be the body of an exclusively owned, dead MUTEX heap object.
pub(crate) unsafe fn finalize_mutex(body: *mut u8) {
    unsafe {
        let slot = body as *mut *const TorclMutex;
        let pointer = *slot;
        *slot = std::ptr::null();
        if !pointer.is_null() {
            drop(Arc::from_raw(pointer));
        }
    }
}

/// Private Lisp bridge. Public lambda lists and policy live in boot.lisp.
pub fn call(args: &[TorclVal]) -> Result<TorclVal, TorclError> {
    let operation = args
        .first()
        .and_then(|v| v.symbol_index())
        .and_then(torcl_rt::symbols::symbol_name)
        .unwrap_or_default();
    let truth = |value| if value { T } else { NIL };
    match (
        operation.rsplit(':').next().unwrap_or_default(),
        &args[args.len().min(1)..],
    ) {
        ("MAKE", [name, recursive]) => {
            let name = if *name == NIL {
                None
            } else {
                let bytes = crate::sequences::string_content_bytes(*name).ok_or_else(|| {
                    TorclError::TypeError {
                        datum: *name,
                        expected: "(OR NULL STRING)".into(),
                    }
                })?;
                Some(String::from_utf8(bytes).expect("string_content_bytes is UTF-8"))
            };
            make_mutex(name, *recursive != NIL)
        }
        ("P", [value]) => Ok(truth(mutex_p(*value))),
        ("GRAB", [value, wait, timeout]) => {
            let duration = if *timeout == NIL {
                None
            } else {
                if !timeout.is_double_float() {
                    return Err(TorclError::TypeError {
                        datum: *timeout,
                        expected: "(OR NULL (DOUBLE-FLOAT 0))".into(),
                    });
                }
                Some(
                    Duration::try_from_secs_f64(timeout.as_double_float()).map_err(|_| {
                        TorclError::TypeError {
                            datum: *timeout,
                            expected: "finite non-negative timeout".into(),
                        }
                    })?,
                )
            };
            Ok(truth(grab_mutex(*value, *wait != NIL, duration)?))
        }
        ("RELEASE", [value]) => {
            release_mutex(*value, false)?;
            Ok(NIL)
        }
        ("RELEASE-IF-OWNED", [value]) => Ok(truth(release_mutex(*value, true)?)),
        _ => Err(TorclError::ProgramError(format!(
            "invalid %NATIVE-MUTEX operation {operation:?} with {} arguments",
            args.len().saturating_sub(1)
        ))),
    }
}
