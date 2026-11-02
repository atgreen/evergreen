// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Lazy frame publication for runtime calls from compiled code
//! (bliss-gg3i1.7.15.10.8). A runtime builtin entered from T2 through the
//! named adapter links only the frame anchor; the exact code owner of the
//! suspended caller is resolved when something walks it. Each test reaches a
//! real builtin publication from an inner T2 frame whose owner is not the
//! segment's root owner, so resolution needs the code directory.

use super::ordinary_t2::install as install_ordinary;
use super::*;
use native_transfer_entry::root_publication;
use std::cell::Cell;

const OUTER_MARKER: i64 = 716_242;
const INNER_MARKER: i64 = 716_241;

const DEFINITIONS: &str = "(defun lazy-pub-inner (n s)
       (let ((held (cons 716241 nil)))
         (string s)
         (cons (+ n 1) held)))
     (defun lazy-pub-outer (n s)
       (let ((mine (cons 716242 nil)))
         (cons mine (lazy-pub-inner n s))))";

struct Fixture {
    outer_symbol: u32,
    outer: Rc<NativeCode>,
    inner: Rc<NativeCode>,
}

fn mapped(code: &NativeCode) -> &native_transfer_entry::TransferCode {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped ordinary owner")
    };
    mapped
}

fn inner_owner(fixture: &Fixture) -> usize {
    std::ptr::from_ref(mapped(&fixture.inner)) as usize
}

/// Two simultaneous physical T2 owners: the root `outer` and `inner`, which
/// calls the STRING builtin through the named runtime adapter.
fn fixture(env: &mut Env) -> Fixture {
    fixture_with_inner_value(env, "(+ n 1)")
}

fn fixture_with_inner_value(env: &mut Env, value: &str) -> Fixture {
    crate::cli::read_eval_all_env(&DEFINITIONS.replace("(+ n 1)", value), env).unwrap();
    native_transfer_entry::with_ordinary_non_tail_emission_for_test(|| {
        let (_, inner) = install_ordinary(
            "LAZY-PUB-INNER",
            "(n s)",
            &format!("((let ((held (cons 716241 nil))) (string s) (cons {value} held)))"),
            env,
        );
        let (outer_symbol, outer) = install_ordinary(
            "LAZY-PUB-OUTER",
            "(n s)",
            "((let ((mine (cons 716242 nil))) (cons mine (lazy-pub-inner n s))))",
            env,
        );
        for code in [&inner, &outer] {
            assert!(code.is_t2, "fixture owners must be installed T2");
            assert!(
                mapped(code).ordinary_entry.is_some(),
                "fixture owners must use the ordinary ABI"
            );
        }
        Fixture {
            outer_symbol,
            outer,
            inner,
        }
    })
}

fn symbol_value(name: &str, env: &mut Env) -> EgclVal {
    crate::cli::read_eval_all_env(&format!("'{name}"), env).unwrap()
}

/// Check the shape `((716242) expected_sum 716241)` and return its marker conses.
fn markers(result: EgclVal, expected_sum: i64) -> (EgclVal, EgclVal) {
    let (mine, rest) = crate::cli::cp(result);
    assert_eq!(
        crate::cli::cp(mine),
        (EgclVal::from_fixnum(OUTER_MARKER), NIL)
    );
    let (sum, held) = crate::cli::cp(rest);
    assert_eq!(sum, EgclVal::from_fixnum(expected_sum));
    assert_eq!(
        crate::cli::cp(held),
        (EgclVal::from_fixnum(INNER_MARKER), NIL)
    );
    (mine, held)
}

#[derive(Default)]
struct Moves {
    expected_owner: usize,
    observed: bool,
    lazy: bool,
    inner: (u64, u64),
    outer: (u64, u64),
}

thread_local! {
    static MOVES: Cell<*mut Moves> = const { Cell::new(std::ptr::null_mut()) };
}

struct Hook(Option<fn()>);
impl Hook {
    fn install(hook: fn()) -> Self {
        Self(root_publication::PROFILE_OBSERVE.with(|slot| slot.replace(Some(hook))))
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.0));
        MOVES.with(|slot| slot.set(std::ptr::null_mut()));
    }
}

/// At the STRING builtin's publication from `inner`: the anchor must not have
/// resolved its owner yet. Then collect with the two marker conses held only
/// by the suspended native frames, and record both homes before and after.
fn collect_inside_builtin() {
    let probe = MOVES.with(Cell::get);
    if probe.is_null() || unsafe { (*probe).observed } {
        return;
    }
    let Some((resolved, owner)) = root_publication::active_anchor_for_test() else {
        return;
    };
    if owner != unsafe { (*probe).expected_owner } {
        return;
    }
    let inner = root_publication::native_only_root_for_test(INNER_MARKER)
        .expect("inner frame holds its marker only in native homes");
    let outer = root_publication::native_only_root_for_test(OUTER_MARKER)
        .expect("outer frame holds its marker only in native homes");
    let before = unsafe { (inner.read().to_raw(), outer.read().to_raw()) };
    HeapCollector::new().minor_gc().unwrap();
    let after = unsafe { (inner.read().to_raw(), outer.read().to_raw()) };
    unsafe {
        (*probe).observed = true;
        (*probe).lazy = !resolved;
        (*probe).inner = (before.0, after.0);
        (*probe).outer = (before.1, after.1);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn lazy_publication_gc_inside_builtin_relocates_native_caller_values() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // Keep arithmetic out of this publication probe: its generic runtime
    // adapter is independent of STRING and would add another named entry.
    let fixture = fixture_with_inner_value(&mut env, "n");
    egcl_rt::rooted!(name = symbol_value("lazy-pub-symbol", &mut env));
    // Warm the linkage so the measured call is generated code end to end.
    for _ in 0..2 {
        egcl_rt::rooted!(
            warm = run_native(
                &fixture.outer,
                fixture.outer_symbol,
                &[EgclVal::from_fixnum(1), *name],
                &mut env
            )
            .unwrap()
        );
        markers(*warm, 1);
    }
    let mut moves = Moves {
        expected_owner: inner_owner(&fixture),
        ..Moves::default()
    };
    let _hook = Hook::install(collect_inside_builtin);
    MOVES.with(|slot| slot.set(&mut moves));
    native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    native_transfer_entry::ordinary_calls::start_named_entry_symbols_for_test();
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    egcl_rt::rooted!(
        result = run_native(
            &fixture.outer,
            fixture.outer_symbol,
            &[EgclVal::from_fixnum(7), *name],
            &mut env
        )
        .unwrap()
    );
    assert!(
        moves.observed,
        "the builtin publication from inner must run"
    );
    assert!(
        moves.lazy,
        "the builtin's publication must leave owner resolution to the walk"
    );
    let named_entries = native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    let named: Vec<_> = native_transfer_entry::ordinary_calls::take_named_entry_symbols_for_test()
        .into_iter()
        .map(|symbol| egcl_rt::symbols::registry_key(symbol).unwrap())
        .collect();
    assert_eq!(
        named_entries,
        named.len(),
        "the trace accounts for every named runtime entry"
    );
    assert_eq!(
        named
            .iter()
            .filter(|name| name.as_str() == "STRING")
            .count(),
        1,
        "STRING is entered exactly once through the named runtime adapter: {named:?}"
    );
    // CONS's allocating fallback also uses this adapter. The total named count
    // is not STRING's count; forbid every other target, including an adapter
    // call of the inner Lisp function that must instead run generated code.
    assert!(
        named
            .iter()
            .all(|name| matches!(name.as_str(), "STRING" | "CONS")),
        "unexpected runtime target: {named:?}"
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
        0,
        "outer enters inner's generated body directly"
    );
    assert_ne!(moves.inner.0, moves.inner.1, "inner's cons must move");
    assert_ne!(moves.outer.0, moves.outer.1, "outer's cons must move");
    let (mine, held) = markers(*result, 7);
    assert_eq!(
        held.to_raw(),
        moves.inner.1,
        "inner resumes with the relocated value"
    );
    assert_eq!(
        mine.to_raw(),
        moves.outer.1,
        "outer resumes with the relocated value"
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert!(!root_publication::any_publication_for_test());
    assert_eq!(mapped(&fixture.inner).deopt_count(), 0);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn lazy_publication_error_inside_builtin_unwinds_with_native_backtrace() {
    use egcl_rt::debug_stack::FrameOrigin;
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixture = fixture(&mut env);
    egcl_rt::rooted!(name = symbol_value("lazy-pub-symbol", &mut env));
    egcl_rt::rooted!(
        warm = run_native(
            &fixture.outer,
            fixture.outer_symbol,
            &[EgclVal::from_fixnum(1), *name],
            &mut env
        )
        .unwrap()
    );
    markers(*warm, 2);
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let segment = egcl_rt::native_transfer::current_segment();
    // (STRING 5) signals inside the builtin, beneath both anchored frames.
    egcl_rt::rooted!(
        error = run_native(
            &fixture.outer,
            fixture.outer_symbol,
            &[EgclVal::from_fixnum(1), EgclVal::from_fixnum(5)],
            &mut env
        )
        .unwrap_err()
    );
    assert!(
        matches!(error.without_backtrace(), EgclError::TypeError { datum, expected }
            if *datum == EgclVal::from_fixnum(5) && expected == "(OR STRING SYMBOL CHARACTER)"),
        "{:?}",
        &*error
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before,
        "the error retires both native frames and their segment exactly once"
    );
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    assert!(!root_publication::any_publication_for_test());
    // The backtrace was copied through the builtin's anchor, which resolved
    // the inner owner then; both suspended native callers appear in order.
    HeapCollector::new().minor_gc().unwrap();
    let trace = error.backtrace().expect("captured error backtrace");
    let names: Vec<_> = trace
        .iter()
        .filter_map(|frame| {
            let name = frame.function.as_deref()?;
            (name.ends_with("LAZY-PUB-INNER") || name.ends_with("LAZY-PUB-OUTER"))
                .then_some((name.to_owned(), frame.origin))
        })
        .collect();
    assert_eq!(names.len(), 2, "{trace:?}");
    assert!(names[0].0.ends_with("LAZY-PUB-INNER"), "{names:?}");
    assert!(names[1].0.ends_with("LAZY-PUB-OUTER"), "{names:?}");
    // The physical root enriches its managed segment anchor; only the inner
    // physical-only frame adds a native-origin row (as in ordinary_register_roots).
    assert_eq!(names[0].1, FrameOrigin::Native, "{names:?}");
    assert_eq!(names[1].1, FrameOrigin::Managed, "{names:?}");

    // A Lisp handler around the installed definitions catches the same error
    // through the generated frames, and the next call runs normally.
    let caught = crate::cli::read_eval_all_env(
        "(list (handler-case (lazy-pub-outer 1 5) (type-error () :caught))
               (car (cdr (lazy-pub-outer 2 'lazy-pub-symbol))))",
        &mut env,
    )
    .unwrap();
    assert_eq!(
        crate::cli::format_val(caught),
        crate::cli::format_val(
            crate::cli::read_eval_all_env("(list :caught 3)", &mut env).unwrap()
        )
    );
    assert!(!root_publication::any_publication_for_test());
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn lazy_publication_guard_deopt_after_builtin_keeps_relocated_values() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixture = fixture(&mut env);
    egcl_rt::rooted!(name = symbol_value("lazy-pub-symbol", &mut env));
    for _ in 0..2 {
        egcl_rt::rooted!(
            warm = run_native(
                &fixture.outer,
                fixture.outer_symbol,
                &[EgclVal::from_fixnum(1), *name],
                &mut env
            )
            .unwrap()
        );
        markers(*warm, 2);
    }
    let inner = mapped(&fixture.inner);
    assert!(inner.has_deopt, "(+ n 1) must use a native fixnum guard");
    let deopts = inner.deopt_count();
    let mut moves = Moves {
        expected_owner: inner_owner(&fixture),
        ..Moves::default()
    };
    let _hook = Hook::install(collect_inside_builtin);
    MOVES.with(|slot| slot.set(&mut moves));
    // The fixnum guard after the builtin fails: inner deoptimizes from the
    // same frame the collection inside the builtin just relocated.
    let n = egcl_rt::bignum::FIXNUM_MAX;
    egcl_rt::rooted!(
        result = run_native(
            &fixture.outer,
            fixture.outer_symbol,
            &[EgclVal::from_fixnum(n), *name],
            &mut env
        )
        .unwrap()
    );
    assert!(moves.observed && moves.lazy);
    assert_ne!(moves.inner.0, moves.inner.1, "inner's cons must move");
    assert_eq!(inner.deopt_count(), deopts + 1, "the guard must deopt once");
    let (mine, rest) = crate::cli::cp(*result);
    assert_eq!(mine.to_raw(), moves.outer.1);
    let (sum, held) = crate::cli::cp(rest);
    assert_eq!(held.to_raw(), moves.inner.1);
    assert_eq!(
        crate::cli::format_val(sum),
        crate::cli::format_val(
            crate::cli::read_eval_all_env(&format!("(+ {n} 1)"), &mut env).unwrap()
        )
    );
    assert!(!root_publication::any_publication_for_test());
}

thread_local! {
    static SAMPLE_OWNER: Cell<usize> = const { Cell::new(0) };
    static SAMPLES: Cell<usize> = const { Cell::new(0) };
}

fn sample_inside_builtin() {
    let Some((resolved, owner)) = root_publication::active_anchor_for_test() else {
        return;
    };
    if owner != SAMPLE_OWNER.with(Cell::get) {
        return;
    }
    assert!(!resolved, "sampling must find an unresolved anchor");
    SAMPLES.with(|count| count.set(count.get() + 1));
    crate::cli::sprof::request_for_test();
    crate::cli::sprof::maybe_sample();
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn lazy_publication_sprof_samples_anchored_callers_of_a_builtin() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixture = fixture(&mut env);
    egcl_rt::rooted!(name = symbol_value("lazy-pub-symbol", &mut env));
    egcl_rt::rooted!(
        warm = run_native(
            &fixture.outer,
            fixture.outer_symbol,
            &[EgclVal::from_fixnum(1), *name],
            &mut env
        )
        .unwrap()
    );
    markers(*warm, 2);
    struct Recording {
        _hook: Hook,
    }
    impl Drop for Recording {
        fn drop(&mut self) {
            crate::cli::sprof::stop();
        }
    }
    crate::cli::sprof::begin_for_test();
    let _recording = Recording {
        _hook: Hook::install(sample_inside_builtin),
    };
    SAMPLE_OWNER.with(|owner| owner.set(inner_owner(&fixture)));
    SAMPLES.with(|count| count.set(0));
    for n in 0..4 {
        egcl_rt::rooted!(
            result = run_native(
                &fixture.outer,
                fixture.outer_symbol,
                &[EgclVal::from_fixnum(n), *name],
                &mut env
            )
            .unwrap()
        );
        markers(*result, n + 1);
    }
    assert_eq!(SAMPLES.with(Cell::get), 4, "one sample per builtin call");
    let folded = crate::cli::sprof::fold();
    assert!(
        folded
            .lines()
            .any(|line| line.contains("LAZY-PUB-OUTER [T2];LAZY-PUB-INNER [T2]")),
        "{folded}"
    );
}

// Keep the anchor unresolved until a different execution collects. Diagnostic
// snapshots contain only integers and cannot retain the marker conses.
static FOREIGN_PARKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FOREIGN_COLLECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FOREIGN_FRAME: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0, 0))) };

fn park_unresolved_builtin() {
    if FOREIGN_PARKED.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let (start, end, expected_owner) = FOREIGN_FRAME.with(Cell::get);
    let Some((resolved, pc)) = root_publication::active_anchor_snapshot_for_test() else {
        return;
    };
    if !(start..end).contains(&pc) {
        return;
    }
    assert!(!resolved, "the builtin must park an unresolved anchor");
    FOREIGN_PARKED.store(true, std::sync::atomic::Ordering::SeqCst);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !FOREIGN_COLLECTED.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "foreign collection did not finish"
        );
        egcl_rt::sync::fiber_sleep(std::time::Duration::from_millis(10)).unwrap();
    }
    let (resolved, owner) = root_publication::active_anchor_for_test().unwrap();
    assert!(
        resolved,
        "the foreign collector must resolve the parked owner first"
    );
    assert_eq!(
        owner, expected_owner,
        "use the parked execution's directory"
    );
}

fn unresolved_publication_fiber() -> EgclVal {
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixture = fixture(&mut env);
    egcl_rt::rooted!(name = symbol_value("lazy-pub-symbol", &mut env));
    for _ in 0..2 {
        egcl_rt::rooted!(
            warm = run_native(
                &fixture.outer,
                fixture.outer_symbol,
                &[EgclVal::from_fixnum(7), *name],
                &mut env
            )
            .unwrap()
        );
        markers(*warm, 8);
    }
    let inner = mapped(&fixture.inner);
    let (start, len) = inner
        .ordinary_code_bounds()
        .expect("ordinary inner code bounds");
    FOREIGN_FRAME.with(|frame| frame.set((start, start + len, inner_owner(&fixture))));
    let hook = Hook::install(park_unresolved_builtin);
    egcl_rt::rooted!(
        result = run_native(
            &fixture.outer,
            fixture.outer_symbol,
            &[EgclVal::from_fixnum(7), *name],
            &mut env
        )
        .unwrap()
    );
    drop(hook);
    markers(*result, 8);
    assert!(!root_publication::any_publication_for_test());
    EgclVal::from_fixnum(1)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn lazy_publication_foreign_collector_resolves_a_parked_builtin_anchor() {
    use std::sync::atomic::Ordering;
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    FOREIGN_PARKED.store(false, Ordering::SeqCst);
    FOREIGN_COLLECTED.store(false, Ordering::SeqCst);
    let group =
        egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers: 1 }).unwrap();
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            FOREIGN_COLLECTED.store(true, Ordering::SeqCst);
        }
    }
    let release = Release;
    let entry =
        unsafe { EgclVal::from_function_ptr(unresolved_publication_fiber as *const () as *mut u8) };
    group
        .submit(egcl_rt::thread::make_fiber(entry).unwrap())
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !FOREIGN_PARKED.load(Ordering::SeqCst) {
        egcl_rt::poll_safepoint();
        assert!(
            std::time::Instant::now() < deadline,
            "builtin anchor never parked"
        );
        std::thread::yield_now();
    }
    root_publication::take_foreign_walk_counts();
    HeapCollector::new().minor_gc().unwrap();
    let (walks, visited) = root_publication::take_foreign_walk_counts();
    assert!(
        walks > 0 && visited > 0,
        "foreign collection must walk real native roots"
    );
    drop(release);
    assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(1)]);
}
