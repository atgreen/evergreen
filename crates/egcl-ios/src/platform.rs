// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::ffi::CStr;
use std::mem::size_of;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Default)]
struct HostState {
    status: Status,
    error: String,
}
static HOST: Mutex<HostState> = Mutex::new(HostState {
    status: Status {
        state: IDLE,
        exit_code: 0,
    },
    error: String::new(),
});
fn lock() -> MutexGuard<'static, HostState> {
    HOST.lock().unwrap_or_else(|e| e.into_inner())
}

struct OwnedConfig {
    source: PathBuf,
    writable: PathBuf,
    entry: PathBuf,
}

unsafe fn string(pointer: *const c_char) -> Result<String, &'static str> {
    if pointer.is_null() {
        return Err("null configuration string");
    }
    // SAFETY: start's caller owns each readable NUL-terminated string.
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| "configuration strings must be UTF-8")
}

pub(super) unsafe fn start(config: *const Config) -> c_int {
    if config.is_null() {
        return INVALID_ARGUMENT;
    }
    // SAFETY: C caller supplies a complete aligned configuration.
    let config = unsafe { &*config };
    if config.abi_version != ABI_VERSION || (config.struct_size as usize) < size_of::<Config>() {
        return INVALID_ARGUMENT;
    }
    // Copy before returning to C; no borrowed host memory reaches a worker.
    let owned = (|| -> Result<OwnedConfig, &'static str> {
        let source = PathBuf::from(unsafe { string(config.source_directory) }?);
        let writable = PathBuf::from(unsafe { string(config.writable_directory) }?);
        let entry = PathBuf::from(unsafe { string(config.entry_file) }?);
        if !source.is_absolute() || !writable.is_absolute() {
            return Err("source and writable directories must be absolute");
        }
        if entry.as_os_str().is_empty()
            || entry
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err("entry file must be relative to the source directory");
        }
        Ok(OwnedConfig {
            source,
            writable,
            entry,
        })
    })();
    let owned = match owned {
        Ok(config) => config,
        Err(error) => {
            lock().error = error.to_owned();
            return INVALID_ARGUMENT;
        }
    };
    {
        let mut host = lock();
        if host.status.state != IDLE {
            return ALREADY_STARTED;
        }
        if let Err(error) = egcl_rt::host::select_embedded() {
            host.error = error.to_string();
            return INVALID_ARGUMENT;
        }
        host.status.state = STARTING;
    }
    let result = std::thread::Builder::new()
        .name("egcl-ios-supervisor".into())
        .spawn(move || {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| supervise(owned)));
            if outcome.is_err() {
                // A control-thread panic is not completion: Lisp may still run.
                egcl_rt::host::request_stop();
                let mut host = lock();
                host.status.state = STOPPING;
                host.error =
                    "supervisor panicked; cooperative stop requested, completion unknown".into();
            }
        });
    if let Err(error) = result {
        let mut host = lock();
        egcl_rt::host::request_stop();
        host.status.state = FAILED;
        host.status.exit_code = 1;
        host.error = format!("cannot start supervisor: {error}");
        return INTERNAL_ERROR;
    }
    OK
}

pub(super) fn request_stop() -> c_int {
    {
        let mut host = lock();
        match host.status.state {
            IDLE => return INVALID_ARGUMENT,
            FINISHED | FAILED => return OK,
            _ => host.status.state = STOPPING,
        }
    }
    egcl_rt::host::request_stop();
    OK
}
pub(super) fn status() -> Status {
    lock().status
}
pub(super) fn error_text() -> String {
    lock().error.replace('\0', "\\0")
}

fn lisp_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}
fn directory_name(path: &Path) -> Result<String, String> {
    let mut name = path
        .to_str()
        .ok_or("application directory is not UTF-8")?
        .to_owned();
    if !name.ends_with('/') {
        name.push('/');
    }
    Ok(name)
}

fn arguments(config: OwnedConfig) -> Result<Vec<String>, String> {
    let source = config
        .source
        .canonicalize()
        .map_err(|e| format!("source directory: {e}"))?;
    if !source.is_dir() {
        return Err("source directory is not a directory".into());
    }
    std::fs::create_dir_all(&config.writable).map_err(|e| format!("writable directory: {e}"))?;
    let writable = config
        .writable
        .canonicalize()
        .map_err(|e| format!("writable directory: {e}"))?;
    if !writable.is_dir() {
        return Err("writable directory is not a directory".into());
    }
    let entry = source
        .join(config.entry)
        .canonicalize()
        .map_err(|e| format!("entry file: {e}"))?;
    if !entry.starts_with(&source) || !entry.is_file() {
        return Err("entry file must be within the source directory".into());
    }
    let source = lisp_string(&directory_name(&source)?);
    let writable = lisp_string(&directory_name(&writable)?);
    let entry = lisp_string(entry.to_str().ok_or("entry file is not UTF-8")?);
    // Separate forms: DEFPACKAGE is evaluated before later qualified symbols
    // are read. No process CWD or environment mutation is used.
    Ok(vec![
        "egcl".into(),
        "--no-init".into(),
        "--no-image".into(),
        "--eval".into(),
        "(defpackage :egcl-ios (:use :cl) (:export #:*source-directory* #:*writable-directory*))"
            .into(),
        "--eval".into(),
        format!(
            "(progn (defparameter egcl-ios:*source-directory* (pathname {source})) (defparameter egcl-ios:*writable-directory* (pathname {writable})))"
        ),
        "--eval".into(),
        format!("(let ((*default-pathname-defaults* egcl-ios:*source-directory*)) (load {entry}))"),
    ])
}

/// The channel contains only owned Rust data. Never retain an EgclError or
/// arbitrary panic payload after the Lisp execution/roots have retired.
fn root(args: Vec<String>) -> Result<i32, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        match egcl::run_embedded(&args) {
            Ok(code) => Ok(code),
            Err(mut error) => {
                egcl_rt::rooted_ref!(_error_root = &mut error);
                Err(error.to_string())
            }
        }
    })) {
        Ok(result) => result,
        Err(_) => Err("Lisp worker panicked".into()),
    }
}

fn supervise(config: OwnedConfig) {
    // SAFETY: this control thread never evaluates Lisp, reads its heap, or
    // changes root slots. Configuration, status and outcomes are Rust-owned.
    // Keep it outside the admitted mutator set even while host locks block.
    let _native_control = unsafe { egcl_rt::safepoint::NativeBlockingScope::enter() };
    let args = match arguments(config) {
        Ok(args) => args,
        Err(error) => {
            let mut host = lock();
            egcl_rt::host::request_stop();
            host.error = error;
            host.status.state = FAILED;
            host.status.exit_code = 1;
            return;
        }
    };
    let result: Arc<Mutex<Option<Result<i32, String>>>> = Arc::new(Mutex::new(None));
    let published = Arc::clone(&result);
    let identity = Arc::new(Mutex::new(None));
    let worker_identity = Arc::clone(&identity);
    let worker = match std::thread::Builder::new()
        .name("egcl-ios-lisp".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                *worker_identity.lock().unwrap_or_else(|e| e.into_inner()) = Some(
                    egcl_rt::thread::HostExecutionTarget::Native(egcl_rt::current_thread_id().0),
                );
                egcl_rt::host::notify();
                root(args)
            }))
            .unwrap_or_else(|_| Err("Lisp worker initialization panicked".into()));
            *published.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
            egcl_rt::host::notify();
        }) {
        Ok(worker) => worker,
        Err(error) => {
            let mut host = lock();
            egcl_rt::host::request_stop();
            host.error = format!("cannot start Lisp worker: {error}");
            host.status.state = FAILED;
            host.status.exit_code = 1;
            return;
        }
    };
    {
        let mut host = lock();
        if host.status.state == STARTING {
            host.status.state = RUNNING;
        }
    }
    let mut worker = Some(worker);
    let mut outcome = None;
    loop {
        if outcome.is_none() {
            outcome = result.lock().unwrap_or_else(|e| e.into_inner()).take();
            if matches!(outcome, Some(Err(_))) {
                egcl_rt::host::request_stop();
            }
        }
        if worker
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            // Off the UI thread, and only after completion. Retire the root's
            // TLS/GC attachment before claiming the whole world is inactive.
            if worker.take().expect("finished worker").join().is_err() && outcome.is_none() {
                outcome = Some(Err(
                    "Lisp worker terminated without publishing an outcome".into()
                ));
                egcl_rt::host::request_stop();
            }
        }
        let active = egcl_rt::thread::host_active_executions();
        if egcl_rt::host::stop_requested() {
            lock().status.state = STOPPING;
            if worker.is_some() {
                if let Some(target) = *identity.lock().unwrap_or_else(|e| e.into_inner()) {
                    egcl_rt::thread::host_request_execution_shutdown(target);
                }
            }
            for target in &active {
                // Never re-inject shutdown into an UNWIND-PROTECT cleanup.
                egcl_rt::thread::host_request_execution_shutdown(*target);
            }
        } else if outcome.is_some() && !active.is_empty() {
            lock().status.state = ROOT_COMPLETED;
        }
        if worker.is_none() && outcome.is_some() && active.is_empty() {
            // Linearize completion against new admission, then re-snapshot
            // after every already-admitted producer has published or failed.
            egcl_rt::host::close_admission();
            if egcl_rt::host::creations_in_flight() != 0
                || !egcl_rt::thread::host_active_executions().is_empty()
            {
                egcl_rt::host::wait_for_change(Duration::from_millis(20));
                continue;
            }
            let mut host = lock();
            match outcome.take().expect("root outcome") {
                Ok(code) => {
                    host.status.exit_code = egcl_rt::host::exit_code().unwrap_or(code);
                    host.status.state = FINISHED;
                }
                Err(error) => {
                    host.status.state = FAILED;
                    host.status.exit_code = 1;
                    host.error = error;
                }
            }
            break;
        }
        egcl_rt::host::wait_for_change(Duration::from_millis(20));
    }
}
