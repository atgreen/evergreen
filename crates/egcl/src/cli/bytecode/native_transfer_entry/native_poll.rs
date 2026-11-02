// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Generated poll state belongs to the active native activation, never carrier
//! TLS. A migrated fiber forces the existing runtime poll before reusing its
//! carrier-dependent signal and deadline snapshots.

use super::*;
use egcl_compiler::t2::emit::{NativePollControl, NativePollFastPath};

pub(super) fn binding() -> Option<NativePollFastPath> {
    let (gc_requested, sigusr1_pending) = egcl_rt::safepoint::native_poll_flag_addresses()?;
    Some(NativePollFastPath {
        initialize: initialize as *const () as u64,
        gc_requested: gc_requested as u64,
        sigusr1_pending: sigusr1_pending as u64,
        posted_signal: egcl_rt::thread::posted_pending_signals_address() as u64,
        native_errors: std::ptr::addr_of!(PENDING_NATIVE_ERRORS) as u64,
        signal_epoch: egcl_rt::runtime::process_signal_activity_address() as u64,
        observe_forces_cold: cfg!(test),
        instrument_skips: cfg!(test),
        baseline_heat: None,
    })
}

/// The managed anchor already roots entry arguments. This initializer does not
/// allocate Lisp objects, collect, park or yield; a missing execution falls back
/// to the existing runtime poll rather than guessing a thread identity.
unsafe extern "C" fn initialize() -> *mut NativePollControl {
    let context = CAPTURE.with(Cell::get);
    let Some(context) = (unsafe { context.as_mut() }) else {
        return std::ptr::null_mut();
    };
    let Some(execution) = egcl_rt::thread::current_execution_context_for_native() else {
        return std::ptr::null_mut();
    };
    let Some(seen_signal_epoch) = egcl_rt::runtime::current_seen_signal_activity() else {
        return std::ptr::null_mut();
    };
    context.native_poll = Some(NativePollControl {
        execution,
        seen_carrier: unsafe { &*execution }.carrier_id(),
        seen_signal_epoch,
        deadline_armed: u8::from(egcl_rt::sandbox_cpu_deadline_armed()),
        reserved: [0; 7],
        test_observer: std::ptr::null(),
        test_skips: std::ptr::null_mut(),
    });
    let control = context.native_poll.as_mut().unwrap();
    #[cfg(test)]
    {
        control.test_observer = root_publication::OBSERVE.with(Cell::as_ptr).cast();
        control.test_skips = GENERATED_SKIPS.with(Cell::as_ptr);
    }
    control
}

/// Loop-header visits baseline (T1) code counts in generated code per runtime
/// heat sample (bliss-tudsy). Each sample records the whole batch, so batching
/// delays a T2 request or OSR entry by at most this many visits; a low
/// back-edge threshold keeps sampling every visit.
pub(in crate::cli::bytecode) fn heat_batch() -> u32 {
    // Unit tests count exact heat; a test may raise this around its own
    // compilation and run, under the heap test lock.
    #[cfg(test)]
    {
        TEST_HEAT_BATCH.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(test))]
    {
        static BATCH: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        *BATCH.get_or_init(|| {
            std::env::var("EGCL_T1_HEAT_BATCH")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|batch| *batch > 0)
                .unwrap_or_else(|| (t2_backedge_threshold() / 64).clamp(1, 128))
        })
    }
}

#[cfg(test)]
pub(in crate::cli::bytecode) static TEST_HEAT_BATCH: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

/// Fresh function boundary prevents carrier TLS reads moving across a yielding
/// poll. Copy only the consumed epoch; copying the producer would acknowledge
/// an arrival that raced the drain without actually processing it.
#[inline(never)]
pub(super) fn refresh() {
    let context = CAPTURE.with(Cell::get);
    let Some(context) = (unsafe { context.as_mut() }) else {
        return;
    };
    let Some(control) = context.native_poll.as_mut() else {
        return;
    };
    control.seen_carrier = unsafe { &*control.execution }.carrier_id();
    // An inaccessible snapshot keeps the old epoch and forces future runtime
    // polls through the armed byte. Ordinary mounted execution always has TLS.
    match egcl_rt::runtime::current_seen_signal_activity() {
        Some(epoch) => {
            control.seen_signal_epoch = epoch;
            control.deadline_armed = u8::from(egcl_rt::sandbox_cpu_deadline_armed());
        }
        None => control.deadline_armed = 1,
    }
    #[cfg(test)]
    if let Some(hook) = AFTER_REFRESH.with(Cell::get) {
        hook();
    }
}

#[cfg(test)]
type PollObserver = egcl_rt::execution_local::ExecutionLocal<Cell<Option<fn()>>>;

/// Unlike root_publication::OBSERVE, this observer never forces an idle guard
/// into the runtime. It runs only after a real slow poll has published roots.
#[cfg(test)]
pub(in super::super) static OBSERVE_REQUEST: PollObserver =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };

/// Completes request handshakes only after runtime consumption and control
/// refresh, so a requester cannot post its next epoch during the previous drain.
#[cfg(test)]
pub(in super::super) static AFTER_REFRESH: PollObserver =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };

#[cfg(test)]
pub(super) fn observe_request() {
    if let Some(hook) = OBSERVE_REQUEST.with(Cell::get) {
        hook();
    }
}

#[cfg(test)]
static GENERATED_SKIPS: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };

#[cfg(test)]
pub(in super::super) fn take_generated_skips() -> usize {
    GENERATED_SKIPS.with(|skips| skips.replace(0))
}

#[cfg(test)]
pub(in super::super) fn initialized_for_test() -> bool {
    unsafe { CAPTURE.with(Cell::get).as_ref() }.is_some_and(|context| context.native_poll.is_some())
}
