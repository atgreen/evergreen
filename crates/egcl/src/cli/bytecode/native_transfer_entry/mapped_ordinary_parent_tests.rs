// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::bytecode::native_transfer_tests::ordinary_t2;
use egcl_rt::Collector;

const PARENT_MARKER: i64 = 716_239;

struct ParentProbe {
    child_owner: usize,
    observed: bool,
    before: u64,
    after: u64,
}

thread_local! {
    static PARENT_PROBE: Cell<*mut ParentProbe> = const { Cell::new(std::ptr::null_mut()) };
}

fn collect_with_only_the_suspended_parent_holding_the_marker() {
    let probe = PARENT_PROBE.with(Cell::get);
    if probe.is_null() || unsafe { (*probe).observed } {
        return;
    }
    if !root_publication::published_frame_has_owner_for_test(unsafe { (*probe).child_owner }) {
        return;
    }
    // The child owner above proves a real native helper publication. Search
    // every boundary and reject managed-stack or MV copies of the parent cons.
    let Some(slot) = root_publication::native_only_root_for_test(PARENT_MARKER) else {
        return;
    };
    unsafe {
        (*probe).observed = true;
        (*probe).before = slot.read().to_raw();
    }
    // Keep only address bits in the test: the older native frame, reached
    // through its still-linked publication, must keep the object alive.
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    unsafe {
        (*probe).after = slot.read().to_raw();
    }
}

#[test]
fn mapped_parent_native_only_root_moves_during_an_ordinary_bridge_call() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun bridge-parent-poll-child (limit)
           (if (= (logxor limit limit limit) limit) 42 0))
         (defun bridge-parent-root-caller (&key (limit (1+ 1999)))
           (let ((marker (cons 716239 nil)))
             (values nil)
             (bridge-parent-poll-child limit)
             marker))",
        &mut env,
    )
    .unwrap();
    let (child_symbol, child) = ordinary_t2::install(
        "BRIDGE-PARENT-POLL-CHILD",
        "(limit)",
        "((if (= (logxor limit limit limit) limit) 42 0))",
        &mut env,
    );
    let (parent_symbol, parent) = ordinary_t2::install(
        "BRIDGE-PARENT-ROOT-CALLER",
        "(&key (limit (1+ 1999)))",
        "((let ((marker (cons 716239 nil))) (values nil) (bridge-parent-poll-child limit) marker))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(child_code) = &child._storage else {
        panic!("ordinary child owner")
    };
    let NativeCodeStorage::Mapped(parent_code) = &parent._storage else {
        panic!("mapped parent owner")
    };
    assert!(child_code.ordinary_entry.is_some());
    assert!(parent_code.ordinary_entry.is_none());
    // Resolve cold cells before the collection probe; the observed invocation
    // must use the installed ordinary child, not interpreter fallback.
    super::super::run_native(&parent, parent_symbol, &[], &mut env).unwrap();
    env.clear_mv();
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    let mut probe = ParentProbe {
        child_owner: Rc::as_ptr(child_code) as usize,
        observed: false,
        before: 0,
        after: 0,
    };
    mapped_ordinary::take_entries_for_test();
    PARENT_PROBE.with(|slot| slot.set(&mut probe));
    let previous = root_publication::PROFILE_OBSERVE.with(|hook| {
        hook.replace(Some(
            collect_with_only_the_suspended_parent_holding_the_marker,
        ))
    });
    let result = super::super::run_native(&parent, parent_symbol, &[], &mut env);
    root_publication::PROFILE_OBSERVE.with(|hook| hook.set(previous));
    PARENT_PROBE.with(|slot| slot.set(std::ptr::null_mut()));
    let entries = mapped_ordinary::take_entries_for_test();
    egcl_rt::rooted!(result = result.unwrap());
    for symbol in [child_symbol, parent_symbol] {
        egcl_rt::symbols::set_symbol_function(symbol, NIL);
        super::super::registry_remove(symbol);
    }
    assert!(entries > 0, "the call must use the ordinary bridge");
    assert!(
        probe.observed,
        "the child must publish with the parent's only root live"
    );
    assert_ne!(
        probe.before, probe.after,
        "the probe must actually relocate the parent value"
    );
    assert_eq!(result.to_raw(), probe.after);
    assert_eq!(
        crate::cli::cp(*result),
        (EgclVal::from_fixnum(PARENT_MARKER), NIL)
    );
}
