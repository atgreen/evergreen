// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_rt::call_table::CallCell;
use egcl_rt::exec_context::LispExecutionContext;
use egcl_rt::thread::{self, FiberId};
use egcl_rt::value::EgclVal;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Probe {
    selector: usize,
    original: AtomicU64,
    resumed: AtomicU64,
    blocked: AtomicBool,
    release: AtomicBool,
}
static ACTIVE: Mutex<Option<Arc<Probe>>> = Mutex::new(None);

fn fiber() -> EgclVal {
    let probe = ACTIVE.lock().unwrap().as_ref().unwrap().clone();
    let context = thread::current_execution_context_for_native().unwrap();
    let cell = Arc::new(CallCell::new(7, 1, 1));
    // SAFETY: this fiber owns the execution context throughout suspension.
    unsafe { &*context }
        .call_linkage()
        .install(Arc::clone(&cell));
    // SAFETY: the test owns executable storage until this fiber is joined.
    let select: extern "C" fn(&LispExecutionContext) -> usize =
        unsafe { std::mem::transmute(probe.selector) };
    let expected = Arc::as_ptr(&cell) as usize;
    let initial = select(unsafe { &*context });
    probe
        .original
        .store(thread::current_thread_id().0, Ordering::Release);
    if thread::park_current_fiber().is_err() {
        return EgclVal::from_fixnum(1);
    }
    probe
        .resumed
        .store(thread::current_thread_id().0, Ordering::Release);
    let current = thread::current_execution_context_for_native().unwrap();
    let correct =
        current == context && initial == expected && select(unsafe { &*current }) == expected;
    probe.release.store(true, Ordering::Release);
    EgclVal::from_fixnum(i64::from(!correct))
}

struct Cleanup<'a> {
    group: &'a egcl_rt::SchedulerGroup,
    probe: Arc<Probe>,
    fiber: Option<FiberId>,
    finished: bool,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        // Release the carrier before any join, including assertion unwinding.
        self.probe.release.store(true, Ordering::Release);
        self.group.set_idle_hook(None);
        if !self.finished {
            if let Some(fiber) = self.fiber {
                // Cancellation closes the race where cleanup precedes park;
                // it wakes a current wait or makes the next wait return.
                let _ = thread::request_fiber_cancellation(fiber);
            }
            let _ = self.group.finish();
        }
        *ACTIVE.lock().unwrap() = None;
    }
}

#[test]
fn indexed_selector_keeps_its_execution_cell_after_forced_actual_carrier_migration() {
    egcl_rt::gc::ensure_heap_initialized();
    thread::current_thread_id();
    let mut a = Asm::new();
    let missing = a.label();
    a.extend_from_slice(&[0x41, 0x55, 0x49, 0x89, 0xfd]); // save r13; r13=context
    emit_execution_cell(&mut a, 7, missing);
    mov_rr(&mut a, RAX, 10);
    a.extend_from_slice(&[0x41, 0x5d, 0xc3]);
    a.bind(missing);
    mov_imm64(&mut a, RAX, 0);
    a.extend_from_slice(&[0x41, 0x5d, 0xc3]);
    let code = egcl_rt::jit::JitBuffer::new(&a.finish().unwrap()).unwrap();
    let probe = Arc::new(Probe {
        selector: code.as_ptr() as usize,
        original: AtomicU64::new(0),
        resumed: AtomicU64::new(0),
        blocked: AtomicBool::new(false),
        release: AtomicBool::new(false),
    });
    *ACTIVE.lock().unwrap() = Some(Arc::clone(&probe));
    let group =
        egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers: 2 }).unwrap();
    let mut cleanup = Cleanup {
        group: &group,
        probe: Arc::clone(&probe),
        fiber: None,
        finished: false,
    };
    let hook = Arc::clone(&probe);
    group.set_idle_hook(Some(Arc::new(move || {
        if hook.original.load(Ordering::Acquire) != thread::current_thread_id().0 {
            return;
        }
        // worker_loop clones and drops idle_hook's lock before invoking it,
        // outside pop_or_steal and before taking park_mutex (thread.rs2314).
        // This hook touches synchronization state only, never Lisp heap roots.
        let _blocked = unsafe { egcl_rt::safepoint::NativeBlockingScope::enter() };
        hook.blocked.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !hook.release.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    })));
    let entry = unsafe { EgclVal::from_function_ptr(fiber as *const () as *mut u8) };
    let id = thread::make_fiber(entry).unwrap();
    group.submit(id).unwrap();
    cleanup.fiber = Some(id);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !probe.blocked.load(Ordering::Acquire) && Instant::now() < deadline {
        egcl_rt::poll_safepoint();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        probe.blocked.load(Ordering::Acquire),
        "original carrier must reach its idle hook after unmounting the parked fiber"
    );
    group.unpark(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while probe.resumed.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
        egcl_rt::poll_safepoint();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_ne!(
        probe.resumed.load(Ordering::Acquire),
        0,
        "second carrier must resume the fiber while the first remains blocked"
    );
    assert_ne!(
        probe.original.load(Ordering::Acquire),
        probe.resumed.load(Ordering::Acquire),
        "the real fiber must change carrier, not merely its context metadata"
    );
    probe.release.store(true, Ordering::Release);
    group.set_idle_hook(None);
    let result = group.finish();
    cleanup.finished = true;
    assert_eq!(result.unwrap(), vec![EgclVal::from_fixnum(0)]);
}
