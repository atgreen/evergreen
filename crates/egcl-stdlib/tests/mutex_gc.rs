// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native mutex handles retain stable ownership across moving GC and images.
use std::sync::Arc;
use egcl_rt::sync::EgclMutex;
use egcl_stdlib::synchronization::{grab_mutex, make_mutex, mutex_p, release_mutex};

#[test]
fn mutex_moves_serializes_without_native_pointer_and_finalizes() {
    let weak;
    let (image, saved);
    {
        egcl_rt::rooted!(mutex = make_mutex(Some("rooted mutex".into()), true).unwrap());
        let original = mutex.to_raw();
        let pointer = unsafe { *(mutex.as_ptr().add(8) as *const *const EgclMutex) };
        // An observation-only Weak proves finalization releases native storage.
        unsafe { Arc::increment_strong_count(pointer) };
        let native = unsafe { Arc::from_raw(pointer) };
        weak = Arc::downgrade(&native);
        drop(native);
        assert!(grab_mutex(*mutex, true, None).unwrap());
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_ne!(original, mutex.to_raw(), "the handle must actually move");
        assert!(mutex_p(*mutex));
        assert!(grab_mutex(*mutex, true, None).unwrap());
        release_mutex(*mutex, false).unwrap();
        release_mutex(*mutex, false).unwrap();
        assert_eq!(weak.upgrade().unwrap().name(), Some("rooted mutex"));

        saved = mutex.to_raw();
        image = egcl_rt::gc::serialize_heap_objects();
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
    let restored = egcl_rt::EgclVal::from_raw(egcl_rt::gc::remap_saved_pointer(saved));
    assert!(mutex_p(restored));
    assert!(
        matches!(
            grab_mutex(restored, true, None),
            Err(egcl_rt::EgclError::ProgramError(_))
        ),
        "restored mutex must fail explicitly, not dereference native state"
    );
}
