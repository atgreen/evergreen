#![cfg(all(target_arch = "x86_64", any(unix, windows)))]

use std::sync::OnceLock;
use torcl_rt::thread::{current_fiber, join_fiber, make_fiber};
use torcl_rt::{SchedulerConfig, SchedulerGroup, TorclVal};

static GROUP: OnceLock<SchedulerGroup> = OnceLock::new();

fn function(entry: fn() -> TorclVal) -> TorclVal {
    unsafe { TorclVal::from_function_ptr(entry as *const () as *mut u8) }
}

fn collecting_child() -> TorclVal {
    torcl_rt::collect_t0_minor().unwrap();
    torcl_rt::gc::alloc_double_float(42.0)
}

fn joining_parent() -> TorclVal {
    torcl_rt::rooted!(kept = torcl_rt::gc::alloc_double_float(41.0));
    let original = kept.to_raw();
    let child = make_fiber(function(collecting_child)).unwrap();
    GROUP.get().unwrap().submit(child).unwrap();
    // The child is queued on this carrier. It cannot run until JOIN unmounts
    // the parent; there are no polls between submission and this call.
    torcl_rt::rooted!(answer = join_fiber(child).unwrap());
    assert_ne!(kept.to_raw(), original);
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 41.0);
    torcl_rt::collect_t0_minor().unwrap();
    assert_eq!(unsafe { answer.as_ptr().add(8).cast::<f64>().read() }, 42.0);
    *answer
}

fn pinned_parent() -> TorclVal {
    use torcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    set_pinned_blocking_action(PinnedBlockingAction::Error);
    let child = make_fiber(function(collecting_child)).unwrap();
    let fiber = current_fiber().unwrap();
    fiber.pin();
    let error = join_fiber(child).unwrap_err();
    fiber.unpin().unwrap();
    assert!(matches!(error, torcl_rt::TorclError::ProgramError(_)));
    // Rejection must leave the target joinable once blocking is permitted.
    GROUP.get().unwrap().submit(child).unwrap();
    join_fiber(child).unwrap()
}

fn pinned_native_parent() -> TorclVal {
    use torcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    set_pinned_blocking_action(PinnedBlockingAction::Native);
    torcl_rt::rooted!(kept = torcl_rt::gc::alloc_double_float(41.0));
    let child = make_fiber(function(collecting_child)).unwrap();
    current_fiber().unwrap().pin();
    GROUP.get().unwrap().submit(child).unwrap();
    torcl_rt::rooted!(answer = join_fiber(child).unwrap());
    current_fiber().unwrap().unpin().unwrap();
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 41.0);
    *answer
}

fn isolated(name: &str, entry: fn() -> TorclVal, num_workers: usize) {
    const CHILD: &str = "TORCL_TEST_MANAGED_JOIN_CHILD";
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
    torcl_rt::thread::current_thread_id();
    torcl_rt::gc::ensure_heap_initialized();
    assert!(
        GROUP
            .set(SchedulerGroup::init(&SchedulerConfig { num_workers }).unwrap())
            .is_ok()
    );
    let parent = make_fiber(function(entry)).unwrap();
    GROUP.get().unwrap().submit(parent).unwrap();
    torcl_rt::rooted!(answer = join_fiber(parent).unwrap());
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
