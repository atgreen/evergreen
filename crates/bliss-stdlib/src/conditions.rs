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
use bliss_rt::value::{BlissVal, NIL, TAG_SYMBOL};
use crate::clos::{
    allocate_instance_pinned_gc, bootstrap_clos, class_direct_superclasses, class_name, class_of,
    define_class, find_class, initialize_instance, make_instance,
};
use crate::streams::make_lisp_string_fresh;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

// ── Well-known symbol indices ────────────────────────────────────
//
// These are reserved symbol-table slots for condition-system names.
// Using named constants instead of magic numbers (fixes issue 8).

/// Symbol index for the CONTINUE restart name.
pub const SYMBOL_CONTINUE: u32 = 110;
/// Symbol index for the MUFFLE-WARNING restart name.
pub const SYMBOL_MUFFLE_WARNING: u32 = 111;
const INTERNAL_CONTINUE_RESTART_FN: u32 = 112;
const INTERNAL_MUFFLE_WARNING_RESTART_FN: u32 = 113;

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
pub const SYMBOL_SIMPLE_CONDITION: u32 = 108;
pub const SYMBOL_STORAGE_CONDITION: u32 = 109;
const SYMBOL_FORMAT_CONTROL: u32 = 120;
const SYMBOL_FORMAT_ARGUMENTS: u32 = 121;
const SYMBOL_DATUM: u32 = 122;
const SYMBOL_EXPECTED_TYPE: u32 = 123;
const INTERNAL_HANDLER_CASE_FN_BASE: i64 = -9_000_000;
const STORAGE_CONDITION_POOL_SIZE: usize = 4;

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
    SYMBOL_SIMPLE_CONDITION,
    SYMBOL_STORAGE_CONDITION,
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
    if function == BlissVal::from_symbol_index(INTERNAL_CONTINUE_RESTART_FN)
        || function == BlissVal::from_symbol_index(INTERNAL_MUFFLE_WARNING_RESTART_FN)
    {
        return Ok(args.first().copied().unwrap_or(NIL));
    }
    if function.is_fixnum() && function.as_fixnum() <= INTERNAL_HANDLER_CASE_FN_BASE {
        let matched = STATE.with(|s| {
            let mut state = s.borrow_mut();
            let handler_val = state.handler_case_clauses.get(&function.to_raw()).copied();
            if let Some(handler_val) = handler_val {
                state.pending_handler_case =
                    Some((args.first().copied().unwrap_or(NIL), handler_val));
            }
            handler_val
        });
        if matched.is_some() {
            return Err(BlissError::Internal("__HANDLER_CASE__".into()));
        }
    }
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
    #[expect(
        dead_code,
        reason = "restart metadata is stored for later reporting hooks"
    )]
    report_function: Option<BlissVal>,
    interactive_function: Option<BlissVal>,
    test_function: Option<BlissVal>,
}

/// Per-thread condition system state.
struct ConditionState {
    /// Handler stack for handler_bind (each frame is a set of bindings).
    handler_stack: Vec<Vec<(BlissVal, BlissVal)>>,
    /// Persistent restart registry (restarts survive restart_bind return).
    restart_registry: Vec<RestartEntry>,
    /// Current debugger hook (*DEBUGGER-HOOK*).
    debugger_hook: Option<BlissVal>,
    /// Current *BREAK-ON-SIGNALS* value.
    break_on_signals: Option<BlissVal>,
    /// Flag set when debugger was invoked (for testing).
    debugger_invoked: bool,
    handler_case_clauses: HashMap<u64, BlissVal>,
    pending_handler_case: Option<(BlissVal, BlissVal)>,
    next_handler_case_id: i64,
    storage_condition_pool: [BlissVal; STORAGE_CONDITION_POOL_SIZE],
    next_storage_condition: usize,
    storage_condition_pool_initialized: bool,
}

impl ConditionState {
    fn new() -> Self {
        ConditionState {
            handler_stack: Vec::new(),
            restart_registry: Vec::new(),
            debugger_hook: None,
            break_on_signals: None,
            debugger_invoked: false,
            handler_case_clauses: HashMap::new(),
            pending_handler_case: None,
            next_handler_case_id: 0,
            storage_condition_pool: [NIL; STORAGE_CONDITION_POOL_SIZE],
            next_storage_condition: 0,
            storage_condition_pool_initialized: false,
        }
    }
}

thread_local! {
    static STATE: RefCell<ConditionState> = RefCell::new(ConditionState::new());
    /// Flag set by the MUFFLE-WARNING restart to suppress warning output.
    static WARNING_MUFFLED: RefCell<bool> = const { RefCell::new(false) };
}

// ── Condition construction ────────────────────────────────────────

fn condition_class_spec(name: u32) -> (&'static [u32], &'static [u32]) {
    match name {
        SYMBOL_CONDITION => (&[], &[]),
        SYMBOL_SERIOUS_CONDITION => (&[SYMBOL_CONDITION], &[]),
        SYMBOL_ERROR => (&[SYMBOL_SERIOUS_CONDITION], &[]),
        SYMBOL_WARNING => (&[SYMBOL_CONDITION], &[]),
        SYMBOL_SIMPLE_CONDITION => (
            &[SYMBOL_CONDITION],
            &[SYMBOL_FORMAT_CONTROL, SYMBOL_FORMAT_ARGUMENTS],
        ),
        SYMBOL_SIMPLE_ERROR => (&[SYMBOL_ERROR, SYMBOL_SIMPLE_CONDITION], &[]),
        SYMBOL_TYPE_ERROR => (&[SYMBOL_ERROR], &[SYMBOL_DATUM, SYMBOL_EXPECTED_TYPE]),
        SYMBOL_SIMPLE_WARNING => (&[SYMBOL_WARNING, SYMBOL_SIMPLE_CONDITION], &[]),
        SYMBOL_CONTROL_ERROR => (&[SYMBOL_ERROR], &[]),
        SYMBOL_STORAGE_CONDITION => (&[SYMBOL_SERIOUS_CONDITION], &[]),
        _ => (&[SYMBOL_CONDITION], &[]),
    }
}

fn ensure_condition_class(name: u32) -> Result<BlissVal, BlissError> {
    let sym = BlissVal::from_symbol_index(name);
    if let Some(class) = find_class(sym) {
        return Ok(class);
    }
    if find_class(bliss_rt::value::T).is_none() {
        let _ = bootstrap_clos();
        if let Some(class) = find_class(sym) {
            return Ok(class);
        }
    }
    let (supers, slots) = condition_class_spec(name);
    let super_vals: Vec<BlissVal> = supers
        .iter()
        .map(|idx| ensure_condition_class(*idx))
        .collect::<Result<_, _>>()?;
    let slot_vals: Vec<BlissVal> = slots.iter().map(|idx| BlissVal::from_symbol_index(*idx)).collect();
    define_class(sym, sym, &super_vals, &slot_vals)?;
    Ok(sym)
}

fn ensure_builtin_condition_classes() -> Result<(), BlissError> {
    ensure_condition_class(SYMBOL_CONDITION)?;
    ensure_condition_class(SYMBOL_SERIOUS_CONDITION)?;
    ensure_condition_class(SYMBOL_ERROR)?;
    ensure_condition_class(SYMBOL_WARNING)?;
    ensure_condition_class(SYMBOL_SIMPLE_CONDITION)?;
    ensure_condition_class(SYMBOL_SIMPLE_ERROR)?;
    ensure_condition_class(SYMBOL_TYPE_ERROR)?;
    ensure_condition_class(SYMBOL_SIMPLE_WARNING)?;
    ensure_condition_class(SYMBOL_CONTROL_ERROR)?;
    ensure_condition_class(SYMBOL_STORAGE_CONDITION)?;
    Ok(())
}

/// Number of preallocated STORAGE-CONDITION pool instances.
pub fn storage_condition_pool_size() -> usize {
    STORAGE_CONDITION_POOL_SIZE
}

/// Install a caller-built STORAGE-CONDITION pool, replacing any instances the
/// stdlib preallocated (bliss-5mf). The interpreter uses this to seed the pool
/// with CLI-native condition instances — whose class is the one the CLI's
/// condition matcher / TYPE-OF recognize — after its condition classes are live,
/// while still keeping the acquire path allocation-free. Instances should be
/// pinned in the GC heap by the caller (D5.13). Requires exactly
/// `storage_condition_pool_size()` instances.
pub fn set_storage_condition_pool(instances: &[BlissVal]) -> Result<(), BlissError> {
    if instances.len() != STORAGE_CONDITION_POOL_SIZE {
        return Err(BlissError::Internal(format!(
            "STORAGE-CONDITION pool needs {STORAGE_CONDITION_POOL_SIZE} instances, got {}",
            instances.len()
        )));
    }
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        state.storage_condition_pool[..].copy_from_slice(instances);
        state.next_storage_condition = 0;
        state.storage_condition_pool_initialized = true;
    });
    Ok(())
}

fn initialize_storage_condition_pool() -> Result<(), BlissError> {
    ensure_builtin_condition_classes()?;
    let class = ensure_condition_class(SYMBOL_STORAGE_CONDITION)?;
    let mut pool = [NIL; STORAGE_CONDITION_POOL_SIZE];
    for entry in &mut pool {
        // D5.13 (bliss-4v8): the pool lives in the GC heap, pinned, so a moving
        // collection never relocates or frees these preallocated instances — the
        // acquire path hands out raw addresses on the storage-exhaustion path.
        let inst = allocate_instance_pinned_gc(class)?;
        initialize_instance(inst, &[])?;
        *entry = inst;
    }
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        state.storage_condition_pool = pool;
        state.next_storage_condition = 0;
        state.storage_condition_pool_initialized = true;
    });
    Ok(())
}

fn storage_condition_pool_is_live() -> bool {
    STATE.with(|s| {
        let state = s.borrow();
        if !state.storage_condition_pool_initialized {
            return false;
        }
        let first = state.storage_condition_pool[0];
        first != NIL
            && class_inherits_from(
                class_of(first),
                BlissVal::from_symbol_index(SYMBOL_STORAGE_CONDITION),
            )
    })
}

/// Acquire a preallocated `STORAGE-CONDITION` instance for the heap-exhaustion /
/// stack-overflow signalling path (R5.110). This runs when allocation is already
/// failing, so it MUST NOT allocate, intern, define classes, resolve symbols, or
/// take non-essential locks: it only reads the thread-local pool array and
/// advances the rotation cursor. The pool is filled once at startup by
/// `initialize_condition_runtime_support` (called from the interpreter's
/// `Env::new` after CLOS/condition-class bootstrap and before any user code). If
/// it is somehow not initialized, we fail hard with a fixed `Internal` error
/// rather than lazily allocating on the low-memory path — that lazy fallback was
/// the bug this replaces (bliss-uh4.2).
///
/// Deliberately does NOT call `storage_condition_pool_is_live`, whose
/// `class_of` / class-graph walk could allocate or lock.
pub fn acquire_preallocated_storage_condition() -> Result<BlissVal, BlissError> {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        if !state.storage_condition_pool_initialized {
            return Err(BlissError::Internal(
                "STORAGE-CONDITION pool not initialized before the storage-failure path".into(),
            ));
        }
        let condition = state.storage_condition_pool[state.next_storage_condition];
        if condition == NIL {
            return Err(BlissError::Internal("STORAGE-CONDITION pool slot empty".into()));
        }
        state.next_storage_condition =
            (state.next_storage_condition + 1) % STORAGE_CONDITION_POOL_SIZE;
        Ok(condition)
    })
}

fn runtime_init_storage_condition_support() -> Result<(), BlissError> {
    initialize_condition_runtime_support()
}

pub fn install_runtime_init_hook() {
    static INSTALL_HOOK: Once = Once::new();
    INSTALL_HOOK.call_once(|| {
        bliss_rt::set_runtime_init_hook(runtime_init_storage_condition_support);
    });
}

pub fn initialize_condition_runtime_support() -> Result<(), BlissError> {
    install_runtime_init_hook();
    if storage_condition_pool_is_live() {
        Ok(())
    } else {
        initialize_storage_condition_pool()
    }
}

fn class_inherits_from(class: BlissVal, target: BlissVal) -> bool {
    if class == target || class_name(class) == target {
        return true;
    }
    class_direct_superclasses(class)
        .into_iter()
        .any(|super_class| class_inherits_from(super_class, target))
}

/// Create a simple-error condition.
///
/// Returns a BlissVal fixnum derived from the hash of the format string.
/// The condition is registered in thread-local state so handler_case can
/// recognize it as a condition value.
pub fn make_simple_error(format_control: &str, _format_args: &[BlissVal]) -> BlissVal {
    initialize_condition_runtime_support().expect("bootstrap condition runtime support");
    let class = ensure_condition_class(SYMBOL_SIMPLE_ERROR).expect("resolve SIMPLE-ERROR class");
    make_instance(
        class,
        &[
            BlissVal::from_symbol_index(SYMBOL_FORMAT_CONTROL),
            make_lisp_string_fresh(format_control),
            BlissVal::from_symbol_index(SYMBOL_FORMAT_ARGUMENTS),
            NIL,
        ],
    )
    .expect("make SIMPLE-ERROR instance")
}

/// Create a type-error condition.
///
/// Returns a BlissVal fixnum combining datum and expected type information.
/// Registered as a condition in thread-local state.
pub fn make_type_error(datum: BlissVal, expected_type: BlissVal) -> BlissVal {
    initialize_condition_runtime_support().expect("bootstrap condition runtime support");
    let class = ensure_condition_class(SYMBOL_TYPE_ERROR).expect("resolve TYPE-ERROR class");
    make_instance(
        class,
        &[
            BlissVal::from_symbol_index(SYMBOL_DATUM),
            datum,
            BlissVal::from_symbol_index(SYMBOL_EXPECTED_TYPE),
            expected_type,
        ],
    )
    .expect("make TYPE-ERROR instance")
}

/// Check if a BlissVal is a condition instance rooted at CONDITION.
fn is_condition(val: BlissVal) -> bool {
    class_inherits_from(class_of(val), BlissVal::from_symbol_index(SYMBOL_CONDITION))
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
    if is_known_condition_type(clause_type) {
        class_inherits_from(class_of(condition), clause_type)
    } else {
        is_condition(condition)
    }
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
    initialize_condition_runtime_support()?;
    break_on_signals_gate(condition)?;

    // Each frame of `handler_stack` is one cluster (the handlers established by a
    // single HANDLER-BIND). Walk clusters most-recent-first, trying a cluster's
    // handlers in source order. Per A5.04 (R5.94/R5.102), while a handler runs,
    // its whole cluster and every newer cluster are disestablished so a re-signal
    // is seen only by strictly-older clusters. Disestablishment moves the tail
    // `handler_stack[ci..]` out with `split_off` and appends it back afterwards —
    // no clone of the whole stack, only the current cluster is copied so it can
    // be iterated while the TLS stack is mutated.
    let cluster_count = STATE.with(|s| s.borrow().handler_stack.len());
    for ci in (0..cluster_count).rev() {
        let Some(cluster) = STATE.with(|s| s.borrow().handler_stack.get(ci).cloned()) else {
            continue;
        };
        for (condition_type, handler_fn) in &cluster {
            if condition_type_matches(condition, *condition_type) {
                let tail = STATE.with(|s| s.borrow_mut().handler_stack.split_off(ci));
                let handler_result = funcall(*handler_fn, &[condition]);
                STATE.with(|s| s.borrow_mut().handler_stack.extend(tail));
                // A handler that returns normally *declines* — keep searching;
                // one that transferred control surfaces here as Err and propagates.
                handler_result?;
            }
        }
    }

    // All handlers declined (or none matched) — SIGNAL returns NIL / Ok.
    Ok(())
}

fn break_on_signals_gate(condition: BlissVal) -> Result<(), BlissError> {
    let break_spec = STATE.with(|s| s.borrow().break_on_signals);
    let Some(break_spec) = break_spec else {
        return Ok(());
    };
    if !condition_type_matches(condition, break_spec) {
        return Ok(());
    }

    STATE.with(|s| {
        s.borrow_mut().break_on_signals = None;
    });
    let _ = invoke_debugger(condition);
    STATE.with(|s| {
        s.borrow_mut().break_on_signals = Some(break_spec);
    });
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
        function: BlissVal::from_symbol_index(INTERNAL_CONTINUE_RESTART_FN),
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
    initialize_condition_runtime_support()?;
    // Establish a MUFFLE-WARNING restart using the named constant (issue 8 fix).
    let muffle_name = BlissVal::from_symbol_index(SYMBOL_MUFFLE_WARNING);
    let muffle_restart = RestartEntry {
        name: muffle_name,
        function: BlissVal::from_symbol_index(INTERNAL_MUFFLE_WARNING_RESTART_FN),
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
        return Ok(form);
    }
    if !form.is_function() {
        return Ok(form);
    }

    handler_case_fn(clauses, || funcall(form, &[]))
}

/// Establish handler case around a protected computation (HANDLER-CASE). R5.19.
///
/// This is the closure-based entrypoint used when the protected form must be
/// evaluated with handler clauses dynamically installed.
pub fn handler_case_fn(
    clauses: &[(BlissVal, BlissVal)],
    body: impl FnOnce() -> Result<BlissVal, BlissError>,
) -> Result<BlissVal, BlissError> {
    if clauses.is_empty() {
        return body();
    }

    let mut bindings = Vec::with_capacity(clauses.len());
    let mut installed = Vec::with_capacity(clauses.len());
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        state.pending_handler_case = None;
        for (clause_type, handler_val) in clauses {
            let id = INTERNAL_HANDLER_CASE_FN_BASE - state.next_handler_case_id;
            state.next_handler_case_id += 1;
            let token = BlissVal::from_fixnum(id);
            state.handler_case_clauses.insert(token.to_raw(), *handler_val);
            bindings.push((*clause_type, token));
            installed.push(token.to_raw());
        }
    });

    let result = handler_bind_fn(&bindings, body);

    for raw in installed {
        STATE.with(|s| {
            s.borrow_mut().handler_case_clauses.remove(&raw);
        });
    }

    match result {
        Ok(value) => Ok(value),
        Err(BlissError::Internal(message)) if message == "__HANDLER_CASE__" => {
            let matched = STATE.with(|s| s.borrow_mut().pending_handler_case.take());
            if let Some((condition, handler_val)) = matched {
                if handler_val.is_function() {
                    funcall(handler_val, &[condition])
                } else {
                    Ok(handler_val)
                }
            } else {
                Err(BlissError::Internal("handler-case lost pending match".into()))
            }
        }
        Err(err) => Err(err),
    }
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
pub fn restart_bind(restarts: &[RestartSpec], body: BlissVal) -> Result<BlissVal, BlissError> {
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

/// Set `*BREAK-ON-SIGNALS*`.
pub fn set_break_on_signals(type_spec: Option<BlissVal>) {
    STATE.with(|s| {
        s.borrow_mut().break_on_signals = type_spec;
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
    initialize_condition_runtime_support()?;
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

/// Signal a runtime low-memory/storage failure using a preallocated
/// `STORAGE-CONDITION` instance per R5.110.
pub fn signal_storage_condition_for_runtime_error(
    error: &BlissError,
) -> Result<BlissVal, BlissError> {
    match error {
        BlissError::Oom | BlissError::StackOverflow(_) => {
            let condition = acquire_preallocated_storage_condition()?;
            signal_condition(condition)?;
            Ok(condition)
        }
        _ => Err(BlissError::Internal(
            "runtime error does not map to STORAGE-CONDITION".into(),
        )),
    }
}
