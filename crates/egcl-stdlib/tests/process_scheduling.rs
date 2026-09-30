//! A child waits for a collecting peer: capture must release GC and carriers.
#![cfg(all(target_arch = "x86_64", any(unix, windows)))]

use std::path::PathBuf;
use std::time::{Duration, Instant};
use egcl_rt::{SchedulerConfig, SchedulerGroup, EgclVal};
use egcl_stdlib::process::{ProcessCommand, launch_program, run_program};

fn marker(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("EGCL_PROCESS_TEST_DIR").unwrap()).join(name)
}

#[test]
#[ignore = "subprocess fixture, explicitly selected by the capture tests"]
fn child_waits_for_collecting_peer() {
    std::fs::write(marker("ready"), b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker("release").exists() {
        assert!(
            Instant::now() < deadline,
            "collecting peer never released child"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("child-released-after-gc");
}

fn collecting_peer() -> EgclVal {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !marker("ready").exists() {
        assert!(Instant::now() < deadline, "capture never started its child");
        egcl_rt::sync::fiber_sleep(Duration::from_millis(2)).unwrap();
    }
    egcl_rt::collect_t0_minor().unwrap();
    std::fs::write(marker("release"), b"collected").unwrap();
    egcl_rt::value::T
}

fn child_command() -> ProcessCommand {
    ProcessCommand::Argv(vec![
        std::env::current_exe().unwrap().to_str().unwrap().into(),
        "--exact".into(),
        "child_waits_for_collecting_peer".into(),
        "--ignored".into(),
        "--nocapture".into(),
    ])
}

fn capturing_caller() -> EgclVal {
    egcl_rt::rooted!(kept = egcl_rt::gc::alloc_double_float(41.0));
    let original = kept.to_raw();
    let output = run_program(child_command()).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("child-released-after-gc"));
    assert_ne!(
        kept.to_raw(),
        original,
        "peer must actually relocate a live root"
    );
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 41.0);
    egcl_rt::value::T
}

fn function(entry: fn() -> EgclVal) -> EgclVal {
    unsafe { EgclVal::from_function_ptr(entry as *const () as *mut u8) }
}

fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var_os("EGCL_PROCESS_TEST_DIR").is_none() {
        let directory =
            std::env::temp_dir().join(format!("egcl-process-{name}-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("EGCL_PROCESS_TEST_DIR", &directory)
            .output()
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(15));
        eprintln!("subprocess capture blocked runtime progress");
        std::process::exit(124);
    });
    egcl_rt::thread::current_thread_id();
    egcl_rt::gc::ensure_heap_initialized();
    body();
}

fn run_fibers(entries: &[fn() -> EgclVal], num_workers: usize) {
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers }).unwrap();
    for &entry in entries {
        let fiber = egcl_rt::thread::make_fiber(function(entry)).unwrap();
        group.submit(fiber).unwrap();
    }
    group.finish().unwrap();
}

fn pinned_native_caller() -> EgclVal {
    use egcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    set_pinned_blocking_action(PinnedBlockingAction::Native);
    let fiber = egcl_rt::thread::current_fiber().unwrap();
    fiber.pin();
    let answer = capturing_caller();
    fiber.unpin().unwrap();
    answer
}

fn pinned_error_caller() -> EgclVal {
    use egcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    set_pinned_blocking_action(PinnedBlockingAction::Error);
    // If the policy is ignored, let the child return promptly so the test
    // reports the wrong return value instead of merely hitting a deadline.
    std::fs::write(marker("release"), b"not waiting for gc").unwrap();
    let fiber = egcl_rt::thread::current_fiber().unwrap();
    fiber.pin();
    let result = run_program(child_command());
    fiber.unpin().unwrap();
    assert!(
        matches!(result, Err(egcl_rt::EgclError::ProgramError(_))),
        "{result:?}"
    );
    assert!(
        !marker("ready").exists(),
        "policy rejection spawned a child"
    );
    egcl_rt::value::T
}

#[test]
fn native_capture_allows_peer_collection() {
    isolated("native_capture_allows_peer_collection", || {
        let peer = std::thread::spawn(collecting_peer);
        capturing_caller();
        // Collection finishes before child exit; the peer does no more Lisp work.
        peer.join().unwrap();
    });
}

#[test]
fn managed_capture_releases_its_only_carrier() {
    isolated("managed_capture_releases_its_only_carrier", || {
        run_fibers(&[capturing_caller, collecting_peer], 1);
    });
}

#[test]
fn pinned_native_capture_allows_peer_collection() {
    isolated("pinned_native_capture_allows_peer_collection", || {
        run_fibers(&[pinned_native_caller, collecting_peer], 2);
    });
}

#[test]
fn pinned_error_capture_rejects_before_starting_child() {
    isolated("pinned_error_capture_rejects_before_starting_child", || {
        run_fibers(&[pinned_error_caller], 1);
    });
}

fn waiting_caller() -> EgclVal {
    egcl_rt::rooted!(kept = egcl_rt::gc::alloc_double_float(42.0));
    let original = kept.to_raw();
    let process = launch_program(child_command()).unwrap();
    assert!(process.wait(None).unwrap().unwrap().success());
    assert_ne!(kept.to_raw(), original, "wait must permit a relocating GC");
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 42.0);
    egcl_rt::value::T
}

#[test]
fn native_process_wait_allows_peer_collection() {
    isolated("native_process_wait_allows_peer_collection", || {
        let peer = std::thread::spawn(collecting_peer);
        waiting_caller();
        peer.join().unwrap();
    });
}

#[test]
fn managed_process_wait_releases_its_only_carrier() {
    isolated("managed_process_wait_releases_its_only_carrier", || {
        run_fibers(&[waiting_caller, collecting_peer], 1);
    });
}

fn pinned_launch_error() -> EgclVal {
    use egcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    set_pinned_blocking_action(PinnedBlockingAction::Error);
    let fiber = egcl_rt::thread::current_fiber().unwrap();
    fiber.pin();
    let result = launch_program(child_command());
    fiber.unpin().unwrap();
    assert!(matches!(result, Err(egcl_rt::EgclError::ProgramError(_))));
    assert!(!marker("ready").exists(), "rejected launch spawned a child");
    egcl_rt::value::T
}

#[test]
fn pinned_launch_rejects_before_starting_child() {
    isolated("pinned_launch_rejects_before_starting_child", || {
        run_fibers(&[pinned_launch_error], 1);
    });
}

fn pinned_wait_error() -> EgclVal {
    use egcl_rt::sync::{PinnedBlockingAction, set_pinned_blocking_action};
    let process = launch_program(child_command()).unwrap();
    set_pinned_blocking_action(PinnedBlockingAction::Error);
    let fiber = egcl_rt::thread::current_fiber().unwrap();
    fiber.pin();
    let result = process.wait(None);
    fiber.unpin().unwrap();
    assert!(matches!(result, Err(egcl_rt::EgclError::ProgramError(_))));
    assert!(process.try_wait().unwrap().is_none());
    std::fs::write(marker("release"), b"release after rejected wait").unwrap();
    assert!(process.wait(None).unwrap().unwrap().success());
    egcl_rt::value::T
}

#[test]
fn pinned_wait_rejects_without_killing_child() {
    isolated("pinned_wait_rejects_without_killing_child", || {
        run_fibers(&[pinned_wait_error], 1);
    });
}

static SHARED_PROCESS: std::sync::OnceLock<egcl_stdlib::process::Process> =
    std::sync::OnceLock::new();

fn shared_waiter() -> EgclVal {
    let process = SHARED_PROCESS.get().unwrap();
    assert!(process.wait(None).unwrap().unwrap().success());
    egcl_rt::value::T
}

#[test]
fn multiple_fibers_can_wait_for_the_same_process() {
    isolated("multiple_fibers_can_wait_for_the_same_process", || {
        assert!(
            SHARED_PROCESS
                .set(launch_program(child_command()).unwrap())
                .is_ok()
        );
        run_fibers(&[shared_waiter, shared_waiter, collecting_peer], 1);
    });
}
