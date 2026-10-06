// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::gc::{GcConfig, collect_t0_minor, init_heap};
use egcl_rt::types::{bit_vector_len, bit_vector_ref, make_bit_vector};

#[test]
fn bit_vectors_clear_recycled_payload_before_packing() {
    // One heap-owning test in its own process; set poison before any allocation.
    unsafe { std::env::set_var("EGCL_GC_POISON", "1") };
    init_heap(&GcConfig {
        heap_size: 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 256,
        region_size: 1024,
        promotion_threshold: 15,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.9,
    })
    .unwrap();

    for len in [0, 1, 7, 8, 9, 63, 64, 65, 511] {
        for pattern in [0, 1, 2] {
            let previous_address = make_bit_vector(&vec![1; len]).to_raw();
            // The old vector is deliberately unreachable, so its nursery
            // storage is reclaimed and poisoned before the next allocation.
            collect_t0_minor().unwrap();
            let bits: Vec<u8> = (0..len)
                .map(|i| match pattern {
                    0 => 0,
                    1 => 1,
                    _ => u8::from(i % 3 == 0),
                })
                .collect();
            let vector = make_bit_vector(&bits);
            assert_eq!(
                vector.to_raw(),
                previous_address,
                "precondition: this allocation must reuse the poisoned object storage"
            );
            assert_eq!(bit_vector_len(vector), Some(len));
            for (i, bit) in bits.into_iter().enumerate() {
                assert_eq!(bit_vector_ref(vector, i), Some(bit), "len={len} bit={i}");
            }
            collect_t0_minor().unwrap();
        }
    }
}
