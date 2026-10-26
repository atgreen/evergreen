// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A real generated loop poll must refresh the vector used by the fast clone.

use super::*;

thread_local! {
    static VECTOR: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static BEFORE: Cell<u64> = const { Cell::new(0) };
    static REQUEST: Cell<u64> = const { Cell::new(0) };
    static MOVED: Cell<bool> = const { Cell::new(false) };
}

fn post_request() {
    egcl_rt::runtime::mark_process_signal_activity();
    REQUEST.with(|request| {
        request.set(
            unsafe { &*egcl_rt::runtime::process_signal_activity_address() }
                .load(std::sync::atomic::Ordering::Acquire),
        );
    });
}

fn request_after_admission() {
    post_request();
    native_transfer_entry::root_publication::OBSERVE
        .with(|hook| hook.set(Some(collect_at_loop_poll)));
    native_transfer_entry::root_publication::OBSERVE_AFTER
        .with(|hook| hook.set(Some(repost_after_early_poll)));
}

fn repost_after_early_poll() {
    if !MOVED.with(Cell::get) {
        post_request();
    }
}

fn collect_at_loop_poll() {
    let request = REQUEST.with(Cell::get);
    assert!(
        egcl_rt::runtime::current_seen_signal_activity().is_none_or(|seen| seen < request),
        "the request posted after entry must still be pending at the generated poll"
    );
    let pointer = VECTOR.with(Cell::get);
    assert!(!pointer.is_null());
    // This points into the rooted argument Vec, not an unregistered value copy.
    assert_eq!(unsafe { (*pointer).to_raw() }, BEFORE.with(Cell::get));
    if egcl_stdlib::sequences::aref(unsafe { *pointer }, 0).unwrap() == EgclVal::from_fixnum(-1) {
        // Entry/straight polls may precede the loop. Repost after their real
        // runtime drain so collection is delayed until a generated backedge.
        return;
    }
    assert_eq!(
        egcl_stdlib::sequences::aref(unsafe { *pointer }, 0).unwrap(),
        EgclVal::from_fixnum(0),
        "collection must follow the loop's first store"
    );
    egcl_rt::collect_t0_minor().unwrap();
    assert_ne!(
        unsafe { (*pointer).to_raw() },
        BEFORE.with(Cell::get),
        "the vector must actually move at the generated poll"
    );
    MOVED.with(|moved| moved.set(true));
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
    native_transfer_entry::root_publication::OBSERVE_AFTER.with(|hook| hook.set(None));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_vector_range_loop_reloads_relocated_vector() {
    const CHILD: &str = "EGCL_TEST_VECTOR_RANGE_RELOCATION_CHILD";
    const TEST: &str = "cli::bytecode::native_transfer_tests::ordinary_t2::vector_range_relocation::ordinary_vector_range_loop_reloads_relocated_vector";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_DISABLE_VECTOR_RANGES")
            .output()
            .expect("run isolated vector range relocation fixture");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "child failed: {}\n{stdout}\n{stderr}",
            output.status
        );
        assert!(
            stdout.contains("VECTOR-RANGE-RELOCATED"),
            "fixture did not run: {stdout}\n{stderr}"
        );
        assert!(
            stderr.contains("VECTOR-RANGE-RELOCATION: vector range versions: 1 stores=1"),
            "the fast loop clone must really be emitted: {stderr}"
        );
        return;
    }
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = install(
        "VECTOR-RANGE-RELOCATION",
        "(array n)",
        "((declare (type fixnum n)) (let ((i 0)) (tagbody loop (if (>= i n) (go done)) (setf (aref array i) i) (setq i (1+ i)) (go loop) done)) array)",
        &mut env,
    );
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(
        mapped.ordinary_entry.is_some(),
        "the target must admit the ordinary native ABI"
    );
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        array = egcl_stdlib::sequences::build_filled_simple_vector(8, EgclVal::from_fixnum(-1))
    );
    egcl_rt::rooted!(args = vec![*array, EgclVal::from_fixnum(8)]);
    BEFORE.with(|before| before.set(args[0].to_raw()));
    VECTOR.with(|pointer| pointer.set(args.as_ptr()));
    MOVED.with(|moved| moved.set(false));
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    native_transfer_entry::reset_poll_paths_for_test();
    let deopts = mapped.deopt_count();
    struct ClearObserver;
    impl Drop for ClearObserver {
        fn drop(&mut self) {
            native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
            native_transfer_entry::root_publication::OBSERVE_AFTER.with(|hook| hook.set(None));
            VECTOR.with(|pointer| pointer.set(std::ptr::null()));
        }
    }
    let _clear = ClearObserver;
    egcl_rt::rooted!(
        result = native_transfer_entry::with_ordinary_entry_hook_for_test(
            request_after_admission,
            || run_native(&code, symbol, &args, &mut env)
        )
        .unwrap()
    );
    assert!(MOVED.with(Cell::get));
    assert_ne!(args[0].to_raw(), BEFORE.with(Cell::get));
    assert_eq!(
        *result, args[0],
        "native return must use the updated object"
    );
    assert_eq!(mapped.deopt_count(), deopts);
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
        0
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_named_entries_for_test(),
        0
    );
    assert!(
        native_transfer_entry::take_poll_paths_for_test().1 > 0,
        "the original generated poll must remain in the fast clone"
    );
    for index in 0..8 {
        assert_eq!(
            egcl_stdlib::sequences::aref(*result, index).unwrap(),
            EgclVal::from_fixnum(index as i64)
        );
    }
    println!("VECTOR-RANGE-RELOCATED");
}
