//! Condition system — condition types, handlers, and restarts.
//!
//! See spec §5.4.
//!
//! Handler functions, restart functions, and body thunks are invoked through
//! a pluggable `funcall` mechanism.  By default the hook is unset and a
//! token-level default is used (returns the function value itself with no args,
//! or the first arg when args are supplied).  The evaluator installs a real
//! hook via `set_funcall_hook` so that handlers, restarts, and debugger hooks
//! are actually called at runtime.

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, TAG_SYMBOL};

use std::cell::RefCell;
use std::collections::HashMap;

// ── Well-known symbol indices ────────────────────────────────────
//
// These are reserved symbol-table slots for condition-system names.
// Using named constants instead of magic numbers (fixes issue 8).

/// Symbol index for the CONTINUE restart name.
pub const SYMBOL_CONTINUE: u32 = 110;
/// Symbol index for the MUFFLE-WARNING restart name.
pub const SYMBOL_MUFFLE_WARNING: u32 = 111;

/// Symbol index for the CONDITION type (root of the condition hierarchy).
pub const SYMBOL_CONDITION: u32 = 100;
/// Symbol index for WARNING.
pub const SYMBOL_WARNING: u32 = 101;
/// Symbol index for SERIOUS-CONDITION.
pub const SYMBOL_SERIOUS_CONDITION: u32 = 102;
/// Symbol index for ERROR.
pub const SYMBOL_ERROR: u32 = 103;
/// Symbol index for SIMPLE-ERROR.
pub const SYMBOL_SIMPLE_ERROR: u32 = 104;
/// Symbol index for TYPE-ERROR.
pub const SYMBOL_TYPE_ERROR: u32 = 105;
/// Symbol index for SIMPLE-WARNING.
pub const SYMBOL_SIMPLE_WARNING: u32 = 106;
/// Symbol index for CONTROL-ERROR.
pub const SYMBOL_CONTROL_ERROR: u32 = 107;

/// All known condition-type symbol indices (used for hierarchy discrimination).
const KNOWN_CONDITION_TYPES: &[u32] = &[
    SYMBOL_CONDITION,
    SYMBOL_WARNING,
    SYMBOL_SERIOUS_CONDITION,
    SYMBOL_ERROR,
    SYMBOL_SIMPLE_ERROR,
    SYMBOL_TYPE_ERROR,
    SYMBOL_SIMPLE_WARNING,
    SYMBOL_CONTROL_ERROR,
];

/// Check whether a BlissVal represents a known condition type symbol.
fn is_known_condition_type(val: BlissVal) -> bool {
    if (val.0 & 0b111) == TAG_SYMBOL {
        let idx = val.as_symbol_index();
        KNOWN_CONDITION_TYPES.contains(&idx)
    } else {
        false
    }
}

// ── Funcall hook ─────────────────────────────────────────────────
//
// A pluggable function-invocation mechanism.  The evaluator sets this
// so that handler fns, restart fns, debugger hooks, and body thunks
// are actually called.  When unset, the default token-level behaviour
// is used (backward-compatible with unit tests).

type FuncallFn = Box<dyn Fn(BlissVal, &[BlissVal]) -> Result<BlissVal, BlissError>>;

thread_local! {
    static FUNCALL_HOOK: RefCell<Option<FuncallFn>> = RefCell::new(None);
}

/// Install a funcall hook for the condition system.
///
/// When set, every handler invocation, restart invocation, and debugger-hook
/// call will go through this hook, enabling real function calls.
pub fn set_funcall_hook(
    hook: impl Fn(BlissVal, &[BlissVal]) -> Result<BlissVal, BlissError> + 'static,
) {
    FUNCALL_HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(hook));
    });
}

/// Clear the funcall hook.
pub fn clear_funcall_hook() {
    FUNCALL_HOOK.with(|h| {
        *h.borrow_mut() = None;
    });
}

/// Call a function value with args, going through the hook if set.
fn funcall(function: BlissVal, args: &[BlissVal]) -> Result<BlissVal, BlissError> {
    FUNCALL_HOOK.with(|h| {
        let borrow = h.borrow();
        if let Some(hook) = borrow.as_ref() {
            hook(function, args)
        } else {
            // Default token-level behaviour:
            //  - (funcall fn) → fn
            //  - (funcall fn a b ...) → first arg
            if args.is_empty() {
                Ok(function)
            } else {
                Ok(args[0])
            }
        }
    })
}

// ── Thread-local condition system state ───────────────────────────

/// Internal restart entry stored in thread-local state.
#[allow(dead_code)]
struct RestartEntry {
    name: BlissVal,
    function: BlissVal,
    #[expect(dead_code, reason = "restart metadata is stored for later reporting hooks")]
    report_function: Option<BlissVal>,
    interactive_function: Option<BlissVal>,
    test_function: Option<BlissVal>,
}

/// Per-thread condition system state.
struct ConditionState {
    /// Map of BlissVal raw bits → type hierarchy (list of supertype raw bits).
    condition_registry: HashMap<u64, Vec<u64>>,
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
            condition_registry: HashMap::new(),
            handler_stack: Vec::new(),
            restart_registry: Vec::new(),
            debugger_hook: None,
            debugger_invoked: false,
        }
    }
}

thread_local! {
    static STATE: RefCell<ConditionState> = RefCell::new(ConditionState::new());
    /// Flag set by the MUFFLE-WARNING restart to suppress warning output.
    static WARNING_MUFFLED: RefCell<bool> = const { RefCell::new(false) };
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

/// Build the type hierarchy for SIMPLE-ERROR:
/// SIMPLE-ERROR <: ERROR <: SERIOUS-CONDITION <: CONDITION
fn simple_error_types() -> Vec<u64> {
    vec![
        BlissVal::from_symbol_index(SYMBOL_SIMPLE_ERROR).to_raw(),
        BlissVal::from_symbol_index(SYMBOL_ERROR).to_raw(),
        BlissVal::from_symbol_index(SYMBOL_SERIOUS_CONDITION).to_raw(),
        BlissVal::from_symbol_index(SYMBOL_CONDITION).to_raw(),
    ]
}

/// Build the type hierarchy for TYPE-ERROR:
/// TYPE-ERROR <: ERROR <: SERIOUS-CONDITION <: CONDITION
fn type_error_types() -> Vec<u64> {
    vec![
        BlissVal::from_symbol_index(SYMBOL_TYPE_ERROR).to_raw(),
        BlissVal::from_symbol_index(SYMBOL_ERROR).to_raw(),
        BlissVal::from_symbol_index(SYMBOL_SERIOUS_CONDITION).to_raw(),
        BlissVal::from_symbol_index(SYMBOL_CONDITION).to_raw(),
    ]
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
        s.borrow_mut()
            .condition_registry
            .insert(val.to_raw(), simple_error_types());
    });
    val
}

/// Create a type-error condition.
///
/// Returns a BlissVal fixnum combining datum and expected type information.
/// Registered as a condition in thread-local state.
pub fn make_type_error(datum: BlissVal, expected_type: BlissVal) -> BlissVal {
    let combined = datum
        .to_raw()
        .wrapping_mul(31)
        .wrapping_add(expected_type.to_raw());
    let hash = (combined & 0x0FFF_FFFF_FFFF_FFFF) as i64;
    let val = BlissVal::from_fixnum(hash);
    STATE.with(|s| {
        s.borrow_mut()
            .condition_registry
            .insert(val.to_raw(), type_error_types());
    });
    val
}

/// Check if a BlissVal is a registered condition.
fn is_condition(val: BlissVal) -> bool {
    STATE.with(|s| s.borrow().condition_registry.contains_key(&val.to_raw()))
}

/// Check if a handler's condition-type specification matches a given condition.
///
/// Matching rules (issue 7 fix — proper type discrimination):
///  1. Exact match (condition == clause_type) always succeeds.
///  2. NIL clause_type never matches (no valid type).
///  3. If clause_type is a *known* condition type symbol (e.g. ERROR, WARNING),
///     it matches only when it appears in the condition's stored type hierarchy.
///     This prevents a WARNING handler from catching an ERROR, etc.
///  4. If clause_type is an *unknown* symbol (not in the well-known set), it is
///     treated as a catch-all CONDITION-level specifier for backward compat.
fn condition_type_matches(condition: BlissVal, clause_type: BlissVal) -> bool {
    // Rule 1: exact match.
    if condition == clause_type {
        return true;
    }
    // Rule 2: NIL never matches.
    if clause_type.is_nil() {
        return false;
    }
    STATE.with(|s| {
        let state = s.borrow();
        if let Some(types) = state.condition_registry.get(&condition.to_raw()) {
            if is_known_condition_type(clause_type) {
                // Rule 3: known type → check hierarchy.
                types.contains(&clause_type.to_raw())
            } else {
                // Rule 4: unknown type → catch-all (backward compat).
                true
            }
        } else {
            false
        }
    })
}

// ── Signalling ────────────────────────────────────────────────────

/// Signal a condition (CL `SIGNAL`). Does not unwind.
///
/// Searches the handler stack from most-recent to oldest for a matching
/// handler.  Each matching handler is *invoked* via `funcall(handler_fn,
/// condition)`.  Per CL semantics, if a handler returns normally (does not
/// perform a non-local transfer), SIGNAL continues searching.  If no handler
/// handles the condition, returns `Ok(())`.
///
/// Per A5.04, the handler stack is temporarily rebound to exclude the
/// current cluster (and everything established after it) before invoking
/// the handler, preventing infinite recursion when a handler re-signals.
pub fn signal_condition(condition: BlissVal) -> Result<(), BlissError> {
    // Snapshot the handler stack so we can iterate without holding the borrow.
    let handlers: Vec<Vec<(BlissVal, BlissVal)>> =
        STATE.with(|s| s.borrow().handler_stack.clone());

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

                // Invoke the handler function: (funcall handler_fn condition).
                let handler_result = funcall(*handler_fn, &[condition]);

                // Restore the full handler stack after handler returns normally.
                STATE.with(|s| {
                    s.borrow_mut().handler_stack = handlers.clone();
                });

                // If the handler itself errored, propagate the error
                // (after having already restored the handler stack above).
                handler_result?;
                // Per CL SIGNAL semantics, a handler that returns normally
                // *declines* the condition — continue searching the next handler.
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
    // Establish a CONTINUE restart using the named constant (issue 8 fix).
    let continue_name = BlissVal::from_symbol_index(SYMBOL_CONTINUE);
    let continue_restart = RestartEntry {
        name: continue_name,
        function: BlissVal::from_fixnum(0), // identity / no-op function
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
        if let Some(pos) = state
            .restart_registry
            .iter()
            .rposition(|e| e.name == continue_name)
        {
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
    // Establish a MUFFLE-WARNING restart using the named constant (issue 8 fix).
    let muffle_name = BlissVal::from_symbol_index(SYMBOL_MUFFLE_WARNING);
    let muffle_restart = RestartEntry {
        name: muffle_name,
        function: BlissVal::from_fixnum(0),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    // Reset the muffled flag before signalling.
    WARNING_MUFFLED.with(|m| *m.borrow_mut() = false);

    STATE.with(|s| {
        s.borrow_mut().restart_registry.push(muffle_restart);
    });

    // Signal the warning through handlers via signal_condition.
    // Per CL semantics, warnings do not enter the debugger.
    signal_condition(condition)?;

    // Remove the MUFFLE-WARNING restart (dynamic extent).
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        if let Some(pos) = state
            .restart_registry
            .iter()
            .rposition(|e| e.name == muffle_name)
        {
            state.restart_registry.remove(pos);
        }
    });

    // Per R5.104: only print the warning if MUFFLE-WARNING was NOT invoked.
    let muffled = WARNING_MUFFLED.with(|m| *m.borrow());
    if !muffled {
        eprintln!("WARNING: condition {:?}", condition);
    }

    Ok(())
}

// ── Handler binding ───────────────────────────────────────────────

/// A condition handler binding.
///
/// Represents a binding between a condition type and a handler function,
/// used to construct bindings for `handler_bind` / `handler_bind_fn`.
pub struct HandlerBinding {
    /// The condition type this handler matches against.
    pub condition_type: BlissVal,
    /// The handler function to invoke when the condition type matches.
    pub handler_fn: BlissVal,
}

impl HandlerBinding {
    /// Create a new handler binding.
    pub fn new(condition_type: BlissVal, handler_fn: BlissVal) -> Self {
        HandlerBinding {
            condition_type,
            handler_fn,
        }
    }

    /// Convert to the tuple representation used by handler_bind.
    pub fn as_tuple(&self) -> (BlissVal, BlissVal) {
        (self.condition_type, self.handler_fn)
    }
}

/// Establish handler bindings (without unwinding — HANDLER-BIND). R5.19.
///
/// Pushes handler bindings onto the handler stack, evaluates the body,
/// then pops the bindings.  When `body` is a pre-evaluated BlissVal,
/// it is returned directly (no conditions can be signalled during a
/// pre-evaluated body).  For real body evaluation with conditions
/// active, use `handler_bind_fn`.
pub fn handler_bind(
    bindings: &[(BlissVal, BlissVal)],
    body: BlissVal,
) -> Result<BlissVal, BlissError> {
    let frame: Vec<(BlissVal, BlissVal)> = bindings.to_vec();
    STATE.with(|s| {
        s.borrow_mut().handler_stack.push(frame);
    });

    // Evaluate the body via funcall if the value looks like a callable
    // (function tag), otherwise return it directly.  This provides backward
    // compat for tests that pass a pre-evaluated BlissVal while supporting
    // real thunks when the evaluator wraps the body in a closure.
    let result = if body.is_function() {
        funcall(body, &[])
    } else {
        Ok(body)
    };

    // Pop bindings from handler stack (dynamic extent).
    STATE.with(|s| {
        s.borrow_mut().handler_stack.pop();
    });

    result
}

/// Establish handler bindings with a Rust closure body (HANDLER-BIND). R5.19.
///
/// This is the closure-based variant that allows conditions to be signalled
/// during body evaluation.  The handlers are active during the closure call.
pub fn handler_bind_fn(
    bindings: &[(BlissVal, BlissVal)],
    body: impl FnOnce() -> Result<BlissVal, BlissError>,
) -> Result<BlissVal, BlissError> {
    let frame: Vec<(BlissVal, BlissVal)> = bindings.to_vec();
    STATE.with(|s| {
        s.borrow_mut().handler_stack.push(frame);
    });

    let result = body();

    // Pop bindings from handler stack (dynamic extent).
    STATE.with(|s| {
        s.borrow_mut().handler_stack.pop();
    });

    result
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
                // Clause matches — unwind and return the clause handler value.
                // Per CL HANDLER-CASE, the stack is unwound before the
                // handler runs.  If handler_val is a function, funcall it
                // with the condition; otherwise return it directly as the
                // pre-evaluated clause result.
                if handler_val.is_function() {
                    return funcall(*handler_val, &[form]);
                }
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
///
/// When `body` is a pre-evaluated BlissVal, it is returned directly.
/// For real body evaluation with restarts active, use `restart_bind_fn`.
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

    // Evaluate the body.  If it's a function, invoke it via funcall;
    // otherwise return the pre-evaluated value directly.
    let result = if body.is_function() {
        funcall(body, &[])
    } else {
        Ok(body)
    };

    // Remove the restarts we added (dynamic extent).
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let len = state.restart_registry.len();
        state.restart_registry.truncate(len - count);
    });

    result
}

/// Establish restart bindings with a Rust closure body (RESTART-BIND). R5.20.
///
/// This is the closure-based variant that allows restarts to be exercised
/// during body evaluation.  The restarts are active during the closure call.
pub fn restart_bind_fn(
    restarts: &[RestartSpec],
    body: impl FnOnce() -> Result<BlissVal, BlissError>,
) -> Result<BlissVal, BlissError> {
    let count = restarts.len();

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

    let result = body();

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
                if let (Some(test_fn), Some(cond)) = (entry.test_function, condition) {
                    // Funcall the test function with the condition.
                    // A result of NIL (or error) means reject; non-NIL means accept.
                    match funcall(test_fn, &[cond]) {
                        Ok(result) => !result.is_nil(),
                        Err(_) => false,
                    }
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
/// Searches the restart registry (most recent first) for the most recently
/// established *applicable* restart with the given name.  Per R5.98, when
/// a condition is provided, restarts whose test_function rejects the
/// condition are skipped.  Returns the restart's function value if found.
pub fn find_restart(name: BlissVal, condition: Option<BlissVal>) -> Option<BlissVal> {
    STATE.with(|s| {
        let state = s.borrow();
        for entry in state.restart_registry.iter().rev() {
            if entry.name == name {
                // Apply test_function filtering when a condition is provided,
                // consistent with compute_restarts (per R5.98).
                if let (Some(test_fn), Some(cond)) = (entry.test_function, condition) {
                    match funcall(test_fn, &[cond]) {
                        Ok(result) if !result.is_nil() => return Some(entry.function),
                        _ => continue, // test rejected or errored — skip
                    }
                } else {
                    // No test function — restart is applicable.
                    return Some(entry.function);
                }
            }
        }
        None
    })
}

/// Invoke a restart by its function value or name.
///
/// Takes the function value (as returned by `find_restart`) or a restart
/// name (symbol) and the arguments to pass.  Per A5.09, funcalls the
/// restart's function with the provided args.
///
/// If the argument is a symbol (restart name) and the restart is not found,
/// a CONTROL-ERROR is signalled per §5.4.9.
pub fn invoke_restart(restart: BlissVal, args: &[BlissVal]) -> Result<BlissVal, BlissError> {
    // Look up the restart entry by function value or name.
    let restart_entry = STATE.with(|s| {
        let state = s.borrow();
        for entry in state.restart_registry.iter().rev() {
            if entry.function == restart || entry.name == restart {
                return Some((entry.function, entry.name));
            }
        }
        None
    });

    // If invoking MUFFLE-WARNING, set the muffled flag so warn_condition
    // knows to suppress the warning message (issue 2 fix).
    if let Some((_, name)) = restart_entry {
        let muffle_name = BlissVal::from_symbol_index(SYMBOL_MUFFLE_WARNING);
        if name == muffle_name {
            WARNING_MUFFLED.with(|m| *m.borrow_mut() = true);
        }
    }

    let restart_fn = restart_entry.map(|(f, _)| f);

    match restart_fn {
        Some(func) => {
            // Found in registry — funcall the restart function with args.
            funcall(func, args)
        }
        None => {
            // Not found in registry.
            if restart.is_symbol() {
                // Per A5.09 / §5.4.9: if the restart name is not found,
                // signal a CONTROL-ERROR.
                Err(BlissError::Internal(format!(
                    "CONTROL-ERROR: no restart named {:?} is active",
                    restart
                )))
            } else {
                // Treat as a direct restart function value (restart object)
                // and funcall it with the provided args.
                funcall(restart, args)
            }
        }
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
            if entry.function == restart || entry.name == restart {
                return entry.interactive_function;
            }
        }
        None
    });

    if let Some(int_fn) = interactive_fn {
        // Per A5.09: funcall the interactive function to produce an arg list.
        let produced_args = funcall(int_fn, &[])?;
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
        // Per R5.101 / A5.11 the hook receives (condition, hook-fn).
        let _hook_result = funcall(hook_fn, &[condition, hook_fn]);

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
