// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use std::cell::{Cell, RefCell};
use std::sync::Arc;

fn child_process(test: &str) -> bool {
    const CHILD: &str = "EGCL_TEST_ROOT_PREPARER_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(test) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static PREPARED_PASS: Cell<u64> = const { Cell::new(u64::MAX) };
    static EVENTS: RefCell<Vec<(u64, ExternalRootScanKind, u8)>> = const { RefCell::new(Vec::new()) };
    static CACHED_ROOTS: RefCell<Vec<Arc<CrossThreadRoot<EgclVal>>>> = const { RefCell::new(Vec::new()) };
}

fn prepare_order() {
    if !ACTIVE.with(Cell::get) {
        return;
    }
    assert!(
        root_preparers().try_lock().is_ok(),
        "preparer registry lock leaked into callback"
    );
    assert!(
        root_scanners().try_lock().is_ok(),
        "scanner registry lock leaked into callback"
    );
    // A deduplicating registration is also safe from inside the callback.
    register_root_preparer(prepare_order);
    let pass = root_scan_pass();
    assert_ne!(
        PREPARED_PASS.with(|previous| previous.replace(pass)),
        pass,
        "preparer ran twice in one pass"
    );
    EVENTS.with(|events| {
        events
            .borrow_mut()
            .push((pass, external_root_scan_kind(), 0))
    });
}

fn record_scanner(which: u8) {
    if !ACTIVE.with(Cell::get) {
        return;
    }
    assert_eq!(
        PREPARED_PASS.with(Cell::get),
        root_scan_pass(),
        "scanner ran before preparation"
    );
    EVENTS.with(|events| {
        events
            .borrow_mut()
            .push((root_scan_pass(), external_root_scan_kind(), which))
    });
}
fn scanner_registered_first(_: &mut dyn FnMut(*mut EgclVal)) {
    record_scanner(1);
}
fn scanner_registered_last(_: &mut dyn FnMut(*mut EgclVal)) {
    record_scanner(2);
}

#[test]
fn root_preparer_orders_deduplicates_and_unlocks_actual_gc_passes() {
    if !child_process(
        "gc::root_preparer_tests::root_preparer_orders_deduplicates_and_unlocks_actual_gc_passes",
    ) {
        return;
    }
    let config = GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 8 * 1024 * 1024,
        nursery_size: 1024 * 1024,
        tlab_size: 4096,
        region_size: 8192,
        promotion_threshold: 2,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    };
    init_heap(&config).unwrap();
    register_root_scanner(scanner_registered_first);
    register_root_preparer(prepare_order);
    register_root_preparer(prepare_order);
    register_root_scanner(scanner_registered_last);
    crate::rooted!(array = crate::packed_u8::make(32, 71).unwrap());
    ACTIVE.with(|active| active.set(true));
    collect_t0_minor().unwrap();
    full_gc().unwrap();
    with_heap_snapshot(|| {
        scan_external_roots(|_| {});
        unsafe {
            visit_shake_host_roots(&[], &mut |_| {});
        }
    })
    .unwrap();
    ACTIVE.with(|active| active.set(false));
    let events = EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()));
    assert!(!events.is_empty());
    for pass in events.chunk_by(|left, right| left.0 == right.0) {
        assert_eq!(
            pass.iter().map(|event| event.2).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }
    for kind in [
        ExternalRootScanKind::MinorMark,
        ExternalRootScanKind::MinorRelocate,
        ExternalRootScanKind::Full,
    ] {
        assert!(
            events.iter().any(|event| event.1 == kind),
            "missing actual scan kind {kind:?}"
        );
    }
    assert_eq!(crate::packed_u8::get(*array, 0), Some(71));
}

fn discard_cache_roots() {
    CACHED_ROOTS.with(|roots| roots.borrow_mut().clear());
}

#[test]
fn root_preparer_evicts_cross_thread_cache_before_shake_but_keeps_active_root() {
    if !child_process(
        "gc::root_preparer_tests::root_preparer_evicts_cross_thread_cache_before_shake_but_keeps_active_root",
    ) {
        return;
    }
    init_heap(&GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 8 * 1024 * 1024,
        nursery_size: 1024 * 1024,
        tlab_size: 4096,
        region_size: 8192,
        promotion_threshold: 2,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    })
    .unwrap();
    // This installs the real cross-thread scanner BEFORE the preparer. A
    // scanner-ordered cache eviction would observe the stale cache root first.
    let inactive = Arc::new(CrossThreadRoot::new(EgclVal::from_fixnum(918271)));
    let active = Arc::new(CrossThreadRoot::new(EgclVal::from_fixnum(918272)));
    let dead = Arc::downgrade(&inactive);
    CACHED_ROOTS.with(|roots| roots.borrow_mut().extend([inactive, Arc::clone(&active)]));
    register_root_preparer(discard_cache_roots);
    let mut values = Vec::new();
    with_heap_snapshot(|| unsafe {
        visit_shake_host_roots(&[], &mut |slot| values.push(*slot));
    })
    .unwrap();
    assert!(dead.upgrade().is_none(), "inactive cache root was retained");
    assert!(
        !values.contains(&EgclVal::from_fixnum(918271)),
        "cache-only reference reached scanner"
    );
    assert!(
        values.contains(&EgclVal::from_fixnum(918272)),
        "active independent root was lost"
    );
    assert_eq!(
        active.with_gc_stable_mutator(|value| *value),
        EgclVal::from_fixnum(918272)
    );
}
