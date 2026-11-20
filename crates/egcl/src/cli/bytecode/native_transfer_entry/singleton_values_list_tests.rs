// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Runtime counterparts of the compiler's fail-closed single-MV rewrite.
use super::super::native_transfer_tests::ordinary_t2;
use super::*;

#[derive(Clone, Copy, Default)]
struct Probe {
    phase: usize,
    before_symbol: u32,
    after_symbol: u32,
    collect: bool,
    stress: bool,
    native_child: bool,
    primary_before: u64,
    keep_before: u64,
    observed_list_home: usize,
    expected_primary_home: usize,
}
thread_local! {
    static PROBE: Cell<Probe> = const { Cell::new(Probe {
        phase: 0, before_symbol: 0, after_symbol: 0, collect: false,
        stress: false, native_child: false, primary_before: 0,
        keep_before: 0, observed_list_home: 0, expected_primary_home: 0,
    }) };
}
fn owner(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("mapped T2 owner")
    };
    code
}
fn helper_local(expected_symbol: u32, index: usize) -> *const EgclVal {
    let frame = egcl_rt::current_stack().fp();
    assert!(!frame.is_null());
    unsafe {
        let function = (*frame).function;
        let expected = egcl_rt::symbols::symbol_function(expected_symbol)
            .expect("registered fixture helper callable");
        assert_eq!(
            function, expected,
            "inspect the exact pinned helper callable's parameter frame"
        );
        assert!(egcl_rt::function::is_interpreted_function(function));
        assert_eq!(egcl_rt::function::tier(function), 0, "helper stays in T0");
        assert!(index < usize::from((*frame).num_locals));
        frame.add(1).cast::<EgclVal>().add(index)
    }
}
fn observe_body() {
    let mut probe = PROBE.with(Cell::get);
    if probe.phase == 0 {
        let primary = helper_local(probe.before_symbol, 0);
        let keep = helper_local(probe.before_symbol, 1);
        if probe.stress {
            assert_ne!(probe.expected_primary_home, 0);
            // EVAL's setup has finished. This fresh value returns directly
            // through slot zero; the native singleton CONS is the next Lisp
            // allocation, with inline allocation disabled by this test.
            egcl_rt::rooted!(fresh = crate::cli::arena_cons(unsafe { *primary }, unsafe { *keep }));
            // This mutator owns the matched live helper frame. Raw home
            // addresses are stable; no mutable borrow spans the allocation.
            unsafe {
                primary.cast_mut().write(*fresh);
                (probe.expected_primary_home as *mut EgclVal).write(*fresh);
            }
        }
        probe.primary_before = unsafe { (*primary).to_raw() };
        probe.keep_before = unsafe { (*keep).to_raw() };
        if probe.collect {
            egcl_rt::gc::collect_t0_minor().unwrap();
            assert_ne!(
                unsafe { (*primary).to_raw() },
                probe.primary_before,
                "live primary moves"
            );
            assert_ne!(
                unsafe { (*keep).to_raw() },
                probe.keep_before,
                "live caller payload moves"
            );
        }
    } else if probe.phase == 1 {
        let slot = helper_local(probe.after_symbol, 0);
        egcl_rt::rooted!(list = unsafe { *slot });
        assert!(list.is_cons());
        assert_eq!(crate::cli::cp(*list).1, NIL);
        if probe.stress {
            assert_eq!(
                crate::cli::cp(*list).0,
                unsafe { *(probe.expected_primary_home as *const EgclVal) },
                "the returned primary is the exact rooted producer payload"
            );
            assert_ne!(
                crate::cli::cp(*list).0.to_raw(),
                probe.primary_before,
                "fresh primary relocates between the before/after helper, across CONS allocation"
            );
        }
        if probe.collect {
            let before = list.to_raw();
            egcl_rt::gc::collect_t0_minor().unwrap();
            assert_ne!(
                list.to_raw(),
                before,
                "new singleton list moves while native home is live"
            );
            assert_eq!(unsafe { *slot }, *list);
        }
        if probe.observed_list_home != 0 {
            // The caller owns this rooted home until Observing is dropped.
            // Later CONS allocations must update both this witness and the
            // native result home, preserving exact identity after relocation.
            unsafe { (probe.observed_list_home as *mut EgclVal).write(*list) };
        }
    } else {
        panic!("producer/consumer helpers must not be replayed");
    }
    if probe.native_child {
        let rows = egcl_rt::debug_stack::capture_current(32);
        assert!(
            rows.iter().any(
                |row| row.origin == egcl_rt::debug_stack::FrameOrigin::Native
                    && row
                        .function
                        .as_deref()
                        .is_some_and(|name| name.ends_with("SINGLETON-CHILD"))
            ),
            "{rows:?}"
        );
        assert_eq!(
            activation_counts_for_test(),
            (0, 0),
            "native child needs no managed callee activation"
        );
    }
    probe.phase += 1;
    PROBE.with(|slot| slot.set(probe));
}
struct Observing(Option<fn()>);
impl Observing {
    fn new(
        collect: bool,
        stress: bool,
        native_child: bool,
        observed_list_home: *mut EgclVal,
    ) -> Self {
        PROBE.with(|slot| {
            slot.set(Probe {
                before_symbol: egcl_rt::symbols::intern("SINGLETON-BEFORE"),
                after_symbol: egcl_rt::symbols::intern("SINGLETON-AFTER"),
                collect,
                stress,
                native_child,
                observed_list_home: observed_list_home as usize,
                ..Probe::default()
            })
        });
        Self(ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.replace(Some(observe_body))))
    }
}
impl Drop for Observing {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.0));
        PROBE.with(|slot| slot.set(Probe::default()));
        take_activation_counts_for_test();
    }
}
fn bootstrap(env: &mut Env) -> ordinary_t2::RecursiveFixturePins {
    crate::cli::read_eval_all_env(
        "(defun singleton-before (x keep) (%ordinary-t2-entry-for-test) x)
         (defun singleton-after (answer) (%ordinary-t2-entry-for-test) nil)
         (defun singleton-unknown (mode x keep) (if mode (values) (values x keep)))",
        env,
    )
    .unwrap();
    // A pinned, source-only function is evaluated against an EnvFrame, not a
    // managed VM frame. Register these exact helper bodies before pinning so
    // the observer can inspect their real bytecode parameter homes. Merely
    // relaxing the old name/definition-index check would read caller slots.
    // The test marker is an evaluator-only operator. EVAL is the existing
    // compileable boundary used by native GC probes; it keeps this VM frame.
    for name in ["SINGLETON-BEFORE", "SINGLETON-AFTER"] {
        let symbol = egcl_rt::symbols::intern(name);
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        egcl_rt::rooted!(params = egcl_rt::function::lambda_list(*function));
        let source = if name == "SINGLETON-BEFORE" {
            "((eval '(%ordinary-t2-entry-for-test)) x)"
        } else {
            "((eval '(%ordinary-t2-entry-for-test)) nil)"
        };
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function(name, *params, *forms, env, false, false)
                .expect("fixture helper bytecode"),
        );
        registry_put(symbol, body);
    }
    ordinary_t2::RecursiveFixturePins::new(&[
        "SINGLETON-BEFORE",
        "SINGLETON-AFTER",
        "SINGLETON-UNKNOWN",
    ])
}
fn install(env: &mut Env, name: &str, params: &str, source: &str) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} {params} {source})"), env).unwrap();
    ordinary_t2::install(name, params, &format!("({source})"), env)
}
fn assert_singleton_source(native: &NativeCode, ordinary: bool) {
    assert!(native.is_t2);
    assert!(
        native
            .body
            .as_ref()
            .unwrap()
            .code
            .iter()
            .any(|op| matches!(op, Instr::ValuesToList))
    );
    assert_eq!(owner(native).ordinary_entry.is_some(), ordinary);
}
fn run(
    symbol: u32,
    native: &NativeCode,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    ordinary_t2::expect_entry(native);
    run_native(native, symbol, args, env)
}
fn assert_single_value(env: &Env) {
    assert!(!env.mv_active);
    assert!(env.mv.is_empty());
}
fn assert_singleton(list: EgclVal, primary: EgclVal) {
    assert!(list.is_cons());
    assert_eq!(crate::cli::cp(list), (primary, NIL));
}
const SINGLETON_SOURCE: &str = "(locally (declare (notinline singleton-before singleton-after))
        (let ((answer (multiple-value-list (progn (singleton-before x keep) (setq x x)))))
          (singleton-after answer) (cons answer (cons x keep))))";

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_singleton_list_heap_and_nil_from_mapped_parent() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = bootstrap(&mut env);
    let child = install(&mut env, "SINGLETON-CHILD", "(x keep)", SINGLETON_SOURCE);
    assert_singleton_source(&child.1, true);
    let parent = install(
        &mut env,
        "SINGLETON-MAPPED-PARENT",
        "(x keep &key (k (1+ 0)) &aux (u 0))",
        "(declare (notinline singleton-child)) (singleton-child x keep)",
    );
    assert!(owner(&parent.1).ordinary_entry.is_none());
    // Named linkage is recreated during parent compilation: settle it before
    // claiming the native child route (same setup as existing bridge fixtures).
    struct NativeEnv(*mut Env);
    impl Drop for NativeEnv {
        fn drop(&mut self) {
            NATIVE_ENV.with(|slot| slot.set(self.0));
        }
    }
    {
        let _native_env = NativeEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        ordinary_t2::expect_entry(&child.1);
        super::super::call_table::ordinary_call(child.0, &[NIL, NIL]).unwrap();
    }
    for heap in [true, false] {
        egcl_rt::gc::collect_t0_minor().unwrap();
        egcl_rt::rooted!(
            x = if heap {
                crate::cli::arena_cons(EgclVal::from_fixnum(71), NIL)
            } else {
                NIL
            }
        );
        egcl_rt::rooted!(keep = crate::cli::arena_cons(EgclVal::from_fixnum(72), NIL));
        egcl_rt::rooted!(args = vec![*x, *keep]);
        let frame = egcl_rt::current_stack().fp();
        let sp = egcl_rt::current_stack().sp();
        egcl_rt::rooted!(observed_list = NIL);
        let _observe = Observing::new(heap, false, true, std::ptr::from_mut(&mut *observed_list));
        start_activation_counts_for_test(child.0);
        mapped_ordinary::take_entries_for_test();
        ordinary_t2::expect_entry(&child.1);
        egcl_rt::rooted!(result = run_native(&parent.1, parent.0, &args, &mut env).unwrap());
        assert!(ordinary_t2::observed_entry());
        assert_eq!(PROBE.with(|slot| slot.get().phase), 2);
        let list = crate::cli::cp(*result).0;
        assert_singleton(list, *x);
        assert_eq!(
            list, *observed_list,
            "the same singleton list survives subsequent native allocations"
        );
        let tail = crate::cli::cp(*result).1;
        assert_eq!(crate::cli::cp(tail), (*x, *keep));
        assert_eq!(activation_counts_for_test(), (0, 0));
        assert_eq!(mapped_ordinary::take_entries_for_test(), 1);
        assert_eq!(owner(&child.1).deopt_count(), 0);
        assert_single_value(&env);
        assert_eq!(
            (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()),
            (frame, sp)
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_singleton_list_local_block_merge_requires_all_paths_clear() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = bootstrap(&mut env);
    let (symbol, native) = install(
        &mut env,
        "SINGLETON-BLOCK-MERGE",
        "(x keep flag)",
        "(locally (declare (notinline singleton-before singleton-after))
           (singleton-before x keep)
           (let ((answer (multiple-value-list
                    (block done (if flag (return-from done (setq x x)) (setq x nil))))))
             (singleton-after answer) answer))",
    );
    assert_singleton_source(&native, true);
    egcl_rt::rooted!(x = crate::cli::arena_cons(EgclVal::from_fixnum(73), NIL));
    for flag in [T, NIL] {
        let _observe = Observing::new(false, false, false, std::ptr::null_mut());
        egcl_rt::rooted!(args = vec![*x, NIL, flag]);
        egcl_rt::rooted!(result = run(symbol, &native, &args, &mut env).unwrap());
        assert!(ordinary_t2::observed_entry());
        assert_eq!(PROBE.with(|slot| slot.get().phase), 2);
        assert_singleton(*result, if flag.is_nil() { NIL } else { *x });
        assert_single_value(&env);
        assert_eq!(owner(&native).deopt_count(), 0);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_singleton_list_unknown_zero_many_and_mixed_remain_mapped() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = bootstrap(&mut env);
    for mixed in [false, true] {
        let source = if mixed {
            "(multiple-value-list (if mode (setq x x) (singleton-unknown nil x keep)))"
        } else {
            "(multiple-value-list (singleton-unknown mode x keep))"
        };
        let (symbol, native) = install(
            &mut env,
            if mixed {
                "SINGLETON-MIXED-UNKNOWN"
            } else {
                "SINGLETON-UNKNOWN-CALL"
            },
            "(mode x keep)",
            &format!(
                "(locally (declare (notinline singleton-unknown singleton-before)) (singleton-before x keep) {source})"
            ),
        );
        assert_singleton_source(&native, false);
        for mode in [T, NIL] {
            egcl_rt::rooted!(args = vec![mode, EgclVal::from_fixnum(11), EgclVal::from_fixnum(12)]);
            egcl_rt::rooted!(result = run(symbol, &native, &args, &mut env).unwrap());
            assert!(
                ordinary_t2::observed_entry(),
                "unknown consumer actually executes mapped native code"
            );
            assert_eq!(owner(&native).deopt_count(), 0);
            if mode == T {
                if mixed {
                    assert_singleton(*result, args[1]);
                } else {
                    assert_eq!(*result, NIL);
                }
            } else {
                let (first, rest) = crate::cli::cp(*result);
                assert_eq!(first, args[1]);
                assert_singleton(rest, args[2]);
            }
            assert_single_value(&env);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct GuardProbe {
    owner: usize,
    metadata: usize,
    body: usize,
    bcp: usize,
    body_seen: bool,
    payloads: usize,
    resumes: usize,
}
thread_local! {
    static GUARD_PROBE: Cell<GuardProbe> = const { Cell::new(GuardProbe {
        owner: 0, metadata: 0, body: 0, bcp: 0, body_seen: false, payloads: 0, resumes: 0,
    }) };
}
fn observe_guard_body() {
    GUARD_PROBE.with(|cell| {
        let mut probe = cell.get();
        probe.body_seen |= root_publication::published_frame_has_owner_for_test(probe.owner);
        cell.set(probe);
    });
}
fn observe_guard_payload(words: *const u64, len: usize, boxed: bool) {
    GUARD_PROBE.with(|cell| {
        let mut probe = cell.get();
        assert!(!boxed);
        assert!(len >= 4);
        assert!(root_publication::published_frame_has_owner_for_test(
            probe.owner
        ));
        // This existing hook runs only after validation and precise rooting.
        // No allocation or borrowed slice crosses a collection here.
        let semantic = u32::try_from(unsafe { *words }).unwrap();
        // The caller retains the native Rc and therefore this immutable Rust
        // metadata throughout the hook. No Lisp value is cached in the probe.
        let metadata = unsafe { &*(probe.metadata as *const T2InstalledMetadata) };
        assert!(
            metadata
                .deopt_bodies
                .get(&semantic)
                .is_some_and(|body| { Arc::as_ptr(body) as usize == probe.body }),
            "the stream belongs to the exact retained body, not its public alias"
        );
        assert_eq!(unsafe { *words.add(1) }, probe.bcp as u64);
        probe.payloads += 1;
        cell.set(probe);
    });
}
fn observe_guard_resume(scopes: u64) {
    assert_eq!(scopes, 1);
    GUARD_PROBE.with(|cell| {
        let mut probe = cell.get();
        probe.resumes += 1;
        cell.set(probe);
    });
}
struct GuardObserving(Option<fn()>);
impl GuardObserving {
    fn new(symbol: u32, native: &NativeCode) -> Self {
        assert!(owner(native).lean_root_entry().is_some());
        assert_eq!(owner(native).installed_symbol, Some(symbol));
        // Recovery headers name semantic code, not necessarily the public
        // function binding. Match the exact retained body in its metadata.
        let metadata = owner(native).deopt_metadata.as_ref().unwrap();
        let plus = egcl_rt::symbols::intern("+");
        let bcp = native
            .body
            .as_ref()
            .unwrap()
            .code
            .iter()
            .position(|op| matches!(op, Instr::CallNamed { sym, nargs: 2 } if *sym == plus))
            .expect("the guarded addition has its original bytecode continuation");
        GUARD_PROBE.with(|cell| {
            cell.set(GuardProbe {
                owner: Rc::as_ptr(owner(native)) as usize,
                metadata: Arc::as_ptr(metadata) as usize,
                body: Arc::as_ptr(&owner(native).body) as usize,
                bcp,
                ..GuardProbe::default()
            })
        });
        Self(root_publication::PROFILE_OBSERVE.with(|hook| hook.replace(Some(observe_guard_body))))
    }
}
impl Drop for GuardObserving {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|hook| hook.set(self.0));
        GUARD_PROBE.with(|cell| cell.set(GuardProbe::default()));
    }
}
fn run_guarded(
    native: &NativeCode,
    symbol: u32,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    with_ordinary_guard_payload_observer_for_test(observe_guard_payload, || {
        with_ordinary_guard_resume_observer_for_test(observe_guard_resume, || {
            run_native(native, symbol, args, env)
        })
    })
}
fn assert_guard_effects(env: &mut Env) {
    for name in [
        "*singleton-cold-before-count*",
        "*singleton-cold-after-count*",
    ] {
        assert_eq!(
            crate::cli::read_eval_all_env(name, env).unwrap(),
            EgclVal::from_fixnum(1),
            "completed producer/list effects must not replay"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_singleton_list_cold_guard_preserves_effect_and_retry() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = bootstrap(&mut env);
    crate::cli::read_eval_all_env(
        "(setq *singleton-cold-before-count* 0 *singleton-cold-after-count* 0)
         (defun singleton-cold-before (x keep) (declare (ignore x keep))
           (setq *singleton-cold-before-count* (+ *singleton-cold-before-count* 1)) nil)
         (defun singleton-cold-after (answer) (declare (ignore answer))
           (setq *singleton-cold-after-count* (+ *singleton-cold-after-count* 1)) nil)
         (defun singleton-cold-value (value) value)",
        &mut env,
    )
    .unwrap();
    let _cold_pins = ordinary_t2::RecursiveFixturePins::new(&[
        "SINGLETON-COLD-BEFORE",
        "SINGLETON-COLD-AFTER",
        "SINGLETON-COLD-VALUE",
    ]);
    // Guard the opaque result, not an entry parameter: parameter speculation
    // would recover at BCP 0 before the producer/list effects have executed.
    let source = "(locally (declare (notinline singleton-cold-before singleton-cold-after singleton-cold-value))
        (singleton-cold-before x keep)
        (let ((answer (multiple-value-list (setq x x))))
          (singleton-cold-after answer) (+ (singleton-cold-value bad) 1) answer))";
    let (symbol, native) = install(&mut env, "SINGLETON-COLD-GUARD", "(x keep bad)", source);
    assert_singleton_source(&native, true);
    egcl_rt::rooted!(x = crate::cli::arena_cons(EgclVal::from_fixnum(74), NIL));
    egcl_rt::rooted!(bad = crate::cli::arena_cons(EgclVal::from_fixnum(75), NIL));
    egcl_rt::rooted!(args = vec![*x, NIL, *bad]);
    let frame = egcl_rt::current_stack().fp();
    let sp = egcl_rt::current_stack().sp();
    {
        let _observe = GuardObserving::new(symbol, &native);
        take_lean_root_entries_for_test();
        egcl_rt::rooted!(error = run_guarded(&native, symbol, &args, &mut env).unwrap_err());
        let probe = GUARD_PROBE.with(Cell::get);
        assert!(
            probe.body_seen,
            "this exact physical native owner was published"
        );
        assert_eq!((probe.payloads, probe.resumes), (1, 1));
        assert_eq!(
            take_lean_root_entries_for_test(),
            1,
            "actual ordinary root ingress"
        );
        assert_guard_effects(&mut env);
        assert_eq!(owner(&native).deopt_count(), 1);
        egcl_rt::rooted!(
            condition = match error.without_backtrace() {
                EgclError::Signalled(details) => details.condition,
                other => panic!("expected a signalled TYPE-ERROR: {other:?}"),
            }
        );
        assert!(crate::cli::condition_matches_handler(
            &env,
            *condition,
            "TYPE-ERROR"
        ));
        egcl_rt::rooted!(datum_key = crate::cli::resolve_sym("DATUM").unwrap());
        assert_eq!(
            crate::cli::read_slot_value(*condition, *datum_key, &env).unwrap(),
            *bad
        );
    }
    assert_eq!(
        (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()),
        (frame, sp)
    );
    // The failed version can retire. A fresh version proves a native retry.
    let (retry_symbol, retry) = install(&mut env, "SINGLETON-COLD-RETRY", "(x keep bad)", source);
    crate::cli::read_eval_all_env(
        "(setq *singleton-cold-before-count* 0 *singleton-cold-after-count* 0)",
        &mut env,
    )
    .unwrap();
    args[2] = EgclVal::from_fixnum(3);
    let _observe = GuardObserving::new(retry_symbol, &retry);
    take_lean_root_entries_for_test();
    egcl_rt::rooted!(result = run_guarded(&retry, retry_symbol, &args, &mut env).unwrap());
    let probe = GUARD_PROBE.with(Cell::get);
    assert!(probe.body_seen, "the exact fresh native owner executed");
    assert_eq!((probe.payloads, probe.resumes), (0, 0));
    assert_eq!(take_lean_root_entries_for_test(), 1);
    assert_guard_effects(&mut env);
    assert_singleton(*result, *x);
    assert_eq!(owner(&retry).deopt_count(), 0);
    assert_single_value(&env);
    assert_eq!(
        (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()),
        (frame, sp)
    );
}

#[test]
#[ignore = "requires native entry, EGCL_INLINE_ALLOC=0 and EGCL_GC_STRESS=1 EGCL_GC_POISON=1"]
fn ordinary_singleton_list_cons_collects_fresh_primary_at_allocation() {
    assert_eq!(std::env::var("EGCL_INLINE_ALLOC").as_deref(), Ok("0"));
    assert_eq!(std::env::var("EGCL_GC_STRESS").as_deref(), Ok("1"));
    assert_eq!(std::env::var("EGCL_GC_POISON").as_deref(), Ok("1"));
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = bootstrap(&mut env);
    let (symbol, native) = install(
        &mut env,
        "SINGLETON-CONS-ALLOCATION",
        "(x keep)",
        "(locally (declare (notinline singleton-before singleton-after))
          (let ((answer (multiple-value-list (setq x (singleton-before x keep)))))
            (singleton-after answer) answer))",
    );
    assert_singleton_source(&native, true);
    // Warm dispatch before the observer creates a fresh primary after EVAL's
    // allocating setup. The helper then returns that exact slot-zero value.
    ordinary_t2::expect_entry(&native);
    run_native(&native, symbol, &[EgclVal::from_fixnum(76), NIL], &mut env).unwrap();
    egcl_rt::rooted!(expected_primary = NIL);
    let _observe = Observing::new(false, true, false, std::ptr::null_mut());
    PROBE.with(|cell| {
        let mut probe = cell.get();
        probe.expected_primary_home = std::ptr::from_mut(&mut *expected_primary) as usize;
        cell.set(probe);
    });
    egcl_rt::rooted!(
        result = run(symbol, &native, &[EgclVal::from_fixnum(76), NIL], &mut env).unwrap()
    );
    assert!(ordinary_t2::observed_entry());
    assert_eq!(PROBE.with(|slot| slot.get().phase), 2);
    let (primary, tail) = crate::cli::cp(*result);
    assert_eq!(
        primary, *expected_primary,
        "exact producer identity survives native allocation"
    );
    assert_eq!(tail, NIL);
    assert_eq!(crate::cli::cp(primary), (EgclVal::from_fixnum(76), NIL));
    assert_eq!(owner(&native).deopt_count(), 0);
    assert_single_value(&env);
}
