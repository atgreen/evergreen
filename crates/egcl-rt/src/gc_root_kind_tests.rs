// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use std::cell::Cell;

#[test]
fn root_kind_scope_restores_nested_full_and_unwind() {
    assert_eq!(external_root_scan_kind(), ExternalRootScanKind::Full);
    let outer = root_kind::Scope::enter(ExternalRootScanKind::MinorMark);
    {
        let _full = root_kind::Scope::enter(ExternalRootScanKind::Full);
        assert_eq!(external_root_scan_kind(), ExternalRootScanKind::Full);
    }
    assert_eq!(external_root_scan_kind(), ExternalRootScanKind::MinorMark);
    let result = std::panic::catch_unwind(|| {
        let _relocate = root_kind::Scope::enter(ExternalRootScanKind::MinorRelocate);
        panic!("test scope unwind");
    });
    assert!(result.is_err());
    assert_eq!(external_root_scan_kind(), ExternalRootScanKind::MinorMark);
    drop(outer);
    assert_eq!(external_root_scan_kind(), ExternalRootScanKind::Full);
}

thread_local! {
    static PANIC_NEXT: Cell<bool> = const { Cell::new(false) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static NEST: Cell<bool> = const { Cell::new(false) };
    static NESTING: Cell<bool> = const { Cell::new(false) };
    static SEEN: RefCell<Vec<ExternalRootScanKind>> = const { RefCell::new(Vec::new()) };
}

fn record_kind(_: &mut dyn FnMut(*mut EgclVal)) {
    if !ACTIVE.with(Cell::get) {
        return;
    }
    if PANIC_NEXT.with(|panic| panic.replace(false)) {
        panic!("test registered scanner unwind");
    }
    let kind = external_root_scan_kind();
    SEEN.with(|seen| seen.borrow_mut().push(kind));
    if NEST.with(Cell::get) && kind != ExternalRootScanKind::Full && !NESTING.with(Cell::get) {
        NESTING.with(|nested| nested.set(true));
        scan_external_roots(|_| {});
        NESTING.with(|nested| nested.set(false));
        assert_eq!(
            external_root_scan_kind(),
            kind,
            "nested full scan must restore its caller"
        );
    }
}

#[test]
fn actual_minor_major_and_snapshot_scanners_report_scoped_kind() {
    // Collecting RT tests use a dedicated process, so no unrelated test roots
    // or registered callback state can race with a stop-the-world collection.
    const CHILD: &str = "EGCL_TEST_ROOT_KIND_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "gc::root_kind_tests::actual_minor_major_and_snapshot_scanners_report_scoped_kind",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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
    register_root_scanner(record_kind);
    crate::rooted!(array = crate::packed_u8::make(32, 71).unwrap());
    ACTIVE.with(|active| active.set(true));
    NEST.with(|nested| nested.set(true));
    collect_t0_minor().unwrap();
    let seen = SEEN.with(|seen| std::mem::take(&mut *seen.borrow_mut()));
    assert!(seen.contains(&ExternalRootScanKind::MinorMark));
    assert!(seen.contains(&ExternalRootScanKind::MinorRelocate));
    assert!(
        seen.contains(&ExternalRootScanKind::Full),
        "nested full invocation must be visible"
    );
    assert_eq!(external_root_scan_kind(), ExternalRootScanKind::Full);
    assert_eq!(crate::packed_u8::get(*array, 0), Some(71));
    NEST.with(|nested| nested.set(false));
    full_gc().unwrap();
    let seen = SEEN.with(|seen| std::mem::take(&mut *seen.borrow_mut()));
    assert!(
        seen.contains(&ExternalRootScanKind::Full),
        "actual major scanner must remain full"
    );
    with_heap_snapshot(|| {
        scan_external_roots(|_| {});
        unsafe {
            visit_shake_host_roots(&[], &mut |_| {});
        }
    })
    .unwrap();
    let seen = SEEN.with(|seen| std::mem::take(&mut *seen.borrow_mut()));
    assert_eq!(
        seen,
        [ExternalRootScanKind::Full, ExternalRootScanKind::Full]
    );
    with_heap_snapshot(|| {
        let _outer = root_kind::Scope::enter(ExternalRootScanKind::MinorMark);
        PANIC_NEXT.with(|panic| panic.set(true));
        let result = std::panic::catch_unwind(|| scan_external_roots(|_| {}));
        assert!(result.is_err());
        assert_eq!(external_root_scan_kind(), ExternalRootScanKind::MinorMark);
    })
    .unwrap();
    ACTIVE.with(|active| active.set(false));
    assert_eq!(external_root_scan_kind(), ExternalRootScanKind::Full);
}
