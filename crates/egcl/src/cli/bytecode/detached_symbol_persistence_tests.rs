// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn detached(name: &str) -> EgclVal {
    let package = egcl_rt::packages::find_or_create("DETACHED-POOL-TEST");
    let index = egcl_rt::symbols::intern(&format!("DETACHED-POOL-TEST::{name}"));
    egcl_rt::symbols::set_symbol_package(index, package);
    egcl_rt::symbols::detach_symbol_from_package(index, package);
    EgclVal::from_symbol_index(index)
}

#[test]
fn portable_detached_symbols_keep_bare_names_and_artifact_sharing() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let first = detached("FOO");
    let second = detached("FOO");
    assert_ne!(first, second);
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    let first_ref = pool.value(first).unwrap();
    assert_eq!(pool.value(first), Some(first_ref));
    let second_ref = pool.value(second).unwrap();
    assert_ne!(first_ref, second_ref);
    let constants: Vec<_> = pool
        .entries
        .iter()
        .map(|bytes| {
            let mut cursor = BbuCursor::new(bytes);
            let constant = parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap();
            assert!(cursor.done());
            constant
        })
        .collect();
    assert!(matches!(
        constants[first_ref as usize],
        BbuConstant::UninternedSymbol(_)
    ));
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    let restored_first = values[first_ref as usize];
    let restored_second = values[second_ref as usize];
    assert_ne!(restored_first, first);
    assert_ne!(restored_first, restored_second);
    for restored in [restored_first, restored_second] {
        assert_eq!(super::super::symbol_name_of(restored), "FOO");
        assert_eq!(
            egcl_rt::symbols::symbol_package(restored.as_symbol_index()),
            Some(NIL)
        );
    }
}

#[test]
fn image_detached_dense_symbol_constants_keep_the_exact_symbol_index() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let symbol = detached("IMAGE");
    let mut pool = BbuConstPool {
        saved_heap_refs: true,
        ..Default::default()
    };
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    let reference = pool.value(symbol).unwrap();
    let mut cursor = BbuCursor::new(&pool.entries[reference as usize]);
    match parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap() {
        BbuConstant::SavedHeapRef(bits) => assert_eq!(bits, symbol.to_raw()),
        other => panic!("image changed a retained dense symbol identity: {other:?}"),
    }
}
