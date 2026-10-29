// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy)]
enum CleanupCase {
    ProtectedNormal,
    ProtectedGuardThrow,
    RunningNormal,
    RunningNormalRetry,
    RunningNormalGuard,
    RunningThrowGuard,
}
impl CleanupCase {
    fn running(self) -> bool {
        matches!(
            self,
            Self::RunningNormal
                | Self::RunningNormalRetry
                | Self::RunningNormalGuard
                | Self::RunningThrowGuard
        )
    }
    fn guard(self) -> bool {
        matches!(
            self,
            Self::ProtectedGuardThrow | Self::RunningNormalGuard | Self::RunningThrowGuard
        )
    }
    fn throws(self) -> bool {
        matches!(self, Self::ProtectedGuardThrow | Self::RunningThrowGuard)
    }
    fn name(self) -> &'static str {
        match self {
            Self::ProtectedNormal => "OSR-PROTECTED-CLEANUP-NORMAL",
            Self::ProtectedGuardThrow => "OSR-PROTECTED-CLEANUP-GUARD-THROW",
            Self::RunningNormal => "OSR-RUNNING-CLEANUP-NORMAL",
            Self::RunningNormalRetry => "OSR-RUNNING-CLEANUP-PREPARATION-RETRY",
            Self::RunningNormalGuard => "OSR-RUNNING-CLEANUP-NORMAL-GUARD",
            Self::RunningThrowGuard => "OSR-RUNNING-CLEANUP-THROW-GUARD",
        }
    }
}

#[derive(Default)]
struct CleanupObservation {
    symbol: u32,
    replacement: Option<Rc<NativeCode>>,
    parent_owner: usize,
    parent_frame: usize,
    parent_segment: usize,
    parent_root: Option<*const EgclVal>,
    parent_moved: bool,
    nested_entry_seen: bool,
    first_owner: usize,
    next_owner: usize,
    frame: usize,
    catch_base: usize,
    token: Option<String>,
    // Only pointers into the test's already rooted locals cross callbacks.
    roots: Option<[*const EgclVal; 4]>,
    running: bool,
    preparation_retry: bool,
    declined: bool,
    baseline_osr_count: u32,
    baseline_frame_map: usize,
    baseline_frame_slots: u16,
    baseline_sp: usize,
    guard: bool,
    entered: bool,
    steps_at_handoff: i64,
    finish_observations: usize,
    collected: bool,
}
thread_local! {
    static CLEANUP_OBSERVATION: RefCell<CleanupObservation> = RefCell::new(CleanupObservation::default());
}

fn assert_cleanup_token(env: &Env) {
    CLEANUP_OBSERVATION.with(|state| {
        let state = state.borrow();
        let roots = state.roots.unwrap();
        assert_eq!(env.catch_stack.len(), state.catch_base + 1);
        let (tag, token) = env.catch_stack.last().unwrap();
        assert_eq!(
            Some(token),
            state.token.as_ref(),
            "cleanup must retain the original catch token"
        );
        assert_eq!(*tag, unsafe { *roots[1] });
    });
}

fn collect_cleanup_roots(env: &mut Env, native_frame: Option<*mut Frame>) -> Result<(), EgclError> {
    let before = CLEANUP_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert!(!state.collected);
        state.roots.unwrap().map(|root| unsafe { (*root).to_raw() })
    });
    // The collector must also update the saved normal MV / pending THROW
    // payload. Its secondary root is checked against the final restored tuple.
    let parent_before = CLEANUP_OBSERVATION.with(|state| {
        state
            .borrow()
            .parent_root
            .map(|root| unsafe { (*root).to_raw() })
    });
    egcl_rt::collect_t0_minor()?;
    assert_cleanup_token(env);
    CLEANUP_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        let roots = state.roots.unwrap();
        for (index, root) in roots.into_iter().enumerate() {
            assert_ne!(
                unsafe { (*root).to_raw() },
                before[index],
                "root {index} must move during this exact OSR/recovery collection"
            );
        }
        if let Some(frame) = native_frame {
            assert_eq!(unsafe { slot_get(frame, 0) }, unsafe { *roots[0] });
            assert_eq!(unsafe { slot_get(frame, 1) }, unsafe { *roots[1] });
            // PAYLOAD (slot 2) is deliberately overwritten in running cleanup.
            assert_eq!(unsafe { slot_get(frame, 3) }, unsafe { *roots[3] });
        }
        if let Some(before) = parent_before {
            let held = unsafe { *state.parent_root.unwrap() };
            assert_ne!(held.to_raw(), before, "suspended parent's root must move");
            assert_eq!(
                unsafe { slot_get(state.parent_frame as *mut Frame, 0) },
                held,
                "suspended mapped parent must receive relocated root"
            );
            state.parent_moved = true;
        }
        state.collected = true;
    });
    Ok(())
}

fn observe_cleanup_osr_poll() {
    let Some((owner, nested)) = native_transfer_entry::child_capture_for_test() else {
        return;
    };
    let frame = egcl_rt::current_stack().fp() as *mut Frame;
    let env_ptr = NATIVE_ENV.with(Cell::get);
    assert!(!frame.is_null() && !env_ptr.is_null());
    let segment = egcl_rt::native_transfer::current_segment() as usize;
    let parent_poll = CLEANUP_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        if owner == state.parent_owner {
            assert!(!nested, "mapped parent is the outer activation");
            if state.parent_frame != 0 {
                assert_eq!(frame as usize, state.parent_frame);
                assert_eq!(segment, state.parent_segment);
            }
            state.parent_frame = frame as usize;
            state.parent_segment = segment;
            return true;
        }
        if state.parent_owner != 0 {
            assert_ne!(state.parent_frame, 0, "mapped parent must execute first");
            assert_eq!(
                unsafe { (*frame).prev_fp } as usize,
                state.parent_frame,
                "child must retain its suspended mapped parent"
            );
            if owner == state.first_owner {
                assert!(nested, "T1 child must enter via a mapped call record");
                assert_eq!(segment, state.parent_segment);
                state.nested_entry_seen = true;
            }
        } else {
            assert!(!nested, "direct case enters an outer activation");
        }
        false
    });
    if parent_poll {
        return;
    }
    let steps = super::super::super::cp(unsafe { slot_get(frame, 0) })
        .1
        .as_fixnum();
    let (publication, collect) = CLEANUP_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        assert!(owner == state.first_owner || owner == state.next_owner);
        if state.frame == 0 {
            state.frame = frame as usize;
        }
        assert_eq!(
            frame as usize, state.frame,
            "OSR must preserve this activation"
        );
        if owner == state.first_owner {
            if steps == 4 && state.replacement.is_some() {
                let env = unsafe { &*env_ptr };
                assert_eq!(env.catch_stack.len(), state.catch_base + 1);
                let (tag, token) = env.catch_stack.last().unwrap();
                assert_eq!(*tag, unsafe { *state.roots.unwrap()[1] });
                state.token = Some(token.clone());
                let cleanup = unsafe { slot_get(frame, 3) };
                assert_eq!(
                    super::super::super::cp(cleanup).0.as_fixnum(),
                    i64::from(state.running),
                    "handoff must occur in the intended protected/cleanup phase"
                );
                if state.preparation_retry {
                    state.baseline_frame_map = unsafe { (*frame).code_info as usize };
                    state.baseline_frame_slots = unsafe { (*frame).num_locals };
                    state.baseline_sp = egcl_rt::current_stack().sp() as usize;
                    native_transfer_entry::fail_next_osr_preparation_for_test();
                }
                state.steps_at_handoff = steps;
                let symbol = state.symbol;
                return (state.replacement.take().map(|code| (symbol, code)), false);
            }
            if state.preparation_retry && state.replacement.is_none() && steps > 4 {
                assert_eq!(steps, 5, "T1 must execute one more iteration before retry");
                // The observer sees ordinary polls as well as loop headers.
                // Repeated polls at effect5 are valid; the consumed hook proves
                // that destination preparation really declined at effect4.
                assert!(
                    !native_transfer_entry::osr_preparation_failure_pending_for_test(),
                    "injected preparation failure must be consumed before T1 advances"
                );
                assert_eq!(osr_entry_count(state.symbol), state.baseline_osr_count);
                assert_eq!(
                    unsafe { (*frame).code_info as usize },
                    state.baseline_frame_map
                );
                assert_eq!(unsafe { (*frame).num_locals }, state.baseline_frame_slots);
                assert_eq!(egcl_rt::current_stack().sp() as usize, state.baseline_sp);
                let env = unsafe { &*env_ptr };
                assert_eq!(env.catch_stack.len(), state.catch_base + 1);
                let (tag, token) = env.catch_stack.last().unwrap();
                assert_eq!(Some(token), state.token.as_ref());
                assert_eq!(*tag, unsafe { *state.roots.unwrap()[1] });
                let cleanup = unsafe { *state.roots.unwrap()[3] };
                assert_eq!(super::super::super::cp(cleanup).0.as_fixnum(), 1);
                state.declined = true;
            }
            return (None, false);
        }
        if state.preparation_retry && !state.entered {
            assert!(
                state.declined,
                "a failed preparation must return to live T1"
            );
            assert_eq!(steps, 5, "the next header retries in this activation");
        }
        state.entered = true;
        (None, !state.guard && !state.collected)
    });
    if let Some((symbol, code)) = publication {
        NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&code)));
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &code);
    }
    if collect {
        collect_cleanup_roots(unsafe { &mut *env_ptr }, Some(frame)).unwrap();
    }
}

/// An interpreted wrapper calls this once after the loop, including when an
/// optimized guard has transferred its live cleanup continuation back to T0.
pub(in crate::cli) fn ordinary_cleanup_osr_observe_for_test(
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    // Unmeasured zero-iteration calls establish the mapped child call cell.
    if CLEANUP_OBSERVATION.with(|state| state.borrow().symbol == 0) {
        return Ok(NIL);
    }
    assert_cleanup_token(env);
    let guard = CLEANUP_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        state.finish_observations += 1;
        assert_eq!(
            state.finish_observations, 1,
            "loop completion must not replay"
        );
        assert_eq!(state.steps_at_handoff, 4);
        let cleanup = unsafe { *state.roots.unwrap()[3] };
        assert_eq!(
            super::super::super::cp(cleanup).0.as_fixnum(),
            i64::from(state.running)
        );
        if !state.guard {
            assert!(state.entered && state.collected);
        }
        state.guard
    });
    if guard {
        collect_cleanup_roots(env, None)?;
    }
    Ok(NIL)
}

struct CleanupFixtureScope {
    observer_symbol: u32,
}
impl Drop for CleanupFixtureScope {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        CLEANUP_OBSERVATION.with(|state| *state.borrow_mut() = CleanupObservation::default());
        profile_unpin(self.observer_symbol);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_protected_cleanup_preserves_normal_values() {
    exercise_cleanup_osr(CleanupCase::ProtectedNormal);
}
#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_protected_cleanup_guard_throw_runs_once() {
    exercise_cleanup_osr(CleanupCase::ProtectedGuardThrow);
}
#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_running_cleanup_preserves_normal_values_and_guard_recovery() {
    for case in [CleanupCase::RunningNormal, CleanupCase::RunningNormalGuard] {
        exercise_cleanup_osr(case);
    }
}
#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_running_cleanup_guard_preserves_pending_throw() {
    exercise_cleanup_osr(CleanupCase::RunningThrowGuard);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_preparation_failure_retains_running_cleanup_then_retries() {
    exercise_cleanup_osr(CleanupCase::RunningNormalRetry);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_child_osr_running_cleanup_preserves_normal_values_and_guard_recovery() {
    for case in [CleanupCase::RunningNormal, CleanupCase::RunningNormalGuard] {
        exercise_cleanup_osr_entry(case, true);
    }
}

fn exercise_cleanup_osr(case: CleanupCase) {
    exercise_cleanup_osr_entry(case, false);
}

fn exercise_cleanup_osr_entry(case: CleanupCase, nested_child: bool) {
    use egcl_compiler::control_scope::{Ownership, ScopeKind, ScopeMap};
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let initial = if case.guard() { "sample" } else { "0" };
    let loop_body = format!(
        "(let ((sum {initial}))
           (dotimes (i n)
             (setq sum (+ sum i))
             (setf (cdr marker) (1+ (cdr marker))))
           {}
           (ordinary-cleanup-osr-observe)
           {})",
        if case.running() {
            "(setf (cdr cleanup) sum)"
        } else {
            "nil"
        },
        if case.running() {
            "(values :discard :cleanup)"
        } else if case.throws() {
            "(throw tag (values payload sum tag))"
        } else {
            "(values payload sum tag)"
        }
    );
    let protected = if case.running() {
        if case.throws() {
            "(throw tag (values payload marker tag))".to_string()
        } else {
            "(values payload marker tag)".to_string()
        }
    } else {
        loop_body.clone()
    };
    let cleanup_body = if case.running() {
        loop_body
    } else {
        "(setf (cdr cleanup) 55) (values :discard :cleanup)".into()
    };
    let source = format!(
        "((setf (car marker) (1+ (car marker)))
          (catch tag
            (unwind-protect {protected}
              (setf (car cleanup) (1+ (car cleanup)))
              (setq payload :discard)
              {cleanup_body})))"
    );
    let name_owned = format!(
        "{}{}",
        case.name(),
        if nested_child { "-CHILD" } else { "" }
    );
    let name = name_owned.as_str();
    super::super::super::read_eval_all_env(
        &format!(
            "(defun ordinary-cleanup-osr-observe () (%ordinary-cleanup-osr-observe-for-test))
         (defun {name} (marker tag payload cleanup n sample) (progn . {source}))"
        ),
        &mut env,
    )
    .unwrap();
    let observer_symbol = egcl_rt::symbols::intern("ORDINARY-CLEANUP-OSR-OBSERVE");
    profile_pin(observer_symbol);
    let _fixture = CleanupFixtureScope { observer_symbol };
    egcl_rt::rooted!(
        params = reader::read_from_string("(marker tag payload cleanup n sample)")
            .unwrap()
            .0
    );
    egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
    let scopes = ScopeMap::analyze_function(&body).unwrap();
    let intended_scope = |scope: &egcl_compiler::control_scope::ControlScope| {
        scope.ownership == Ownership::Local
            && if case.running() {
                matches!(scope.kind, ScopeKind::Cleanup { .. })
            } else {
                matches!(scope.kind, ScopeKind::Unwind { .. })
            }
    };
    let sum_sites: Vec<_> = body
        .code
        .windows(4)
        .enumerate()
        .filter_map(|(start, window)| {
            let [
                Instr::LoadLocal(sum),
                Instr::LoadLocal(index),
                Instr::CallNamed { sym, nargs: 2 },
                Instr::StoreLocal(destination),
            ] = window
            else {
                return None;
            };
            let bcp = (start + 2) as u32;
            (sum == destination
                && sum != index
                && egcl_rt::symbols::symbol_name(*sym).as_deref() == Some("+")
                && scopes
                    .before(bcp)
                    .is_some_and(|active| active.iter().any(&intended_scope)))
            .then_some(bcp)
        })
        .collect();
    let [sum_bcp] = sum_sites.as_slice() else {
        panic!("expected exactly one accumulator update in intended cleanup phase: {sum_sites:?}")
    };
    registry_put(symbol, Arc::clone(&body));
    let first = try_promote_to_t1(symbol).expect("ordinary mapped cleanup T1");
    let NativeCodeStorage::Mapped(first_mapped) = &first._storage else {
        panic!("mapped T1 required")
    };
    let input = snapshot_t2_input(symbol, 0).expect("ordinary worker snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("ordinary worker must optimize the protected/running cleanup loop");
    assert!(
        artifact
            .speculations
            .iter()
            .any(|(bcp, kind)| bcp == sum_bcp && matches!(kind, SpecType::Fixnum)),
        "real worker numeric speculation, not relabeled baseline code"
    );
    let next = install_t2_completion(T2Completion {
        panicked: false,
        _permit: None,
        dependencies: Vec::new(),
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("ordinary mapped cleanup T2 installation");
    let NativeCodeStorage::Mapped(next_mapped) = &next._storage else {
        panic!("mapped T2 required")
    };
    assert!(next.is_t2 && Arc::ptr_eq(next.body.as_ref().unwrap(), &body));
    assert!(
        next.osr_entries.keys().any(|bcp| scopes
            .before(*bcp)
            .is_some_and(|active| active.iter().any(&intended_scope))),
        "OSR entry must exist in the exact protected/running cleanup phase"
    );
    let next_owner = Rc::as_ptr(next_mapped) as usize;
    let next_code = Rc::clone(next_mapped);
    let before_deopts = next_code.deopt_count();
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&first)));
    mark_fresh_promotion(symbol);
    publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &first);
    let parent = if nested_child {
        let parent_name = format!("{name}-PARENT");
        let parent_source = format!(
            "((dotimes (j 1) (setf (car held) (1+ (car held))))
              (multiple-value-bind (answer marker-result caught-tag)
                  ({name} marker tag payload cleanup n sample)
                (setf (car held) (+ (car held) 10))
                (values answer marker-result caught-tag)))"
        );
        super::super::super::read_eval_all_env(
            &format!("(defun {parent_name} (held marker tag payload cleanup n sample) (progn . {parent_source}))"),
            &mut env,
        ).unwrap();
        egcl_rt::rooted!(
            parent_params = reader::read_from_string("(held marker tag payload cleanup n sample)")
                .unwrap()
                .0
        );
        egcl_rt::rooted!(parent_forms = reader::read_from_string(&parent_source).unwrap().0);
        let parent_symbol = egcl_rt::symbols::intern(&parent_name);
        let parent_body = Arc::new(
            compile_function(
                &parent_name,
                *parent_params,
                *parent_forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        registry_put(parent_symbol, parent_body);
        let parent = try_promote_to_t1(parent_symbol).expect("ordinary mapped cleanup parent");
        assert!(!parent.is_t2 && matches!(parent._storage, NativeCodeStorage::Mapped(_)));
        egcl_rt::rooted!(warm_held = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
        egcl_rt::rooted!(
            warm_marker =
                super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
        );
        egcl_rt::rooted!(
            warm_cleanup =
                super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
        );
        for _ in 0..2 {
            run_native(
                &parent,
                parent_symbol,
                &[
                    *warm_held,
                    *warm_marker,
                    NIL,
                    NIL,
                    *warm_cleanup,
                    EgclVal::from_fixnum(0),
                    EgclVal::from_fixnum(0),
                ],
                &mut env,
            )
            .unwrap();
        }
        Some((parent_symbol, parent))
    } else {
        None
    };
    let parent_owner = parent.as_ref().map_or(0, |(_, parent)| {
        let NativeCodeStorage::Mapped(code) = &parent._storage else {
            unreachable!()
        };
        Rc::as_ptr(code) as usize
    });
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(tag = super::super::super::arena_cons(EgclVal::from_fixnum(79), NIL));
    egcl_rt::rooted!(payload = super::super::super::arena_cons(EgclVal::from_fixnum(73), NIL));
    egcl_rt::rooted!(
        cleanup = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(
        held = if nested_child {
            super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL)
        } else {
            NIL
        }
    );
    let stack = egcl_rt::current_stack();
    let before_stack = (stack.fp(), stack.sp());
    let before_scopes = (env.catch_stack.len(), env.handlers.len());
    let before_depth = NATIVE_DEPTH.with(Cell::get);
    let before_osr = osr_entry_count(symbol);
    if matches!(case, CleanupCase::RunningNormalRetry) {
        // The optimized entry must reserve guard recovery before touching the
        // protected form. A T1 entry has no guards and would not hit this seam.
        native_transfer_entry::fail_next_guard_reservation_for_test();
        let failed = run_native(
            &next,
            symbol,
            &[
                *marker,
                *tag,
                *payload,
                *cleanup,
                EgclVal::from_fixnum(40),
                EgclVal::from_single_float(1.25),
            ],
            &mut env,
        );
        assert!(
            matches!(failed, Err(EgclError::Oom)),
            "reservation failure must propagate Oom: {failed:?}"
        );
        let zero_pair = (EgclVal::from_fixnum(0), EgclVal::from_fixnum(0));
        assert_eq!(super::super::super::cp(*marker), zero_pair);
        assert_eq!(super::super::super::cp(*cleanup), zero_pair);
        assert_eq!((stack.fp(), stack.sp()), before_stack);
        assert_eq!((env.catch_stack.len(), env.handlers.len()), before_scopes);
        assert_eq!(NATIVE_DEPTH.with(Cell::get), before_depth);
        assert_eq!(osr_entry_count(symbol), before_osr);
        assert_eq!(next_code.deopt_count(), before_deopts);
    }
    CLEANUP_OBSERVATION.with(|state| {
        *state.borrow_mut() = CleanupObservation {
            symbol,
            parent_owner,
            parent_root: nested_child.then(|| std::ptr::from_ref(&*held)),
            replacement: Some(next),
            first_owner: Rc::as_ptr(first_mapped) as usize,
            next_owner,
            catch_base: before_scopes.0,
            roots: Some([
                std::ptr::from_ref(&*marker),
                std::ptr::from_ref(&*tag),
                std::ptr::from_ref(&*payload),
                std::ptr::from_ref(&*cleanup),
            ]),
            running: case.running(),
            preparation_retry: matches!(case, CleanupCase::RunningNormalRetry),
            baseline_osr_count: before_osr,
            guard: case.guard(),
            ..CleanupObservation::default()
        }
    });
    native_transfer_entry::root_publication::OBSERVE
        .with(|hook| hook.set(Some(observe_cleanup_osr_poll)));
    egcl_rt::rooted!(
        result = if let Some((parent_symbol, parent)) = &parent {
            run_native(
                parent,
                *parent_symbol,
                &[
                    *held,
                    *marker,
                    *tag,
                    *payload,
                    *cleanup,
                    EgclVal::from_fixnum(40),
                    EgclVal::from_single_float(1.25),
                ],
                &mut env,
            )
        } else {
            run_native(
                &first,
                symbol,
                &[
                    *marker,
                    *tag,
                    *payload,
                    *cleanup,
                    EgclVal::from_fixnum(40),
                    EgclVal::from_single_float(1.25),
                ],
                &mut env,
            )
        }
        .unwrap()
    );
    let sum = if case.guard() {
        EgclVal::from_single_float(781.25)
    } else {
        EgclVal::from_fixnum(780)
    };
    assert_eq!(
        *result, *payload,
        "restore the original protected result, not cleanup values"
    );
    assert!(env.mv_active);
    assert_eq!(
        env.mv.as_slice(),
        &[*payload, if case.running() { *marker } else { sum }, *tag]
    );
    assert_eq!(
        super::super::super::cp(*marker),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(40)),
        "preloop/loop effects must not replay during OSR or guard recovery"
    );
    assert_eq!(
        super::super::super::cp(*cleanup),
        (
            EgclVal::from_fixnum(1),
            if case.running() {
                sum
            } else {
                EgclVal::from_fixnum(55)
            }
        ),
        "cleanup executes exactly once"
    );
    assert_eq!(
        super::super::super::cp(*payload),
        (EgclVal::from_fixnum(73), NIL)
    );
    assert_eq!(
        super::super::super::cp(*tag),
        (EgclVal::from_fixnum(79), NIL)
    );
    if nested_child {
        assert_eq!(
            super::super::super::cp(*held),
            (EgclVal::from_fixnum(11), NIL),
            "parent must resume once and use its relocated root"
        );
    }
    assert_eq!(osr_entry_count(symbol), before_osr + 1);
    assert_eq!(
        next_code.deopt_count(),
        before_deopts + u32::from(case.guard())
    );
    CLEANUP_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert_eq!(state.steps_at_handoff, 4);
        assert_eq!(state.finish_observations, 1);
        assert!(state.replacement.is_none() && state.collected && state.token.is_some());
        assert_eq!(state.declined, state.preparation_retry);
        if nested_child {
            assert!(state.nested_entry_seen && state.parent_moved);
            assert_ne!(state.parent_frame, 0);
            assert_ne!(state.parent_segment, 0);
            assert_ne!(state.parent_frame, state.frame);
        }
        if !case.guard() {
            assert!(state.entered, "actual optimized loop execution required");
        }
    });
    assert_eq!((stack.fp(), stack.sp()), before_stack);
    assert_eq!((env.catch_stack.len(), env.handlers.len()), before_scopes);
    assert_eq!(NATIVE_DEPTH.with(Cell::get), before_depth);
}
