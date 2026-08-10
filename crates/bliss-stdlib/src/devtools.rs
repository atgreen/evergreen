//! Developer tools — REPL, debugger, profiler, disassembler, SWANK.
//!
//! See spec §6.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── REPL ───────────────────────────────────────────────────────────

/// REPL state. D6.01.
pub struct ReplState {
    _private: (),
}

impl ReplState {
    /// Create a new REPL state.
    pub fn new() -> Self {
        unimplemented!("ReplState::new")
    }

    /// Get the current package.
    pub fn package(&self) -> BlissVal {
        unimplemented!("ReplState::package")
    }

    /// Get the debugger nesting level.
    pub fn level(&self) -> usize {
        unimplemented!("ReplState::level")
    }
}

/// Run the interactive REPL loop. A6.01.
pub fn repl_loop(state: &mut ReplState) -> Result<(), BlissError> {
    unimplemented!("repl_loop")
}

// ── Debugger ───────────────────────────────────────────────────────

/// Debug frame — runtime representation of a single stack frame. D6.02.
pub struct DebugFrame {
    _private: (),
}

impl DebugFrame {
    /// Get the function for this frame.
    pub fn function(&self) -> BlissVal {
        unimplemented!("DebugFrame::function")
    }

    /// Get the source location.
    pub fn source_location(&self) -> Option<(String, u32, u32)> {
        unimplemented!("DebugFrame::source_location")
    }

    /// Get local variable bindings (when debug ≥ 2).
    pub fn locals(&self) -> Option<Vec<(BlissVal, BlissVal)>> {
        unimplemented!("DebugFrame::locals")
    }

    /// Whether this frame is still on the stack.
    pub fn is_live(&self) -> bool {
        unimplemented!("DebugFrame::is_live")
    }
}

/// Invoke the debugger on a condition. A6.02.
pub fn invoke_debugger_ui(
    condition: BlissVal,
    repl_state: &mut ReplState,
) -> Result<(), BlissError> {
    unimplemented!("invoke_debugger_ui")
}

/// Walk the current thread's stack, producing debug frames. A6.03.
pub fn walk_stack() -> Vec<DebugFrame> {
    unimplemented!("walk_stack")
}

/// Evaluate a form in the lexical environment of a debug frame. R6.18.
pub fn eval_in_frame(form: BlissVal, frame: &DebugFrame) -> Result<BlissVal, BlissError> {
    unimplemented!("eval_in_frame")
}

// ── Breakpoints ────────────────────────────────────────────────────

/// Breakpoint ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BreakpointId(pub u64);

/// Set a breakpoint on function entry. R6.14.
pub fn break_on_entry(
    function_name: BlissVal,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    unimplemented!("break_on_entry")
}

/// Set a breakpoint at a source location. R6.15.
pub fn break_at(
    file: &str,
    line: u32,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    unimplemented!("break_at")
}

/// Remove a breakpoint.
pub fn remove_breakpoint(id: BreakpointId) -> Result<(), BlissError> {
    unimplemented!("remove_breakpoint")
}

/// List all active breakpoints.
pub fn list_breakpoints() -> Vec<BreakpointId> {
    unimplemented!("list_breakpoints")
}

// ── Profiler ───────────────────────────────────────────────────────

/// Start the sampling profiler. A6.05.
pub fn start_profiler(sample_rate_hz: u32) -> Result<(), BlissError> {
    unimplemented!("start_profiler")
}

/// Stop the profiler and return a report.
pub fn stop_profiler() -> Result<BlissVal, BlissError> {
    unimplemented!("stop_profiler")
}

/// Start the allocation profiler. A6.06.
pub fn start_allocation_profiler() -> Result<(), BlissError> {
    unimplemented!("start_allocation_profiler")
}

/// Stop the allocation profiler and return a report.
pub fn stop_allocation_profiler() -> Result<BlissVal, BlissError> {
    unimplemented!("stop_allocation_profiler")
}

// ── Disassembler ───────────────────────────────────────────────────

/// Disassemble a function. R6.29–R6.32a.
pub fn disassemble(
    function: BlissVal,
    tier: Option<BlissVal>,
    stream: BlissVal,
) -> Result<(), BlissError> {
    unimplemented!("disassemble")
}

// ── Trace/Untrace ──────────────────────────────────────────────────

/// Install a trace on a function. R6.39–R6.40.
pub fn trace_function(
    function_name: BlissVal,
    break_on_entry: bool,
    condition: Option<BlissVal>,
) -> Result<(), BlissError> {
    unimplemented!("trace_function")
}

/// Remove a trace from a function.
pub fn untrace_function(function_name: BlissVal) -> Result<(), BlissError> {
    unimplemented!("untrace_function")
}

// ── Describe/Inspect ───────────────────────────────────────────────

/// Describe an object (CL `DESCRIBE`). R6.41.
pub fn describe(object: BlissVal, stream: BlissVal) -> Result<(), BlissError> {
    unimplemented!("describe")
}

/// Inspect an object interactively (CL `INSPECT`). R6.42.
pub fn inspect(object: BlissVal) -> Result<(), BlissError> {
    unimplemented!("inspect")
}

// ── Room / Time ────────────────────────────────────────────────────

/// Report heap statistics (CL `ROOM`). R6.43.
pub fn room(verbosity: Option<BlissVal>, stream: BlissVal) -> Result<(), BlissError> {
    unimplemented!("room")
}

// ── SWANK ──────────────────────────────────────────────────────────

/// Start the SWANK server on the given port. R6.33.
pub fn start_swank_server(port: u16, host: &str) -> Result<(), BlissError> {
    unimplemented!("start_swank_server")
}

/// Stop the SWANK server.
pub fn stop_swank_server() -> Result<(), BlissError> {
    unimplemented!("stop_swank_server")
}
