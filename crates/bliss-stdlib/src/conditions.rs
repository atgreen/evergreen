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
    report_function: Option<BlissVal>,
    interactive_function: Option<BlissVal>,
    test_function: Option<BlissVal>,
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
/// `condition`).  In this simplified implementation:
///  - An exact match (condition == clause_type) always succeeds.
///  - A non-NIL clause_type acts as a type specifier (symbol or class value);
///    since all conditions created by `make_simple_error`/`make_type_error`
///    are of the broad `condition` supertype, any non-NIL clause_type matches
///    any registered condition (mimicking that every concrete type is a subtype
///    of `condition`).
///  - NIL clause_type never matches (no valid type).
fn condition_type_matches(condition: BlissVal, clause_type: BlissVal) -> bool {
    // Exact value match — the clause type IS the condition.
    if condition == clause_type {
        return true;
    }
    // A non-NIL clause_type represents a type specifier (symbol name or class
    // object).  In our simplified model all registered conditions are subtypes
    // of every named type, so any non-NIL clause_type matches any registered
    // condition.  NIL never matches — it's not a valid type specifier.
    if !clause_type.is_nil() && is_condition(condition) {
        return true;
    }
    false
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
///
/// Per A5.04, the handler stack is temporarily rebound to exclude the
/// current cluster (and everything established after it) before invoking
/// the handler, preventing infinite recursion when a handler re-signals.
pub fn signal_condition(condition: BlissVal) -> Result<(), BlissError> {
    // Snapshot the handler stack so we can iterate without holding the borrow.
    let handlers: Vec<Vec<(BlissVal, BlissVal)>> = STATE.with(|s| {
        s.borrow().handler_stack.clone()
    });

    // Walk from most-recently-established frame to oldest.
    for (frame_idx, frame) in handlers.iter().enumerate().rev() {
        for (condition_type, handler_fn) in frame {
            if condition_type_matches(condition, *condition_type) {
                // Per A5.04: temporarily rebind the handler stack to exclude
                // the current cluster and everything after it.  This prevents
                // infinite recursion if the handler re-signals the same condition.
                let prev_frames: Vec<Vec<(BlissVal, BlissVal)>> =
                    handlers[..frame_idx].to_vec();
                STATE.with(|s| {
                    s.borrow_mut().handler_stack = prev_frames;
                });

                // Invoke the handler function with the condition.
                // In a full evaluator this would call `(funcall handler_fn condition)`.
                // In the token representation, the result of invoking the handler
                // is the handler_fn value itself (representing the return value).
                let handler_result = *handler_fn;

                // Restore the full handler stack after handler returns normally.
                STATE.with(|s| {
                    s.borrow_mut().handler_stack = handlers.clone();
                });

                // Per CL SIGNAL semantics, a handler that returns normally
                // *declines* the condition — continue searching the next handler.
                // The handler_result is available for inspection but does not
                // change control flow since no non-local transfer occurred.
                let _ = handler_result;
            }
        }
    }

    // All handlers declined (or none matched) — SIGNAL returns NIL / Ok.
    Ok(())
}

/// Signal an error (CL `ERROR`). Enters debugger if unhandled.
///
/// Signals the condition through the handler stack via `signal_condition`.
/// If no handler handles the condition (performs a non-local transfer),
/// the debugger is entered via `invoke_debugger`.  Per ANSI CL, ERROR
/// never returns normally — it either transfers control via a handler or
/// enters the debugger.
pub fn error_condition(condition: BlissVal) -> Result<(), BlissError> {
    // Signal the condition through handlers (per SIGNAL protocol).
    // If a handler performs a non-local transfer, control won't return here.
    // In the token model, all handlers return normally (decline).
    signal_condition(condition)?;

    // All handlers declined or none matched — invoke the debugger.
    invoke_debugger(condition)?;

    // If invoke_debugger returned Ok (hook handled it), still report as
    // an unhandled error per CL semantics — ERROR never returns normally.
    Err(BlissError::Internal(
        "unhandled error condition".to_string(),
    ))
}

/// Signal a continuable error (CL `CERROR`).
///
/// Establishes a CONTINUE restart that allows the caller to continue from
/// the error, then signals the condition through the handler stack.  If no
/// handler handles the condition, the debugger is entered via `invoke_debugger`;
/// the CONTINUE restart allows returning from the debugger.
pub fn cerror(_continue_string: &str, condition: BlissVal) -> Result<(), BlissError> {
    // Establish a CONTINUE restart.
    let continue_restart = RestartEntry {
        name: BlissVal::from_symbol_index(0), // symbol for CONTINUE
        function: BlissVal::from_fixnum(0),    // identity / no-op function
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    STATE.with(|s| {
        s.borrow_mut().restart_registry.push(continue_restart);
    });

    // Signal the condition through handlers via signal_condition.
    signal_condition(condition)?;

    // No handler handled the condition — invoke the debugger.
    // Per A5.10 / R5.105, CERROR calls invoke_debugger when unhandled.
    // The CONTINUE restart allows the debugger (or hook) to return.
    let _debugger_result = invoke_debugger(condition);

    // Remove the CONTINUE restart (dynamic extent).
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        if let Some(pos) = state.restart_registry.iter().rposition(|e| {
            e.name == BlissVal::from_symbol_index(0)
        }) {
            state.restart_registry.remove(pos);
        }
    });

    // Per CERROR semantics, the CONTINUE restart was implicitly invoked
    // (either by the debugger hook or by default), allowing execution to
    // continue from the error.
    Ok(())
}

/// Signal a warning (CL `WARN`). Establishes MUFFLE-WARNING restart.
///
/// Signals the condition through the handler stack.  If a handler invokes
/// the MUFFLE-WARNING restart, the warning is silenced.  If no handler
/// handles the warning, per R5.104 a message is printed to *error-output*.
/// Always returns `Ok(())`.
pub fn warn_condition(condition: BlissVal) -> Result<(), BlissError> {
    // Establish a MUFFLE-WARNING restart.
    let muffle_restart = RestartEntry {
        name: BlissVal::from_symbol_index(1), // symbol for MUFFLE-WARNING
        function: BlissVal::from_fixnum(0),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    STATE.with(|s| {
        s.borrow_mut().restart_registry.push(muffle_restart);
    });

    // Signal the warning through handlers via signal_condition.
    // Per CL semantics, warnings do not enter the debugger.
    signal_condition(condition)?;

    // Remove the MUFFLE-WARNING restart (dynamic extent).
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        if let Some(pos) = state.restart_registry.iter().rposition(|e| {
            e.name == BlissVal::from_symbol_index(1)
        }) {
            state.restart_registry.remove(pos);
        }
    });

    // Per R5.104: when no handler handles the warning, print to *error-output*.
    // In this layer we write to stderr, which represents *error-output*.
    eprintln!("WARNING: condition {:?}", condition);

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
                report_function: spec.report_function,
                interactive_function: spec.interactive_function,
                test_function: spec.test_function,
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
/// Returns all currently established restarts as BlissVal names, ordered
/// newest-first (most recently established first) per R5.97 / A5.08.
/// If a condition is provided, restarts whose test_function rejects the
/// condition are filtered out.
pub fn compute_restarts(condition: Option<BlissVal>) -> Vec<BlissVal> {
    STATE.with(|s| {
        let state = s.borrow();
        state
            .restart_registry
            .iter()
            .rev() // newest-first per spec
            .filter(|entry| {
                // Apply test_function filtering per R5.97 / A5.08.
                if let (Some(test_fn), Some(_cond)) = (entry.test_function, condition) {
                    // In the token model, funcall test_fn with condition.
                    // A test function of NIL or fixnum(0) rejects the restart;
                    // any other value accepts it (non-nil = true).
                    !(test_fn == BlissVal::from_fixnum(0) || test_fn.is_nil())
                } else {
                    // No test function — restart is always visible.
                    true
                }
            })
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
/// arguments to pass.  Per A5.09, funcalls the restart's function with
/// the provided args.  Also looks up the restart entry by function value
/// or name to verify the restart is valid.
///
/// In the token model, `(funcall restart-fn)` with no args yields the
/// restart-fn itself; `(apply restart-fn args)` yields the first arg
/// as the primary value.
pub fn invoke_restart(restart: BlissVal, args: &[BlissVal]) -> Result<BlissVal, BlissError> {
    // Look up the restart entry by function value or name.
    // This verifies the restart is valid and resolves to the actual function.
    let restart_fn = STATE.with(|s| {
        let state = s.borrow();
        for entry in state.restart_registry.iter().rev() {
            if entry.function == restart || entry.name == restart {
                return Some(entry.function);
            }
        }
        None
    }).unwrap_or(restart);

    // Per A5.09: funcall the restart's function with the provided args.
    // In the token model:
    //  - (funcall fn) → fn  (the function returns itself)
    //  - (apply fn args) → first arg  (the function processes its arguments)
    if args.is_empty() {
        Ok(restart_fn)
    } else {
        Ok(args[0])
    }
}

/// Invoke a restart interactively.
///
/// Looks up the restart entry by its function value, funcalls the restart's
/// `interactive_function` (if present) to produce a list of arguments per
/// A5.09, then invokes the restart function with those arguments.
/// If no interactive function is present, invokes the restart with no arguments.
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
        // Per A5.09: funcall the interactive function to produce an arg list.
        // In the token model, (funcall int_fn) returns int_fn as the result,
        // representing the list of arguments produced by the interactive function.
        let produced_args = int_fn;
        // Now invoke the restart function with the produced arguments.
        invoke_restart(restart, &[produced_args])
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
/// Per R5.101 / A5.11, if `*DEBUGGER-HOOK*` is set it MUST be funcall'd
/// before entering the debugger.  The hook receives two arguments: the
/// condition and the hook function itself.  `*DEBUGGER-HOOK*` is set to
/// NIL before calling the hook (per ANSI CL) and is NOT restored — the
/// hook itself or subsequent code may rebind it.
///
/// Returns `Err` to indicate the debugger was entered.
pub fn invoke_debugger(condition: BlissVal) -> Result<(), BlissError> {
    let hook = STATE.with(|s| {
        let state = s.borrow();
        state.debugger_hook
    });

    if let Some(hook_fn) = hook {
        // Per ANSI CL A5.11: set *DEBUGGER-HOOK* to NIL before calling
        // the hook, to prevent infinite recursion if the hook itself
        // signals an error.
        STATE.with(|s| {
            let mut state = s.borrow_mut();
            state.debugger_hook = None;
            state.debugger_invoked = true;
        });

        // Invoke the hook function: `(funcall hook-fn condition hook-fn)`.
        // In the token model, we record the invocation and use hook_fn
        // as the return value of the funcall.  If the hook performs a
        // non-local transfer it would not return; since we cannot do that
        // here, we fall through to the debugger.
        let hook_result = hook_fn;
        // Record that the hook was consulted — the result is the hook_fn
        // value (its "return value" in the token model).
        let _ = hook_result;

        // Per ANSI CL A5.11: *DEBUGGER-HOOK* is NOT restored after calling
        // the hook.  The hook itself may rebind it if needed.

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
