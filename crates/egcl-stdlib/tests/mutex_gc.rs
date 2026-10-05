// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native mutex handles retain stable ownership across moving GC and images.
use egcl_rt::sync::EgclMutex;
use egcl_stdlib::synchronization::{grab_mutex, make_mutex, mutex_p, release_mutex};
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

#[test]
fn mutex_moves_serializes_without_native_pointer_and_finalizes() {
    let weak;
    let (image, region_image, saved);
    {
        egcl_rt::rooted!(mutex = make_mutex(Some("rooted mutex".into()), true).unwrap());
        let original = mutex.to_raw();
        let original_name = unsafe { *(mutex.as_ptr().add(24) as *const egcl_rt::EgclVal) };
        let pointer = unsafe { *(mutex.as_ptr().add(8) as *const *const EgclMutex) };
        // An observation-only Weak proves finalization releases native storage.
        unsafe { Arc::increment_strong_count(pointer) };
        let native = unsafe { Arc::from_raw(pointer) };
        weak = Arc::downgrade(&native);
        drop(native);
        assert!(grab_mutex(*mutex, true, None).unwrap());
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_ne!(original, mutex.to_raw(), "the handle must actually move");
        let moved_name = unsafe { *(mutex.as_ptr().add(24) as *const egcl_rt::EgclVal) };
        // Stress may already have evacuated the name during handle allocation.
        if std::env::var_os("EGCL_GC_STRESS").is_none() {
            assert_ne!(
                original_name, moved_name,
                "the traced name must actually move"
            );
        }
        assert_eq!(moved_name.as_string(), "rooted mutex");
        assert!(mutex_p(*mutex));
        assert!(grab_mutex(*mutex, true, None).unwrap());
        release_mutex(*mutex, false).unwrap();
        release_mutex(*mutex, false).unwrap();
        assert_eq!(weak.upgrade().unwrap().name(), Some("rooted mutex"));

        // Ownership and recursive depth are intentionally discarded by restore.
        assert!(grab_mutex(*mutex, true, None).unwrap());
        assert!(grab_mutex(*mutex, true, None).unwrap());

        saved = mutex.to_raw();
        image = egcl_rt::gc::serialize_heap_objects();
        // This process has one mutator until the restoration workers below.
        region_image = unsafe { egcl_rt::gc::snapshot_heap_regions(false) }
            .unwrap()
            .encode()
            .unwrap();
        let mut offset = 0;
        let mut found = false;
        while offset < image.len() {
            let address = u64::from_le_bytes(image[offset..offset + 8].try_into().unwrap());
            let size =
                u32::from_le_bytes(image[offset + 9..offset + 13].try_into().unwrap()) as usize;
            if address == unsafe { mutex.as_ptr() as u64 + 8 } {
                found = true;
                assert_eq!(
                    &image[offset + 13..offset + 21],
                    &[0; 8],
                    "saved mutex must not contain a process-local native pointer"
                );
            }
            offset += 13 + size;
        }
        assert!(found, "mutex handle must not disappear from the saved heap");
    }
    egcl_rt::full_gc().unwrap();
    egcl_rt::full_gc().unwrap();
    assert!(weak.upgrade().is_none(), "dead mutex leaked its native Arc");
    egcl_rt::gc::restore_heap(&image).unwrap();
    verify_restored_mutex(saved);
    // Workers have all joined, and finalization dropped the first native Arc.
    unsafe { egcl_rt::gc::restore_heap_regions(&region_image, None) }.unwrap();
    verify_restored_mutex(saved);

    // The old two-word layout's alignment padding is not name metadata.
    let body = egcl_rt::alloc_typed(16, egcl_rt::object::type_id::MUTEX).unwrap();
    unsafe {
        *(body as *mut u64) = 0;
        *(body.add(8) as *mut u64) = egcl_rt::object::mutex_flags::RECURSIVE;
        *(body.add(16) as *mut u64) = 0xfafafafafafafafa;
    }
    egcl_rt::rooted!(legacy = unsafe { egcl_rt::EgclVal::from_heap_ptr(body.sub(8)) });
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert!(grab_mutex(*legacy, false, None).unwrap());
    assert!(grab_mutex(*legacy, false, None).unwrap());
    release_mutex(*legacy, false).unwrap();
    release_mutex(*legacy, false).unwrap();
    let key = egcl_rt::gc::finalizer_key(*legacy).unwrap();
    assert_eq!(egcl_rt::gc::run_finalizers_for(key).len(), 1);
}

fn verify_restored_mutex(saved: u64) {
    let restored = egcl_rt::EgclVal::from_raw(egcl_rt::gc::remap_saved_pointer(saved));
    assert!(mutex_p(restored));
    // All callers must publish or observe the same reconstructed native state.
    // Restored image objects are pinned; no heap allocation occurs in workers.
    let barrier = std::sync::Barrier::new(16);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    assert!(!release_mutex(restored, true).unwrap());
                    unsafe {
                        (&*(restored.as_ptr().add(8) as *const AtomicPtr<EgclMutex>))
                            .load(Ordering::Acquire) as usize
                    }
                })
            })
            .collect();
        let pointers: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_ne!(pointers[0], 0);
        assert!(pointers.iter().all(|p| *p == pointers[0]));
    });
    assert!(grab_mutex(restored, false, None).unwrap());
    assert!(grab_mutex(restored, false, None).unwrap());
    release_mutex(restored, false).unwrap();
    release_mutex(restored, false).unwrap();
    assert!(release_mutex(restored, false).is_err());
    let pointer = unsafe {
        (&*(restored.as_ptr().add(8) as *const AtomicPtr<EgclMutex>)).load(Ordering::Acquire)
    };
    unsafe { Arc::increment_strong_count(pointer) };
    let native = unsafe { Arc::from_raw(pointer) };
    assert_eq!(native.name(), Some("rooted mutex"));
    let restored_weak = Arc::downgrade(&native);
    drop(native);
    let key = egcl_rt::gc::finalizer_key(restored).unwrap();
    assert_eq!(egcl_rt::gc::run_finalizers_for(key).len(), 1);
    assert!(
        restored_weak.upgrade().is_none(),
        "restored native Arc leaked"
    );
}
