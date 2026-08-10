//! Condition system — condition types, handlers, and restarts.
//!
//! See spec §5.4.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── Signalling ─────────────────────────────────────────────────────

/// Signal a condition (CL `SIGNAL`). Does not unwind.
pub fn signal_condition(condition: BlissVal) -> Result<(), BlissError> {
    unimplemented!("signal_condition")
}

/// Signal an error (CL `ERROR`). Enters debugger if unhandled.
pub fn error_condition(condition: BlissVal) -> Result<(), BlissError> {
    unimplemented!("error_condition")
}

/// Signal a continuable error (CL `CERROR`).
pub fn cerror(continue_string: &str, condition: BlissVal) -> Result<(), BlissError> {
    unimplemented!("cerror")
}

/// Signal a warning (CL `WARN`). Establishes MUFFLE-WARNING restart.
pub fn warn_condition(condition: BlissVal) -> Result<(), BlissError> {
    unimplemented!("warn_condition")
}

// ── Handler binding ────────────────────────────────────────────────

/// A condition handler binding.
pub struct HandlerBinding {
    _private: (),
}

/// Establish handler bindings (without unwinding — HANDLER-BIND). R5.19.
pub fn handler_bind(
    bindings: &[(BlissVal, BlissVal)], // (condition-type, handler-fn)
    body: BlissVal,
) -> Result<BlissVal, BlissError> {
    unimplemented!("handler_bind")
}

/// Establish handler case (unwind before handler — HANDLER-CASE). R5.19.
pub fn handler_case(
    form: BlissVal,
    clauses: &[(BlissVal, BlissVal)], // (condition-type, handler-fn)
) -> Result<BlissVal, BlissError> {
    unimplemented!("handler_case")
}

// ── Restart protocol ───────────────────────────────────────────────

/// Establish restart bindings (RESTART-BIND / RESTART-CASE). R5.20.
pub fn restart_bind(
    restarts: &[RestartSpec],
    body: BlissVal,
) -> Result<BlissVal, BlissError> {
    unimplemented!("restart_bind")
}

/// Specification for a restart.
pub struct RestartSpec {
    pub name: BlissVal,
    pub function: BlissVal,
    pub report_function: Option<BlissVal>,
    pub interactive_function: Option<BlissVal>,
    pub test_function: Option<BlissVal>,
}

/// Compute available restarts for a condition.
pub fn compute_restarts(condition: Option<BlissVal>) -> Vec<BlissVal> {
    unimplemented!("compute_restarts")
}

/// Find a restart by name.
pub fn find_restart(name: BlissVal, condition: Option<BlissVal>) -> Option<BlissVal> {
    unimplemented!("find_restart")
}

/// Invoke a restart.
pub fn invoke_restart(restart: BlissVal, args: &[BlissVal]) -> Result<BlissVal, BlissError> {
    unimplemented!("invoke_restart")
}

/// Invoke a restart interactively.
pub fn invoke_restart_interactively(restart: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("invoke_restart_interactively")
}

// ── Debugger hook ──────────────────────────────────────────────────

/// Set `*DEBUGGER-HOOK*`. R5.22.
pub fn set_debugger_hook(hook: Option<BlissVal>) {
    unimplemented!("set_debugger_hook")
}

/// Invoke the debugger for an unhandled condition.
pub fn invoke_debugger(condition: BlissVal) -> Result<(), BlissError> {
    unimplemented!("invoke_debugger")
}

// ── Condition construction ─────────────────────────────────────────

/// Create a simple-error condition.
pub fn make_simple_error(format_control: &str, format_args: &[BlissVal]) -> BlissVal {
    unimplemented!("make_simple_error")
}

/// Create a type-error condition.
pub fn make_type_error(datum: BlissVal, expected_type: BlissVal) -> BlissVal {
    unimplemented!("make_type_error")
}
