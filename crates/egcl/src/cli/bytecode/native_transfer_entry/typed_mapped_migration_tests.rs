// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Keep the established external collector and forced-carrier controller.
//! Only the middle's fifth, checked scalar argument is new.
use super::*;

fn typed_fiber() -> EgclVal {
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (middle_symbol, middle) = with_ordinary_non_tail_emission_for_test(|| {
        install_mapped(
            &mut env,
            "MF-OBJECT-FIBER-MIDDLE",
            "(a b c d n)",
            "(declare (type fixnum n))
         (if (= n 1) (mf-object-fiber-terminal b d a c)
                     (mf-object-fiber-terminal a b c d))",
        )
    });
    assert!(middle.is_t2 && owner(&middle).ordinary_checks_params);
    assert!(owner(&middle).lean_root_entry().is_none());
    let descriptor = owner(&middle).ordinary_descriptor(0).unwrap();
    assert_eq!(descriptor.arity, 5);
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(middle_symbol).unwrap());
    assert!(
        owner(&middle)
            .checked_typed_mapped_entry(*function)
            .is_some()
    );
    let middle_owner = Rc::as_ptr(owner(&middle)) as usize;
    let (symbol, outer) = install_mapped(
        &mut env,
        "MF-OBJECT-FIBER-OUTER",
        "(f a b c d &key (seed (1+ 0)))",
        "(progn (funcall f 1 2 3 4 1)
           (multiple-value-prog1 (funcall f d c b a 1)
             (funcall f 5 6 7 8 1)))",
    );
    assert!(owner(&outer).ordinary_entry.is_none());
    let warm = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    for _ in 0..2 {
        assert_eq!(
            run_mapped(
                symbol,
                &outer,
                &[*function, warm[0], warm[1], warm[2], warm[3]],
                &mut env,
            )
            .unwrap(),
            warm[1]
        );
    }
    let storage = mapped_ordinary::idle_storage_for_test(owner(&middle));
    assert_eq!(storage.0, 1);
    drop(middle);
    egcl_rt::rooted!(arguments = Vec::new());
    for index in 0..4 {
        arguments.push(crate::cli::arena_cons(
            EgclVal::from_fixnum(200 + index),
            NIL,
        ));
    }
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    egcl_rt::poll_safepoint();
    assert_eq!(crate::cli::bytecode::native_loop_should_exit(), 0);
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    mapped_ordinary::take_storage_births_for_test();
    take_lean_root_entries_for_test();
    start_activation_counts_for_test(middle_symbol);
    let mut probe = Probe {
        owner: middle_owner,
        tier: 2,
        third_entry: 3,
        observed: false,
        observed_after_resume: false,
        control: 0,
    };
    let _observing = observing(&mut probe);
    let origin_before = crate::cli::activation_census::scope_snapshot_for_test();
    let origin_scope = crate::cli::activation_census::migration_scope_for_test();
    let origin_active = crate::cli::activation_census::scope_snapshot_for_test();
    egcl_rt::rooted!(
        result = run_mapped(
            symbol,
            &outer,
            &[
                *function,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3]
            ],
            &mut env,
        )
        .unwrap()
    );
    let origin = PARKED_ORIGIN.load(Ordering::Acquire);
    assert_ne!(origin, 0);
    assert_ne!(egcl_rt::current_thread_id().0, origin);
    assert_eq!(mapped_ordinary::take_entries_for_test(), 3);
    assert_eq!(take_activation_counts_for_test(), (0, 0));
    assert_eq!(take_lean_root_entries_for_test(), 0);
    assert_eq!(mapped_ordinary::take_storage_births_for_test(), 0);
    assert!(probe.observed && probe.observed_after_resume);
    assert_eq!(*result, arguments[2]);
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 4);
    for (value, index) in env.mv.iter().zip([2, 0, 3, 1]) {
        assert_eq!(*value, arguments[index]);
        assert_eq!(
            crate::cli::cp(*value),
            (EgclVal::from_fixnum(200 + index as i64), NIL)
        );
    }
    let current = NATIVE_REGISTRY
        .with(|registry| registry.borrow().get(&middle_symbol).cloned())
        .unwrap();
    assert_eq!(Rc::as_ptr(owner(&current)) as usize, middle_owner);
    assert_eq!(
        mapped_ordinary::idle_storage_for_test(owner(&current)),
        storage
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    assert_eq!(unsafe { &*execution }.active_native_control(), control);
    assert_eq!(
        crate::cli::activation_census::scope_snapshot_for_test(),
        origin_active
    );
    drop(origin_scope);
    assert_eq!(
        crate::cli::activation_census::scope_snapshot_for_test(),
        origin_before
    );
    EgclVal::from_fixnum(1)
}

#[test]
#[ignore = "requires typed ordinary ON and actual migration/external moving GC"]
fn typed_published_middle_survives_external_gc_and_actual_carrier_migration() {
    run_migration(typed_fiber, 2);
}
