// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native waits must publish roots and cannot return into Lisp during a GC pause.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use egcl_rt::sync::{EgclCondVar, EgclMutex, EgclSemaphore};
use egcl_rt::{Collector, GcConfig, HeapCollector, EgclVal};

#[test]
fn native_waits_publish_roots_and_wait_for_gc_resume() {
    // R13.12: blocked native stacks remain scannable without waking the waiter.
    egcl_rt::init_heap(&GcConfig {
        heap_size: 64 * 1024,
        heap_max: 128 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4 * 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    })
    .unwrap();
    let main_thread = egcl_rt::current_thread_id();
    for kind in ["mutex", "condition", "semaphore"] {
        let mutex = Arc::new(EgclMutex::default());
        let condition = Arc::new(EgclCondVar::default());
        let semaphore = Arc::new(EgclSemaphore::new(None, 0).unwrap());
        if kind == "mutex" {
            assert!(mutex.grab(true, None).unwrap());
        }
        let returned = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let worker = {
            let mutex = Arc::clone(&mutex);
            let returned = Arc::clone(&returned);
            std::thread::spawn(move || {
                let body = egcl_rt::alloc_typed(16, egcl_rt::object::type_id::BIGNUM).unwrap();
                unsafe { *(body as *mut u64) = 0x5123_4567 };
                egcl_rt::rooted!(value = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
                let original = value.to_raw();
                if kind == "condition" {
                    assert!(mutex.grab(true, None).unwrap());
                }
                ready_tx.send(()).unwrap();
                let timeout = Some(Duration::from_millis(300));
                let result = match kind {
                    "mutex" => mutex.grab(true, timeout),
                    "condition" => condition.wait(&mutex, timeout),
                    _ => semaphore.wait(timeout),
                };
                returned.store(true, Ordering::Release);
                if kind == "condition" {
                    mutex.release().unwrap();
                }
                let marker = unsafe { *(value.as_ptr().add(8) as *const u64) };
                (result, original != value.to_raw(), marker)
            })
        };
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        while egcl_rt::thread::safepoint_participant_count_excluding(main_thread) != 0
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let collection = HeapCollector::new().minor_gc();
        if collection.is_ok() {
            egcl_rt::safepoint::wait_for_all_threads().unwrap();
            // The wait times out during this pause. It must remain out of Lisp
            // until resume, even when all other threads were already blocked.
            std::thread::sleep(Duration::from_millis(350));
            let escaped = returned.load(Ordering::Acquire);
            egcl_rt::safepoint::resume_all_threads().unwrap();
            assert!(!escaped, "{kind} returned while GC still owned its roots");
        }
        if kind == "mutex" {
            mutex.release().unwrap();
        }
        let (result, moved, marker) = worker.join().unwrap();
        collection.unwrap_or_else(|error| panic!("GC while blocked on {kind}: {error}"));
        assert!(!result.unwrap(), "{kind} must time out");
        assert!(moved, "{kind} root must actually be relocated");
        assert_eq!(marker, 0x5123_4567, "{kind} root lost its payload");
    }

    // Race requests with both entry into and return from short native waits.
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let worker = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let body = egcl_rt::alloc_typed(16, egcl_rt::object::type_id::BIGNUM).unwrap();
            unsafe { *(body as *mut u64) = 0x5123_4567 };
            egcl_rt::rooted!(value = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
            let semaphore = EgclSemaphore::new(None, 0).unwrap();
            ready_tx.send(()).unwrap();
            while !stop.load(Ordering::Acquire) {
                assert!(!semaphore.wait(Some(Duration::from_micros(10))).unwrap());
                assert_eq!(
                    unsafe { *(value.as_ptr().add(8) as *const u64) },
                    0x5123_4567
                );
            }
        })
    };
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut result = Ok(());
    for _ in 0..50 {
        if let Err(error) = HeapCollector::new().minor_gc() {
            result = Err(error);
            break;
        }
        std::thread::yield_now();
    }
    stop.store(true, Ordering::Release);
    worker.join().unwrap();
    result.expect("repeated collections racing native wait transitions");
}
