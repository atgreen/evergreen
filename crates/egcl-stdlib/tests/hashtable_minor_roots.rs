// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Real moving-GC coverage for remembered off-heap hash-table roots. This binary
//! has one test, so initialization and explicit collection are isolated.
use egcl_rt::gc::{self, ExternalRootScanKind, GcConfig};
use egcl_rt::object::type_id;
use egcl_rt::value::{EgclVal, NIL};
use egcl_stdlib::{
    MakeHashTableOptions, gethash, hash_table_entries, make_hash_table, set_gethash,
};
use std::sync::atomic::{AtomicUsize, Ordering};

static MARKS: AtomicUsize = AtomicUsize::new(0);
static RELOCATIONS: AtomicUsize = AtomicUsize::new(0);
static FULLS: AtomicUsize = AtomicUsize::new(0);

fn observe_kind(_: &mut dyn FnMut(*mut EgclVal)) {
    let counter = match gc::external_root_scan_kind() {
        ExternalRootScanKind::Full => &FULLS,
        ExternalRootScanKind::MinorMark => &MARKS,
        ExternalRootScanKind::MinorRelocate => &RELOCATIONS,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

fn cons(marker: i64) -> EgclVal {
    let body = egcl_rt::alloc_typed(16, type_id::CONS).expect("allocate young cons");
    unsafe {
        body.cast::<EgclVal>().write(EgclVal::from_fixnum(marker));
        body.cast::<EgclVal>().add(1).write(NIL);
        EgclVal::from_cons_ptr(body)
    }
}

fn marker(value: EgclVal) -> i64 {
    assert!(value.is_cons());
    unsafe { value.as_ptr().cast::<EgclVal>().read().as_fixnum() }
}

fn check_entries(table: EgclVal, expected_value: i64) {
    let entries = hash_table_entries(table).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(marker(entries[0].0), 17);
    assert_eq!(marker(entries[0].1), expected_value);
    assert_eq!(
        gethash(entries[0].0, table, NIL).unwrap(),
        (entries[0].1, true)
    );
}

#[test]
fn minor_tracking_moves_new_keys_and_overwrites_then_preserves_full_roots() {
    gc::init_heap(&GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 16 * 1024 * 1024,
        nursery_size: 1024 * 1024,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 15,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    })
    .unwrap();
    gc::register_root_scanner(observe_kind);
    assert!(matches!(
        gc::external_root_scan_kind(),
        ExternalRootScanKind::Full
    ));
    // Exercise both ordinary and synchronized publication with identical roots.
    for synchronized in [false, true] {
        let table = make_hash_table(&MakeHashTableOptions {
            synchronized,
            ..Default::default()
        })
        .unwrap();
        let before = {
            egcl_rt::rooted!(key = cons(17));
            egcl_rt::rooted!(value = cons(23));
            set_gethash(*key, table, *value).unwrap();
            ((*key).to_raw(), (*value).to_raw())
        };
        // No Rust-local key/value root remains; the table is their only owner.
        gc::collect_t0_minor().unwrap();
        {
            let entries = hash_table_entries(table).unwrap();
            assert_ne!(
                entries[0].0.to_raw(),
                before.0,
                "young key must really move"
            );
            assert_ne!(
                entries[0].1.to_raw(),
                before.1,
                "young value must really move"
            );
        }
        check_entries(table, 23);
        // An unchanged table can be skipped by the second minor. Survivor
        // references remain valid even though its original nursery was reset.
        gc::collect_t0_minor().unwrap();
        check_entries(table, 23);
        let replacement_before = {
            egcl_rt::rooted!(key = hash_table_entries(table).unwrap()[0].0);
            egcl_rt::rooted!(replacement = cons(31));
            set_gethash(*key, table, *replacement).unwrap();
            (*replacement).to_raw()
        };
        gc::collect_t0_minor().unwrap();
        assert_ne!(
            hash_table_entries(table).unwrap()[0].1.to_raw(),
            replacement_before,
            "overwrite must dirty a previously clean table and relocate its new value"
        );
        check_entries(table, 31);
        // Major/full-image preparation must still retain both strong halves.
        gc::full_gc().unwrap();
        check_entries(table, 31);
        let mut visits = 0;
        gc::with_heap_snapshot(|| {
            assert!(matches!(
                gc::external_root_scan_kind(),
                ExternalRootScanKind::Full
            ));
            // Snapshot/shake uses the same registered scanner under default Full.
            egcl_stdlib::hashtable::shake_root_scanner()(&mut |_| visits += 1);
        })
        .unwrap();
        assert!(visits >= 2);
        gc::collect_t0_minor().unwrap();
        check_entries(table, 31);
    }
    // Fill to the resize threshold, collect it clean, then publish a fresh
    // key/value while growing. Rebuilding buckets must not lose the dirty bit.
    let resized = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    for index in 0..12 {
        set_gethash(EgclVal::from_fixnum(index), resized, NIL).unwrap();
    }
    gc::collect_t0_minor().unwrap();
    let old_capacity = egcl_stdlib::hash_table_size(resized).unwrap();
    let before = {
        egcl_rt::rooted!(key = cons(41));
        egcl_rt::rooted!(value = cons(43));
        set_gethash(*key, resized, *value).unwrap();
        ((*key).to_raw(), (*value).to_raw())
    };
    assert!(egcl_stdlib::hash_table_size(resized).unwrap() > old_capacity);
    gc::collect_t0_minor().unwrap();
    let entries = hash_table_entries(resized).unwrap();
    let &(key, value) = entries.iter().find(|(key, _)| key.is_cons()).unwrap();
    assert_ne!(key.to_raw(), before.0, "new resize key must really move");
    assert_ne!(
        value.to_raw(),
        before.1,
        "new resize value must really move"
    );
    assert_eq!((marker(key), marker(value)), (41, 43));
    assert_eq!(gethash(key, resized, NIL).unwrap(), (value, true));
    assert!(
        MARKS.load(Ordering::Relaxed) >= 6,
        "actual minor mark scopes were observed"
    );
    assert!(
        RELOCATIONS.load(Ordering::Relaxed) >= 6,
        "actual minor relocation scopes were observed"
    );
    assert!(
        FULLS.load(Ordering::Relaxed) > 0,
        "actual major full scopes were observed"
    );
    assert!(matches!(
        gc::external_root_scan_kind(),
        ExternalRootScanKind::Full
    ));
}
