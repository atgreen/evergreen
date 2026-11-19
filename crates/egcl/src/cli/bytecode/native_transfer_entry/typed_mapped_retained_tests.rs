// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

struct RetainedProbe {
    env: *mut Env,
    payload: *mut EgclVal,
    old: std::rc::Weak<TransferCode>,
    old_owner: usize,
    new: Option<Rc<NativeCode>>,
    old_calls: usize,
    new_calls: usize,
    before: u64,
    after: u64,
}
thread_local! {
    static RETAINED: Cell<*mut RetainedProbe> = const { Cell::new(std::ptr::null_mut()) };
}

fn retained_observe() {
    let pointer = RETAINED.with(Cell::get);
    assert!(!pointer.is_null());
    // The test owns and roots Env and payload outside run_parent. This probe
    // retains only their stable Rust homes, numeric PCs, and native owners.
    let probe = unsafe { &mut *pointer };
    if let Some(new) = &probe.new {
        assert!(root_publication::published_frame_has_owner_for_test(
            Rc::as_ptr(owner(new)) as usize
        ));
        probe.new_calls += 1;
        return;
    }
    assert_eq!(probe.old_calls, 0);
    assert!(root_publication::published_frame_has_owner_for_test(
        probe.old_owner
    ));
    probe.old_calls += 1;

    // Move the fresh argument before compiling the replacement can pin or
    // promote its region. The active old frame must still return this value.
    probe.before = unsafe { (*probe.payload).to_raw() };
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    probe.after = unsafe { (*probe.payload).to_raw() };
    assert_ne!(probe.before, probe.after, "the argument actually relocates");

    let (_, new) = install(
        unsafe { &mut *probe.env },
        "TYPED-RETAINED-CHILD",
        "(x payload)",
        "(declare (type fixnum x)) (typed-retained-observe) (typed-retained-values payload :new (+ x 100))",
    );
    assert!(new.is_t2 && owner(&new).ordinary_checks_params);
    assert!(owner(&new).ordinary_descriptor(0).is_some());
    assert!(owner(&new).lean_root_entry().is_none());
    assert_ne!(Rc::as_ptr(owner(&new)) as usize, probe.old_owner);
    // Do not keep a test-owned strong lease on the old executable across GC.
    assert!(probe.old.upgrade().is_some());
    egcl_rt::gc::full_gc().unwrap();
    assert!(
        probe.old.upgrade().is_some(),
        "the active old code survives"
    );
    assert!(root_publication::published_frame_has_owner_for_test(
        probe.old_owner
    ));
    ordinary_t2::expect_entry(&new);
    probe.new = Some(new);
}

struct RetainedObserving {
    previous: Option<fn()>,
    probe: *mut RetainedProbe,
}
impl RetainedObserving {
    fn install(probe: &mut RetainedProbe) -> Self {
        let previous_probe = RETAINED.with(|slot| slot.replace(probe));
        let previous =
            ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(retained_observe)));
        Self {
            previous,
            probe: previous_probe,
        }
    }
}
impl Drop for RetainedObserving {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.previous));
        RETAINED.with(|slot| slot.set(self.probe));
    }
}

#[test]
#[ignore = "actual native retained definition; run typed ordinary ON and OFF separately"]
fn typed_named_redefinition_retains_active_code_and_checks_the_replacement() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _ = reader::intern_symbol("%ORDINARY-T2-ENTRY-FOR-TEST");
    crate::cli::read_eval_all_env(
        "(setq *typed-retained-effects* 0 *typed-retained-tags* nil)
         (defun typed-retained-observe () (%ordinary-t2-entry-for-test))
         (defun typed-retained-values (payload tag value)
           (setq *typed-retained-effects* (+ *typed-retained-effects* 1))
           (setq *typed-retained-tags* (cons tag *typed-retained-tags*))
           (values payload tag value))",
        &mut env,
    )
    .unwrap();
    let _pins = ordinary_t2::RecursiveFixturePins::new(&[
        "TYPED-RETAINED-OBSERVE",
        "TYPED-RETAINED-VALUES",
    ]);
    let (symbol, old) = install(
        &mut env,
        "TYPED-RETAINED-CHILD",
        "(x payload)",
        "(declare (type fixnum x)) (typed-retained-observe) (typed-retained-values payload :old x)",
    );
    assert!(old.is_t2 && owner(&old).ordinary_checks_params);
    assert!(owner(&old).ordinary_descriptor(0).is_some());
    egcl_rt::rooted!(old_function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let (parent_symbol, parent) = install(
        &mut env,
        "TYPED-RETAINED-PARENT",
        "(x payload &key (seed (1+ 0)) &aux (u 0))",
        "(declare (notinline typed-retained-child))
         (typed-retained-child x payload) (typed-retained-child x payload)
         (typed-retained-child x payload) (typed-retained-child x payload)",
    );
    let _native_env = NativeEnv::install(&mut env);
    warm(symbol, &old, &[EgclVal::from_fixnum(1), NIL]);
    crate::cli::read_eval_all_env(
        "(setq *typed-retained-effects* 0 *typed-retained-tags* nil)",
        &mut env,
    )
    .unwrap();
    let old_count = egcl_rt::function::invoke_count(*old_function);
    let weak = Rc::downgrade(owner(&old));
    let old_owner = Rc::as_ptr(owner(&old)) as usize;
    drop(old);
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(73), NIL));
    let mut probe = RetainedProbe {
        env: &mut env,
        payload: &mut *payload,
        old: weak,
        old_owner,
        new: None,
        old_calls: 0,
        new_calls: 0,
        before: 0,
        after: 0,
    };
    let _observing = RetainedObserving::install(&mut probe);
    reset(symbol);
    let fp = egcl_rt::current_stack().fp();
    let sp = egcl_rt::current_stack().sp();
    assert_eq!(
        run_parent(
            parent_symbol,
            &parent,
            &[EgclVal::from_fixnum(31), *payload],
            &mut env
        )
        .unwrap(),
        *payload
    );
    assert_eq!((probe.old_calls, probe.new_calls), (1, 3));
    assert_ne!(probe.before, probe.after);
    assert!(env.mv_active);
    let new_tag = reader::intern_keyword_symbol("NEW");
    let old_tag = reader::intern_keyword_symbol("OLD");
    assert_eq!(env.mv, vec![*payload, new_tag, EgclVal::from_fixnum(131)]);
    assert_eq!(egcl_rt::function::invoke_count(*old_function), old_count);
    assert_ne!(
        egcl_rt::symbols::symbol_function(symbol).unwrap(),
        *old_function
    );
    assert_eq!(
        crate::cli::read_eval_all_env("*typed-retained-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(4)
    );
    egcl_rt::rooted!(
        tags = crate::cli::read_eval_all_env("*typed-retained-tags*", &mut env).unwrap()
    );
    egcl_rt::rooted!(tag_values = crate::cli::list_to_vec(*tags));
    assert_eq!(tag_values.len(), 4);
    assert_eq!(&tag_values[..3], &[new_tag; 3]);
    assert_eq!(tag_values[3], old_tag);
    if enabled() {
        assert!(mapped_ordinary::take_entries_for_test() >= 2);
        assert!(mapped_generated::take_fast_entries_for_test() >= 1);
    } else {
        assert_eq!(mapped_ordinary::take_entries_for_test(), 0);
        assert_eq!(mapped_generated::take_fast_entries_for_test(), 0);
    }
    // The next named call must validate the NEW entry before observing a body
    // marker or executing the producer, for both canonical and generated paths.
    egcl_rt::rooted!(bad = crate::cli::arena_cons(EgclVal::from_fixnum(99), NIL));
    let error = run_parent(parent_symbol, &parent, &[*bad, *payload], &mut env).unwrap_err();
    assert_type_error(error, *bad, &env);
    assert_eq!((probe.old_calls, probe.new_calls), (1, 3));
    assert_eq!(
        crate::cli::read_eval_all_env("*typed-retained-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(4)
    );
    assert_eq!(egcl_rt::current_stack().fp(), fp);
    assert_eq!(egcl_rt::current_stack().sp(), sp);
    assert!(!native_error_pending());
}
