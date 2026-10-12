// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::collections::HashMap;

#[derive(Default)]
struct Probe {
    observed: bool,
    duplicate_visits: usize,
    homes: Vec<usize>,
    moved: bool,
    homes_agree: bool,
}

thread_local! {
    static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) };
    static VISITS: RefCell<HashMap<(u64, usize), usize>> = RefCell::new(HashMap::new());
}

fn count_visit(slot: *mut EgclVal) {
    VISITS.with(|counts| {
        *counts
            .borrow_mut()
            .entry((egcl_rt::gc::root_scan_pass(), slot as usize))
            .or_default() += 1;
    });
}

struct Hooks {
    observer: ObserveHook,
    probe: *mut Probe,
    visitor: Option<fn(*mut EgclVal)>,
}

impl Drop for Hooks {
    fn drop(&mut self) {
        OBSERVE.with(|slot| slot.set(self.observer));
        PROBE.with(|slot| slot.set(self.probe));
        SCAN_VISIT_HOOK.with(|slot| slot.set(self.visitor));
    }
}

fn observe_aliases() {
    let probe = unsafe { &mut *PROBE.with(Cell::get) };
    if probe.observed {
        return;
    }
    let current = ACTIVE.with(Cell::get);
    let original = unsafe { &*current };
    let mut target_homes = Vec::new();
    let mut target = 0;
    unsafe {
        walk_boundary(original, &mut |slot| {
            let value = slot.read();
            if value.is_cons() && crate::cli::cp(value) == (EgclVal::from_fixnum(718), NIL) {
                target = value.to_raw();
                target_homes.push(slot as usize);
            }
        });
    }
    target_homes.sort_unstable();
    target_homes.dedup();
    if target_homes.len() < 2 {
        return;
    }
    probe.observed = true;

    // Deliberately expose the same real generated-frame homes through a second
    // publication. This makes address aliasing deterministic without depending
    // on which registers the allocator chooses for this particular fixture.
    // Both descriptors use the live frame's exact PC, bounds and unwind maps.
    let mut alias = PublishedBoundary {
        owner: original.owner,
        root_owner: original.root_owner,
        directory: std::rc::Rc::clone(&original.directory),
        context: original.context,
        cursor: original.cursor,
        bounds: original.bounds.clone(),
        segment: original.segment,
        stack: original.stack,
        previous: current,
        visited: Cell::new(0),
    };
    ACTIVE.with(|slot| slot.set(&mut alias));
    let _linked = Linked(&mut alias);
    VISITS.with(|counts| counts.borrow_mut().clear());

    // Separate homes containing identical values must all be rewritten; only
    // repeated addresses are duplicates. Prove movement, not just visitation.
    tests::force_minor_gc();
    VISITS.with(|counts| {
        let counts = counts.borrow();
        probe.duplicate_visits = counts.values().map(|count| count.saturating_sub(1)).sum();
        probe.homes = target_homes
            .into_iter()
            .filter(|slot| counts.keys().any(|(_, visited)| visited == slot))
            .collect();
    });
    let after: Vec<_> = probe
        .homes
        .iter()
        .map(|&slot| unsafe { (slot as *const EgclVal).read().to_raw() })
        .collect();
    probe.moved = after.iter().all(|&bits| bits != target);
    probe.homes_agree = after.windows(2).all(|pair| pair[0] == pair[1]);
}

#[test]
fn aliased_native_publications_visit_each_physical_home_once() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x y)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((let ((n 0))
             (tagbody again (setq n (+ n 1)) (if (< n 3) (go again))))
          (cons x y))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function(
            "PUBLICATION-ALIASED-HOMES",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap(),
    );
    let code = TransferCode::compile(body).expect("fixture must execute generated code");
    code.run(&[NIL, NIL], &mut env).unwrap();
    env.mv.clear();
    tests::force_minor_gc();
    egcl_rt::rooted!(value = crate::cli::arena_cons(EgclVal::from_fixnum(718), NIL));
    egcl_rt::rooted!(args = vec![*value, *value]);
    let mut probe = Probe::default();
    let hooks = Hooks {
        probe: PROBE.with(|slot| slot.replace(&mut probe)),
        observer: OBSERVE.with(|slot| slot.replace(Some(observe_aliases))),
        visitor: SCAN_VISIT_HOOK.with(|slot| slot.replace(Some(count_visit))),
    };
    let result = code.run(&args, &mut env).unwrap();
    drop(hooks);
    assert!(
        probe.observed,
        "generated poll needs two distinct homes of the same object"
    );
    assert_eq!(
        probe.duplicate_visits, 0,
        "one physical root home must be visited once per scan"
    );
    assert!(
        probe.homes.len() >= 2,
        "equal values in distinct homes must not be deduplicated"
    );
    assert!(
        probe.moved && probe.homes_agree,
        "all distinct homes must track actual relocation"
    );
    assert_eq!(crate::cli::cp(result), (*value, *value));
    assert!(ACTIVE.with(Cell::get).is_null());
}
