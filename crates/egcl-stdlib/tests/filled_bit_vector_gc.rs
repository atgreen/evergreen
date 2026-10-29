// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{EgclVal, NIL};
use egcl_stdlib::sequences::{build_filled_bit_vector, elt, set_elt};

#[test]
fn filled_bit_constructor_collects_with_live_moving_roots() {
    const CHILD: &str = "EGCL_FILLED_BIT_GC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "filled_bit_constructor_collects_with_live_moving_roots",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_STRESS_AT", "2")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_DISABLE")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("FILLED-BIT-CONSTRUCTOR-RELOCATED"));
        return;
    }
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 1.0,
    })
    .unwrap();
    // The witness is allocation #1; filled bit storage is #2. The builder
    // stages only Rust bytes, so this collection happens in its final allocator.
    let expected = egcl_rt::bignum::BigInt::from_mag(1, vec![0xdead, 1 << 16]);
    egcl_rt::rooted!(witness = expected.to_val());
    let before = witness.to_raw();
    assert_eq!(egcl_rt::gc::heap_stats().minor_gc_count, 0);
    egcl_rt::rooted!(array = build_filled_bit_vector(67, EgclVal::from_fixnum(1)).unwrap());
    assert_ne!(
        witness.to_raw(),
        before,
        "the collection must actually relocate the live root"
    );
    assert_eq!(
        egcl_rt::bignum::bigint_from_val(*witness).unwrap().mag,
        expected.mag
    );
    for index in 0..67 {
        assert_eq!(elt(*array, index).unwrap(), EgclVal::from_fixnum(1));
    }
    set_elt(*array, 66, EgclVal::from_fixnum(0)).unwrap();
    assert_eq!(elt(*array, 66).unwrap(), EgclVal::from_fixnum(0));
    for fill in [NIL, EgclVal::from_fixnum(-1), EgclVal::from_fixnum(2)] {
        assert!(matches!(
            build_filled_bit_vector(1, fill),
            Err(egcl_rt::EgclError::TypeError { .. })
        ));
    }
    println!("FILLED-BIT-CONSTRUCTOR-RELOCATED");
}
