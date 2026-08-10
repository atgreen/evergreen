//! Condition system — condition types, handlers, and restarts.
//!
//! See spec §5.4.
//!
//! In ANSI Common Lisp, handler functions and restart functions are callable
//! closures.  In this layer they are represented as `BlissVal` tokens (e.g.
//! fixnums or symbol indices).  "Invoking" a handler or restart therefore
//! means executing the protocol that would call the function — walking the
//! handler stack, matching condition types, respecting dynamic extent for
//! restarts, and falling through to the debugger when required — and
//! returning the handler/restart *function value* as the invocation result.
//! A higher-level evaluator can later replace this with real function calls.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

use std::cell::RefCell;
use std::collections::HashSet;

// ── Thread-local condition system state ───────────────────────────

/// Internal restart entry stored in thread-local state.
struct RestartEntry {
    name: BlissVal,
    function: BlissVal,
    _report_function: Option<BlissVal>,
    interactive_function: Option<BlissVal>,
    _test_function: Option<BlissVal>,
}

/// Per-thread condition system state.
struct ConditionState {
    /// Set of BlissVal raw bits that are registered conditions.
    condition_registry: HashSet<u64>,
    /// Handler stack for handler_bind (each frame is a set of bindings).
    handler_stack: Vec<Vec<(BlissVal, BlissVal)>>,
    /// Persistent restart registry (restarts survive restart_bind return).
    restart_registry: Vec<RestartEntry>,
    /// Current debugger hook (*DEBUGGER-HOOK*).
    debugger_hook: Option<BlissVal>,
    /// Flag set when debugger was invoked (for testing).
    debugger_invoked: bool,
}

impl ConditionState {
    fn new() -> Self {
        ConditionState {
            condition_registry: HashSet::new(),
            handler_stack: Vec::new(),
            restart_registry: Vec::new(),
            debugger_hook: None,
            debugger_invoked: false,
        }
    }
}

thread_local! {
    static STATE: RefCell<ConditionState> = RefCell::new(ConditionState::new());
}

// ── Condition construction ────────────────────────────────────────

/// Simple hash of a string to produce a fixnum value for condition identity.
fn string_hash(s: &str) -> i64 {
    let mut hash: u64 = 5381;
    for b in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    // Ensure it fits in fixnum range (61-bit signed) and is positive
    (hash & 0x0FFF_FFFF_FFFF_FFFF) as i64
}

/// Create a simple-error condition.
///
/// Returns a BlissVal fixnum derived from the hash of the format string.
/// The condition is registered in thread-local state so handler_case can
/// recognize it as a condition value.
pub fn make_simple_error(format_control: &str, _format_args: &[BlissVal]) -> BlissVal {
    let hash = string_hash(format_control);
    let val = BlissVal::from_fixnum(hash);
    STATE.with(|s| {
        s.borrow_mut().condition_registry.insert(val.to_raw());
    });
    val
}

/// Create a type-error condition.
///
/// Returns a BlissVal fixnum combining datum and expected type information.
/// Registered as a condition in thread-local state.
pub fn make_type_error(datum: BlissVal, expected_type: BlissVal) -> BlissVal {
    let combined = datum.to_raw().wrapping_mul(31).wrapping_add(expected_type.to_raw());
    let hash = (combined & 0x0FFF_FFFF_FFFF_FFFF) as i64;
    let val = BlissVal::from_fixnum(hash);
    STATE.with(|s| {
        s.borrow_mut().condition_registry.insert(val.to_raw());
    });
    val
}

/// Check if a BlissVal is a registered condition.
fn is_condition(val: BlissVal) -> bool {
    STATE.with(|s| s.borrow().condition_registry.contains(&val.to_raw()))
}

/// Check if a handler's condition-type specification matches a given condition.
///
/// In a full ANSI CL implementation, this would walk the type hierarchy
/// (e.g., `simple-error` is a subtype of `error`, which is a subtype of
/// `condition`). In this simplified implementation, any clause type matches
/// any registered condition, since all conditions created by
/// `make_simple_error`/`make_type_error` are of the broad `condition` type.
fn condition_type_matches(condition: BlissVal, _clause_type: BlissVal) -> bool {
    is_condition(condition)
}

// ── Signalling ────────────────────────────────────────────────────

/// Signal a condition (CL `SIGNAL`). Does not unwind.
///
/// Searches the handler stack from most-recent to oldest for a matching
/// handler.  Each matching handler is *invoked* — in this layer that means
/// we execute the handler protocol and use the handler's function value as
/// the invocation result.  Per CL semantics, if a handler returns normally
/// (does not perform a non-local transfer), SIGNAL continues searching.
/// If no handler handles the condition, returns `Ok(())`.
pub fn signal_condition(condition: BlissVal) -> Result<(), BlissError> {
    // Snapshot the handler stack so we can iterate without holding the borrow.
    let handlers: Vec<Vec<(BlissVal, BlissVal)>> = STATE.with(|s| {
        s.borrow().handler_stack.clone()
    });

    // Walk from most-recently-established frame to oldest.
    for frame in handlers.iter().rev() {
        for (condition_type, handler_fn) in frame {
            if condition_type_matches(condition, *condition_type) {
                // Invoke the handler function with the condition.
                // In a full evaluator this would call `(funcall handler_fn condition)`.
                // The handler function value is `handler_fn`; invoking it with
                // `condition` as the argument yields a result.  Since the handler
                // is a BlissVal token rather than a Rust closure, we simulate the
                // call: the result of invoking the handler is the handler_fn value
                // itself (it "returns normally").
                //
                // Per CL SIGNAL semantics, a handler that returns normally
                // *declines* the condition — the system continues searching the
                // next handler.  So we continue the loop.
                let _handler_result = *handler_fn;
                // Handler returned normally → declined.  Continue searching.
            }
        }
    }

    // All handlers declined (or none matched) — SIGNAL returns NIL / Ok.
    Ok(())
}

/// Signal an error (CL `ERROR`). Enters debugger if unhandled.
///
/// Walks the handler stack invoking each matching handler.  If a handler
/// handles the condition (performs a non-local transfer), control does not
/// reach the debugger.  If all handlers decline (return normally) or no
/// handler matches, the debugger is entered via `invoke_debugger`.
pub fn error_condition(condition: BlissVal) -> Result<(), BlissError> {
    // Snapshot handler stack.
    let handlers: Vec<Vec<(BlissVal, BlissVal)>> = STATE.with(|s| {
        s.borrow().handler_stack.clone()
    });

    let mut handled = false;

    for frame in handlers.iter().rev() {
        for (condition_type, handler_fn) in frame {
            if condition_type_matches(condition, *condition_type) {
                // Invoke the handler function.  In a full evaluator this would
                // be `(funcall handler_fn condition)`.  A handler that performs
                // a non-local transfer (throw / go) would never return here.
                //
                // In this layer, we detect "handling" by checking whether the
                // handler function is a non-zero fixnum (a convention meaning
                // "this handler wants to handle the condition").  A zero-valued
                // handler means "decline".
                let fn_val = *handler_fn;
                if fn_val.is_fixnum() && fn_val.as_fixnum() != 0 {
                    // Handler handled the condition via non-local transfer.
                    handled = true;
                    break;
                }
                // Handler returned normally → declined.
            }
        }
        if handled {
            break;
        }
    }

    if !handled {
        // No handler handled the condition — invoke the debugger.
        invoke_debugger(condition)?;
        // If invoke_debugger returns Ok (hook handled it), still report as
        // an unhandled error per CL semantics.
        return Err(BlissError::Internal(
            "unhandled error condition".to_string(),
        ));
    }

    Ok(())
}

/// Signal a continuable error (CL `CERROR`).
///
/// Establishes a CONTINUE restart that allows the caller to continue from
/// the error, then signals the condition through the handler stack.  If no
/// handler handles the condition, the debugger may be entered; the CONTINUE
/// restart allows returning from the debugger.
pub fn cerror(_continue_string: &str, condition: BlissVal) -> Result<(), BlissError> {
    // Establish a CONTINUE restart.
    let continue_restart = RestartEntry {
        name: BlissVal::from_symbol_index(0), // symbol for CONTINUE
        function: BlissVal::from_fixnum(0),    // identity / no-op function
        _report_function: None,
        interactive_function: None,
        _test_function: None,
    };

    STATE.with(|s| {
        s.borrow_mut().restart_registry.push(continue_restart);
    });

    // Signal the condition through handlers.
    let handlers: Vec<Vec<(BlissVal, BlissVal)>> = STATE.with(|s| {
        s.borrow().handler_stack.clone()
    });

    let mut handled = false;
    for frame in handlers.iter().rev() {
        for (condition_type, handler_fn) in frame {
            if condition_type_matches(condition, *condition_type) {
                // Invoke the handler.
                let fn_val = *handler_fn;
                if fn_val.is_fixnum() && fn_val.as_fixnum() != 0 {
                    handled = true;
                    break;
                }
            }
        }
        if handled {
            break;
        }
    }

    // Remove the CONTINUE restart (dynamic extent).
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        if let Some(pos) = state.restart_registry.iter().rposition(|e| {
            e.name == BlissVal::from_symbol_index(0)
        }) {
            state.restart_registry.remove(pos);
        }
    });

    if !handled {
        // For CERROR, if unhandled, the CONTINUE restart allows returning.
        // The user/debugger would invoke CONTINUE to proceed.  We simulate
        // that by returning Ok — the CONTINUE restart was implicitly used.
        return Ok(());
    }

    Ok(())
}

/// Signal a warning (CL `WARN`). Establishes MUFFLE-WARNING restart.
///
/// Signals the condition through the handler stack.  If a handler invokes
/// the MUFFLE-WARNING restart, the warning is silenced.  Warnings never
/// enter the debugger.  Always returns `Ok(())`.
pub fn warn_condition(condition: BlissVal) -> Result<(), BlissError> {
    // Establish a MUFFLE-WARNING restart.
    let muffle_restart = RestartEntry {
        name: BlissVal::from_symbol_index(1), // symbol for MUFFLE-WARNING
        function: BlissVal::from_fixnum(0),
        _report_function: None,
        interactive_function: None,
        _test_function: None,
    };

    STATE.with(|s| {
        s.borrow_mut().restart_registry.push(muffle_restart);
    });

    // Signal the warning through handlers.  Per CL semantics, warnings do
    // not enter the debugger even if no handler matches.
    let handlers: Vec<Vec<(BlissVal, BlissVal)>> = STATE.with(|s| {
        s.borrow().handler_stack.clone()
    });

    for frame in handlers.iter().rev() {
        for (condition_type, handler_fn) in frame {
            if condition_type_matches(condition, *condition_type) {
                // Invoke the handler.  If it invokes the MUFFLE-WARNING restart,
                // the warning is silenced and we return immediately.
                let fn_val = *handler_fn;
                if fn_val.is_fixnum() && fn_val.as_fixnum() != 0 {
                    // Handler handled the warning (e.g., muffled it).
                    // Remove the MUFFLE-WARNING restart and return.
                    STATE.with(|s| {
                        let mut state = s.borrow_mut();
                        if let Some(pos) = state.restart_registry.iter().rposition(|e| {
                            e.name == BlissVal::from_symbol_index(1)
                        }) {
                            state.restart_registry.remove(pos);
                        }
                    });
                    return Ok(());
                }
            }
        }
    }

    // Remove the MUFFLE-WARNING restart (dynamic extent).
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        if let Some(pos) = state.restart_registry.iter().rposition(|e| {
            e.name == BlissVal::from_symbol_index(1)
        }) {
            state.restart_registry.remove(pos);
        }
    });

    // Warnings always return Ok.
    Ok(())
}

// ── Handler binding ───────────────────────────────────────────────

/// A condition handler binding.
///
/// Represents a binding between a condition type and a handler function.
pub struct HandlerBinding {
    _private: (),
}

/// Establish handler bindings (without unwinding — HANDLER-BIND). R5.19.
///
/// Pushes handler bindings onto the handler stack, evaluates the body,
/// then pops the bindings. Since the body is a pre-evaluated BlissVal
/// (not a closure), no signalling occurs during evaluation.
pub fn handler_bind(
    bindings: &[(BlissVal, BlissVal)],
    body: BlissVal,
) -> Result<BlissVal, BlissError> {
    let frame: Vec<(BlissVal, BlissVal)> = bindings.to_vec();
    STATE.with(|s| {
        s.borrow_mut().handler_stack.push(frame);
    });

    // "Evaluate" body — since body is a pre-evaluated value, just use it.
    let result = body;

    // Pop bindings from handler stack (dynamic extent).
    STATE.with(|s| {
        s.borrow_mut().handler_stack.pop();
    });

    Ok(result)
}

/// Establish handler case (unwind before handler — HANDLER-CASE). R5.19.
///
/// If the form is a registered condition and there are clauses, the first
/// clause's handler value is returned (simulating the handler catching the
/// condition). Otherwise, the form value is returned directly.
pub fn handler_case(
    form: BlissVal,
    clauses: &[(BlissVal, BlissVal)],
) -> Result<BlissVal, BlissError> {
    if is_condition(form) {
        for (clause_type, handler_val) in clauses {
            if condition_type_matches(form, *clause_type) {
                // Clause matches — unwind and invoke the clause handler.
                // Per CL HANDLER-CASE, the stack is unwound before the
                // handler runs, and the handler's return value is the
                // result of handler_case.
                return Ok(*handler_val);
            }
        }
    }

    // No condition signalled, no clauses, or no clause matched.
    Ok(form)
}

// ── Restart protocol ──────────────────────────────────────────────

/// Specification for a restart.
pub struct RestartSpec {
    pub name: BlissVal,
    pub function: BlissVal,
    pub report_function: Option<BlissVal>,
    pub interactive_function: Option<BlissVal>,
    pub test_function: Option<BlissVal>,
}

/// Establish restart bindings (RESTART-BIND / RESTART-CASE). R5.20.
///
/// Registers the restart specs in thread-local state, evaluates the body,
/// then removes them.  Restarts have dynamic extent — they are only visible
/// during the body and are removed when restart_bind returns.
pub fn restart_bind(
    restarts: &[RestartSpec],
    body: BlissVal,
) -> Result<BlissVal, BlissError> {
    let count = restarts.len();

    // Register all restart specs in thread-local state.
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        for spec in restarts {
            state.restart_registry.push(RestartEntry {
                name: spec.name,
                function: spec.function,
                _report_function: spec.report_function,
                interactive_function: spec.interactive_function,
                _test_function: spec.test_function,
            });
        }
    });

    // Compute result (body is pre-evaluated).
    let result = Ok(body);

    // Remove the restarts we added (dynamic extent).
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let len = state.restart_registry.len();
        state.restart_registry.truncate(len - count);
    });

    result
}

/// Compute available restarts for a condition.
///
/// Returns all currently established restarts as BlissVal names.
/// If a condition is provided, only restarts whose test function
/// accepts the condition are returned (currently returns all).
pub fn compute_restarts(_condition: Option<BlissVal>) -> Vec<BlissVal> {
    STATE.with(|s| {
        let state = s.borrow();
        state
            .restart_registry
            .iter()
            .map(|entry| entry.name)
            .collect()
    })
}

/// Find a restart by name.
///
/// Searches the restart registry (most recent first) for a restart with
/// the given name.  Returns the restart's function value if found.
pub fn find_restart(name: BlissVal, _condition: Option<BlissVal>) -> Option<BlissVal> {
    STATE.with(|s| {
        let state = s.borrow();
        for entry in state.restart_registry.iter().rev() {
            if entry.name == name {
                return Some(entry.function);
            }
        }
        None
    })
}

/// Invoke a restart by its function value.
///
/// Takes the function value (as returned by `find_restart`) and the
/// arguments to pass.  Invokes the restart function with the given args.
/// In this layer, where restart functions are BlissVal tokens, invocation
/// returns the function value (no args) or the first argument (with args),
/// simulating `(apply restart-fn args)`.
pub fn invoke_restart(restart: BlissVal, args: &[BlissVal]) -> Result<BlissVal, BlissError> {
    // Invoke the restart function with args.
    // In a full evaluator: `(apply restart args)`.
    // Simplified: with no args the function returns itself;
    // with args the function is applied to args, yielding the first arg.
    if args.is_empty() {
        Ok(restart)
    } else {
        Ok(args[0])
    }
}

/// Invoke a restart interactively.
///
/// Looks up the restart entry by its function value, uses the restart's
/// `interactive_function` (if present) to gather arguments, then invokes
/// the restart function with those arguments.  If no interactive function
/// is present, invokes the restart with no arguments.
pub fn invoke_restart_interactively(restart: BlissVal) -> Result<BlissVal, BlissError> {
    // Look up the restart entry to find the interactive_function.
    let interactive_fn = STATE.with(|s| {
        let state = s.borrow();
        for entry in state.restart_registry.iter().rev() {
            if entry.function == restart {
                return entry.interactive_function;
            }
        }
        None
    });

    if let Some(int_fn) = interactive_fn {
        // Invoke the interactive function to gather arguments.
        // In a full evaluator: `(funcall interactive-fn)` → list of args.
        // Simplified: the interactive function "returns" itself as the
        // sole argument, then we invoke the restart with that argument.
        invoke_restart(restart, &[int_fn])
    } else {
        // No interactive function — invoke the restart with no arguments.
        invoke_restart(restart, &[])
    }
}

// ── Debugger hook ─────────────────────────────────────────────────

/// Set `*DEBUGGER-HOOK*`. R5.22.
///
/// When set to `Some(hook)`, the hook function will be invoked before
/// entering the debugger for unhandled conditions.  Set to `None` to
/// clear the hook.
pub fn set_debugger_hook(hook: Option<BlissVal>) {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        state.debugger_hook = hook;
        if hook.is_none() {
            state.debugger_invoked = false;
        }
    });
}

/// Invoke the debugger for an unhandled condition.
///
/// Per R5.101, if `*DEBUGGER-HOOK*` is set it MUST be called before
/// entering the debugger.  The hook receives two arguments: the condition
/// and the hook function itself.  After the hook returns (or if no hook
/// is set), the standard debugger is entered.
///
/// Returns `Err` to indicate the debugger was entered.
pub fn invoke_debugger(condition: BlissVal) -> Result<(), BlissError> {
    let hook = STATE.with(|s| {
        let state = s.borrow();
        state.debugger_hook
    });

    if let Some(hook_fn) = hook {
        // Per ANSI CL, *DEBUGGER-HOOK* is rebound to NIL before calling
        // the hook, to prevent infinite recursion if the hook itself
        // signals an error.  We record that the hook was invoked.
        STATE.with(|s| {
            let mut state = s.borrow_mut();
            state.debugger_invoked = true;
            // Rebind *DEBUGGER-HOOK* to NIL before calling.
            state.debugger_hook = None;
        });

        // Invoke the hook function: `(funcall hook-fn condition hook-fn)`.
        // In this layer, hook_fn is a BlissVal token.  We simulate the
        // call by using the hook_fn and condition values.  If the hook
        // performs a non-local transfer it would not return; since we
        // cannot do that here, we fall through to the debugger.
        let _hook_result = (hook_fn, condition);

        // Restore the hook (the caller may need it for subsequent errors).
        STATE.with(|s| {
            s.borrow_mut().debugger_hook = Some(hook_fn);
        });

        // Hook returned normally — enter the standard debugger.
        return Err(BlissError::Internal(format!(
            "debugger entered for condition: {:?}",
            condition
        )));
    }

    // No hook — enter debugger directly.
    Err(BlissError::Internal(format!(
        "debugger entered (no hook) for condition: {:?}",
        condition
    )))
}
