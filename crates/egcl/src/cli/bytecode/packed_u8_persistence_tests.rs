// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn parse_pool(pool: &BbuConstPool) -> Vec<BbuConstant> {
    pool.entries
        .iter()
        .map(|bytes| {
            let mut cursor = BbuCursor::new(bytes);
            let value = parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap();
            assert!(cursor.done());
            value
        })
        .collect()
}

#[test]
fn packed_u8_bbu_preserves_payload_sharing_and_md_backing() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let expected: Vec<u8> = (0..=255).collect();
    egcl_rt::rooted!(bytes = egcl_rt::packed_u8::make(expected.len(), 0).unwrap());
    for (index, &byte) in expected.iter().enumerate() {
        assert!(egcl_rt::packed_u8::set(*bytes, index, byte));
    }
    egcl_rt::rooted!(md = egcl_stdlib::sequences::build_md_array_with_storage(&[16, 16], *bytes).unwrap());
    egcl_rt::rooted!(carrier = egcl_stdlib::build_simple_vector(&[*bytes, *bytes, *md]));
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    let index = pool.value(*carrier).expect("typed graph must be poolable");
    assert!(pool.entries.iter().any(|entry| entry.first() == Some(&23)));
    assert!(pool.entries.iter().any(|entry| entry.first() == Some(&24)));
    let constants = parse_pool(&pool);
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    let carrier = values[index as usize];
    let first = egcl_stdlib::elt(carrier, 0).unwrap();
    assert_eq!(first, egcl_stdlib::elt(carrier, 1).unwrap());
    let restored_md = egcl_stdlib::elt(carrier, 2).unwrap();
    assert_eq!(egcl_rt::types::md_array_storage(restored_md), Some(first));
    let dims = egcl_rt::types::md_array_dims(restored_md).unwrap();
    assert_eq!(egcl_stdlib::elt(dims, 0).unwrap(), EgclVal::from_fixnum(16));
    assert_eq!(egcl_stdlib::elt(dims, 1).unwrap(), EgclVal::from_fixnum(16));
    for (index, &byte) in expected.iter().enumerate() {
        assert_eq!(egcl_rt::packed_u8::get(first, index), Some(byte));
    }
    assert!(!egcl_rt::packed_f64::is_vector(first));
}

#[test]
fn packed_u8_bbu_shell_relocates_and_forward_md_reference_is_fixed_up() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let constants = vec![BbuConstant::PackedU8(vec![0, 255, 1, 128])];
    egcl_rt::rooted!(values = allocate_bbu_constants(&constants).unwrap());
    let before = values[0];
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(values[0], before, "new typed shell must actually move");
    fill_bbu_constant_edges(&constants, &values).unwrap();
    assert_eq!(egcl_rt::packed_u8::get(values[0], 1), Some(255));
    let constants = vec![
        BbuConstant::MdPackedU8 {
            dimensions: vec![2, 2],
            storage_ref: 1,
        },
        BbuConstant::PackedU8(vec![0, 255, 1, 128]),
        BbuConstant::Vector(vec![0, 1, 1]),
    ];
    egcl_rt::rooted!(restored = materialize_bbu_constants(&constants).unwrap());
    assert_eq!(
        egcl_rt::types::md_array_storage(restored[0]),
        Some(restored[1])
    );
    assert_eq!(egcl_stdlib::elt(restored[2], 1).unwrap(), restored[1]);
    assert_eq!(egcl_stdlib::elt(restored[2], 2).unwrap(), restored[1]);
}

#[test]
fn packed_u8_bbu_rejects_bad_payloads_and_inconsistent_md_storage() {
    for bytes in [vec![23, 4, 0, 0, 0, 1, 2], vec![24, 1, 0, 0, 0]] {
        assert!(parse_bbu_constant(&mut BbuCursor::new(&bytes), BBU_BYTECODE_VERSION).is_err());
    }
    assert!(parse_bbu_constant(&mut BbuCursor::new(&[23, 0, 0, 0, 0]), 0x0203).is_err());
    assert!(
        parse_bbu_constant(&mut BbuCursor::new(&[24, 0, 0, 0, 0, 0, 0, 0, 0]), 0x0203).is_err()
    );
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for constants in [
        vec![BbuConstant::MdPackedU8 {
            dimensions: vec![2],
            storage_ref: 1,
        }],
        vec![
            BbuConstant::MdPackedU8 {
                dimensions: vec![2],
                storage_ref: 1,
            },
            BbuConstant::Vector(vec![]),
        ],
        vec![
            BbuConstant::MdPackedU8 {
                dimensions: vec![2],
                storage_ref: 1,
            },
            BbuConstant::PackedU8(vec![1]),
        ],
        vec![
            BbuConstant::MdPackedU8 {
                dimensions: vec![u64::MAX, 2],
                storage_ref: 1,
            },
            BbuConstant::PackedU8(vec![]),
        ],
    ] {
        assert!(materialize_bbu_constants(&constants).is_err());
    }
}

#[test]
fn packed_u8_image_registry_constants_keep_saved_heap_identity() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(bytes = egcl_rt::packed_u8::make(3, 42).unwrap());
    egcl_rt::rooted!(md = egcl_stdlib::sequences::build_md_array_with_storage(&[1, 3], *bytes).unwrap());
    let mut pool = BbuConstPool {
        saved_heap_refs: true,
        ..Default::default()
    };
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    for index in 0..2 {
        egcl_rt::rooted!(value = if index == 0 { *bytes } else { *md });
        let reference = pool.value(*value).unwrap();
        let mut cursor = BbuCursor::new(&pool.entries[reference as usize]);
        match parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap() {
            BbuConstant::SavedHeapRef(bits) => assert_eq!(bits, value.to_raw()),
            other => panic!("image registry copied a shared object: {other:?}"),
        }
    }
}

#[test]
fn packed_u8_bbu_flattens_complex_active_elements_and_displaced_md_extent() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(base = egcl_rt::packed_u8::make(10, 0).unwrap());
    for index in 0..10 {
        assert!(egcl_rt::packed_u8::set(*base, index, 40 + index as u8));
    }
    egcl_rt::rooted!(
        complex = egcl_stdlib::sequences::build_u8_complex_vector(6, 2, true, 17, true).unwrap()
    );
    egcl_stdlib::sequences::set_aref(*complex, 5, EgclVal::from_fixnum(99)).unwrap();
    egcl_rt::rooted!(
        view =
            egcl_stdlib::sequences::build_u8_displaced_vector(*base, 2, 6, 2, false, true).unwrap()
    );
    egcl_rt::rooted!(
        md = egcl_stdlib::sequences::build_md_array_with_storage(&[2, 3], *view).unwrap()
    );
    egcl_rt::rooted!(carrier = egcl_stdlib::build_simple_vector(&[*complex, *view, *md, *view]));
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    let reference = pool
        .value(*carrier)
        .expect("complex U8 constants must be poolable");
    egcl_rt::rooted!(values = materialize_bbu_constants(&parse_pool(&pool)).unwrap());
    let carrier = values[reference as usize];
    let complex = egcl_stdlib::elt(carrier, 0).unwrap();
    assert_eq!(egcl_rt::packed_u8::len(complex), Some(2));
    assert_eq!(egcl_rt::packed_u8::get(complex, 0), Some(17));
    assert_eq!(egcl_rt::packed_u8::get(complex, 1), Some(17));
    let view = egcl_stdlib::elt(carrier, 1).unwrap();
    assert_eq!(view, egcl_stdlib::elt(carrier, 3).unwrap());
    assert_eq!(egcl_rt::packed_u8::len(view), Some(2));
    assert_eq!(egcl_rt::packed_u8::get(view, 0), Some(42));
    assert_eq!(egcl_rt::packed_u8::get(view, 1), Some(43));
    let md = egcl_stdlib::elt(carrier, 2).unwrap();
    let storage = egcl_rt::types::md_array_storage(md).unwrap();
    assert_eq!(egcl_rt::packed_u8::len(storage), Some(6));
    for index in 0..6 {
        assert_eq!(
            egcl_rt::packed_u8::get(storage, index),
            Some(42 + index as u8)
        );
    }
    let dims = egcl_rt::types::md_array_dims(md).unwrap();
    assert_eq!(egcl_stdlib::elt(dims, 0).unwrap(), EgclVal::from_fixnum(2));
    assert_eq!(egcl_stdlib::elt(dims, 1).unwrap(), EgclVal::from_fixnum(3));
}

#[test]
fn packed_u8_image_registry_keeps_complex_and_displaced_md_identity() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(
        base = egcl_stdlib::sequences::build_u8_complex_vector(8, 3, true, 255, true).unwrap()
    );
    egcl_rt::rooted!(
        view =
            egcl_stdlib::sequences::build_u8_displaced_vector(*base, 1, 6, 2, false, true).unwrap()
    );
    egcl_rt::rooted!(
        md = egcl_stdlib::sequences::build_md_array_with_storage(&[2, 3], *view).unwrap()
    );
    let mut pool = BbuConstPool {
        saved_heap_refs: true,
        ..Default::default()
    };
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    for index in 0..3 {
        egcl_rt::rooted!(
            value = match index {
                0 => *base,
                1 => *view,
                _ => *md,
            }
        );
        let reference = pool.value(*value).unwrap();
        let mut cursor = BbuCursor::new(&pool.entries[reference as usize]);
        match parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap() {
            BbuConstant::SavedHeapRef(bits) => assert_eq!(bits, value.to_raw()),
            other => panic!("image registry flattened shared displaced object: {other:?}"),
        }
    }
    assert_eq!(egcl_rt::types::md_array_storage(*md), Some(*view));
    assert_eq!(
        egcl_stdlib::sequences::cvec_displacement(*view),
        Some((*base, 1, 6))
    );
}
