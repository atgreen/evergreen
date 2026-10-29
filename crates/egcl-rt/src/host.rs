// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Process-lifetime ownership for an embedded Lisp world. No Lisp values live
//! here: host control contains only atomics and Rust synchronization objects.
use crate::EgclError;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

const UNSELECTED: u8 = 0;
const PROCESS: u8 = 1;
const EMBEDDED: u8 = 2;
static OWNER: AtomicU8 = AtomicU8::new(UNSELECTED);
static ROOT_STARTED: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static DRAINING: AtomicBool = AtomicBool::new(false);
static EXIT: AtomicU64 = AtomicU64::new(0);
static CREATING: AtomicUsize = AtomicUsize::new(0);
static EVENTS: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

/// Select before any runtime signal installation; this choice is permanent.
pub fn select_embedded() -> Result<(), EgclError> {
    match OWNER.compare_exchange(UNSELECTED, EMBEDDED, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) | Err(EMBEDDED) => Ok(()),
        Err(_) => Err(EgclError::ProgramError(
            "runtime already owns process signal handling".into(),
        )),
    }
}

pub fn is_embedded() -> bool {
    OWNER.load(Ordering::Acquire) == EMBEDDED
}

/// Ordinary internal installation inherits the already-selected host profile.
pub(crate) fn claim_process_signals() -> bool {
    let _ = OWNER.compare_exchange(UNSELECTED, PROCESS, Ordering::AcqRel, Ordering::Acquire);
    !is_embedded()
}

/// A world is intentionally single-start: interpreter registries and TLS are
/// not isolated generations, and completion does not promise full teardown.
pub fn begin_embedded_root() -> Result<(), EgclError> {
    select_embedded()?;
    if ROOT_STARTED.swap(true, Ordering::AcqRel) {
        return Err(EgclError::ProgramError(
            "an embedded Lisp root has already started in this process".into(),
        ));
    }
    Ok(())
}

pub fn stop_requested() -> bool {
    STOP.load(Ordering::Acquire)
}
pub fn is_draining() -> bool {
    DRAINING.load(Ordering::Acquire)
}

/// Atomic-only variant safe for a defensive signal-handler path.
pub(crate) fn request_stop_signal_safe() {
    DRAINING.store(true, Ordering::Release);
    STOP.store(true, Ordering::Release);
}

pub fn request_stop() {
    request_stop_signal_safe();
    notify();
}

/// First explicit Lisp EXIT wins; a host stop alone has no numeric exit code.
pub fn request_exit(code: i32) {
    crate::thread::host_begin_current_shutdown_unwind();
    let encoded = (1u64 << 32) | u64::from(code as u32);
    let _ = EXIT.compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire);
    request_stop();
}

pub fn exit_code() -> Option<i32> {
    let encoded = EXIT.load(Ordering::Acquire);
    (encoded != 0).then_some(encoded as u32 as i32)
}

pub fn notify() {
    let mut generation = EVENTS.0.lock().unwrap_or_else(|e| e.into_inner());
    *generation = generation.wrapping_add(1);
    EVENTS.1.notify_all();
}

/// Only Rust control state is accessed during the native blocking interval.
pub fn wait_for_change(duration: Duration) {
    // SAFETY: no Lisp values or root slots are read while waiting.
    let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
    let generation = EVENTS.0.lock().unwrap_or_else(|e| e.into_inner());
    drop(
        EVENTS
            .1
            .wait_timeout(generation, duration)
            .unwrap_or_else(|e| e.into_inner()),
    );
}

pub struct CreationAdmission(bool);
impl Drop for CreationAdmission {
    fn drop(&mut self) {
        if self.0 {
            CREATING.fetch_sub(1, Ordering::Release);
        }
    }
}

/// Cover creation/publication and initial submission, including the interval
/// before an execution appears in a registry. No lock spans allocation or GC.
pub(crate) fn admit_creation() -> Result<CreationAdmission, EgclError> {
    if !is_embedded() {
        return Ok(CreationAdmission(false));
    }
    if is_draining() {
        return Err(EgclError::Shutdown);
    }
    CREATING.fetch_add(1, Ordering::AcqRel);
    let guard = CreationAdmission(true);
    if is_draining() {
        return Err(EgclError::Shutdown);
    }
    Ok(guard)
}

pub fn creations_in_flight() -> usize {
    CREATING.load(Ordering::Acquire)
}

/// Close the one-shot world to new work before final completion publication.
pub fn close_admission() {
    DRAINING.store(true, Ordering::Release);
}
