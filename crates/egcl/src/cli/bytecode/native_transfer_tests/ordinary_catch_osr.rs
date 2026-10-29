// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::{Cell, RefCell};

#[derive(Default)]
struct CatchObservation {
    symbol: u32,
    replacement: Option<Rc<NativeCode>>,
    first_owner: usize,
    next_owner: usize,
    parent_owner: usize,
    parent_frame: usize,
    parent_segment: usize,
    nested_entry_seen: bool,
    parent_moved: bool,
    parent_root: Option<*const EgclVal>,
    frame: usize,
    catch_base: usize,
    token: Option<String>,
    // Pointers into the fixture's rooted locals, not unrooted heap values.
    tag_root: Option<*const EgclVal>,
    marker_root: Option<*const EgclVal>,
    steps_at_handoff: i64,
    entered: bool,
    moved_at_t2_poll: bool,
    moved_during_recovery: bool,
    finish_observations: usize,
    fail_guard: bool,
}

thread_local! {
    static CATCH_OBSERVATION: RefCell<CatchObservation> = RefCell::new(CatchObservation::default());
}

fn assert_live_catch(env: &Env) {
    CATCH_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert_eq!(env.catch_stack.len(), state.catch_base + 1);
        let (tag, token) = env.catch_stack.last().unwrap();
        assert_eq!(
            Some(token),
            state.token.as_ref(),
            "retain the original T1 catch token"
        );
        assert_eq!(
            *tag,
            unsafe { *state.tag_root.unwrap() },
            "the active catch tag follows moving GC"
        );
    });
}

fn observe_local_catch_poll() {
    let Some((owner, nested)) = native_transfer_entry::child_capture_for_test() else {
        return;
    };
    let frame = egcl_rt::current_stack().fp() as *mut Frame;
    let env_ptr = NATIVE_ENV.with(Cell::get);
    assert!(!frame.is_null() && !env_ptr.is_null());
    let segment = egcl_rt::native_transfer::current_segment() as usize;
    let parent_poll = CATCH_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        if owner == state.parent_owner {
            assert!(!nested, "the mapped parent is the outer activation");
            if state.parent_frame != 0 {
                assert_eq!(frame as usize, state.parent_frame);
                assert_eq!(segment, state.parent_segment);
            }
            state.parent_frame = frame as usize;
            state.parent_segment = segment;
            return true;
        }
        assert!(owner == state.first_owner || owner == state.next_owner);
        if state.parent_owner != 0 {
            assert_ne!(
                state.parent_frame, 0,
                "parent native poll must execute first"
            );
            assert_eq!(
                unsafe { (*frame).prev_fp } as usize,
                state.parent_frame,
                "the child retains its suspended mapped parent"
            );
            if owner == state.first_owner {
                assert!(
                    nested,
                    "the T1 child must enter through a mapped call record"
                );
                assert_eq!(segment, state.parent_segment);
                state.nested_entry_seen = true;
            }
        } else {
            assert!(!nested, "this case enters the child directly");
        }
        false
    });
    if parent_poll {
        return;
    }
    let steps = super::super::super::cp(unsafe { slot_get(frame, 0) })
        .1
        .as_fixnum();
    // Straight-line polls before CATCH establishment are not the handoff site.
    let live_catch = unsafe { &*env_ptr }
        .catch_stack
        .last()
        .map(|(tag, token)| (*tag, token.clone()));
    let (publication, collect) = CATCH_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        assert!(owner == state.first_owner || owner == state.next_owner);
        let Some((tag, token)) = live_catch else {
            assert!(
                (steps == 0 && state.token.is_none())
                    || (steps == 40 && state.finish_observations == 1),
                "a catch-free poll must precede establishment or follow completed THROW"
            );
            return (None, false);
        };
        assert_eq!(tag, unsafe { *state.tag_root.unwrap() });
        assert_eq!(unsafe { &*env_ptr }.catch_stack.len(), state.catch_base + 1);
        if let Some(saved) = &state.token {
            assert_eq!(&token, saved);
        } else {
            assert_eq!(owner, state.first_owner);
            state.token = Some(token);
            state.frame = frame as usize;
        }
        assert_eq!(
            state.frame, frame as usize,
            "handoff must retain the managed frame"
        );
        if owner == state.first_owner {
            if steps == 4 && state.replacement.is_some() {
                state.steps_at_handoff = steps;
                let symbol = state.symbol;
                return (state.replacement.take().map(|code| (symbol, code)), false);
            }
            return (None, false);
        }
        state.entered = true;
        // Invoke speculation can retain its guard inside the loop body rather
        // than at the OSR entry. Keep the nursery live for the recovery probe.
        (None, !state.fail_guard && !state.moved_at_t2_poll)
    });
    if let Some((symbol, code)) = publication {
        NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&code)));
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &code);
    }
    if collect {
        // Only raw comparison addresses survive this exact published collection.
        let before_tag = unsafe { slot_get(frame, 1) }.to_raw();
        let before_marker = unsafe { slot_get(frame, 0) }.to_raw();
        let parent_before = parent_root_before_collection();
        // Stress may have moved the parent and catch roots out of the nursery.
        // Keep the relocation assertions below and collect Survivor space too.
        egcl_rt::gc::full_gc().unwrap();
        assert_parent_root_moved(parent_before);
        let after_tag = unsafe { slot_get(frame, 1) }.to_raw();
        let after_marker = unsafe { slot_get(frame, 0) }.to_raw();
        assert_ne!(
            after_tag, before_tag,
            "the live catch tag must move at the T2 poll"
        );
        assert_ne!(
            after_marker, before_marker,
            "the live marker must move at the same poll"
        );
        assert_live_catch(unsafe { &*env_ptr });
        CATCH_OBSERVATION.with(|state| {
            let mut state = state.borrow_mut();
            assert_eq!(after_tag, unsafe { *state.tag_root.unwrap() }.to_raw());
            assert_eq!(
                after_marker,
                unsafe { *state.marker_root.unwrap() }.to_raw()
            );
            state.moved_at_t2_poll = true;
        });
    }
}

fn parent_root_before_collection() -> Option<u64> {
    CATCH_OBSERVATION.with(|state| {
        let state = state.borrow();
        state.parent_root.map(|root| unsafe { (*root).to_raw() })
    })
}

fn assert_parent_root_moved(before: Option<u64>) {
    if let Some(before) = before {
        CATCH_OBSERVATION.with(|state| {
            let mut state = state.borrow_mut();
            let held = unsafe { *state.parent_root.unwrap() };
            assert_ne!(
                held.to_raw(),
                before,
                "the suspended parent's root must move"
            );
            assert_eq!(
                unsafe { slot_get(state.parent_frame as *mut Frame, 0) },
                held,
                "the suspended mapped frame must receive its relocated root"
            );
            state.parent_moved = true;
        });
    }
}

/// Called through an interpreted source wrapper so it sees the actual resumed
/// T0 environment as well as the ordinary mapped caller's dynamic catch stack.
pub(in crate::cli) fn local_catch_osr_observe_for_test(
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    // The two unmeasured calls only resolve the published child call cell.
    // Measured invocations always install a nonzero observation symbol below.
    if CATCH_OBSERVATION.with(|state| state.borrow().symbol == 0) {
        return Ok(NIL);
    }
    assert_live_catch(env);
    let recovery = CATCH_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        state.finish_observations += 1;
        assert_eq!(
            state.finish_observations, 1,
            "post-loop effects must not replay"
        );
        assert_eq!(state.steps_at_handoff, 4);
        if state.fail_guard {
            assert!(
                native_transfer_entry::child_capture_for_test().is_none(),
                "guard recovery must run T0 with the native capture paused"
            );
        } else {
            assert!(state.entered && state.moved_at_t2_poll);
        }
        state.fail_guard
    });
    if recovery {
        let (before_tag, before_marker) = CATCH_OBSERVATION.with(|state| {
            let state = state.borrow();
            (
                unsafe { *state.tag_root.unwrap() }.to_raw(),
                unsafe { *state.marker_root.unwrap() }.to_raw(),
            )
        });
        // No RefCell borrow or copied heap value crosses the collection.
        let parent_before = parent_root_before_collection();
        // Stress may have moved the parent and catch roots out of the nursery.
        // Keep the relocation assertions below and collect Survivor space too.
        egcl_rt::gc::full_gc()?;
        assert_parent_root_moved(parent_before);
        assert_live_catch(env);
        CATCH_OBSERVATION.with(|state| {
            let mut state = state.borrow_mut();
            assert_ne!(
                unsafe { *state.tag_root.unwrap() }.to_raw(),
                before_tag,
                "the original catch tag must move while guard recovery owns it"
            );
            assert_ne!(
                unsafe { *state.marker_root.unwrap() }.to_raw(),
                before_marker
            );
            state.moved_during_recovery = true;
        });
    }
    Ok(NIL)
}

struct CatchObserverPin(u32);
impl Drop for CatchObserverPin {
    fn drop(&mut self) {
        profile_unpin(self.0);
    }
}

struct CatchObservationScope;
impl Drop for CatchObservationScope {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        CATCH_OBSERVATION.with(|state| *state.borrow_mut() = CatchObservation::default());
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_retains_local_catch_token_and_throw_values() {
    exercise_local_catch_osr(false, false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_guard_recovery_retains_local_catch_token() {
    exercise_local_catch_osr(true, false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_child_osr_retains_local_catch_token_and_throw_values() {
    exercise_local_catch_osr(false, true);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_child_osr_guard_recovery_retains_local_catch_token() {
    exercise_local_catch_osr(true, true);
}

fn exercise_local_catch_osr(fail_guard: bool, nested_child: bool) {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = match (fail_guard, nested_child) {
        (false, false) => "LOCAL-CATCH-OSR-NORMAL",
        (true, false) => "LOCAL-CATCH-OSR-GUARD",
        (false, true) => "LOCAL-CATCH-OSR-CHILD",
        (true, true) => "LOCAL-CATCH-OSR-CHILD-GUARD",
    };
    let initial = if fail_guard { "sample" } else { "0" };
    let source = format!(
        "((setf (car marker) (1+ (car marker)))
          (catch tag
            (let ((sum {initial}))
              (dotimes (i n)
                (setq sum (+ sum i))
                (setf (cdr marker) (1+ (cdr marker))))
              (local-catch-osr-observe)
              (throw tag (values marker sum tag)))))"
    );
    super::super::super::read_eval_all_env(
        &format!(
            "(defun local-catch-osr-observe () (%local-catch-osr-observe-for-test))
                  (defun {name} (marker tag n sample) (progn . {source}))"
        ),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(marker tag n sample)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
    let observer_symbol = egcl_rt::symbols::intern("LOCAL-CATCH-OSR-OBSERVE");
    profile_pin(observer_symbol);
    let _observer_pin = CatchObserverPin(observer_symbol);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
    let source_scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body).unwrap();
    // Identify SETQ's two-local accumulator update, not a macro-expanded 1+
    // or DOTIMES' hidden counter increment (which has a constant operand).
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
                && source_scopes.before(bcp).is_some_and(|active| {
                    active.iter().any(|scope| {
                        scope.ownership == egcl_compiler::control_scope::Ownership::Local
                            && matches!(
                                scope.kind,
                                egcl_compiler::control_scope::ScopeKind::Catch { .. }
                            )
                    })
                }))
            .then_some(bcp)
        })
        .collect();
    let [sum_bcp] = sum_sites.as_slice() else {
        panic!("expected exactly one protected two-local accumulator update, found {sum_sites:?}")
    };
    let sum_bcp = *sum_bcp;
    registry_put(symbol, Arc::clone(&body));
    let first = try_promote_to_t1(symbol).expect("ordinary native CATCH baseline");
    let NativeCodeStorage::Mapped(first_mapped) = &first._storage else {
        panic!("CATCH baseline must use the mapped native ABI")
    };
    let input = snapshot_t2_input(symbol, 0).expect("ordinary worker snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("ordinary worker must optimize arithmetic inside local CATCH");
    assert!(
        artifact
            .speculations
            .iter()
            .any(|(bcp, kind)| *bcp == sum_bcp && matches!(kind, SpecType::Fixnum)),
        "the genuine worker artifact must speculate the protected accumulator addition"
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
    .expect("ordinary worker must install mapped T2 for the protected loop");
    let NativeCodeStorage::Mapped(next_mapped) = &next._storage else {
        panic!("optimized CATCH must retain the mapped native ABI")
    };
    assert!(next.is_t2);
    assert!(Arc::ptr_eq(next.body.as_ref().unwrap(), &body));
    let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body).unwrap();
    assert!(
        next.osr_entries
            .keys()
            .any(|bcp| scopes.before(*bcp).is_some_and(|active| {
                active.iter().any(|scope| {
                    matches!(
                        scope.kind,
                        egcl_compiler::control_scope::ScopeKind::Catch { .. }
                    )
                })
            })),
        "optimized OSR entry must lie inside the locally registered catch"
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
              (multiple-value-bind (answer sum caught-tag) ({name} marker tag n sample)
                (setf (car held) (+ (car held) 10))
                (values answer sum caught-tag)))"
        );
        super::super::super::read_eval_all_env(
            &format!("(defun {parent_name} (held marker tag n sample) (progn . {parent_source}))"),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(
            parent_params = reader::read_from_string("(held marker tag n sample)")
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
        let parent = try_promote_to_t1(parent_symbol).expect("ordinary mapped parent");
        assert!(!parent.is_t2);
        assert!(matches!(parent._storage, NativeCodeStorage::Mapped(_)));
        // Establish the actual mapped child call cell before measured entry.
        egcl_rt::rooted!(warm_held = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
        egcl_rt::rooted!(
            warm_marker =
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

    // Parsing, compilation and publication finish before creating moving roots.
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(tag = super::super::super::arena_cons(EgclVal::from_fixnum(73), NIL));
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
    CATCH_OBSERVATION.with(|state| {
        *state.borrow_mut() = CatchObservation {
            symbol,
            replacement: Some(next),
            first_owner: Rc::as_ptr(first_mapped) as usize,
            next_owner,
            parent_owner,
            parent_root: nested_child.then(|| std::ptr::from_ref(&*held)),
            catch_base: before_scopes.0,
            tag_root: Some(std::ptr::from_ref(&*tag)),
            marker_root: Some(std::ptr::from_ref(&*marker)),
            fail_guard,
            ..CatchObservation::default()
        };
    });
    let _observation = CatchObservationScope;
    native_transfer_entry::root_publication::OBSERVE
        .with(|hook| hook.set(Some(observe_local_catch_poll)));
    egcl_rt::rooted!(
        result = if let Some((parent_symbol, parent)) = &parent {
            run_native(
                parent,
                *parent_symbol,
                &[
                    *held,
                    *marker,
                    *tag,
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
                    EgclVal::from_fixnum(40),
                    EgclVal::from_single_float(1.25),
                ],
                &mut env,
            )
        }
        .unwrap()
    );
    assert_eq!(*result, *marker);
    assert_eq!(
        super::super::super::cp(*marker),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(40))
    );
    assert_eq!(
        super::super::super::cp(*tag),
        (EgclVal::from_fixnum(73), NIL)
    );
    assert!(env.mv_active);
    assert_eq!(
        env.mv.as_slice(),
        &[
            *marker,
            if fail_guard {
                EgclVal::from_single_float(781.25)
            } else {
                EgclVal::from_fixnum(780)
            },
            *tag,
        ]
    );
    assert_eq!(osr_entry_count(symbol), before_osr + 1);
    assert_eq!(
        next_code.deopt_count(),
        before_deopts + u32::from(fail_guard)
    );
    CATCH_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert_eq!(state.steps_at_handoff, 4);
        assert_eq!(state.finish_observations, 1);
        assert!(state.token.is_some() && state.replacement.is_none());
        assert!(fail_guard || state.entered);
        assert_eq!(state.moved_at_t2_poll, !fail_guard);
        assert_eq!(state.moved_during_recovery, fail_guard);
        assert_eq!(state.nested_entry_seen, nested_child);
        assert_eq!(state.parent_moved, nested_child);
    });
    if nested_child {
        assert_eq!(
            super::super::super::cp(*held),
            (EgclVal::from_fixnum(11), NIL),
            "native parent pre-call and post-call effects each execute once"
        );
    }
    assert_eq!((env.catch_stack.len(), env.handlers.len()), before_scopes);
    assert_eq!((stack.fp(), stack.sp()), before_stack);
    assert_eq!(NATIVE_DEPTH.with(Cell::get), before_depth);
}
