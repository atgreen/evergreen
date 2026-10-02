// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::{init_heap, GcConfig};

#[test]
fn symbol_allocation_collects_before_reporting_exhaustion() {
    init_heap(&GcConfig {
        heap_size: 64 * 1024,
        heap_max: 64 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    })
    .unwrap();
    egcl_rt::rooted!(held = egcl_rt::symbols::make_uninterned("SURVIVOR"));
    let index = held.as_symbol_index();
    for _ in 0..10_000 {
        egcl_rt::symbols::make_uninterned("DISCARDED");
        assert_eq!(
            egcl_rt::symbols::symbol_name(index).as_deref(),
            Some("SURVIVOR")
        );
    }
    assert!(egcl_rt::heap_stats().major_gc_count > 0);
}
