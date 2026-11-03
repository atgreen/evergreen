// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::activation_census::{self as census, AdmissionCaller, AdmissionReason as Reason};
use crate::cli::bytecode::native_transfer_tests::ordinary_t2;

fn owner(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("real native code")
    };
    code
}
struct NativeEnv(*mut Env);
impl NativeEnv {
    fn install(env: &mut Env) -> Self {
        Self(NATIVE_ENV.with(|slot| slot.replace(env)))
    }
}
impl Drop for NativeEnv {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.0));
    }
}
fn install(env: &mut Env, name: &str, params: &str, forms: &str) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} {params} {forms})"), env).unwrap();
    ordinary_t2::install(name, params, &format!("({forms})"), env)
}

#[test]
#[ignore = "requires both census flags and native request publication"]
fn actual_fixed_and_optional_requests_report_exact_caller_and_preserve_native_routes() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(census::origins_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun admission-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["ADMISSION-OBSERVE"]);
    for optional in [false, true] {
        let name = if optional {
            "ADMISSION-OPTIONAL"
        } else {
            "ADMISSION-FIXED"
        };
        let params = if optional { "(x &optional y)" } else { "(x y)" };
        let (symbol, child) = install(&mut env, name, params, "(admission-observe) x");
        assert!(owner(&child).ordinary_entry.is_some());
        assert_eq!(owner(&child).lean_root_entry().is_none(), optional);
        let (parent_symbol, parent) = install(
            &mut env,
            "ADMISSION-PARENT",
            "(x &key (seed (1+ 0)) &aux (u 0))",
            &format!("(declare (notinline {name})) ({name} x 7)"),
        );
        assert!(owner(&parent).ordinary_entry.is_none());
        let caller = Some(AdmissionCaller {
            owner: Some(parent_symbol),
            ordinary_entry: false,
        });
        // Prime real linkage only after parent compilation invalidation.
        let _env = NativeEnv::install(&mut env);
        ordinary_t2::expect_entry(&child);
        super::super::call_table::ordinary_call(symbol, &[EgclVal::from_fixnum(19), T]).unwrap();
        let reason = if optional {
            Reason::SelectorNoLeanEntry
        } else {
            Reason::SelectorAccepted
        };
        let before = census::admission_count_for_test(reason, symbol, caller);
        let meta_before = census::entry_metadata_counts_for_test(symbol)
            .iter()
            .map(|x| x.2)
            .sum::<u64>();
        let eligible_before =
            census::admission_count_for_test(Reason::BridgeEligible, symbol, caller);
        mapped_ordinary::take_entries_for_test();
        start_activation_counts_for_test(symbol);
        ordinary_t2::expect_entry(&child);
        assert_eq!(
            owner(&parent)
                .run_callable(
                    egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
                    &[EgclVal::from_fixnum(581)],
                    &mut env
                )
                .unwrap(),
            EgclVal::from_fixnum(581)
        );
        assert!(
            ordinary_t2::observed_entry(),
            "exact native callee physical owner, not tier metadata"
        );
        assert_eq!(
            census::admission_count_for_test(reason, symbol, caller),
            before + 1,
            "caller must resolve from this checked request's own PC/directory"
        );
        assert_eq!(
            mapped_ordinary::take_entries_for_test(),
            usize::from(!optional)
        );
        assert_eq!(
            take_activation_counts_for_test(),
            if optional { (1, 1) } else { (0, 0) }
        );
        let metadata = census::entry_metadata_counts_for_test(symbol);
        assert_eq!(
            metadata.iter().map(|x| x.2).sum::<u64>(),
            meta_before + u64::from(optional)
        );
        if optional {
            assert!(
                metadata
                    .iter()
                    .any(|(meta, site, _)| *meta == owner(&child).census_entry()
                        && *site == census::PrepareSite::GeneralRoot)
            );
        } else {
            assert_eq!(
                census::admission_count_for_test(Reason::BridgeEligible, symbol, caller),
                eligible_before + 1
            );
        }
    }
}

#[test]
#[ignore = "requires both census flags and typed native code"]
fn typed_selector_reason_and_installed_parameter_metadata_preserve_native_checks() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(census::origins_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun admission-typed-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["ADMISSION-TYPED-OBSERVE"]);
    let (symbol, child) = install(
        &mut env,
        "ADMISSION-TYPED",
        "(x)",
        "(declare (type fixnum x)) (admission-typed-observe) x",
    );
    assert!(owner(&child).ordinary_entry.is_some());
    assert!(!owner(&child).census_entry().params_any);
    assert!(owner(&child).lean_root_entry().is_none());
    let _env = NativeEnv::install(&mut env);
    ordinary_t2::expect_entry(&child);
    super::super::call_table::ordinary_call(symbol, &[EgclVal::from_fixnum(19)]).unwrap();
    let cell = super::super::call_table::resolve(symbol).unwrap();
    // Unit exercise of the exact selector guard. Caller deliberately unknown:
    // this is not evidence of a generated request or fabricated physical owner.
    let before = census::admission_count_for_test(Reason::SelectorNoLeanEntry, symbol, None);
    {
        let _request = census::admission_scope(|| Some(None));
        assert!(
            unsafe { super::super::call_table::lean_native_target(Arc::as_ptr(&cell) as u64, 1) }
                .is_none()
        );
    }
    assert_eq!(
        census::admission_count_for_test(Reason::SelectorNoLeanEntry, symbol, None),
        before + 1
    );
    ordinary_t2::expect_entry(&child);
    let before = census::entry_metadata_counts_for_test(symbol)
        .iter()
        .map(|x| x.2)
        .sum::<u64>();
    assert_eq!(
        owner(&child)
            .run_callable(
                egcl_rt::symbols::symbol_function(symbol).unwrap(),
                &[EgclVal::from_fixnum(582)],
                &mut env
            )
            .unwrap(),
        EgclVal::from_fixnum(582)
    );
    assert!(ordinary_t2::observed_entry());
    let metadata = census::entry_metadata_counts_for_test(symbol);
    assert_eq!(metadata.iter().map(|x| x.2).sum::<u64>(), before + 1);
    assert!(
        metadata
            .iter()
            .any(|(entry, site, _)| *entry == owner(&child).census_entry()
                && *site == census::PrepareSite::GeneralRoot)
    );
    // Real wrong-type call must retain its error, irrespective of capability.
    assert!(
        owner(&child)
            .run_callable(
                egcl_rt::symbols::symbol_function(symbol).unwrap(),
                &[NIL],
                &mut env
            )
            .is_err()
    );
}

thread_local! {
    // Borrowed stack pointers valid only during the observed native run.
    static ORDINARY_PARENT: Cell<(usize,usize,bool)> = const {Cell::new((0,0,false))};
}
fn observe_ordinary_parent_rejection() {
    let (parent, child, done) = ORDINARY_PARENT.with(Cell::get);
    if parent == 0
        || done
        || root_publication::active_anchor_for_test().map(|x| x.1) != Some(parent)
    {
        return;
    }
    ORDINARY_PARENT.with(|slot| slot.set((parent, child, true)));
    let child = unsafe { &*(child as *const Rc<TransferCode>) };
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!env.is_null());
    // No checked request is manufactured. Unknown caller in this guard-unit
    // scope is intentional; the publication independently proves native parent.
    let _request = census::admission_scope(|| Some(None));
    let function = egcl_rt::symbols::symbol_function(child.installed_symbol.unwrap()).unwrap();
    assert!(
        unsafe {
            mapped_ordinary::call(
                child,
                child.lean_root_entry().unwrap(),
                function,
                &[EgclVal::from_fixnum(3)],
                &mut *env,
                None,
            )
        }
        .is_none()
    );
}
struct ParentHook(Option<fn()>);
impl Drop for ParentHook {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.0));
        ORDINARY_PARENT.with(|slot| slot.set((0, 0, false)));
    }
}
#[test]
#[ignore = "requires both census flags and actual ordinary parent publication"]
fn ordinary_parent_context_guard_is_reported_without_forging_mapped_ingress() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(census::origins_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun admission-parent-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["ADMISSION-PARENT-OBSERVE"]);
    let (symbol, child) = install(&mut env, "ADMISSION-PARENT-CHILD", "(x)", "x");
    let (parent_symbol, parent) = install(
        &mut env,
        "ADMISSION-ORDINARY-PARENT",
        "(x)",
        "(admission-parent-observe) x",
    );
    assert!(owner(&parent).ordinary_entry.is_some());
    let child_code = owner(&child);
    ORDINARY_PARENT.with(|slot| {
        slot.set((
            Rc::as_ptr(owner(&parent)) as usize,
            std::ptr::from_ref(child_code) as usize,
            false,
        ))
    });
    let _hook = ParentHook(
        root_publication::PROFILE_OBSERVE
            .with(|slot| slot.replace(Some(observe_ordinary_parent_rejection))),
    );
    ordinary_t2::expect_entry(&parent);
    mapped_ordinary::take_entries_for_test();
    let before =
        census::admission_count_for_test(Reason::BridgeContextHasOrdinaryEntry, symbol, None);
    assert_eq!(
        owner(&parent)
            .run_callable(
                egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
                &[T],
                &mut env
            )
            .unwrap(),
        T
    );
    assert!(ordinary_t2::observed_entry());
    assert!(ORDINARY_PARENT.with(|slot| slot.get().2));
    assert_eq!(mapped_ordinary::take_entries_for_test(), 0);
    assert_eq!(
        census::admission_count_for_test(Reason::BridgeContextHasOrdinaryEntry, symbol, None),
        before + 1
    );
}
