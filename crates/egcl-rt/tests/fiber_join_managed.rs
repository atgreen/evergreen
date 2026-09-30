#![cfg(all(target_arch = "x86_64", any(unix, windows)))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::sync::OnceLock;
use egcl_rt::thread::{current_fiber, join_fiber, make_fiber};
use egcl_rt::{SchedulerConfig, SchedulerGroup, EgclVal};

static GROUP: OnceLock<SchedulerGroup> = OnceLock::new();

fn function(entry: fn() -> EgclVal) -> EgclVal {
    unsafe { EgclVal::from_function_ptr(entry as *const () as *mut u8) }
}

fn collecting_child() -> EgclVal {
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::gc::alloc_double_float(42.0)
}

fn joining_parent() -> EgclVal {
    egcl_rt::rooted!(kept = egcl_rt::gc::alloc_double_float(41.0));
    let original = kept.to_raw();
    let child = make_fiber(function(collecting_child)).unwrap();
    GROUP.get().unwrap().submit(child).unwrap();
    // The child is queued on this carrier. It cannot run until JOIN unmounts
    // the parent; there are no polls between submission and this call.
    egcl_rt::rooted!(answer = join_fiber(child).unwrap());
    assert_ne!(kept.to_raw(), original);
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 41.0);
    egcl_rt::collect_t0_minor().unwrap();
    assert_eq!(unsafe { answer.as_ptr().add(8).cast::<f64>().read() }, 42.0);
    *answer
}

fn pinned_parent() -> EgclVal {
    use egcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    set_pinned_blocking_action(PinnedBlockingAction::Error);
    let child = make_fiber(function(collecting_child)).unwrap();
    let fiber = current_fiber().unwrap();
    fiber.pin();
    let error = join_fiber(child).unwrap_err();
    fiber.unpin().unwrap();
    assert!(matches!(error, egcl_rt::EgclError::ProgramError(_)));
    // Rejection must leave the target joinable once blocking is permitted.
    GROUP.get().unwrap().submit(child).unwrap();
    join_fiber(child).unwrap()
}

fn pinned_native_parent() -> EgclVal {
    use egcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    set_pinned_blocking_action(PinnedBlockingAction::Native);
    egcl_rt::rooted!(kept = egcl_rt::gc::alloc_double_float(41.0));
    let child = make_fiber(function(collecting_child)).unwrap();
    current_fiber().unwrap().pin();
    GROUP.get().unwrap().submit(child).unwrap();
    egcl_rt::rooted!(answer = join_fiber(child).unwrap());
    current_fiber().unwrap().unpin().unwrap();
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 41.0);
    *answer
}

fn isolated(name: &str, entry: fn() -> EgclVal, num_workers: usize) {
    const CHILD: &str = "EGCL_TEST_MANAGED_JOIN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
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
    // Bound the old single-carrier deadlock without leaving a blocked carrier
    // behind in the test harness. A successful child exits before this fires.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(10));
        eprintln!("managed join failed to release its carrier");
        std::process::exit(124);
    });
    egcl_rt::thread::current_thread_id();
    egcl_rt::gc::ensure_heap_initialized();
    assert!(
        GROUP
            .set(SchedulerGroup::init(&SchedulerConfig { num_workers }).unwrap())
            .is_ok()
    );
    let parent = make_fiber(function(entry)).unwrap();
    GROUP.get().unwrap().submit(parent).unwrap();
    egcl_rt::rooted!(answer = join_fiber(parent).unwrap());
    GROUP.get().unwrap().shutdown().unwrap();
    assert_eq!(unsafe { answer.as_ptr().add(8).cast::<f64>().read() }, 42.0);
}

#[test]
fn single_carrier_join_preserves_roots_and_runs_its_target() {
    isolated(
        "single_carrier_join_preserves_roots_and_runs_its_target",
        joining_parent,
        1,
    );
}

#[test]
fn pinned_join_obeys_error_policy_without_consuming_target() {
    isolated(
        "pinned_join_obeys_error_policy_without_consuming_target",
        pinned_parent,
        1,
    );
}

#[test]
fn pinned_native_join_allows_collection_on_another_carrier() {
    isolated(
        "pinned_native_join_allows_collection_on_another_carrier",
        pinned_native_parent,
        2,
    );
}
