// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Allocation-free hits for immutable closure-template preparation. These are
//! deliberately non-owning GC caches: discard them BEFORE every collection and
//! on heap replacement, rather than extending the lifetime of callable objects.
use super::*;
#[cfg(test)]
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Weak;

const CAPACITY: usize = 32;

#[derive(Clone, Copy)]
pub(super) struct Prepared {
    pub lambda_list: EgclVal,
    pub name: EgclVal,
    pub control_free: bool,
    pub callable: Option<EgclVal>,
}

struct Entry {
    body: Weak<BytecodeFunction>,
    prepared: Prepared,
}

#[derive(Default)]
struct Cache {
    epoch: u64,
    entries: VecDeque<Entry>,
}

static CACHE: egcl_rt::execution_local::ExecutionLocal<RefCell<Cache>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(Cache::default())) };

fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_CLOSURE_TEMPLATE_CACHE").is_none())
}

/// Called with all executions stopped. No value in the cache is traced or read
/// after this point; allocation callers root their own copied metadata first.
pub(super) unsafe fn clear_for_collection() {
    unsafe { CACHE.scan(|cache| cache.borrow_mut().entries.clear()) };
}

pub(super) fn lookup(body: &Arc<BytecodeFunction>) -> Option<Prepared> {
    if !enabled() {
        return None;
    }
    let identity = Arc::as_ptr(body);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let epoch = egcl_rt::gc::heap_identity_epoch();
        if cache.epoch != epoch {
            cache.entries.clear();
            cache.epoch = epoch;
        }
        cache.entries.retain(|entry| entry.body.strong_count() != 0);
        cache
            .entries
            .iter()
            .find_map(|entry| (entry.body.as_ptr() == identity).then_some(entry.prepared))
    })
}

pub(super) fn remember(body: &Arc<BytecodeFunction>, prepared: Prepared) {
    if !enabled() {
        return;
    }
    let identity = Arc::as_ptr(body);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let epoch = egcl_rt::gc::heap_identity_epoch();
        if cache.epoch != epoch {
            cache.entries.clear();
            cache.epoch = epoch;
        }
        cache
            .entries
            .retain(|entry| entry.body.strong_count() != 0 && entry.body.as_ptr() != identity);
        if cache.entries.len() == CAPACITY {
            cache.entries.pop_front();
        }
        cache.entries.push_back(Entry {
            body: Arc::downgrade(body),
            prepared,
        });
    });
}

#[cfg(test)]
static COLLECT_DURING_PREPARATION: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };

#[cfg(test)]
pub(super) fn collect_during_preparation_for_test() {
    if COLLECT_DURING_PREPARATION.with(|armed| armed.replace(false)) {
        egcl_rt::gc::collect_t0_minor().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_template_reuses_only_safe_producer_objects_and_keeps_fresh_api() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(form = crate::cli::vec_to_list(&[NIL]));
        let body =
            Arc::new(compile_function("TEMPLATE-CACHE", NIL, *form, &env, false, false).unwrap());
        egcl_rt::rooted!(first = make_template_closure(&body, None, &env));
        // No Lisp allocation between these two requests: a hit must be literal
        // identity reuse, not merely another function sharing the same body.
        let reused = make_template_closure(&body, None, &env);
        assert_eq!(*first, reused);
        egcl_rt::rooted!(fresh = make_bytecode_closure(&body, None));
        assert_ne!(*first, *fresh);
        assert!(Arc::ptr_eq(
            &body,
            &registry_get(egcl_rt::function::definition_index(*fresh)).unwrap()
        ));
    }

    #[test]
    fn closure_template_cache_does_not_reuse_capturing_objects_or_control_tokens() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(form = crate::cli::vec_to_list(&[NIL]));
        let mut code =
            compile_function("TEMPLATE-CAPTURE", NIL, *form, &env, false, false).unwrap();
        // Free control is rejected independently of the capture_env bit.
        code.names.push("TEMPLATE-BLOCK".into());
        code.constants.push(NIL);
        code.code = vec![
            Instr::Const((code.constants.len() - 1) as u16),
            Instr::ReturnFromNamed { name_idx: 0 },
        ];
        let body = Arc::new(code);
        assert!(bf_has_free_control_exit(&body));
        egcl_rt::rooted!(first = make_template_closure(&body, None, &env));
        egcl_rt::rooted!(second = make_template_closure(&body, None, &env));
        assert_ne!(*first, *second);
        assert!(!lookup(&body).unwrap().control_free);
        let frame = Arc::new(SharedCell::new(EnvFrame {
            vars: Default::default(),
            symbol_vars: Default::default(),
            parent: None,
        }));
        egcl_rt::rooted!(captured_a = make_template_closure(&body, Some(Arc::clone(&frame)), &env));
        egcl_rt::rooted!(captured_b = make_template_closure(&body, Some(Arc::clone(&frame)), &env));
        assert_ne!(*captured_a, *captured_b);
        for callable in [*captured_a, *captured_b] {
            let index = egcl_rt::function::definition_index(callable);
            assert!(Arc::ptr_eq(
                &frame,
                closure_envs().borrow().get(&index).unwrap()
            ));
        }
    }

    #[test]
    fn closure_preparation_roots_sole_captured_cell_before_any_collection() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(form = crate::cli::vec_to_list(&[NIL]));
        let body = Arc::new(
            compile_function("TEMPLATE-SOLE-CAPTURE", NIL, *form, &env, false, false).unwrap(),
        );
        egcl_rt::gc::collect_t0_minor().unwrap();
        let frame = Arc::new(SharedCell::new(EnvFrame::default()));
        egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(73), NIL));
        frame.borrow_mut().vars.insert("HELD".into(), *payload);
        drop(payload);
        let before = frame.borrow().vars["HELD"].0;
        // No Env/activation/host root exposes frame here. The builder's own
        // precise root must be installed before this forced collection fires.
        COLLECT_DURING_PREPARATION.with(|armed| armed.set(true));
        egcl_rt::rooted!(function = make_bytecode_closure(&body, Some(Arc::clone(&frame))));
        let held = frame.borrow().vars["HELD"];
        assert_ne!(
            held.0, before,
            "the sole captured payload must actually move inside preparation"
        );
        assert_eq!(crate::cli::cp(held), (EgclVal::from_fixnum(73), NIL));
        assert!(Arc::ptr_eq(
            &frame,
            closure_envs()
                .borrow()
                .get(&egcl_rt::function::definition_index(*function))
                .unwrap()
        ));
    }

    #[test]
    fn closure_template_gc_clears_all_execution_entries_before_reuse() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(form = crate::cli::vec_to_list(&[NIL]));
        let body =
            Arc::new(compile_function("TEMPLATE-GC", NIL, *form, &env, false, false).unwrap());
        egcl_rt::rooted!(first = make_template_closure(&body, None, &env));
        assert!(lookup(&body).is_some());
        egcl_rt::gc::full_gc().unwrap();
        assert!(
            lookup(&body).is_none(),
            "cached untraced words cannot survive collection"
        );
        egcl_rt::rooted!(second = make_template_closure(&body, None, &env));
        assert_ne!(*first, *second);
        let reused = make_template_closure(&body, None, &env);
        assert_eq!(*second, reused);
        // Empty the nursery before constructing young metadata: no pinned
        // function/symbol allocation may retain its region in place.
        egcl_rt::gc::collect_t0_minor().unwrap();
        egcl_rt::rooted!(young = crate::cli::arena_cons(EgclVal::from_fixnum(1), NIL));
        let before = young.0;
        remember(
            &body,
            Prepared {
                lambda_list: *young,
                name: NIL,
                control_free: true,
                callable: None,
            },
        );
        assert!(lookup(&body).is_some());
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_ne!(
            before, young.0,
            "the cached metadata must actually relocate"
        );
        assert!(lookup(&body).is_none());
    }
}
