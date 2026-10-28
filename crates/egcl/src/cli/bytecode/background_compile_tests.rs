// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::sync::{Condvar, Mutex, OnceLock};

struct PauseState {
    entered: bool,
    released: bool,
    fail: bool,
}
struct Pause {
    state: Mutex<PauseState>,
    ready: Condvar,
}
type Pauses = Mutex<HashMap<(u32, bool), Arc<Pause>>>;
fn pauses() -> &'static Pauses {
    static PAUSES: OnceLock<Pauses> = OnceLock::new();
    PAUSES.get_or_init(|| Mutex::new(HashMap::new()))
}
pub(super) fn pause(symbol: u32, t2: bool) {
    let pause = pauses().lock().unwrap().get(&(symbol, t2)).cloned();
    let Some(pause) = pause else {
        return;
    };
    let mut state = pause.state.lock().unwrap();
    state.entered = true;
    pause.ready.notify_all();
    while !state.released {
        state = pause.ready.wait(state).unwrap();
    }
    let fail = state.fail;
    drop(state);
    assert!(!fail, "injected compiler failure");
}
struct Hold {
    key: (u32, bool),
    pause: Arc<Pause>,
}
impl Hold {
    fn new(symbol: u32, t2: bool) -> Self {
        let pause = Arc::new(Pause {
            state: Mutex::new(PauseState {
                entered: false,
                released: false,
                fail: false,
            }),
            ready: Condvar::new(),
        });
        let key = (symbol, t2);
        pauses().lock().unwrap().insert(key, Arc::clone(&pause));
        Self { key, pause }
    }
    fn await_worker(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !self.pause.state.lock().unwrap().entered {
            assert!(
                std::time::Instant::now() < deadline,
                "actual compiler job never entered independent phase"
            );
            compiler_wait_tick();
        }
    }
}
impl Drop for Hold {
    fn drop(&mut self) {
        let mut state = self.pause.state.lock().unwrap();
        state.released = true;
        self.pause.ready.notify_all();
        pauses().lock().unwrap().remove(&self.key);
    }
}

fn constant_body(symbol: u32, env: &Env, value: EgclVal) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(value = value);
    egcl_rt::rooted!(form = reader::read_from_string("(nil)").unwrap().0);
    let mut body = compile_function("COMPILER-CONSTANT", NIL, *form, env, false, false).unwrap();
    body.code = vec![Instr::Const(0), Instr::Return];
    body.constants = vec![*value];
    body.n_locals = 0;
    body.max_stack = 1;
    let body = Arc::new(body);
    registry_put(symbol, Arc::clone(&body));
    body
}

#[test]
fn background_compilers_allow_moving_gc_and_publish_relocated_constants() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for t2 in [false, true] {
        let symbol = egcl_rt::symbols::intern(if t2 {
            "BACKGROUND-T2-MOVING"
        } else {
            "BACKGROUND-T1-MOVING"
        });
        // Bootstrap/compilation runs before allocating the object whose movement
        // matters; it must start in a fresh nursery, not an already promoted pool.
        constant_body(symbol, &env, NIL);
        egcl_rt::gc::collect_t0_minor().unwrap();
        egcl_rt::rooted!(value = super::super::arena_cons(EgclVal::from_fixnum(71), NIL));
        let original = value.to_raw();
        let mut body = (*registry_get(symbol).unwrap()).clone();
        body.constants[0] = *value;
        let body = Arc::new(body);
        registry_put(symbol, Arc::clone(&body));
        let hold = Hold::new(symbol, t2);
        if t2 {
            assert!(request_t2_compilation(symbol, 1));
        } else {
            assert!(background_t1::request(symbol));
        }
        hold.await_worker();
        // This collection runs while the real compiler worker remains held.
        // A whole-job GC gate deadlocks here; the test runner caps the runtime.
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_ne!(
            value.to_raw(),
            original,
            "the probe must actually relocate the heap constant"
        );
        assert_eq!(super::super::cp(*value), (EgclVal::from_fixnum(71), NIL));
        // Managed execution continues before allowing compilation to finish.
        assert_eq!(
            super::super::read_eval_all_env("(+ 20 22)", &mut env).unwrap(),
            EgclVal::from_fixnum(42)
        );
        drop(hold);
        let code = if t2 {
            wait_for_t2(symbol)
        } else {
            background_t1::wait(symbol)
        }
        .expect("completed compiler job must install native code");
        assert_eq!(code.is_t2, t2);
        assert!(code.code_len > 0);
        egcl_rt::rooted!(answer = run_native(&code, symbol, &[], &mut env).unwrap());
        assert_eq!(
            *answer, *value,
            "native code must load the relocated original pool slot"
        );
        registry_remove(symbol);
    }
}

#[test]
fn background_t1_rejects_stale_completion_without_declining_replacement() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = egcl_rt::symbols::intern("BACKGROUND-T1-REDEFINED");
    constant_body(symbol, &env, EgclVal::from_fixnum(11));
    let hold = Hold::new(symbol, false);
    assert!(background_t1::request(symbol));
    assert!(
        !background_t1::request(symbol),
        "duplicate request must coalesce"
    );
    hold.await_worker();
    constant_body(symbol, &env, EgclVal::from_fixnum(29));
    drop(hold);
    assert!(
        background_t1::wait(symbol).is_none(),
        "stale result must not install"
    );
    assert!(!t1_declined(symbol));
    assert!(
        background_t1::request(symbol),
        "replacement must remain eligible"
    );
    let code = background_t1::wait(symbol).unwrap();
    assert_eq!(
        run_native(&code, symbol, &[], &mut env).unwrap(),
        EgclVal::from_fixnum(29)
    );
    registry_remove(symbol);
}

#[test]
fn background_worker_panic_releases_request_and_allows_retry() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for t2 in [false, true] {
        let symbol = egcl_rt::symbols::intern(if t2 {
            "BACKGROUND-T2-PANIC"
        } else {
            "BACKGROUND-T1-PANIC"
        });
        constant_body(symbol, &env, EgclVal::from_fixnum(43));
        let hold = Hold::new(symbol, t2);
        hold.pause.state.lock().unwrap().fail = true;
        if t2 {
            assert!(request_t2_compilation(symbol, 1));
        } else {
            assert!(background_t1::request(symbol));
        }
        hold.await_worker();
        drop(hold);
        let failed = if t2 {
            wait_for_t2(symbol)
        } else {
            background_t1::wait(symbol)
        };
        assert!(failed.is_none());
        assert!(!t1_declined(symbol));
        assert!(!T2_DECLINED.with(|set| set.borrow().contains(&symbol)));
        if t2 {
            assert!(request_t2_compilation(symbol, 1));
        } else {
            assert!(background_t1::request(symbol));
        }
        let code = if t2 {
            wait_for_t2(symbol)
        } else {
            background_t1::wait(symbol)
        }
        .unwrap();
        assert_eq!(
            run_native(&code, symbol, &[], &mut env).unwrap(),
            EgclVal::from_fixnum(43)
        );
        registry_remove(symbol);
    }
}

#[test]
fn background_full_queue_preserves_generic_replacement_preference() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = egcl_rt::symbols::intern("BACKGROUND-GENERIC-REPLACEMENT");
    constant_body(symbol, &env, EgclVal::from_fixnum(53));
    let hold = Hold::new(symbol, false);
    assert!(background_t1::request(symbol));
    hold.await_worker();
    let full = background_t1::force_full_queue_for_test();
    assert!(!background_t1::request_with_speculation(symbol, false));
    drop(hold);
    assert!(
        background_t1::wait(symbol).is_none(),
        "speculative result must not publish after a generic request"
    );
    assert!(!t1_declined(symbol));
    drop(full);
    assert!(background_t1::request(symbol));
    let code = background_t1::wait(symbol).unwrap();
    assert_eq!(
        run_native(&code, symbol, &[], &mut env).unwrap(),
        EgclVal::from_fixnum(53)
    );
    registry_remove(symbol);
}

#[cfg(all(target_arch = "s390x", target_os = "linux"))]
#[test]
fn background_checked_osr_preserves_exact_body_and_moving_constants() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = egcl_rt::symbols::intern("BACKGROUND-OSR-MOVING");
    let template = constant_body(symbol, &env, NIL);
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(value = super::super::arena_cons(EgclVal::from_fixnum(73), NIL));
    let original = value.to_raw();
    let mut body = (*template).clone();
    body.constants = vec![*value, egcl_rt::value::T];
    body.code = vec![
        Instr::PushTag {
            tagbody_id: 0,
            sp_restore: 0,
        },
        Instr::Const(1),
        Instr::BrIfTrue(4),
        Instr::Go {
            tagbody_id: 0,
            target_bcp: 1,
        },
        Instr::Const(0),
        Instr::Return,
    ];
    let body = Arc::new(body);
    registry_put(symbol, Arc::clone(&body));
    let key = Arc::as_ptr(&body) as usize;
    let hold = Hold::new(symbol, false);
    assert!(compile_osr(symbol, &body).is_none());
    hold.await_worker();
    assert!(compile_osr(symbol, &body).is_none());
    assert!(
        !ANON_OSR_REGISTRY.with(|cache| cache.borrow().contains_key(&key)),
        "pending must not cache a decline"
    );
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(value.to_raw(), original);
    constant_body(symbol, &env, EgclVal::from_fixnum(99));
    drop(hold);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let code = loop {
        if let Some(code) = compile_osr(symbol, &body) {
            break code;
        }
        assert!(std::time::Instant::now() < deadline);
        compiler_wait_tick();
    };
    assert!(Arc::ptr_eq(&code.bf, &body));
    let entry = *code.entries.get(&1).expect("must emit a real OSR header");
    let stack = egcl_rt::current_stack();
    let frame = stack
        .push_frame(NIL, code.code_info, code.num_slots, FLAG_CALL)
        .unwrap();
    let result = run_native_osr(&code, entry, frame, None, &mut env).unwrap();
    stack.pop_frame();
    let OsrOutcome::Finished(answer) = result else {
        panic!("OSR must finish natively")
    };
    assert_eq!(answer, *value);
    registry_remove(symbol);
}

#[test]
fn background_t2_rejects_changed_inline_dependency() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        "(defun background-dependency (x) (null x))
         (defun background-dependent (x) (background-dependency x))
         (compile 'background-dependency)
         (compile 'background-dependent)",
        &mut env,
    )
    .unwrap();
    let callee = resolve_sym("BACKGROUND-DEPENDENCY")
        .unwrap()
        .as_symbol_index();
    let caller = resolve_sym("BACKGROUND-DEPENDENT")
        .unwrap()
        .as_symbol_index();
    let callee_body = registry_get(callee).unwrap();
    FUNCTION_SAMPLE_PROFILE.with(|profiles| {
        profiles
            .borrow_mut()
            .insert(Arc::as_ptr(&callee_body) as usize, 20)
    });
    let snapshot = snapshot_t2_input(caller, 1).unwrap();
    assert!(
        snapshot
            .inline_bodies
            .iter()
            .any(|saved| saved.symbol == callee),
        "probe must capture the dependency"
    );
    let hold = Hold::new(caller, true);
    assert!(request_t2_compilation(caller, 1));
    hold.await_worker();
    super::super::read_eval_all_env("(defun background-dependency (x) 29)", &mut env).unwrap();
    drop(hold);
    assert!(
        wait_for_t2(caller).is_none(),
        "changed dependency must invalidate the completion"
    );
    assert!(!T2_DECLINED.with(|declined| declined.borrow().contains(&caller)));
    assert!(request_t2_compilation(caller, 1));
    let code = wait_for_t2(caller).unwrap();
    assert_eq!(
        run_native(&code, caller, &[NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(29)
    );
    registry_remove(caller);
    registry_remove(callee);
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn background_t2_rejects_changed_return_hint_dependency() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        "(defun background-float-source (x) (float x))
         (defun background-float-user (arithmetic)
           (let ((x (background-float-source 5)))
             (if arithmetic (+ x 1) x)))
         (compile 'background-float-source)
         (compile 'background-float-user)",
        &mut env,
    )
    .unwrap();
    let callee = resolve_sym("BACKGROUND-FLOAT-SOURCE").unwrap().as_symbol_index();
    let caller = resolve_sym("BACKGROUND-FLOAT-USER").unwrap().as_symbol_index();
    let input = snapshot_t2_input(caller, 1).unwrap();
    assert!(input.inline_bodies.iter().any(|saved| saved.symbol == callee && saved.hint_only),
        "a cold helper must supply a completion dependency without enabling inlining");
    let hold = Hold::new(caller, true);
    assert!(request_t2_compilation(caller, 1));
    hold.await_worker();
    super::super::read_eval_all_env("(defun background-float-source (x) 29)", &mut env).unwrap();
    drop(hold);
    assert!(wait_for_t2(caller).is_none(),
        "a summary captured before redefinition must not install afterwards");
    assert!(!T2_DECLINED.with(|declined| declined.borrow().contains(&caller)));
    assert!(request_t2_compilation(caller, 1));
    let code = wait_for_t2(caller).unwrap();
    assert_eq!(run_native(&code, caller, &[NIL], &mut env).unwrap(), EgclVal::from_fixnum(29));
    registry_remove(caller);
    registry_remove(callee);
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn background_t2_queue_pressure_does_not_permanently_decline_double_osr() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        "(defun background-double-pressure (n)
           (let ((sum 0.0d0)) (dotimes (i n) (setq sum (+ sum 1.0d0))) sum))",
        &mut env,
    )
    .unwrap();
    let symbol = resolve_sym("BACKGROUND-DOUBLE-PRESSURE")
        .unwrap()
        .as_symbol_index();
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((let ((sum 0.0d0)) (dotimes (i n) (setq sum (+ sum 1.0d0))) sum))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function(
            "BACKGROUND-DOUBLE-PRESSURE",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap(),
    );
    registry_put(symbol, Arc::clone(&body));
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    egcl_rt::function::record_back_edges(*function, osr_threshold());
    let body_ptr = Arc::as_ptr(&body) as usize;
    let queue = t2_compile_queue();
    let capacity = positive_env(&["EGCL_COMPILE_QUEUE_SIZE"], 64) as usize;
    let mut permits = Vec::new();
    while let Some(permit) = CompilerPermit::acquire(&queue.outstanding, capacity) {
        permits.push(permit);
    }
    let dropped_before = queue.dropped.load(std::sync::atomic::Ordering::Relaxed);
    egcl_rt::rooted!(
        first = run_with_sym(
            Arc::clone(&body),
            &[EgclVal::from_fixnum(8)],
            *function,
            symbol,
            &mut env,
        )
        .unwrap()
    );
    assert_eq!(first.as_double_float(), 8.0);
    assert!(
        queue.dropped.load(std::sync::atomic::Ordering::Relaxed) > dropped_before,
        "the actual OSR request must encounter queue pressure"
    );
    assert!(!T2_QUEUED.with(|queued| queued.borrow().contains_key(&symbol)));
    assert!(
        !MAPPED_OSR_DECLINED.with(|declined| declined
            .borrow()
            .keys()
            .any(|&(candidate, _)| candidate == body_ptr)),
        "temporary queue pressure must not permanently decline this loop body"
    );
    drop(permits);

    // Retry the identical definition and function object, not a replacement
    // whose fresh identity would bypass the erroneous decline cache.
    let hold = Hold::new(symbol, true);
    egcl_rt::rooted!(
        second = run_with_sym(
            Arc::clone(&body),
            &[EgclVal::from_fixnum(8)],
            *function,
            symbol,
            &mut env,
        )
        .unwrap()
    );
    assert_eq!(second.as_double_float(), first.as_double_float());
    assert!(
        T2_QUEUED.with(|queued| queued.borrow().contains_key(&symbol)),
        "the same loop must retry once a compiler slot becomes available"
    );
    hold.await_worker();
    drop(hold);
    let native = wait_for_t2(symbol).expect("retried double loop installs T2");
    assert!(native.is_t2);
    assert!(Arc::ptr_eq(native.body.as_ref().unwrap(), &body));
    registry_remove(symbol);
}

#[cfg(target_arch = "s390x")]
#[test]
fn background_s390x_numeric_deopt_queues_generic_replacement() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env("(defun background-s390x-phase (x) (+ x 1))", &mut env)
        .unwrap();
    let symbol = resolve_sym("BACKGROUND-S390X-PHASE")
        .unwrap()
        .as_symbol_index();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((+ x 1))").unwrap().0);
    let body = Arc::new(
        compile_function(
            "BACKGROUND-S390X-PHASE",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap(),
    );
    registry_put(symbol, Arc::clone(&body));
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    // Establish real fixnum evidence, then emit and execute real checked T1.
    assert_eq!(
        run_with_sym(
            Arc::clone(&body),
            &[EgclVal::from_fixnum(4)],
            *function,
            symbol,
            &mut env,
        )
        .unwrap(),
        EgclVal::from_fixnum(5)
    );
    let original =
        try_promote_to_t1_with_speculation(symbol, true).expect("compile speculative checked T1");
    assert!(
        original.has_deopt,
        "fixture must contain a real arithmetic guard"
    );
    mark_fresh_promotion(symbol);
    publish_native(symbol, Some(*function), &original);
    assert_eq!(
        run_native(&original, symbol, &[EgclVal::from_fixnum(4)], &mut env,).unwrap(),
        EgclVal::from_fixnum(5)
    );

    let hold = Hold::new(symbol, false);
    let before_deopts = deopt_count();
    // Resume each failed arithmetic guard in T0 so the changed numeric phase
    // supplies real profile evidence; never call note_native_deopt directly.
    for _ in 0..deopt_blacklist_threshold().saturating_add(2) {
        assert_eq!(
            run_native(
                &original,
                symbol,
                &[EgclVal::from_single_float(1.5)],
                &mut env,
            )
            .unwrap(),
            EgclVal::from_single_float(2.5)
        );
        if background_t1::pending(symbol) {
            break;
        }
    }
    assert!(
        deopt_count() > before_deopts,
        "actual generated guards must fail"
    );
    assert!(
        background_t1::pending(symbol),
        "numeric phase retirement must queue its replacement, not compile on the mutator"
    );
    assert_eq!(egcl_rt::function::tier(*function), 0);
    assert!(NATIVE_REGISTRY.with(|registry| !registry.borrow().contains_key(&symbol)));
    hold.await_worker();
    drop(hold);
    background_t1::wait(symbol);
    let replacement = NATIVE_REGISTRY
        .with(|registry| registry.borrow().get(&symbol).cloned())
        .expect("background generic T1 must publish");
    assert!(!Rc::ptr_eq(&original, &replacement));
    assert!(Arc::ptr_eq(replacement.body.as_ref().unwrap(), &body));
    let before_generic = deopt_count();
    assert_eq!(
        run_native(
            &replacement,
            symbol,
            &[EgclVal::from_single_float(1.5)],
            &mut env,
        )
        .unwrap(),
        EgclVal::from_single_float(2.5)
    );
    assert_eq!(
        deopt_count(),
        before_generic,
        "generic replacement must handle the observed float phase without another guard failure"
    );
    registry_remove(symbol);
}

#[cfg(all(target_arch = "s390x", target_os = "linux"))]
#[test]
fn background_s390x_declines_local_exit_across_captured_environment() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((let ((x 7) (g nil))
            (let ((f (lambda () x)))
              (block done
                (let ((x 70))
                  (setq g (lambda () x))
                  (return-from done nil)))
              (+ x (funcall f) (funcall g)))))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function("BACKGROUND-S390X-ENV-EXIT", NIL, *forms, &env, false, false)
            .expect("real source must compile to bytecode"),
    );
    let symbol = egcl_rt::symbols::intern("BACKGROUND-S390X-ENV-EXIT");
    registry_put(symbol, Arc::clone(&body));
    assert!(
        local_exit_crosses_env_frame(&body),
        "fixture must leave a captured lexical child: {:?}",
        body.code
    );
    assert!(
        !native_would_lose_captured_control(&body),
        "fixture must exercise environment restoration, not captured control exclusion"
    );
    let counter = std::sync::atomic::AtomicU32::new(0);
    let counter_address = std::ptr::from_ref(&counter) as u64;
    assert!(s390x::prepare_background_t1(symbol, &body, true, counter_address).is_none());
    assert!(s390x::prepare_background_osr(&body, counter_address, true).is_none());
    // Both the escaped closure's inner binding and the restored outer binding
    // must survive RETURN-FROM: 7 + 7 + 70, not a stale inner-frame lookup.
    assert_eq!(
        run(body, &[], NIL, &mut env).unwrap(),
        EgclVal::from_fixnum(84)
    );
    registry_remove(symbol);
}
