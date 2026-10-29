// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn nil_array_graphs_preserve_storage_identity_and_move_without_element_edges() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(
        base = egcl_stdlib::sequences::build_nil_complex_vector(5, 0, true, true).unwrap()
    );
    egcl_rt::rooted!(
        view = egcl_stdlib::sequences::build_nil_displaced_vector(*base, 1, 2, 2, false, false)
            .unwrap()
    );
    egcl_rt::rooted!(empty = egcl_rt::nil_array::make(0).unwrap());
    egcl_rt::rooted!(other = egcl_rt::nil_array::make(0).unwrap());
    egcl_rt::rooted!(
        graph = egcl_stdlib::build_simple_vector(&[*base, *base, *view, *empty, *other])
    );
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    let index = pool.value(*graph).unwrap();
    let old = base.to_raw();
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(
        base.to_raw(),
        old,
        "memoized NIL backing wrapper must actually move"
    );
    assert_eq!(pool.value(*graph), Some(index));
    let constants = pool
        .entries
        .iter()
        .map(|bytes| {
            let mut cursor = BbuCursor::new(bytes);
            let value = parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap();
            assert!(cursor.done());
            value
        })
        .collect::<Vec<_>>();
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    egcl_rt::rooted!(loaded = values[index as usize]);
    egcl_rt::rooted!(loaded_base = egcl_stdlib::elt(*loaded, 0).unwrap());
    egcl_rt::rooted!(loaded_view = egcl_stdlib::elt(*loaded, 2).unwrap());
    assert_eq!(*loaded_base, egcl_stdlib::elt(*loaded, 1).unwrap());
    assert_eq!(
        egcl_stdlib::cvec_displacement(*loaded_view).unwrap().0,
        *loaded_base
    );
    assert_eq!(egcl_stdlib::array_total_size(*loaded_base), Some(5));
    assert_eq!(egcl_stdlib::length(*loaded_base).unwrap(), 0);
    assert_eq!(
        egcl_stdlib::sequences::array_element_kind(*loaded_base),
        Some(egcl_stdlib::sequences::ArrayElementKind::Nil)
    );
    assert_ne!(
        egcl_stdlib::elt(*loaded, 3).unwrap(),
        egcl_stdlib::elt(*loaded, 4).unwrap(),
        "independent zero arrays must not be coalesced"
    );
    assert!(egcl_stdlib::elt(*loaded_view, 0).is_err());
}

#[test]
fn nil_array_constants_reject_invalid_length_and_older_minor_tag() {
    let mut bytes = vec![32];
    put_u64(&mut bytes, egcl_rt::bignum::FIXNUM_MAX as u64);
    assert!(parse_bbu_constant(&mut BbuCursor::new(&bytes), BBU_BYTECODE_VERSION).is_err());
    bytes.truncate(1);
    put_u64(&mut bytes, 7);
    for older in [0x0300, 0x0301] {
        assert!(parse_bbu_constant(&mut BbuCursor::new(&bytes), older).is_err());
    }
    assert!(matches!(
        parse_bbu_constant(&mut BbuCursor::new(&bytes), BBU_BYTECODE_VERSION).unwrap(),
        BbuConstant::NilArray { length: 7 }
    ));
    assert!(parse_bbu_constant(&mut BbuCursor::new(&bytes[..8]), BBU_BYTECODE_VERSION).is_err());
}
