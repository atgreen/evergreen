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
