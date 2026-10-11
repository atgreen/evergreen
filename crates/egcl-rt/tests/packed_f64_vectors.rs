// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::image::{ImageCompression, SaveImageOptions};
use egcl_rt::{Collector, HeapCollector, packed_f64};

#[test]
fn packed_values_move_without_tracing_bits_and_round_trip_in_images() {
    assert!(packed_f64::make(usize::MAX, 0.0).is_err());
    // The allocator records exact body size in a u32 header field.
    assert!(packed_f64::make(u32::MAX as usize / 8, 0.0).is_err());
    egcl_rt::rooted!(empty = packed_f64::make(0, 0.0).unwrap());
    assert_eq!(packed_f64::len(*empty), Some(0));
    assert_eq!(packed_f64::get(*empty, 0), None);
    assert!(!packed_f64::set(*empty, 0, 1.0));

    egcl_rt::rooted!(other = egcl_rt::gc::alloc_double_float(42.0));
    let pointer_bits = other.to_raw();
    let bits = [
        0,
        (-0.0f64).to_bits(),
        f64::INFINITY.to_bits(),
        0x7ff8_1234_5678_abcd,
        pointer_bits,
    ];
    egcl_rt::rooted!(array = packed_f64::make(bits.len(), 0.0).unwrap());
    for (i, &value) in bits.iter().enumerate() {
        assert!(packed_f64::set(*array, i, f64::from_bits(value)));
    }
    let before = array.to_raw();
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(array.to_raw(), before, "packed vector must really move");
    assert_ne!(
        other.to_raw(),
        pointer_bits,
        "pointer-shaped payload must remain raw"
    );
    for (i, &value) in bits.iter().enumerate() {
        assert_eq!(packed_f64::get(*array, i).unwrap().to_bits(), value);
    }

    // Exercise the large-object header and both ends of its packed payload.
    egcl_rt::rooted!(large = packed_f64::make(200_000, 1.25).unwrap());
    assert_eq!(packed_f64::len(*large), Some(200_000));
    assert!(packed_f64::set(*large, 199_999, -3.5));
    assert_eq!(packed_f64::get(*large, 0), Some(1.25));
    assert_eq!(packed_f64::get(*large, 199_999), Some(-3.5));
    assert!(!packed_f64::set(*large, 200_000, 2.0));

    // The image entry is the actual vector, not a host-side copy of its data.
    egcl_rt::gc::set_entry_continuation(*array);
    let path = std::env::temp_dir().join(format!("egcl-packed-f64-{}.core", std::process::id()));
    egcl_rt::image::save_image(
        path.to_str().unwrap(),
        &SaveImageOptions {
            executable: false,
            compression: ImageCompression::None,
            purify: false,
        },
    )
    .unwrap();
    // Drop roots before the image replaces the heap and invalidates old addresses.
    drop(large);
    drop(array);
    drop(other);
    drop(empty);
    let restored = egcl_rt::image::load_image(path.to_str().unwrap()).unwrap();
    assert_eq!(packed_f64::len(restored), Some(bits.len()));
    for (i, &value) in bits.iter().enumerate() {
        assert_eq!(packed_f64::get(restored, i).unwrap().to_bits(), value);
    }
    std::fs::remove_file(path).unwrap();
}
