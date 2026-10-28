// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn parse_pool(pool: &BbuConstPool) -> Vec<BbuConstant> {
    pool.entries
        .iter()
        .map(|bytes| {
            let mut cursor = BbuCursor::new(bytes);
            let constant = parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap();
            assert!(cursor.done());
            constant
        })
        .collect()
}

#[test]
fn adjustable_md_storage_graph_preserves_displacement_and_updates_through_aliases() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let contents: Vec<_> = (1..=6).map(EgclVal::from_fixnum).collect();
    egcl_rt::rooted!(
        base = egcl_stdlib::build_complex_vector(&contents, 6, 4, true, false, false, true)
    );
    egcl_rt::rooted!(
        view = egcl_stdlib::build_displaced_vector(*base, 1, 4, 4, true, false, false, false)
    );
    egcl_rt::rooted!(
        md = egcl_stdlib::sequences::build_md_array_with_storage(&[2, 2], *view).unwrap()
    );
    egcl_rt::rooted!(carrier = egcl_stdlib::build_simple_vector(&[*base, *view, *md, *base]));
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    let reference = pool
        .value(*carrier)
        .expect("adjustable graph must be portable");
    assert!(pool.entries.iter().any(|bytes| bytes.first() == Some(&25)));
    assert!(pool.entries.iter().any(|bytes| bytes.first() == Some(&26)));
    let md_reference = pool
        .value(*md)
        .expect("a direct MD root must use the same graph entry");
    let constants = parse_pool(&pool);
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    egcl_rt::rooted!(restored = values[reference as usize]);
    assert_eq!(
        egcl_stdlib::elt(*restored, 2).unwrap(),
        values[md_reference as usize]
    );
    egcl_rt::rooted!(base = egcl_stdlib::elt(*restored, 0).unwrap());
    egcl_rt::rooted!(view = egcl_stdlib::elt(*restored, 1).unwrap());
    egcl_rt::rooted!(md = egcl_stdlib::elt(*restored, 2).unwrap());
    assert_eq!(*base, egcl_stdlib::elt(*restored, 3).unwrap());
    assert!(egcl_stdlib::sequences::array_adjustable(*md));
    assert_eq!(egcl_rt::types::md_array_storage(*md), Some(*view));
    assert_eq!(egcl_stdlib::cvec_displacement(*view), Some((*base, 1, 4)));
    assert_eq!(egcl_stdlib::cvec_fill_pointer(*base), 4);
    assert_eq!(egcl_stdlib::array_total_size(*base), Some(6));
    let replacement: Vec<_> = (10..16).map(EgclVal::from_fixnum).collect();
    egcl_rt::rooted!(
        replacement =
            egcl_stdlib::build_complex_vector(&replacement, 6, 4, true, false, false, true)
    );
    assert_eq!(
        egcl_stdlib::sequences::adopt_adjusted_array(*base, *replacement).unwrap(),
        *base
    );
    assert_eq!(egcl_stdlib::aref(*view, 0).unwrap().as_fixnum(), 11);
    assert_eq!(
        egcl_stdlib::aref(egcl_rt::types::md_array_storage(*md).unwrap(), 3)
            .unwrap()
            .as_fixnum(),
        14
    );
}

#[test]
fn array_pool_identity_survives_movement_and_does_not_merge_equal_mutable_backings() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _ = egcl_rt::packed_u8::make(0, 0).unwrap(); // Initialize the isolated test process's heap.
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        first = egcl_stdlib::build_complex_vector(
            &[EgclVal::from_fixnum(1)],
            1,
            1,
            true,
            false,
            false,
            false
        )
    );
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    let first_ref = pool.value(*first).unwrap();
    let before = first.to_raw();
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(
        first.to_raw(),
        before,
        "source wrapper must actually relocate between serialization calls"
    );
    assert_eq!(
        pool.value(*first),
        Some(first_ref),
        "rooted identity cache must follow movement"
    );
    egcl_rt::rooted!(
        second = egcl_stdlib::build_complex_vector(
            &[EgclVal::from_fixnum(1)],
            1,
            1,
            true,
            false,
            false,
            false
        )
    );
    let second_ref = pool.value(*second).unwrap();
    assert_ne!(first_ref, second_ref);
    let constants = parse_pool(&pool);
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    egcl_rt::rooted!(restored_first = values[first_ref as usize]);
    egcl_rt::rooted!(restored_second = values[second_ref as usize]);
    assert_ne!(*restored_first, *restored_second);
    egcl_stdlib::set_aref(*restored_first, 0, EgclVal::from_fixnum(9)).unwrap();
    assert_eq!(
        egcl_stdlib::aref(*restored_second, 0).unwrap().as_fixnum(),
        1
    );
}

#[test]
fn typed_adjustable_storage_and_md_shells_roundtrip_after_actual_relocation() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use egcl_stdlib::sequences::{self, ArrayElementKind};
    egcl_rt::rooted!(
        u8_array = sequences::build_u8_complex_vector(4, 4, true, 255, false).unwrap()
    );
    egcl_rt::rooted!(
        double_array =
            sequences::build_double_float_complex_vector(4, 4, true, -1.25, false).unwrap()
    );
    let exact_bits = [
        (-1.25f64).to_bits(),
        0x8000_0000_0000_0000,
        1,
        0x7ff8_0000_0000_0071,
    ];
    for (index, bits) in exact_bits.into_iter().enumerate() {
        assert!(egcl_rt::packed_f64::set(
            array_constants::complex_parts(*double_array).unwrap().0,
            index,
            f64::from_bits(bits)
        ));
    }
    egcl_rt::rooted!(
        bit_array = egcl_stdlib::build_complex_vector(
            &[EgclVal::from_fixnum(1); 4],
            4,
            4,
            true,
            false,
            true,
            false
        )
    );
    egcl_rt::rooted!(
        char_array = sequences::build_base_char_complex_vector(4, 4, true, 'x', false).unwrap()
    );
    egcl_rt::rooted!(md = sequences::build_md_array_with_storage(&[2, 2], *u8_array).unwrap());
    egcl_rt::rooted!(
        carrier = egcl_stdlib::build_simple_vector(&[
            *u8_array,
            *double_array,
            *bit_array,
            *char_array,
            *md
        ])
    );
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    let reference = pool.value(*carrier).unwrap();
    let constants = parse_pool(&pool);
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(values = allocate_bbu_constants(&constants).unwrap());
    let complex = constants
        .iter()
        .position(|c| matches!(c, BbuConstant::ComplexArray { .. }))
        .unwrap();
    let md_index = constants
        .iter()
        .position(|c| matches!(c, BbuConstant::MdStorage { .. }))
        .unwrap();
    let before = values[complex].to_raw();
    let md_before = values[md_index].to_raw();
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(
        values[md_index].to_raw(),
        md_before,
        "MD shell must actually relocate before edge fixup"
    );
    assert_ne!(
        values[complex].to_raw(),
        before,
        "rooted graph shell must actually relocate before edge fixup"
    );
    fill_bbu_constant_edges(&constants, &values).unwrap();
    array_constants::validate_values(&constants, &values).unwrap();
    egcl_rt::rooted!(restored = values[reference as usize]);
    for (index, kind) in [
        ArrayElementKind::U8,
        ArrayElementKind::DoubleFloat,
        ArrayElementKind::Bit,
        ArrayElementKind::BaseChar,
    ]
    .into_iter()
    .enumerate()
    {
        let array = egcl_stdlib::elt(*restored, index).unwrap();
        assert!(sequences::array_adjustable(array));
        assert_eq!(sequences::array_element_kind(array), Some(kind));
        assert_eq!(egcl_stdlib::array_total_size(array), Some(4));
    }
    assert_eq!(
        egcl_rt::types::md_array_storage(egcl_stdlib::elt(*restored, 4).unwrap()),
        Some(egcl_stdlib::elt(*restored, 0).unwrap())
    );
    assert_eq!(
        egcl_rt::packed_u8::get(egcl_stdlib::elt(*restored, 0).unwrap(), 0),
        None
    );
    assert_eq!(
        sequences::u8_array_element(egcl_stdlib::elt(*restored, 0).unwrap(), 0),
        Some(255)
    );
    assert_eq!(
        sequences::double_float_array_element(egcl_stdlib::elt(*restored, 1).unwrap(), 0),
        Some(-1.25)
    );
    for (index, bits) in exact_bits.into_iter().enumerate() {
        assert_eq!(
            sequences::double_float_array_element(egcl_stdlib::elt(*restored, 1).unwrap(), index)
                .unwrap()
                .to_bits(),
            bits
        );
    }
}

#[test]
fn array_graph_rejects_storage_cycles_bounds_and_new_tags_in_old_versions() {
    let metadata = array_constants::ComplexMetadata {
        fill_pointer: 2,
        adjustable: true,
        element_type: 12,
        has_fill_pointer: false,
        displacement: None,
    };
    assert!(
        array_constants::validate_graph(&[BbuConstant::ComplexArray {
            storage_ref: 0,
            metadata: metadata.clone()
        }])
        .is_err()
    );
    assert!(
        array_constants::validate_graph(&[
            BbuConstant::ComplexArray {
                storage_ref: 1,
                metadata
            },
            BbuConstant::PackedU8(vec![1])
        ])
        .is_err()
    );
    assert!(
        array_constants::validate_graph(&[
            BbuConstant::MdStorage {
                dimensions: vec![2, 2],
                storage_ref: 1
            },
            BbuConstant::PackedU8(vec![1; 3])
        ])
        .is_err()
    );
    for tag in 25..=29 {
        assert!(parse_bbu_constant(&mut BbuCursor::new(&[tag]), 0x0204).is_err());
        assert!(parse_bbu_constant(&mut BbuCursor::new(&[tag]), BBU_BYTECODE_VERSION).is_err());
    }
}

#[test]
fn adjustable_arrays_can_contain_themselves_without_a_storage_cycle() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(
        array = egcl_stdlib::build_complex_vector(&[NIL], 1, 1, true, false, false, false)
    );
    egcl_stdlib::set_aref(*array, 0, *array).unwrap();
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    let reference = pool.value(*array).unwrap();
    let constants = parse_pool(&pool);
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    assert_eq!(
        egcl_stdlib::aref(values[reference as usize], 0).unwrap(),
        values[reference as usize]
    );
}

#[test]
fn array_graph_version_accepts_legacy_minor_and_rejects_future_minor_before_install() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    pool.value(NIL).unwrap();
    let mut unit = assemble_bbu(&pool, &[], &[], BBU_NO_INDEX, b"array-version");
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    unit[4..6].copy_from_slice(&0x0206u16.to_le_bytes());
    assert!(load_bbu_definitions(&unit, &mut env, false).is_err());
    unit[4..6].copy_from_slice(&0x0204u16.to_le_bytes());
    assert_eq!(load_bbu_definitions(&unit, &mut env, false).unwrap(), NIL);
}

#[test]
fn saved_image_array_constants_reference_actual_heap_objects_for_all_storage_kinds() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(doubles = egcl_rt::packed_f64::make(2, 1.25).unwrap());
    egcl_rt::rooted!(bits = egcl_stdlib::sequences::build_bit_vector_from_bits(&[0, 1]).unwrap());
    egcl_rt::rooted!(bytes = egcl_rt::packed_u8::make(2, 255).unwrap());
    egcl_rt::rooted!(general = egcl_stdlib::build_simple_vector(&[NIL, NIL]));
    egcl_rt::rooted!(text = egcl_stdlib::streams::make_lisp_string_fresh("mutable"));
    let mut pool = BbuConstPool {
        saved_heap_refs: true,
        ..BbuConstPool::default()
    };
    egcl_rt::rooted_ref!(_pool = &mut pool);
    for array in [*doubles, *bits, *bytes, *general, *text] {
        let reference = pool.value(array).unwrap();
        assert_eq!(pool.entries[reference as usize][0], 21);
        assert_eq!(
            u64::from_le_bytes(pool.entries[reference as usize][1..9].try_into().unwrap()),
            array.to_raw()
        );
    }
}

#[test]
fn typed_direct_storage_rejects_nested_wrappers_but_displaced_chains_remain_valid() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let metadata = array_constants::ComplexMetadata {
        fill_pointer: 2,
        adjustable: true,
        element_type: 12,
        has_fill_pointer: false,
        displacement: None,
    };
    let mut constants = vec![
        BbuConstant::PackedU8(vec![1, 255]),
        BbuConstant::ComplexArray {
            storage_ref: 0,
            metadata: metadata.clone(),
        },
        BbuConstant::ComplexArray {
            storage_ref: 1,
            metadata: metadata.clone(),
        },
    ];
    assert!(materialize_bbu_constants(&constants).is_err());
    constants[2] = BbuConstant::ComplexArray {
        storage_ref: 1,
        metadata: array_constants::ComplexMetadata {
            displacement: Some((0, 2)),
            ..metadata
        },
    };
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    assert_eq!(
        egcl_stdlib::sequences::u8_array_element(values[2], 1),
        Some(255)
    );
}
