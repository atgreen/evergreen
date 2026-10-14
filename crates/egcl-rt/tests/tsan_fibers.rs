// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(egcl_tsan)]

use egcl_rt::{EgclVal, SchedulerConfig, SchedulerGroup};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

#[path = "support/native_entry.rs"]
mod native_entry;

static READY: AtomicUsize = AtomicUsize::new(0);
static RACE: AtomicBool = AtomicBool::new(false);
static ATOMIC: AtomicU64 = AtomicU64::new(0);
static mut RACY: u64 = 0;

fn racing_fiber() -> EgclVal {
    let race = RACE.load(Ordering::Relaxed);
    let index = READY.fetch_add(1, Ordering::AcqRel);
    // Both fibers must be mounted simultaneously on distinct carriers. No
    // yield, lock, or runtime call orders their accesses after this barrier.
    while READY.load(Ordering::Acquire) != 2 {
        std::hint::spin_loop();
    }
    for value in 0..1000 {
        if race {
            // Deliberate UB, confined to a TSan subprocess that MUST report it.
            unsafe { std::ptr::write_volatile(&raw mut RACY, value + index as u64) };
        } else {
            ATOMIC.store(value + index as u64, Ordering::Relaxed);
        }
    }
    EgclVal::from_fixnum(42)
}

#[test]
fn concurrent_fibers_keep_races_visible() {
    const CHILD: &str = "EGCL_TEST_TSAN_RACE_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(20));
            std::process::abort();
        });
        RACE.store(mode == "race", Ordering::Relaxed);
        let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
        let entry = native_entry::entry(racing_fiber);
        for _ in 0..2 {
            group
                .submit(egcl_rt::thread::make_fiber(entry).unwrap())
                .unwrap();
        }
        assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(42); 2]);
        return;
    }
    for mode in ["atomic", "race"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "concurrent_fibers_keep_races_visible",
                "--nocapture",
            ])
            .env(CHILD, mode)
            .env("TSAN_OPTIONS", "halt_on_error=1:exitcode=66")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if mode == "atomic" {
            assert!(output.status.success(), "atomic control: {stderr}");
            assert!(!stderr.contains("WARNING: ThreadSanitizer"), "{stderr}");
        } else {
            assert_eq!(output.status.code(), Some(66), "race control: {stderr}");
            assert!(
                stderr.contains("WARNING: ThreadSanitizer: data race"),
                "{stderr}"
            );
            assert!(
                stderr.contains("racing_fiber"),
                "wrong race detected: {stderr}"
            );
        }
    }
}
