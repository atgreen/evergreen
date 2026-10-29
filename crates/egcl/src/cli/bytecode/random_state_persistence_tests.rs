// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn opaque_random_state_bbu_sharing_and_distinct_copies_survive_relocation() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(state = egcl_rt::random_state::make(12345).unwrap());
    egcl_rt::rooted!(copy = egcl_rt::random_state::make(12345).unwrap());
    egcl_rt::rooted!(carrier = egcl_stdlib::build_simple_vector(&[*state, *state, *copy]));
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    let index = pool.value(*carrier).unwrap();
    assert_eq!(
        pool.entries
            .iter()
            .filter(|entry| entry.first() == Some(&31))
            .count(),
        2
    );
    let first_index = pool.value(*state).unwrap();
    let before = state.to_raw();
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(state.to_raw(), before, "memoized state must actually move");
    assert_eq!(pool.value(*state), Some(first_index));
    let constants = pool
        .entries
        .iter()
        .map(|entry| {
            let mut cursor = BbuCursor::new(entry);
            let constant = parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap();
            assert!(cursor.done());
            constant
        })
        .collect::<Vec<_>>();
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    egcl_rt::rooted!(carrier = values[index as usize]);
    egcl_rt::rooted!(first = egcl_stdlib::elt(*carrier, 0).unwrap());
    egcl_rt::rooted!(second = egcl_stdlib::elt(*carrier, 2).unwrap());
    assert_eq!(*first, egcl_stdlib::elt(*carrier, 1).unwrap());
    assert_ne!(*first, *second);
    assert_eq!(egcl_rt::random_state::snapshot(*first), Some(12345));
    for _ in 0..12 {
        assert_eq!(
            egcl_rt::random_state::draw(*first),
            egcl_rt::random_state::draw(*second)
        );
    }
}

#[test]
fn undrawn_public_state_roundtrip_preserves_first_draw_without_mutating_writer() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(state = egcl_stdlib::random_state::make_random_state(T, NIL).unwrap());
    let seed = egcl_rt::random_state::snapshot(*state).unwrap();
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_root = &mut pool);
    let index = pool.value(*state).unwrap();
    assert_eq!(egcl_rt::random_state::snapshot(*state), Some(seed));
    let entry = &pool.entries[index as usize];
    assert_eq!(entry.len(), 9);
    let constant = parse_bbu_constant(&mut BbuCursor::new(entry), BBU_BYTECODE_VERSION).unwrap();
    egcl_rt::rooted!(values = materialize_bbu_constants(&[constant]).unwrap());
    egcl_rt::rooted!(loaded = values[0]);
    for _ in 0..32 {
        assert_eq!(
            egcl_stdlib::random_state::draw(*state).unwrap(),
            egcl_stdlib::random_state::draw(*loaded).unwrap()
        );
    }
}

#[test]
fn random_state_tags_validate_seed_and_reject_legacy_units_before_payload() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut entry = vec![31];
    put_u64(&mut entry, 42);
    assert!(parse_bbu_constant(&mut BbuCursor::new(&entry[..8]), BBU_BYTECODE_VERSION).is_err());
    assert!(parse_bbu_constant(&mut BbuCursor::new(&entry), 0x0206).is_err());
    assert!(matches!(
        parse_bbu_constant(&mut BbuCursor::new(&entry), BBU_BYTECODE_VERSION).unwrap(),
        BbuConstant::RandomState { seed: 42 }
    ));
    // The legacy semantic boundary is checked before even verifier/header
    // payload parsing, therefore before any constant/definition/action effects.
    let mut old = BBU_MAGIC.to_vec();
    put_u16(&mut old, 0x0206);
    let mut env = Env::new(false);
    let error = load_bbu_definitions(&old, &mut env, false).unwrap_err();
    assert!(error.to_string().contains("unsupported bytecode version"));
}
