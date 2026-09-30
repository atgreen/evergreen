// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Condition handles move, retain native queues during waits, and finalize.
use std::sync::Arc;
use std::time::Duration;
use egcl_rt::sync::EgclCondVar;
use egcl_stdlib::synchronization::{
    condition_notify, condition_variable_p, condition_wait, grab_mutex, make_condition_variable,
    make_mutex, release_mutex,
};

#[test]
fn condition_handle_moves_finalizes_and_serializes_without_native_pointer() {
    // Exercise the actual stdlib bridge while BOTH of its arguments relocate.
    {
        egcl_rt::rooted!(cv = make_condition_variable(None).unwrap());
        let original_cv = cv.to_raw();
        egcl_rt::rooted!(mutex = make_mutex(None, false).unwrap());
        let original_mutex = mutex.to_raw();
        let cv_argument = *cv;
        let mutex_argument = *mutex;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            egcl_rt::rooted!(cv = cv_argument);
            egcl_rt::rooted!(mutex = mutex_argument);
            grab_mutex(*mutex, true, None).unwrap();
            ready_tx.send(()).unwrap();
            let result = condition_wait(*cv, *mutex, Some(Duration::from_secs(5))).unwrap();
            release_mutex(*mutex, false).unwrap();
            result
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        grab_mutex(*mutex, true, None).unwrap();
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_ne!(original_cv, cv.to_raw());
        assert_ne!(original_mutex, mutex.to_raw());
        assert_eq!(condition_notify(*cv, 1).unwrap(), 1);
        release_mutex(*mutex, false).unwrap();
        // No collections or Lisp allocations occur after the worker wakes.
        let woke = worker.join().unwrap();
        assert!(woke);
    }
    let weak;
    let (image, saved);
    {
        egcl_rt::rooted!(cv = make_condition_variable(Some("gc condition".into())).unwrap());
        let original = cv.to_raw();
        let pointer = unsafe { *(cv.as_ptr().add(8) as *const *const EgclCondVar) };
        unsafe { Arc::increment_strong_count(pointer) };
        let native = unsafe { Arc::from_raw(pointer) };
        weak = Arc::downgrade(&native);
        drop(native);
        egcl_rt::rooted!(mutex = make_mutex(None, false).unwrap());
        grab_mutex(*mutex, true, None).unwrap();
        assert!(!condition_wait(*cv, *mutex, Some(Duration::ZERO)).unwrap());
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_ne!(original, cv.to_raw());
        assert!(condition_variable_p(*cv));
        assert_eq!(condition_notify(*cv, usize::MAX).unwrap(), 0);
        release_mutex(*mutex, false).unwrap();
        assert_eq!(weak.upgrade().unwrap().name(), Some("gc condition"));
        saved = cv.to_raw();
        image = egcl_rt::gc::serialize_heap_objects();
        let mut offset = 0;
        let mut found = false;
        while offset < image.len() {
            let address = u64::from_le_bytes(image[offset..offset + 8].try_into().unwrap());
            let size =
                u32::from_le_bytes(image[offset + 9..offset + 13].try_into().unwrap()) as usize;
            if address == unsafe { cv.as_ptr() as u64 + 8 } {
                found = true;
                assert_eq!(&image[offset + 13..offset + 21], &[0; 8]);
            }
            offset += 13 + size;
        }
        assert!(found);
    }
    egcl_rt::full_gc().unwrap();
    egcl_rt::full_gc().unwrap();
    assert!(
        weak.upgrade().is_none(),
        "dead condition leaked native queue"
    );
    egcl_rt::gc::restore_heap(&image).unwrap();
    let restored = egcl_rt::EgclVal::from_raw(egcl_rt::gc::remap_saved_pointer(saved));
    assert!(condition_variable_p(restored));
    assert!(matches!(
        condition_notify(restored, 1),
        Err(egcl_rt::EgclError::ProgramError(_))
    ));
}
