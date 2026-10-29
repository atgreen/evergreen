// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static MOVED: AtomicBool = AtomicBool::new(false);

pub(super) fn take_armed() -> bool {
    ARMED.swap(false, Ordering::SeqCst)
}

pub(super) fn record_relocation(
    old_source: EgclVal,
    source: EgclVal,
    old_element: EgclVal,
    element: EgclVal,
) {
    assert_ne!(source, old_source, "reader source must actually relocate");
    assert_ne!(
        element, old_element,
        "prior boxed result must actually relocate"
    );
    MOVED.store(true, Ordering::SeqCst);
}

#[test]
fn reader_sequence_elements_roots_relocated_source_and_boxed_results() {
    const CHILD: &str = "EGCL_READER_SEQUENCE_RELOCATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::reader_sequence_gc_tests::reader_sequence_elements_roots_relocated_source_and_boxed_results",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_FORCE_TIER", "interp")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
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
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    egcl_rt::rooted!(storage = egcl_rt::packed_f64::make(10, 0.0).unwrap());
    for index in 0..10 {
        assert!(egcl_rt::packed_f64::set(
            *storage,
            index,
            index as f64 + 0.25
        ));
    }
    egcl_rt::rooted!(
        source = egcl_stdlib::sequences::build_double_float_displaced_vector(
            *storage, 2, 6, 4, true, true,
        )
        .unwrap()
    );
    let before = *source;
    ARMED.store(true, Ordering::SeqCst);
    egcl_rt::rooted!(elements = reader_sequence_elements(*source).unwrap());
    assert!(MOVED.load(Ordering::SeqCst));
    assert_ne!(*source, before, "caller's source root must update too");
    assert_eq!(
        elements.len(),
        4,
        "the visible fill pointer controls length"
    );
    for (index, element) in elements.iter().enumerate() {
        assert!(element.is_double_float());
        assert_eq!(element.as_double_float(), index as f64 + 2.25);
    }
    assert_eq!(egcl_stdlib::length(*source).unwrap(), 4);
    assert_eq!(
        egcl_stdlib::sequences::double_float_array_element(*source, 0),
        Some(2.25)
    );
}
