// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution-owned targets behind the runtime's stable named-call ordinals.
//! Cold entries resolve names and drive tier transitions. Native entries own
//! their activation setup; interpreter/legacy bridges retain their resolved
//! target before any allocation or Lisp re-entry.

use super::*;
use egcl_rt::call_table::{self as linkage, CallCell};

#[derive(Clone)]
enum Target {
    Native {
        function: EgclVal,
        code: Rc<NativeCode>,
        promote: bool,
    },
    Bytecode {
        function: EgclVal,
        body: Arc<BytecodeFunction>,
        promote: bool,
    },
    Builtin {
        slot: u32,
        nargs: usize,
    },
    Function(EgclVal),
}

impl Target {
    fn function(&self) -> EgclVal {
        match self {
            Self::Native { function, .. }
            | Self::Bytecode { function, .. }
            | Self::Function(function) => *function,
            Self::Builtin { .. } => NIL,
        }
    }

    fn trace(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        match self {
            Self::Native { function, .. }
            | Self::Bytecode { function, .. }
            | Self::Function(function) => visit(function),
            Self::Builtin { .. } => {}
        }
    }
}

struct State {
    symbol: u32,
    cell: Arc<CallCell>,
    target: RefCell<Option<Target>>,
    #[cfg(all(target_arch = "x86_64", unix))]
    #[allow(clippy::vec_box)] // generated entries embed stable descriptor addresses
    versions: RefCell<Vec<Box<super::call_table_native::NativeEntry>>>,
    #[cfg(all(target_arch = "x86_64", unix))]
    funcall: RefCell<Option<Box<super::call_table_funcall::Cache>>>,
}

// Box addresses, unlike the vector's storage, stay fixed when ordinals grow.
#[allow(clippy::vec_box)]
type Slots = Vec<Option<Box<State>>>;
static SLOTS: egcl_rt::execution_local::ExecutionLocal<RefCell<Slots>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(Vec::new())) };

/// Interpreter/legacy boundary entries. Their native callers do not perform
/// Rust recovery toggles: the generated bridge owns that ABI conversion.
fn bridge_entries() -> Option<[usize; 4]> {
    let callbacks = [
        cold_register as *const () as usize,
        cold_slice as *const () as usize,
        warm_register as *const () as usize,
        warm_slice as *const () as usize,
    ];
    #[cfg(all(target_arch = "x86_64", unix))]
    {
        static BRIDGES: std::sync::OnceLock<Option<[egcl_rt::jit::JitBuffer; 4]>> =
            std::sync::OnceLock::new();
        BRIDGES
            .get_or_init(|| {
                let toggle = c2i_set_native_sigsegv_recovery as *const () as u64;
                let mut buffers = Vec::with_capacity(4);
                for callback in callbacks {
                    let code =
                        egcl_compiler::t2::emit::emit_native_call_bridge(callback as u64, toggle)
                            .ok()?;
                    buffers.push(egcl_rt::jit::JitBuffer::new(&code)?);
                }
                buffers.try_into().ok()
            })
            .as_ref()
            .map(|buffers| std::array::from_fn(|i| buffers[i].as_ptr() as usize))
    }
    #[cfg(not(all(target_arch = "x86_64", unix)))]
    Some(callbacks)
}

pub(super) fn resolve(symbol: u32) -> Option<Arc<CallCell>> {
    let entries = bridge_entries()?;
    let ordinal = linkage::resolve_ordinal(symbol)?;
    install_bytecode_root_scanner();
    Some(SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let count = slots.len().max(ordinal + 1);
        slots.resize_with(count, || None);
        let state = slots[ordinal].get_or_insert_with(|| {
            let cell = Arc::new(CallCell::new(ordinal, entries[0], entries[1]));
            let state = Box::new(State {
                symbol,
                cell,
                target: RefCell::new(None),
                #[cfg(all(target_arch = "x86_64", unix))]
                versions: RefCell::new(Vec::new()),
                #[cfg(all(target_arch = "x86_64", unix))]
                funcall: RefCell::new(None),
            });
            state.cell.bind_state(&*state as *const State as usize);
            linkage::register(&state.cell);
            state
        });
        Arc::clone(&state.cell)
    }))
}

/// The collector stops all executions before visiting this cache. Invalidated
/// targets are released here; active native version descriptors and bridge
/// calls retain their exact code and root their callable independently. Cells themselves never own code, so
/// mutually recursive compiled functions do not form Rc ownership cycles.
pub(super) unsafe fn scan(visit: &mut dyn FnMut(*mut EgclVal)) {
    unsafe {
        SLOTS.scan(|slots| {
            for state in slots.borrow().iter().flatten() {
                #[cfg(all(target_arch = "x86_64", unix))]
                {
                    if let Some(cache) = state.funcall.borrow_mut().as_mut() {
                        cache.scan();
                    }
                    let mut versions = state.versions.borrow_mut();
                    versions.retain(|version| version.keep(&state.cell));
                    for version in versions.iter_mut() {
                        version.trace(visit);
                    }
                }
                let mut target = state.target.borrow_mut();
                if state.cell.is_cold() {
                    *target = None;
                } else if let Some(target) = target.as_mut() {
                    target.trace(visit);
                }
            }
        })
    };
}

unsafe fn state(cell: u64) -> &'static State {
    // Only generated code belonging to this execution calls these entries.
    // The execution's SLOTS owns the Box until that execution has stopped.
    let cell = unsafe { &*(cell as *const CallCell) };
    unsafe { &*(cell.state_address() as *const State) }
}

fn select_target(symbol: u32, nargs: usize) -> Option<Target> {
    let function = egcl_rt::symbols::symbol_function(symbol)?;
    if replacement_function_value(symbol, function).is_some() {
        return Some(Target::Function(function));
    }
    if egcl_rt::function::is_interpreted_function(function)
        && let Some(body) = registry_get(symbol)
    {
        if let Some(code) = NATIVE_REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned())
        {
            if code
                .body
                .as_ref()
                .is_some_and(|owned| Arc::ptr_eq(owned, &body))
            {
                let promote = !code.is_t2
                    && t2_enabled()
                    && !T2_DECLINED.with(|declined| declined.borrow().contains(&symbol));
                return Some(Target::Native {
                    function,
                    code,
                    promote,
                });
            }
        }
        let promote = !profiling_disabled()
            && !is_profile_pinned(symbol)
            && !T1_DECLINED.with(|declined| declined.borrow().contains(&symbol));
        return Some(Target::Bytecode {
            function,
            body,
            promote,
        });
    }
    if let Some(slot) = super::super::direct_builtin_slot(symbol, nargs) {
        return Some(Target::Builtin { slot, nargs });
    }
    egcl_rt::function::is_interpreted_function(function).then_some(Target::Function(function))
}

fn cold(state: &State, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    #[cfg(test)]
    record_target_lookup();
    let revision = linkage::revision(state.cell.ordinal());
    egcl_rt::rooted!(result = c2i_call_result(state.symbol as u64, args, 0)?);
    #[cfg(all(target_arch = "x86_64", unix))]
    if !args.is_empty() && super::call_table_funcall::is_builtin(state.symbol) {
        let entries = bridge_entries().expect("resolved cell owns bridges");
        let mut cache = state.funcall.borrow_mut();
        if cache.is_none() {
            *cache = super::call_table_funcall::Cache::new([entries[0], entries[1]]);
        }
        if let Some(cache) = cache.as_mut() {
            *state.target.borrow_mut() = None;
            cache.synchronize_revision(revision);
            cache.select(args[0]);
            let entries = cache.entries();
            linkage::publish(&state.cell, revision, entries[0], entries[1]);
        }
        return Ok(*result);
    }
    let target = select_target(state.symbol, args.len());
    if target.is_some() {
        let entries = bridge_entries().expect("resolved cell owns installed bridges");
        #[allow(unused_mut)]
        let mut native = [entries[2], entries[3]];
        #[cfg(all(target_arch = "x86_64", unix))]
        if let Some(Target::Native {
            function,
            code,
            promote: false,
        }) = target.as_ref()
            && let Some(version) = super::call_table_native::NativeEntry::new(
                state.symbol,
                *function,
                Rc::clone(code),
                native,
            )
        {
            native = version.entries();
            state.versions.borrow_mut().push(version);
        }
        *state.target.borrow_mut() = target;
        linkage::publish(&state.cell, revision, native[0], native[1]);
        #[cfg(all(target_arch = "x86_64", unix))]
        state
            .versions
            .borrow_mut()
            .retain(|version| version.keep(&state.cell));
    }
    Ok(*result)
}

fn warm(state: &State, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    // Drop the RefCell borrow BEFORE a call can allocate, yield, redefine the
    // target or trigger the GC scanner. The clone keeps its old code alive.
    let target = { state.target.borrow().clone() };
    let Some(target) = target else {
        return cold(state, args);
    };
    egcl_rt::rooted!(function = target.function());
    let env_ptr = NATIVE_ENV.with(|env| env.get());
    if env_ptr.is_null() {
        return Ok(NIL);
    }
    let env = unsafe { &mut *env_ptr };
    match target {
        Target::Native { code, promote, .. } => {
            let Some(body) = &code.body else {
                return cold(state, args);
            };
            if !arity_accepts(body, args.len()) {
                return cold(state, args);
            }
            if promote
                && (egcl_rt::function::invoke_count(*function).saturating_add(1)
                    >= t2_invoke_threshold()
                    || egcl_rt::function::back_edge_count(*function) >= t2_backedge_threshold())
            {
                return cold(state, args);
            }
            if !profiling_disabled() {
                egcl_rt::function::record_invocation(*function);
            }
            if NATIVE_DEPTH.with(|depth| depth.get()) >= native_depth_cap() {
                run(
                    Arc::clone(body),
                    args,
                    EgclVal::from_symbol_index(state.symbol),
                    env,
                )
            } else {
                run_native(&code, state.symbol, args, env)
            }
        }
        Target::Bytecode { body, promote, .. } => {
            if !arity_accepts(&body, args.len()) {
                return cold(state, args);
            }
            if promote
                && egcl_rt::function::invoke_count(*function).saturating_add(1) >= t1_threshold()
            {
                return cold(state, args);
            }
            if !profiling_disabled() {
                egcl_rt::function::record_invocation(*function);
            }
            run(body, args, EgclVal::from_symbol_index(state.symbol), env)
        }
        Target::Builtin { slot, nargs } => {
            if nargs != args.len() {
                return cold(state, args);
            }
            match super::super::call_direct_builtin(slot, args, env) {
                Some(result) => result,
                None => cold(state, args),
            }
        }
        Target::Function(_) => apply_function(*function, args, env),
    }
}

extern "C" fn cold_register(cell: u64, n: u64, a0: u64, a1: u64, a2: u64, _: u64) -> u64 {
    registers(cell, n, [EgclVal(a0), EgclVal(a1), EgclVal(a2)], false)
}
extern "C" fn warm_register(cell: u64, n: u64, a0: u64, a1: u64, a2: u64, _: u64) -> u64 {
    registers(cell, n, [EgclVal(a0), EgclVal(a1), EgclVal(a2)], true)
}
fn registers(cell: u64, n: u64, args: [EgclVal; 3], ready: bool) -> u64 {
    if n > 3 {
        return NIL.0;
    }
    egcl_rt::rooted!(args = args);
    invoke(cell, &args[..n as usize], ready)
}
extern "C" fn cold_slice(cell: u64, n: u64, args: *const EgclVal, _: u64) -> u64 {
    slice(cell, n, args, false)
}
extern "C" fn warm_slice(cell: u64, n: u64, args: *const EgclVal, _: u64) -> u64 {
    slice(cell, n, args, true)
}
fn slice(cell: u64, n: u64, args: *const EgclVal, ready: bool) -> u64 {
    if n != 0 && args.is_null() {
        return NIL.0;
    }
    // The generated caller owns this GC-scanned activation slice.
    let args = if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(args, n as usize) }
    };
    invoke(cell, args, ready)
}
fn invoke(cell: u64, args: &[EgclVal], ready: bool) -> u64 {
    finish_c2i_call(guard_c2i(|| {
        let state = unsafe { state(cell) };
        if ready {
            warm(state, args)
        } else {
            cold(state, args)
        }
    }))
}

#[cfg(test)]
thread_local! {
    static TARGET_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn record_target_lookup() {
    TARGET_LOOKUPS.with(|count| count.set(count.get() + 1));
}

#[cfg(all(test, target_arch = "x86_64", unix))]
mod tests {
    use super::*;

    #[test]
    fn warmed_native_slot_does_not_reenter_target_resolution() {
        check_native_slot("SLOT-LOOKUP-LEAF", "()", "42", &[], 42);
        check_native_slot(
            "SLOT-LOOKUP-THREE",
            "(a b c)",
            "(+ a (+ b c))",
            &[1, 2, 3],
            6,
        );
        check_native_slot(
            "SLOT-LOOKUP-WIDE",
            "(a b c d)",
            "(+ a (+ b (+ c d)))",
            &[1, 2, 3, 4],
            10,
        );
    }

    #[test]
    fn supplied_optional_arguments_use_the_native_slot_entry() {
        check_native_slot(
            "SLOT-LOOKUP-OPTIONAL",
            "(a &optional (b 20) (c 30) (d 40))",
            "(+ a (+ b (+ c d)))",
            &[1, 2, 3, 4],
            10,
        );
    }

    #[test]
    fn literal_optional_defaults_use_the_native_slot_entry() {
        check_native_slot(
            "SLOT-LOOKUP-DEFAULTS",
            "(a &optional (b 20) (c 30) (d 40))",
            "(+ a (+ b (+ c d)))",
            &[1, 2],
            73,
        );
        check_native_slot(
            "SLOT-LOOKUP-NO-REQUIRED",
            "(&optional (a 10) (b 20))",
            "(+ a b)",
            &[],
            30,
        );
        check_native_slot(
            "SLOT-LOOKUP-NIL-DEFAULT",
            "(a &optional b)",
            "(if b a (+ a 1))",
            &[1],
            2,
        );
    }

    #[test]
    fn large_native_depth_caps_keep_native_calls() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::bytecode::call_table::tests::warmed_native_slot_does_not_reenter_target_resolution",
                "--test-threads=1"])
            .env("EGCL_NATIVE_DEPTH_CAP", "4000000000")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn gethash_slot_caches_builtin_and_preserves_both_values() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(table = super::super::super::read_eval_all_env(
            "(make-hash-table :test 'eql)", &mut env,
        ).unwrap());
        let symbol = super::super::super::resolve_sym("GETHASH").unwrap().as_symbol_index();
        let cell = resolve(symbol).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        egcl_rt::rooted!(args = [EgclVal::from_fixnum(7), *table, EgclVal::from_fixnum(99)]);
        for nargs in [2, 3] {
            egcl_stdlib::remhash(args[0], *table).unwrap();
            let default = if nargs == 2 { NIL } else { args[2] };
            assert_eq!(cold(state, &args[..nargs]).unwrap(), default);
            assert_eq!(env.mv, vec![default, NIL]);
            assert!(env.mv_active);
            assert!(!cell.is_cold(), "GETHASH must install its builtin target");
            assert!(matches!(*state.target.borrow(), Some(Target::Builtin { .. })));
            egcl_stdlib::set_gethash(args[0], *table, NIL).unwrap();
            TARGET_LOOKUPS.with(|count| count.set(0));
            for _ in 0..100 {
                let entry = unsafe { &*cell.entry_address(false) }
                    .load(std::sync::atomic::Ordering::Acquire);
                let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(entry) };
                assert_eq!(call(Arc::as_ptr(&cell) as u64, nargs as u64,
                    args[0].0, args[1].0, args[2].0, 0), NIL.0);
                assert_eq!(env.mv, vec![NIL, T], "a stored NIL is present");
                assert!(env.mv_active);
            }
            assert_eq!(TARGET_LOOKUPS.with(|count| count.get()), 0,
                "warmed GETHASH calls must not resolve their target again");
        }
        assert!(matches!(warm(state, &args[..1]), Err(EgclError::ProgramError(_))));
        assert!(matches!(warm(state, &[args[0], NIL]), Err(EgclError::TypeError { .. })));
    }

    #[test]
    fn native_frame_header_keeps_its_callable_alive() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let stack = egcl_rt::current_stack();
        let (frame, symbol) = {
            egcl_rt::rooted!(form = super::super::super::vec_to_list(&[NIL]));
            let body = Arc::new(
                compile_function("FRAME-OWNED-CALLBACK", NIL, *form, &env, false, false).unwrap(),
            );
            egcl_rt::rooted!(function = make_bytecode_closure(&body, None));
            let symbol = egcl_rt::function::name(*function).as_symbol_index();
            (
                stack
                    .push_frame(*function, std::ptr::null(), 0, FLAG_CALL)
                    .unwrap(),
                symbol,
            )
        };
        env.mv.clear();
        egcl_rt::gc::full_gc().unwrap();
        let retained = is_registered(symbol);
        let callable = unsafe { (*frame).function };
        stack.pop_frame();
        assert!(
            retained,
            "the activation header must keep its callable and captures alive"
        );
        assert!(egcl_rt::function::is_interpreted_function(callable));
    }

    #[test]
    fn native_funcall_does_not_resolve_the_callback_on_warm_calls() {
        check_native_funcall(false);
    }

    #[test]
    fn captured_native_funcall_does_not_resolve_the_callback_on_warm_calls() {
        check_native_funcall(true);
    }

    #[test]
    fn nested_native_funcall_keeps_all_warmed_callback_identities() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        for (name, expression) in [
            ("PIC-LEAF", "(+ x 1)"),
            ("PIC-MIDDLE", "(funcall #'pic-leaf x)"),
            ("PIC-OUTER", "(funcall #'pic-middle x)"),
            ("PIC-INDEPENDENT", "(+ x 2)"),
        ] {
            super::super::super::read_eval_all_env(
                &format!("(defun {name} (x) {expression})"), &mut env,
            ).unwrap();
            let symbol = egcl_rt::symbols::intern(name);
            egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
            egcl_rt::rooted!(form = reader::read_from_string(&format!("({expression})")).unwrap().0);
            registry_put(symbol, Arc::new(compile_function(name, *params, *form, &env, false, false).unwrap()));
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
            assert!(install_t2_completion(T2Completion {
                sym: symbol, generation, artifact: Some(artifact), input,
            }).unwrap().is_t2);
        }
        let cell = resolve(egcl_rt::symbols::intern("FUNCALL")).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("PIC-OUTER")).unwrap());
        egcl_rt::rooted!(args = [*function, EgclVal::from_fixnum(41)]);
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) { NATIVE_ENV.with(|slot| slot.set(self.0)); }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        for _ in 0..3 { assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(42)); }
        TARGET_LOOKUPS.with(|count| count.set(0));
        for _ in 0..100 {
            let entry = unsafe { &*cell.entry_address(false) }.load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 = unsafe { std::mem::transmute(entry) };
            assert_eq!(call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0), EgclVal::from_fixnum(42).0);
        }
        assert_eq!(TARGET_LOOKUPS.with(|count| count.get()), 0,
            "nested callbacks must not evict each other's warmed native identity");
        let symbol = egcl_rt::symbols::intern("PIC-LEAF");
        let input = snapshot_t2_input(symbol, 0).unwrap();
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
        install_t2_completion(T2Completion {
            sym: symbol, generation, artifact: Some(artifact), input,
        }).unwrap();
        args[0] = egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("PIC-INDEPENDENT")).unwrap();
        assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(43));
        args[0] = egcl_rt::symbols::symbol_function(symbol).unwrap();
        TARGET_LOOKUPS.with(|count| count.set(0));
        let entry = unsafe { &*cell.entry_address(false) }.load(std::sync::atomic::Ordering::Acquire);
        let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 = unsafe { std::mem::transmute(entry) };
        assert_eq!(call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0), EgclVal::from_fixnum(42).0);
        assert!(TARGET_LOOKUPS.with(|count| count.get()) > 0,
            "republication must invalidate every identity, even when another callback refills first");
        args[0] = *function;
        assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(42));
        egcl_rt::gc::full_gc().unwrap();
        for name in ["PIC-LEAF", "PIC-MIDDLE", "PIC-OUTER"] {
            args[0] = egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern(name)).unwrap();
            TARGET_LOOKUPS.with(|count| count.set(0));
            let entry = unsafe { &*cell.entry_address(false) }.load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 = unsafe { std::mem::transmute(entry) };
            assert_eq!(call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0), EgclVal::from_fixnum(42).0);
            assert!(TARGET_LOOKUPS.with(|count| count.get()) > 0,
                "GC must clear every weak callable identity: {name}");
        }
    }

    fn check_native_funcall(captured: bool) {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        for (name, params, expression) in [
            ("OBJECT-CALL-LEAF", "(x)", "(+ x 1)"),
            ("OBJECT-CALL-CALLER", "(f x)", "(funcall f x)"),
        ] {
            super::super::super::read_eval_all_env(
                &format!("(defun {name} {params} {expression})"),
                &mut env,
            )
            .unwrap();
            let symbol = egcl_rt::symbols::intern(name);
            egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
            egcl_rt::rooted!(
                form = reader::read_from_string(&format!("({expression})"))
                    .unwrap()
                    .0
            );
            let body = compile_function(name, *params, *form, &env, false, false).unwrap();
            registry_put(symbol, Arc::new(body));
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
            assert!(
                install_t2_completion(T2Completion {
                    sym: symbol,
                    generation,
                    artifact: Some(artifact),
                    input,
                })
                .unwrap()
                .is_t2
            );
        }
        egcl_rt::rooted!(
            function =
                egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("OBJECT-CALL-LEAF"))
                    .unwrap()
        );
        if captured {
            egcl_rt::rooted!(params = reader::read_from_string("(seed)").unwrap().0);
            egcl_rt::rooted!(
                form = reader::read_from_string("((lambda (x) (+ seed x)))")
                    .unwrap()
                    .0
            );
            let factory =
                compile_function("OBJECT-CAPTURE-FACTORY", *params, *form, &env, true, false)
                    .unwrap();
            let body = Arc::clone(&factory.nested_functions[0]);
            let capture = Arc::new(SharedCell::new(EnvFrame {
                vars: Default::default(),
                symbol_vars: Default::default(),
                parent: None,
            }));
            bind_boxed_param(&capture, "SEED", EgclVal::from_fixnum(1));
            *function = make_bytecode_closure(&body, Some(capture));
            let symbol = egcl_rt::function::name(*function).as_symbol_index();
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
            assert!(
                install_t2_completion(T2Completion {
                    sym: symbol,
                    generation,
                    artifact: Some(artifact),
                    input,
                })
                .unwrap()
                .is_t2
            );
        }
        egcl_rt::rooted!(args = [*function, EgclVal::from_fixnum(41)]);
        let cell = resolve(egcl_rt::symbols::intern("OBJECT-CALL-CALLER")).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        TARGET_LOOKUPS.with(|count| count.set(0));
        assert_eq!(cold(state, &args[..]).unwrap(), EgclVal::from_fixnum(42));
        assert!(TARGET_LOOKUPS.with(|count| count.get()) > 0);
        TARGET_LOOKUPS.with(|count| count.set(0));
        for _ in 0..100 {
            let entry =
                unsafe { &*cell.entry_address(false) }.load(std::sync::atomic::Ordering::Acquire);
            let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(entry) };
            assert_eq!(
                call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
                EgclVal::from_fixnum(42).0
            );
        }
        assert_eq!(
            TARGET_LOOKUPS.with(|count| count.get()),
            0,
            "native FUNCALL must bypass target lookup and Rust run_native frame setup"
        );
        let symbol = egcl_rt::function::name(*function).as_symbol_index();
        let input = snapshot_t2_input(symbol, 0).unwrap();
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
        assert!(
            install_t2_completion(T2Completion {
                sym: symbol,
                generation,
                artifact: Some(artifact),
                input,
            })
            .unwrap()
            .is_t2
        );
        TARGET_LOOKUPS.with(|count| count.set(0));
        let entry =
            unsafe { &*cell.entry_address(false) }.load(std::sync::atomic::Ordering::Acquire);
        let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
            unsafe { std::mem::transmute(entry) };
        assert_eq!(
            call(Arc::as_ptr(&cell) as u64, 2, args[0].0, args[1].0, NIL.0, 0),
            EgclVal::from_fixnum(42).0
        );
        assert!(
            TARGET_LOOKUPS.with(|count| count.get()) > 0,
            "replacing installed native code must invalidate the callback cache"
        );
        // A previous ordinary target must not remain a strong root when the
        // same slot switches to the weak native-FUNCALL dispatcher.
        let funcall_cell = resolve(egcl_rt::symbols::intern("FUNCALL")).unwrap();
        let funcall_state = unsafe { super::state(Arc::as_ptr(&funcall_cell) as u64) };
        let obsolete_symbol = {
            egcl_rt::rooted!(form = super::super::super::vec_to_list(&[NIL]));
            let body = Arc::new(compile_function("OBSOLETE-FUNCALL-TARGET", NIL, *form, &env, false, false).unwrap());
            egcl_rt::rooted!(obsolete = make_bytecode_closure(&body, None));
            *funcall_state.target.borrow_mut() = Some(Target::Function(*obsolete));
            egcl_rt::function::name(*obsolete).as_symbol_index()
        };
        assert_eq!(cold(funcall_state, &args[..]).unwrap(), EgclVal::from_fixnum(42));
        env.mv.clear();
        egcl_rt::gc::full_gc().unwrap();
        assert!(!is_registered(obsolete_symbol),
            "installing native FUNCALL must release the previous generic target");

    }

    fn check_native_slot(name: &str, params: &str, expression: &str, args: &[i64], expected: i64) {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        super::super::super::read_eval_all_env(
            &format!("(defun {name} {params} {expression})"),
            &mut env,
        )
        .unwrap();
        let symbol = egcl_rt::symbols::intern(name);
        egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
        egcl_rt::rooted!(
            form = reader::read_from_string(&format!("({expression})"))
                .unwrap()
                .0
        );
        let body = compile_function(name, *params, *form, &env, false, false)
            .expect("compile native slot body");
        let args: Vec<EgclVal> = args.iter().copied().map(EgclVal::from_fixnum).collect();
        egcl_rt::rooted!(args = args);
        registry_put(symbol, Arc::new(body));
        let input = snapshot_t2_input(symbol, 0).expect("snapshot leaf");
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input
            .with_gc_stable(compile_t2_artifact)
            .expect("compile T2 leaf");
        let code = install_t2_completion(T2Completion {
            sym: symbol,
            generation,
            artifact: Some(artifact),
            input,
        })
        .expect("install T2 leaf");
        assert!(code.is_t2);
        let cell = resolve(symbol).unwrap();
        let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        TARGET_LOOKUPS.with(|count| count.set(0));
        assert_eq!(cold(state, &args).unwrap(), EgclVal::from_fixnum(expected));
        assert!(
            TARGET_LOOKUPS.with(|count| count.get()) > 0,
            "cold-call probe must be live"
        );
        assert!(!cell.is_cold());
        assert!(matches!(
            *state.target.borrow(),
            Some(Target::Native { .. })
        ));
        TARGET_LOOKUPS.with(|count| count.set(0));
        for _ in 0..100 {
            let entry = unsafe { &*cell.entry_address(args.len() > 3) }
                .load(std::sync::atomic::Ordering::Acquire);
            let result = if args.len() > 3 {
                let call: extern "C" fn(u64, u64, *const EgclVal, u64) -> u64 =
                    unsafe { std::mem::transmute(entry) };
                call(
                    Arc::as_ptr(&cell) as u64,
                    args.len() as u64,
                    args.as_ptr(),
                    0,
                )
            } else {
                let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
                    unsafe { std::mem::transmute(entry) };
                let arg = |i| args.get(i).copied().unwrap_or(NIL).0;
                call(
                    Arc::as_ptr(&cell) as u64,
                    args.len() as u64,
                    arg(0),
                    arg(1),
                    arg(2),
                    0,
                )
            };
            assert_eq!(result, EgclVal::from_fixnum(expected).0);
        }
        assert_eq!(
            TARGET_LOOKUPS.with(|count| count.get()),
            0,
            "warm native calls must bypass target resolution and Rust run_native frame setup"
        );
        registry_remove(symbol);
    }
}
