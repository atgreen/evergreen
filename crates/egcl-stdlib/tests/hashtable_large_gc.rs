// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::object::{ConsCell, type_id};
use egcl_rt::value::{EgclVal, NIL};
use egcl_stdlib::hashtable::*;

fn cons(marker: i64) -> EgclVal {
    let body = egcl_rt::alloc_typed(16, type_id::CONS).unwrap();
    // SAFETY: a fresh cons body has two tagged-value slots.
    unsafe {
        let cell = body as *mut ConsCell;
        (*cell).car = EgclVal::from_fixnum(marker);
        (*cell).cdr = NIL;
        EgclVal::from_cons_ptr(body)
    }
}

#[test]
fn large_tables_find_relocated_keys_and_remove_dead_weak_entries() {
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 16 * 1024 * 1024,
        heap_max: 64 * 1024 * 1024,
        nursery_size: 64 * 1024,
        tlab_size: 4096,
        region_size: 4096,
        promotion_threshold: 3,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.9,
    })
    .unwrap();
    for test in [
        HashTest::Eq,
        HashTest::Eql,
        HashTest::Equal,
        HashTest::Equalp,
    ] {
        for weakness in [
            None,
            Some(Weakness::Key),
            Some(Weakness::Value),
            Some(Weakness::KeyAndValue),
        ] {
            let table = make_hash_table(&MakeHashTableOptions {
                test,
                weakness,
                size: 8192,
                ..Default::default()
            })
            .unwrap();
            egcl_rt::rooted!(key = cons(42));
            egcl_rt::rooted!(value = cons(99));
            set_gethash(*key, table, *value).unwrap();
            {
                egcl_rt::rooted!(doomed_key = cons(43));
                egcl_rt::rooted!(doomed_value = cons(100));
                set_gethash(*doomed_key, table, *doomed_value).unwrap();
            }
            assert_eq!(hash_table_count(table).unwrap(), 2);
            let old_key = *key;
            let old_value = *value;
            egcl_rt::collect_t0_minor().unwrap();
            assert_ne!(*key, old_key, "key must move: {test:?}/{weakness:?}");
            assert_ne!(*value, old_value, "value must move: {test:?}/{weakness:?}");
            assert_eq!(
                hash_table_count(table).unwrap(),
                if weakness.is_some() { 1 } else { 2 }
            );
            assert_eq!(gethash(*key, table, NIL).unwrap(), (*value, true));
            if matches!(test, HashTest::Equal | HashTest::Equalp) {
                egcl_rt::rooted!(equivalent_key = cons(42));
                assert_eq!(
                    gethash(*equivalent_key, table, NIL).unwrap(),
                    (*value, true)
                );
            }
            set_gethash(*key, table, EgclVal::from_fixnum(123)).unwrap();
            assert_eq!(
                gethash(*key, table, NIL).unwrap(),
                (EgclVal::from_fixnum(123), true)
            );
            assert!(remhash(*key, table).unwrap());
            assert_eq!(gethash(*key, table, NIL).unwrap(), (NIL, false));
        }
    }
}
