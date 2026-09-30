// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::native_transfer_entry::{TransferCode, take_native_cleanup_count};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_CASE: AtomicUsize = AtomicUsize::new(0);
static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);

fn cleanup_fiber() -> EgclVal {
    let case = NEXT_CASE.fetch_add(2, Ordering::Relaxed);
    let starting_carrier = egcl_rt::current_thread_id().0;
    // Match thread_entry_runner: workers share initialized classes/packages.
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun fiber-protected-answer (x)
           (if (oddp (car x))
             (throw :fiber-cleanup-exit (values x (list :original)))
             (values x (list :original))))
         (defun fiber-sleeping-cleanup (x)
           (egcl::%native-fiber :sleep 0.1d0)
           (%force-minor-gc-for-test)
           (if (oddp (car x))
             (throw :fiber-cleanup-exit (values x (list :replacement)))
             (values :discard (list :discard))))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((unwind-protect (fiber-protected-answer x) (fiber-sleeping-cleanup x)))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function("FIBER-NATIVE-CLEANUP", *params, *forms, &env, false, false).unwrap(),
    );
    let code = TransferCode::compile(body).expect("fiber native cleanup");
    let token = super::super::super::next_control_token("FIBER-CLEANUP-EXIT");
    let tag = reader::read_from_string(":fiber-cleanup-exit").unwrap().0;
    env.catch_stack.push((tag, token.clone()));
    egcl_rt::rooted!(
        args = vec![super::super::super::arena_cons(
            EgclVal::from_fixnum(case as i64),
            NIL
        )]
    );
    let address = args[0].to_raw();
    let frame = egcl_rt::current_stack().fp();
    let fiber = egcl_rt::current_fiber_id();
    let depth = NATIVE_DEPTH.with(|slot| slot.get());
    take_native_cleanup_count();
    egcl_rt::rooted!(result = code.run(&args, &mut env));
    if egcl_rt::current_thread_id().0 != starting_carrier {
        MIGRATIONS.fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(
        take_native_cleanup_count(),
        case % 2,
        "odd fibers suspend inside exceptionally entered native cleanup"
    );
    assert_ne!(
        args[0].to_raw(),
        address,
        "cleanup collected its saved answer"
    );
    assert_eq!(egcl_rt::current_fiber_id(), fiber);
    assert_eq!(egcl_rt::current_stack().fp(), frame);
    assert_eq!(NATIVE_DEPTH.with(|slot| slot.get()), depth);
    assert!(egcl_rt::native_transfer::current_segment().is_null());
    if case % 2 == 0 {
        assert_eq!(result.as_ref().unwrap(), &args[0]);
    } else {
        assert!(
            matches!(&*result, Err(EgclError::Internal(t)) if t == &token),
            "{:?}",
            &*result
        );
        assert_eq!(
            super::super::super::take_control_mv(&token, &mut env),
            args[0]
        );
    }
    assert!(env.mv_active && env.mv.len() == 2);
    let expected = reader::read_from_string(if case % 2 == 0 {
        ":original"
    } else {
        ":replacement"
    })
    .unwrap()
    .0;
    assert_eq!(env.mv[0], args[0]);
    assert_eq!(super::super::super::cp(env.mv[1]).0, expected);
    assert_eq!(
        super::super::super::cp(args[0]).0,
        EgclVal::from_fixnum(case as i64)
    );
    env.catch_stack.pop();
    EgclVal::from_fixnum(1)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_cleanup_values_and_transfers_survive_fiber_suspension() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(egcl_rt::native_transfer::is_supported());
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    for num_workers in [1, 4] {
        for throwing in [false, true] {
            // Homogeneous groups prove the external collection sees at least two
            // pending native transfers, rather than merely two normal cleanups.
            NEXT_CASE.store(usize::from(throwing), Ordering::Relaxed);
            let group =
                egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers }).unwrap();
            let mut fibers = Vec::new();
            for _ in 0..8 {
                let entry =
                    unsafe { EgclVal::from_function_ptr(cleanup_fiber as *const () as *mut u8) };
                let fiber = egcl_rt::thread::make_fiber(entry).unwrap();
                fibers.push(fiber);
                group.submit(fiber).unwrap();
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while fibers
                .iter()
                .filter(|&&id| {
                    matches!(
                        egcl_rt::thread::fiber_state(id),
                        Some(
                            egcl_rt::thread::FiberState::Waiting
                                | egcl_rt::thread::FiberState::Blocked
                        )
                    )
                })
                .count()
                < 2
            {
                egcl_rt::poll_safepoint();
                assert!(
                    std::time::Instant::now() < deadline,
                    "two cleanup continuations must suspend together"
                );
                std::thread::yield_now();
            }
            // Collect from outside the fibers while their native frames and saved
            // multiple values are suspended on distinct stacks.
            HeapCollector::new().minor_gc().unwrap();
            assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(1); 8]);
            if num_workers == 4 {
                assert!(
                    MIGRATIONS.load(Ordering::Relaxed) > 0,
                    "at least one suspended native segment must resume on another carrier"
                );
                MIGRATIONS.store(0, Ordering::Relaxed);
            }
        }
    }
}
