// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{Collector, HeapCollector};
use std::sync::atomic::{AtomicBool, Ordering};

static FAIL_MAJOR: AtomicBool = AtomicBool::new(false);
static SCANNER_PANICKED: AtomicBool = AtomicBool::new(false);

fn failing_scanner(_visit: &mut dyn FnMut(*mut egcl_rt::EgclVal)) {
    if egcl_rt::gc::gc_marking_in_progress() == FAIL_MAJOR.load(Ordering::Relaxed) {
        SCANNER_PANICKED.store(true, Ordering::Relaxed);
        panic!("injected root-scanner failure");
    }
}

#[test]
fn collector_panic_releases_the_safepoint_request() {
    const CHILD: &str = "EGCL_TEST_COLLECTOR_UNWIND";
    let Ok(kind) = std::env::var(CHILD) else {
        for kind in ["minor", "major", "full"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "collector_panic_releases_the_safepoint_request", "--nocapture"])
                .env(CHILD, kind)
                .output()
                .unwrap();
            assert!(output.status.success(), "{kind}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr));
        }
        return;
    };

    // A separate process owns the intentionally interrupted heap. This tests
    // pause cleanup, not recovery of a heap after an arbitrary collector panic.
    egcl_rt::gc::ensure_heap_initialized();
    let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe { *(body as *mut f64) = 42.0 };
    egcl_rt::rooted!(value = unsafe { egcl_rt::EgclVal::from_heap_ptr(body.sub(8)) });
    let original = value.to_raw();
    FAIL_MAJOR.store(kind != "minor", Ordering::Relaxed);
    egcl_rt::gc::register_root_scanner(failing_scanner);
    let result = std::panic::catch_unwind(|| {
        let mut collector = HeapCollector::new();
        match kind.as_str() {
            "minor" => collector.minor_gc(),
            "major" => collector.major_gc(),
            "full" => collector.full_gc(),
            _ => unreachable!(),
        }
    });

    // Attempting another pause detects a stranded owner without waiting for
    // thread teardown to hang. Clean up even on the failing implementation.
    let next_pause = egcl_rt::safepoint::wait_for_all_threads();
    egcl_rt::safepoint::resume_all_threads().unwrap();
    assert!(result.is_err() && SCANNER_PANICKED.load(Ordering::Relaxed));
    assert!(next_pause.is_ok(), "collector panic stranded its pause: {next_pause:?}");
    if kind != "minor" {
        // The nursery drain really relocated a live value before the major
        // scanner failed; this is not a stress run in which nothing moved.
        assert_ne!(value.to_raw(), original);
        assert_eq!(unsafe { *(value.as_ptr().add(8) as *const f64) }, 42.0);
    }
}
