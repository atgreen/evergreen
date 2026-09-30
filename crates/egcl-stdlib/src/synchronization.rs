// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! GC-managed handles for stable, Rust-owned synchronization primitives.
use std::sync::Arc;
use std::time::Duration;
use egcl_rt::EgclError;
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::sync::{EgclCondVar, EgclMutex};
use egcl_rt::value::{NIL, T, EgclVal};

pub fn mutex_p(value: EgclVal) -> bool {
    value.is_heap_object()
        && unsafe { (*(value.as_ptr() as *const ObjectHeader)).type_id() == type_id::MUTEX }
}

pub fn make_mutex(name: Option<String>, recursive: bool) -> Result<EgclVal, EgclError> {
    crate::streams::install_gc_hooks();
    let mutex = Arc::new(EgclMutex::new(name, recursive));
    let body = egcl_rt::alloc_typed(8, type_id::MUTEX).ok_or(EgclError::Oom)?;
    unsafe { *(body as *mut *const EgclMutex) = Arc::into_raw(mutex) };
    // No Lisp allocation occurs between allocating the handle and registering
    // its finalizer. The GC finalizer registry keys objects by body address.
    egcl_rt::gc::register_finalizer(EgclVal::from_raw(body as u64), NIL)?;
    Ok(unsafe { EgclVal::from_heap_ptr(body.sub(8)) })
}

fn native_mutex(value: EgclVal) -> Result<Arc<EgclMutex>, EgclError> {
    if !mutex_p(value) {
        return Err(EgclError::TypeError {
            datum: value,
            expected: "EGCL-THREAD:MUTEX".into(),
        });
    }
    unsafe {
        let pointer = *(value.as_ptr().add(8) as *const *const EgclMutex);
        if pointer.is_null() {
            return Err(EgclError::ProgramError(
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
    value: EgclVal,
    wait: bool,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    native_mutex(value)?.grab(wait, timeout)
}

pub fn release_mutex(value: EgclVal, if_owned: bool) -> Result<bool, EgclError> {
    match native_mutex(value)?.release() {
        Ok(()) => Ok(true),
        Err(EgclError::ProgramError(_)) if if_owned => Ok(false),
        Err(error) => Err(error),
    }
}

/// Drop the handle's Arc, without acquiring a native wait-queue lock in GC.
///
/// # Safety
/// `body` must be the body of an exclusively owned, dead MUTEX heap object.
pub(crate) unsafe fn finalize_mutex(body: *mut u8) {
    unsafe {
        let slot = body as *mut *const EgclMutex;
        let pointer = *slot;
        *slot = std::ptr::null();
        if !pointer.is_null() {
            drop(Arc::from_raw(pointer));
        }
    }
}

pub fn condition_variable_p(value: EgclVal) -> bool {
    value.is_heap_object()
        && unsafe {
            (*(value.as_ptr() as *const ObjectHeader)).type_id() == type_id::CONDITION_VARIABLE
        }
}

pub fn make_condition_variable(name: Option<String>) -> Result<EgclVal, EgclError> {
    crate::streams::install_gc_hooks();
    let condition = Arc::new(EgclCondVar::new(name));
    let body = egcl_rt::alloc_typed(8, type_id::CONDITION_VARIABLE).ok_or(EgclError::Oom)?;
    unsafe { *(body as *mut *const EgclCondVar) = Arc::into_raw(condition) };
    egcl_rt::gc::register_finalizer(EgclVal::from_raw(body as u64), NIL)?;
    Ok(unsafe { EgclVal::from_heap_ptr(body.sub(8)) })
}

fn native_condition(value: EgclVal) -> Result<Arc<EgclCondVar>, EgclError> {
    if !condition_variable_p(value) {
        return Err(EgclError::TypeError {
            datum: value,
            expected: "EGCL-THREAD:CONDITION-VARIABLE".into(),
        });
    }
    unsafe {
        let pointer = *(value.as_ptr().add(8) as *const *const EgclCondVar);
        if pointer.is_null() {
            return Err(EgclError::ProgramError(
                "condition variable is unavailable after image restore".into(),
            ));
        }
        Arc::increment_strong_count(pointer);
        Ok(Arc::from_raw(pointer))
    }
}

pub fn condition_wait(
    condition: EgclVal,
    mutex: EgclVal,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    // Retain both native objects before blocking: neither acquisition allocates
    // on the Lisp heap. Never inspect either moving handle after entering WAIT.
    let condition = native_condition(condition)?;
    let mutex = native_mutex(mutex)?;
    condition.wait(&mutex, timeout)
}

pub fn condition_notify(condition: EgclVal, count: usize) -> Result<usize, EgclError> {
    Ok(native_condition(condition)?.notify(count))
}

/// # Safety
/// `body` is the body of an exclusively owned, dead CONDITION_VARIABLE object.
pub(crate) unsafe fn finalize_condition_variable(body: *mut u8) {
    unsafe {
        let slot = body as *mut *const EgclCondVar;
        let pointer = *slot;
        *slot = std::ptr::null();
        if !pointer.is_null() {
            drop(Arc::from_raw(pointer));
        }
    }
}

fn native_name(value: EgclVal) -> Result<Option<String>, EgclError> {
    if value == NIL {
        return Ok(None);
    }
    let bytes =
        crate::sequences::string_content_bytes(value).ok_or_else(|| EgclError::TypeError {
            datum: value,
            expected: "(OR NULL STRING)".into(),
        })?;
    Ok(Some(
        String::from_utf8(bytes).expect("string_content_bytes is UTF-8"),
    ))
}

fn native_timeout(value: EgclVal) -> Result<Option<Duration>, EgclError> {
    if value == NIL {
        return Ok(None);
    }
    if !value.is_double_float() {
        return Err(EgclError::TypeError {
            datum: value,
            expected: "(OR NULL (DOUBLE-FLOAT 0))".into(),
        });
    }
    Duration::try_from_secs_f64(value.as_double_float())
        .map(Some)
        .map_err(|_| EgclError::TypeError {
            datum: value,
            expected: "finite non-negative timeout".into(),
        })
}

/// Private condition bridge; public argument policy stays in boot.lisp.
pub fn condition_call(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    let operation = args
        .first()
        .and_then(|v| v.symbol_index())
        .and_then(egcl_rt::symbols::symbol_name)
        .unwrap_or_default();
    match (
        operation.rsplit(':').next().unwrap_or_default(),
        &args[args.len().min(1)..],
    ) {
        ("MAKE", [name]) => make_condition_variable(native_name(*name)?),
        ("P", [value]) => Ok(if condition_variable_p(*value) { T } else { NIL }),
        ("WAIT", [condition, mutex, timeout]) => {
            let timeout = native_timeout(*timeout)?;
            Ok(if condition_wait(*condition, *mutex, timeout)? {
                T
            } else {
                NIL
            })
        }
        ("NOTIFY", [condition, count]) => {
            if !count.is_fixnum() || count.as_fixnum() < 0 {
                return Err(EgclError::TypeError {
                    datum: *count,
                    expected: "non-negative fixnum".into(),
                });
            }
            Ok(EgclVal::from_fixnum(
                condition_notify(*condition, count.as_fixnum() as usize)? as i64,
            ))
        }
        ("BROADCAST", [condition]) => Ok(EgclVal::from_fixnum(condition_notify(
            *condition,
            usize::MAX,
        )? as i64)),
        _ => Err(EgclError::ProgramError(format!(
            "invalid %NATIVE-CONDITION operation {operation:?} with {} arguments",
            args.len().saturating_sub(1)
        ))),
    }
}

/// Private Lisp bridge. Public lambda lists and policy live in boot.lisp.
pub fn call(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    let operation = args
        .first()
        .and_then(|v| v.symbol_index())
        .and_then(egcl_rt::symbols::symbol_name)
        .unwrap_or_default();
    let truth = |value| if value { T } else { NIL };
    match (
        operation.rsplit(':').next().unwrap_or_default(),
        &args[args.len().min(1)..],
    ) {
        ("MAKE", [name, recursive]) => make_mutex(native_name(*name)?, *recursive != NIL),
        ("P", [value]) => Ok(truth(mutex_p(*value))),
        ("GRAB", [value, wait, timeout]) => {
            let duration = native_timeout(*timeout)?;
            Ok(truth(grab_mutex(*value, *wait != NIL, duration)?))
        }
        ("RELEASE", [value]) => {
            release_mutex(*value, false)?;
            Ok(NIL)
        }
        ("RELEASE-IF-OWNED", [value]) => Ok(truth(release_mutex(*value, true)?)),
        _ => Err(EgclError::ProgramError(format!(
            "invalid %NATIVE-MUTEX operation {operation:?} with {} arguments",
            args.len().saturating_sub(1)
        ))),
    }
}
