use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use torcl_rt::thread::{
    all_thread_ids, current_thread_id, interrupt_thread, join_thread, make_thread,
    set_thread_entry_runner,
};
use torcl_rt::value::{T, TorclVal};

fn panic_entry() -> TorclVal {
    // A pending interrupt must not turn a panic into successful completion.
    interrupt_thread(current_thread_id(), T).unwrap();
    panic!("intentional native worker panic");
}

#[test]
fn native_worker_panic_completes_join_and_releases_registry_entry() {
    const CHILD: &str = "TORCL_TEST_NATIVE_PANIC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_worker_panic_completes_join_and_releases_registry_entry",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "panic regression child failed: {status}");
                return;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("JOIN-THREAD hung after its native worker panicked");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    torcl_rt::gc::ensure_heap_initialized();
    // Cover both supported entry paths: native code and the host Lisp runner.
    set_thread_entry_runner(|_| std::panic::panic_any(42_u32));
    let native = unsafe { TorclVal::from_function_ptr(panic_entry as *const () as *mut u8) };
    for entry in [native, TorclVal::from_fixnum(42)] {
        let id = make_thread(entry).unwrap();
        let error = join_thread(id).expect_err("a panicked worker must not return a value");
        assert!(
            matches!(error, torcl_rt::TorclError::Internal(ref message)
                if message == &format!("native thread {} panicked", id.0)),
            "unexpected join error: {error:?}"
        );
        assert!(!all_thread_ids().contains(&id), "joined worker leaked");
        // Retirement must also release the worker from GC participation.
        torcl_rt::gc::collect_t0_minor().unwrap();
        assert_eq!(join_thread(make_thread(T).unwrap()).unwrap(), T);
    }
}
