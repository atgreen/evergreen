// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use egcl_rt::{Collector, HeapCollector};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static VISITS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn relocate_on_this_visit() -> bool {
    ARMED.load(Ordering::SeqCst) && VISITS.fetch_add(1, Ordering::SeqCst) == 1
}

#[test]
fn direct_synonym_cycle_detection_roots_visited_handles_across_relocation() {
    const CHILD: &str = "EGCL_SYNONYM_VISIT_RELOCATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "streams::delegation_tests::direct_synonym_cycle_detection_roots_visited_handles_across_relocation", "--nocapture"])
            .env(CHILD, "1").env("EGCL_GC_POISON", "1")
            .env_remove("EGCL_GC_STRESS").env_remove("EGCL_GC_STRESS_AT")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    install_gc_hooks();
    let first_symbol = EgclVal::from_symbol_index(egcl_rt::symbols::intern("SYNONYM-VISIT-FIRST"));
    let second_symbol =
        EgclVal::from_symbol_index(egcl_rt::symbols::intern("SYNONYM-VISIT-SECOND"));
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(first = make_synonym_stream(first_symbol).unwrap());
    egcl_rt::rooted!(second = make_synonym_stream(second_symbol).unwrap());
    set_symbol_stream(first_symbol, *second);
    set_symbol_stream(second_symbol, *first);
    let before_first = first.to_raw();
    let before_second = second.to_raw();
    ARMED.store(true, Ordering::SeqCst);
    let error = resolve_synonym_stream(*first, false).unwrap_err();
    ARMED.store(false, Ordering::SeqCst);
    assert!(matches!(error, EgclError::StreamError(_)));
    assert_eq!(
        VISITS.load(Ordering::SeqCst),
        2,
        "the relocated first synonym remains in the active path"
    );
    assert_ne!(
        first.to_raw(),
        before_first,
        "first synonym must physically move"
    );
    assert_ne!(
        second.to_raw(),
        before_second,
        "current synonym must physically move"
    );
    remove_symbol_stream(first_symbol);
    remove_symbol_stream(second_symbol);
}

#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
mod ownership {
    use super::*;
    use egcl_rt::sync::EgclSemaphore;
    use egcl_rt::thread::{FiberState, fiber_state, make_fiber};
    use egcl_rt::{SchedulerConfig, SchedulerGroup};
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    static LEAF: AtomicU64 = AtomicU64::new(0);
    static ALIAS: AtomicU64 = AtomicU64::new(0);
    static RELEASE: OnceLock<EgclSemaphore> = OnceLock::new();

    fn owner() -> EgclVal {
        egcl_rt::rooted!(stream = EgclVal::from_raw(LEAF.load(Ordering::Acquire)));
        let mut guard = lock_stream(*stream).unwrap();
        guard.stream_write_char(EgclVal::from_char('A')).unwrap();
        RELEASE.get().unwrap().wait(None).unwrap();
        guard.stream_write_char(EgclVal::from_char('B')).unwrap();
        NIL
    }
    fn contender() -> EgclVal {
        egcl_rt::rooted!(alias = EgclVal::from_raw(ALIAS.load(Ordering::Acquire)));
        stream_write_sequence(*alias, &[EgclVal::from_char('C'), EgclVal::from_char('D')]).unwrap();
        NIL
    }
    fn release() -> EgclVal {
        RELEASE.get().unwrap().signal(1).unwrap();
        NIL
    }
    fn entry(function: fn() -> EgclVal) -> EgclVal {
        unsafe { EgclVal::from_function_ptr(function as *const () as *mut u8) }
    }

    #[test]
    fn direct_synonym_bulk_write_preserves_leaf_ownership_under_wait() {
        const NAME: &str = "streams::delegation_tests::ownership::direct_synonym_bulk_write_preserves_leaf_ownership_under_wait";
        const CHILD: &str = "EGCL_DIRECT_SYNONYM_OWNERSHIP_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env(CHILD, "1")
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_STRESS_AT")
                .env("EGCL_GC_POISON", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
            eprintln!("direct synonym operation blocked its carrier");
            std::process::exit(124);
        });
        egcl_rt::thread::current_thread_id();
        egcl_rt::gc::ensure_heap_initialized();
        install_gc_hooks();
        assert!(RELEASE.set(EgclSemaphore::new(None, 0).unwrap()).is_ok());
        let symbol =
            EgclVal::from_symbol_index(egcl_rt::symbols::intern("DIRECT-SYNONYM-OWNERSHIP"));
        egcl_rt::rooted!(alias = make_synonym_stream(symbol).unwrap());
        egcl_rt::rooted!(leaf = make_string_output_stream(NIL).unwrap());
        set_symbol_stream(symbol, *leaf);
        LEAF.store(leaf.to_raw(), Ordering::Release);
        ALIAS.store(alias.to_raw(), Ordering::Release);
        let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
        let first = make_fiber(entry(owner)).unwrap();
        group.submit(first).unwrap();
        while fiber_state(first) != Some(FiberState::Blocked) {
            egcl_rt::poll_safepoint();
            std::thread::yield_now();
        }
        let second = make_fiber(entry(contender)).unwrap();
        group.submit(second).unwrap();
        while fiber_state(second) != Some(FiberState::Blocked) {
            egcl_rt::poll_safepoint();
            std::thread::yield_now();
        }
        let before_alias = alias.to_raw();
        let before_leaf = leaf.to_raw();
        egcl_rt::collect_t0_minor().unwrap();
        assert_ne!(
            alias.to_raw(),
            before_alias,
            "blocked alias must physically move"
        );
        assert_ne!(
            leaf.to_raw(),
            before_leaf,
            "owned leaf must physically move"
        );
        group.submit(make_fiber(entry(release)).unwrap()).unwrap();
        group.finish().unwrap();
        egcl_rt::rooted!(text = get_output_stream_string(*leaf).unwrap());
        assert_eq!(extract_string_str(*text).unwrap(), "ABCD");
        remove_symbol_stream(symbol);
    }
}
