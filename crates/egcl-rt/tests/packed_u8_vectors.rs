// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::image::{ImageCompression, SaveImageOptions};
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::{Collector, EgclVal, HeapCollector, packed_u8};

fn vector(values: &[EgclVal]) -> EgclVal {
    egcl_rt::rooted!(values = values.to_vec());
    let size = 8 + values.len() * 8;
    let body = egcl_rt::gc::alloc_typed(size, type_id::SIMPLE_VECTOR).unwrap();
    unsafe {
        body.cast::<u64>().write(values.len() as u64);
        for (index, value) in values.iter().enumerate() {
            body.add(8 + index * 8).cast::<EgclVal>().write(*value);
        }
        EgclVal::from_heap_ptr(body.sub(egcl_rt::gc::body_header_offset(size)))
    }
}

fn element(vector: EgclVal, index: usize) -> EgclVal {
    unsafe { vector.as_ptr().add(16 + index * 8).cast::<EgclVal>().read() }
}

#[test]
fn packed_u8_bytes_move_as_leaf_data_and_preserve_shared_image_backing() {
    assert!(packed_u8::make(usize::MAX, 0).is_err());
    assert!(packed_u8::make(u32::MAX as usize, 0).is_err());
    assert!(!packed_u8::is_vector(egcl_rt::value::NIL));
    assert_eq!(packed_u8::get(egcl_rt::value::NIL, 0), None);
    assert!(!packed_u8::set(egcl_rt::value::NIL, 0, 7));
    egcl_rt::rooted!(empty = packed_u8::make(0, 255).unwrap());
    assert_eq!(packed_u8::len(*empty), Some(0));
    assert_eq!(packed_u8::get(*empty, 0), None);
    assert!(!packed_u8::set(*empty, 0, 1));

    egcl_rt::rooted!(other = egcl_rt::gc::alloc_double_float(42.0));
    let pointer_bits = other.to_raw();
    let mut expected: Vec<u8> = (0..=255).collect();
    expected.extend_from_slice(&pointer_bits.to_ne_bytes());
    egcl_rt::rooted!(bytes = packed_u8::make(expected.len(), 0).unwrap());
    assert!(!egcl_rt::packed_f64::is_vector(*bytes));
    for (index, &byte) in expected.iter().enumerate() {
        assert!(packed_u8::set(*bytes, index, byte));
    }
    assert!(!packed_u8::set(*bytes, expected.len(), 9));
    let before = bytes.to_raw();
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(bytes.to_raw(), before, "byte vector must actually relocate");
    assert_ne!(
        other.to_raw(),
        pointer_bits,
        "pointer-shaped bytes must stay raw"
    );
    for (index, &byte) in expected.iter().enumerate() {
        assert_eq!(packed_u8::get(*bytes, index), Some(byte));
    }

    // A payload beyond the small-header limit must retain its U8 tag and its
    // true length rather than reading the extended-size word as a payload.
    egcl_rt::rooted!(large = packed_u8::make(600_000, 255).unwrap());
    assert!(unsafe { (*large.as_ptr().cast::<ObjectHeader>()).is_large_object() });
    assert_eq!(packed_u8::len(*large), Some(600_000));
    assert_eq!(packed_u8::get(*large, 0), Some(255));
    assert!(packed_u8::set(*large, 599_999, 3));
    assert_eq!(packed_u8::get(*large, 599_999), Some(3));
    assert_eq!(packed_u8::get(*large, 600_000), None);

    egcl_rt::rooted!(
        dims = vector(&[
            EgclVal::from_fixnum(1),
            EgclVal::from_fixnum(expected.len() as i64)
        ])
    );
    let md_body = egcl_rt::gc::alloc_typed(24, type_id::MD_ARRAY).unwrap();
    let md = unsafe {
        md_body.cast::<EgclVal>().write(*bytes);
        md_body.add(8).cast::<EgclVal>().write(*dims);
        md_body
            .add(16)
            .cast::<EgclVal>()
            .write(EgclVal::from_fixnum(2));
        EgclVal::from_heap_ptr(md_body.sub(8))
    };
    egcl_rt::rooted!(md = md);
    egcl_rt::rooted!(carrier = vector(&[*bytes, *bytes, *md, *large, *empty]));
    egcl_rt::gc::set_entry_continuation(*carrier);
    let path = std::env::temp_dir().join(format!("egcl-packed-u8-{}.core", std::process::id()));
    egcl_rt::image::save_image(
        path.to_str().unwrap(),
        &SaveImageOptions {
            executable: false,
            compression: ImageCompression::None,
            purify: false,
        },
    )
    .unwrap();
    drop(carrier);
    drop(md);
    drop(dims);
    drop(large);
    drop(bytes);
    drop(other);
    drop(empty);
    let restored = egcl_rt::image::load_image(path.to_str().unwrap()).unwrap();
    let bytes = element(restored, 0);
    assert_eq!(bytes, element(restored, 1));
    assert_eq!(
        egcl_rt::types::md_array_storage(element(restored, 2)),
        Some(bytes)
    );
    for (index, &byte) in expected.iter().enumerate() {
        assert_eq!(packed_u8::get(bytes, index), Some(byte));
    }
    let large = element(restored, 3);
    assert_eq!(packed_u8::len(large), Some(600_000));
    assert_eq!(packed_u8::get(large, 0), Some(255));
    assert_eq!(packed_u8::get(large, 599_999), Some(3));
    assert_eq!(packed_u8::len(element(restored, 4)), Some(0));
    std::fs::remove_file(path).unwrap();
}
