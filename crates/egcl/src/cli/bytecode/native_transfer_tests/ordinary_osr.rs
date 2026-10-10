// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::RefCell;

#[derive(Default)]
struct Observation {
    symbol: u32,
    replacement: Option<Rc<NativeCode>>,
    // Points into the fixture's rooted local; never holds an unrooted value.
    replacement_function: Option<*const EgclVal>,
    first_owner: usize,
    next_owner: usize,
    frame: usize,
    entered: bool,
    steps_at_handoff: i64,
    argument_checks: usize,
}

thread_local! {
    static OBSERVATION: RefCell<Observation> = RefCell::new(Observation::default());
}

fn observe_osr() {
    let Some((owner, nested)) = native_transfer_entry::child_capture_for_test() else {
        return;
    };
    assert!(!nested);
    let frame = egcl_rt::current_stack().fp() as usize;
    // MARKER is the first canonical parameter; the mutation is a real Lisp
    // effect, independent of whether T1 keeps its loop counter in a register.
    let marker = unsafe { slot_get(frame as *mut Frame, 0) };
    let steps = super::super::super::cp(marker).1.as_fixnum();
    let publication = OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        if owner == state.first_owner {
            if state.frame == 0 {
                state.frame = frame;
            }
            if steps >= 4 && state.replacement.is_some() {
                state.steps_at_handoff = steps;
                return state
                    .replacement
                    .take()
                    .map(|code| (state.symbol, code, state.replacement_function));
            }
        } else if owner == state.next_owner {
            assert_eq!(
                frame, state.frame,
                "OSR must retain this managed activation"
            );
            state.entered = true;
        } else {
            panic!("unexpected mapped owner at loop poll");
        }
        None
    });
    if let Some((symbol, code, replacement_function)) = publication {
        if let Some(function) = replacement_function {
            // No GC allocation occurs while publishing the selected rooted
            // callable and its retained compiled body as one test operation.
            egcl_rt::symbols::set_symbol_function(symbol, unsafe { *function });
            registry_put(symbol, Arc::clone(code.body.as_ref().unwrap()));
        }
        NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&code)));
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &code);
    } else if OBSERVATION.with(|state| state.borrow().entered) {
        egcl_rt::collect_t0_minor().unwrap();
        // Read the installed native DWARF recipe directly: a recorded outer
        // Rust/interpreter call must not mask a clobbered managed argument home.
        let frame = unsafe { &*(frame as *const Frame) };
        let code_info = unsafe { frame.code_info.as_ref() }.expect("T2 native metadata");
        egcl_rt::rooted!(
            arguments = code_info
                .original_arguments(unsafe { frame.locals() })
                .expect("immutable original arguments remain available after OSR")
        );
        assert_eq!(arguments.len(), 4);
        assert!(
            arguments[3].is_string(),
            "dead heap argument must not become NIL"
        );
        assert_eq!(arguments[3].as_string(), "dead OSR argument");
        OBSERVATION.with(|state| state.borrow_mut().argument_checks += 1);
    }
}

struct ObserveScope;
impl Drop for ObserveScope {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        OBSERVATION.with(|state| *state.borrow_mut() = Observation::default());
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_t1_enters_t2_in_the_current_activation_without_replay() {
    exercise_ordinary_osr(false, false, false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_preserves_inherited_handler_catch_and_cleanup() {
    exercise_ordinary_osr(true, false, false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_entry_guard_resumes_after_committed_effects() {
    exercise_ordinary_osr(false, true, false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_declines_replacement_definition_during_live_t1() {
    exercise_ordinary_osr(false, false, true);
}

fn exercise_ordinary_osr(inherited_scopes: bool, fail_entry_guard: bool, replace_definition: bool) {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    if inherited_scopes {
        super::super::super::read_eval_all_env(
            "(setq *osr-scope-log* nil *osr-finish-values* nil)
             (defun ordinary-osr-finish (marker sum sample)
               (setq *osr-finish-values* (list marker sum sample))
               (error \"OSR inherited handler\"))",
            &mut env,
        )
        .unwrap();
    }
    let finish = if inherited_scopes {
        "(ordinary-osr-finish marker sum sample)"
    } else {
        "(values marker sum sample)"
    };
    // A cold T2 worker speculates FIXNUM at the accumulator addition. Its
    // loop-phi guard split records the sum proof as an OSR entry check. T1
    // computes the generic float sum before publication, so that entry check
    // must resume T0 at the current loop header without replaying effects.
    let initial_sum = if fail_entry_guard { "sample" } else { "0" };
    let forms = format!(
        "((setf (car marker) (1+ (car marker)))
        (let ((sum {initial_sum}))
          (dotimes (i n)
            (setq sum (+ sum i))
            (setf (cdr marker) (1+ (cdr marker))))
          {finish}))"
    );
    super::super::super::read_eval_all_env(
        &format!("(defun ordinary-osr (marker n sample unused) (progn . {forms}))"),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(
        params = reader::read_from_string("(marker n sample unused)")
            .unwrap()
            .0
    );
    let replacement_source = forms.replace(
        "(values marker sum sample)",
        "(values marker (+ sum 1000) sample)",
    );
    egcl_rt::rooted!(forms = reader::read_from_string(&forms).unwrap().0);
    let symbol = egcl_rt::symbols::intern("ORDINARY-OSR");
    let body =
        Arc::new(compile_function("ORDINARY-OSR", *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let first = try_promote_to_t1(symbol).expect("ordinary T1 installation");
    let NativeCodeStorage::Mapped(first_mapped) = &first._storage else {
        panic!("ordinary mapped T1");
    };
    egcl_rt::rooted!(original_function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    egcl_rt::rooted!(replacement_function = NIL);
    let next_body = if replace_definition {
        // Only the post-loop result changes: both bodies have an OSR entry at
        // the same bytecode header, so mismatched PCs cannot mask the test.
        super::super::super::read_eval_all_env(
            &format!(
                "(defun ordinary-osr (marker n sample unused) (progn . {replacement_source}))"
            ),
            &mut env,
        )
        .unwrap();
        *replacement_function = egcl_rt::symbols::symbol_function(symbol).unwrap();
        assert_ne!(*replacement_function, *original_function);
        egcl_rt::rooted!(
            replacement_forms = reader::read_from_string(&replacement_source).unwrap().0
        );
        let replacement_body = Arc::new(
            compile_function(
                "ORDINARY-OSR",
                *params,
                *replacement_forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        registry_put(symbol, Arc::clone(&replacement_body));
        replacement_body
    } else {
        Arc::clone(&body)
    };
    let input = snapshot_t2_input(symbol, 0).expect("ordinary worker snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("ordinary worker artifact");
    let next = install_t2_completion(T2Completion {
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("ordinary T2 installation");
    let NativeCodeStorage::Mapped(next_mapped) = &next._storage else {
        panic!("ordinary T2 loop must retain mapped OSR entries");
    };
    assert!(next.is_t2);
    assert!(!next.osr_entries.is_empty());
    assert!(Arc::ptr_eq(next.body.as_ref().unwrap(), &next_body));
    assert_eq!(Arc::ptr_eq(&body, &next_body), !replace_definition);
    if replace_definition {
        assert!(next.osr_entries.keys().any(|header| {
            body.code.iter().enumerate().any(|(index, instruction)| {
                matches!(instruction, Instr::Br(target) | Instr::BrIfFalse(target)
                    | Instr::BrIfTrue(target) | Instr::Go { target_bcp: target, .. }
                    if target == header && index > *header as usize)
            })
        }), "replacement has an OSR header at an original loop backedge");

    }

    let next_owner = Rc::as_ptr(next_mapped) as usize;
    let next_code = Rc::clone(next_mapped);
    let before_deopts = next_code.deopt_count();
    // Deterministic worker completion: publish the already-built replacement
    // only after the single ordinary T1 activation commits four loop iterations.
    if replace_definition {
        egcl_rt::symbols::set_symbol_function(symbol, *original_function);
        registry_put(symbol, Arc::clone(&body));
    }
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&first)));
    mark_fresh_promotion(symbol);
    publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &first);
    OBSERVATION.with(|state| {
        *state.borrow_mut() = Observation {
            symbol,
            replacement: Some(next),
            replacement_function: replace_definition
                .then(|| std::ptr::from_ref(&*replacement_function)),
            first_owner: Rc::as_ptr(first_mapped) as usize,
            next_owner,
            ..Observation::default()
        }
    });
    let _observation = ObserveScope;
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(Some(observe_osr)));
    // Parse before allocating the nursery values whose movement we observe.
    egcl_rt::rooted!(
        inherited_call = reader::read_from_string(
            "(catch :osr-inherited
               (unwind-protect
                 (handler-bind
                   ((error (lambda (condition)
                             (declare (ignore condition))
                             (setq *osr-scope-log* (cons :handler *osr-scope-log*))
                             (throw :osr-inherited
                               (values (first *osr-finish-values*)
                                       (second *osr-finish-values*)
                                       (third *osr-finish-values*))))))
                   (unwind-protect
                     (progn (ordinary-osr osr-marker 40 1.25 osr-unused)
                            (setq *osr-scope-log* (cons :after-call *osr-scope-log*)))
                     (setq *osr-scope-log* (cons :inner-cleanup *osr-scope-log*))
                     (values :wrong :cleanup)))
                 (setq *osr-scope-log* (cons :outer-cleanup *osr-scope-log*))
                 (values :wrong)))"
        )
        .unwrap()
        .0
    );
    egcl_rt::rooted!(
        marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    // UNUSED never occurs in the body, so its OSR header SSA value is dead.
    // Its original positional home must nevertheless remain debugger-visible.
    egcl_rt::rooted!(unused = super::super::super::arena_str("dead OSR argument"));
    if inherited_scopes {
        env.define_local("OSR-MARKER", *marker);
        env.define_local("OSR-UNUSED", *unused);
    }
    let unused_address = unused.to_raw();
    let address = marker.to_raw();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp());
    let before_scopes = (env.catch_stack.len(), env.handlers.len());
    let before_depth = NATIVE_DEPTH.with(|depth| depth.get());
    let before_osr = osr_entry_count(symbol);
    egcl_rt::rooted!(
        result = if inherited_scopes {
            super::super::super::eval_form(*inherited_call, &mut env)
        } else {
            run_native(
                &first,
                symbol,
                &[
                    *marker,
                    EgclVal::from_fixnum(40),
                    EgclVal::from_single_float(1.25),
                    *unused,
                ],
                &mut env,
            )
        }
        .unwrap()
    );
    assert_eq!(
        osr_entry_count(symbol),
        before_osr + u32::from(!replace_definition),
        "OSR may enter only the running activation's exact definition"
    );
    if replace_definition {
        assert_eq!(next_code.deopt_count(), before_deopts);
        assert!(
            !OBSERVATION.with(|state| state.borrow().entered),
            "a replacement definition must not enter this active T1 frame"
        );
        assert_eq!(OBSERVATION.with(|state| state.borrow().argument_checks), 0);
        assert_eq!(
            egcl_rt::symbols::symbol_function(symbol),
            Some(*replacement_function)
        );
        let retained = NATIVE_REGISTRY
            .with(|registry| registry.borrow().get(&symbol).cloned())
            .unwrap();
        assert!(
            Arc::ptr_eq(retained.body.as_ref().unwrap(), &next_body),
            "the replacement stays installed for future calls"
        );
    } else if fail_entry_guard {
        assert_eq!(
            next_code.deopt_count(),
            before_deopts + 1,
            "the retained T2 version must execute exactly one failing guard"
        );
        assert!(
            !OBSERVATION.with(|state| state.borrow().entered),
            "entry guard must fail before the first optimized loop poll"
        );
        assert_eq!(OBSERVATION.with(|state| state.borrow().argument_checks), 0);
    } else {
        assert!(
            OBSERVATION.with(|state| state.borrow().entered),
            "running T1 must actually enter mapped T2 before return"
        );
        assert!(
            OBSERVATION.with(|state| state.borrow().argument_checks > 0),
            "native debugger arguments must be checked in the mapped T2 activation"
        );
        assert_ne!(
            unused.to_raw(),
            unused_address,
            "dead original argument must relocate"
        );
        assert_ne!(marker.to_raw(), address, "live OSR value must relocate");
    }
    assert_eq!(*result, *marker);
    assert_eq!(
        super::super::super::cp(*marker),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(40)),
        "pre-loop and loop effects must not replay"
    );
    assert_eq!(OBSERVATION.with(|state| state.borrow().steps_at_handoff), 4);
    assert!(env.mv_active);
    assert_eq!(
        env.mv.as_slice(),
        &[
            *marker,
            if fail_entry_guard {
                EgclVal::from_single_float(781.25)
            } else {
                EgclVal::from_fixnum(780)
            },
            EgclVal::from_single_float(1.25)
        ]
    );
    assert_eq!((stack.fp(), stack.sp()), before);
    assert_eq!((env.catch_stack.len(), env.handlers.len()), before_scopes);
    assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), before_depth);
    if replace_definition {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        let retained = NATIVE_REGISTRY
            .with(|registry| registry.borrow().get(&symbol).cloned())
            .unwrap();
        egcl_rt::rooted!(
            later = run_native(
                &retained,
                symbol,
                &[
                    *marker,
                    EgclVal::from_fixnum(0),
                    EgclVal::from_single_float(1.25),
                    *unused
                ],
                &mut env
            )
            .unwrap()
        );
        assert_eq!(*later, *marker);
        assert_eq!(
            env.mv.as_slice(),
            &[
                *marker,
                EgclVal::from_fixnum(1000),
                EgclVal::from_single_float(1.25)
            ],
            "a subsequent invocation runs the replacement body"
        );
        assert_eq!(
            super::super::super::cp(*marker),
            (EgclVal::from_fixnum(2), EgclVal::from_fixnum(40))
        );
        assert_eq!((stack.fp(), stack.sp()), before);
        assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), before_depth);
    }
    if inherited_scopes {
        assert_eq!(
            super::super::super::read_eval_all_env(
                "(equal *osr-scope-log* '(:outer-cleanup :inner-cleanup :handler))",
                &mut env,
            )
            .unwrap(),
            T,
            "the inherited live handler must run before exactly-once cleanups; no call continuation"
        );
    }
}
