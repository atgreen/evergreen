// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tiny live cohorts must not permanently consume a region per collection.
use egcl_rt::value::EgclVal;
use egcl_rt::{Collector, GcConfig, HeapCollector, init_heap};

#[test]
fn repeated_major_collections_pack_sparse_live_cohorts() {
    init_heap(&GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 512 * 1024,
        tlab_size: 1024,
        region_size: 64 * 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.0,
    })
    .unwrap();
    egcl_rt::rooted!(values = Vec::<EgclVal>::new());
    for index in 0..128 {
        let payload = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
        unsafe { *(payload as *mut f64) = index as f64 };
        values.push(unsafe { EgclVal::from_heap_ptr(payload.sub(8)) });
        egcl_rt::gc::full_gc().unwrap();
        for (expected, value) in values.iter().enumerate() {
            assert_eq!(
                unsafe { *(value.as_ptr().add(8) as *const f64) },
                expected as f64
            );
        }
        assert!(
            egcl_rt::heap_stats().regions_free >= 8,
            "tiny live cohort exhausted region reserve after {} collections: {:?}",
            index + 1,
            egcl_rt::heap_stats()
        );
    }
    // These values have already left the nursery. Prove that the major
    // collection really relocates them, rather than trusting a clean stress
    // run in which no relevant object moved.
    let old_addresses: Vec<_> = values.iter().map(|value| value.to_raw()).collect();
    HeapCollector::new().major_gc().unwrap();
    assert!(
        values
            .iter()
            .zip(&old_addresses)
            .any(|(value, &old)| value.to_raw() != old)
    );
    for (expected, value) in values.iter().enumerate() {
        assert_eq!(
            unsafe { *(value.as_ptr().add(8) as *const f64) },
            expected as f64
        );
    }
    eprintln!(
        "128 live values packed; {} regions free",
        egcl_rt::heap_stats().regions_free
    );
}
