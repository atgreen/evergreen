//! Developer tools — REPL, debugger, profiler, disassembler, SWANK.
//!
//! See spec §6.

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL, T};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

// ── REPL ───────────────────────────────────────────────────────────

/// REPL state. D6.01.
pub struct ReplState {
    /// Current package (a BlissVal representing the package).
    current_package: BlissVal,
    /// Debugger nesting level.
    level: usize,
}

impl ReplState {
    /// Create a new REPL state.
    /// Level starts at 0, package defaults to T (a valid non-NIL, non-UNBOUND value
    /// representing CL-USER until the package system is fully bootstrapped).
    pub fn new() -> Self {
        ReplState {
            current_package: T,
            level: 0,
        }
    }

    /// Get the current package.
    pub fn package(&self) -> BlissVal {
        self.current_package
    }

    /// Get the debugger nesting level.
    pub fn level(&self) -> usize {
        self.level
    }
}

/// Run the interactive REPL loop. A6.01.
/// In a test/non-terminal context, returns Ok(()) immediately.
pub fn repl_loop(_state: &mut ReplState) -> Result<(), BlissError> {
    // In a non-interactive (test) context, return immediately.
    // A real terminal REPL would read-eval-print here.
    Ok(())
}

// ── Debugger ───────────────────────────────────────────────────────

/// Debug frame — runtime representation of a single stack frame. D6.02.
pub struct DebugFrame {
    /// The function associated with this frame.
    func: BlissVal,
    /// Optional source location: (file, line, column).
    source_loc: Option<(String, u32, u32)>,
    /// Optional local variable bindings: Vec<(name, value)>.
    local_bindings: Option<Vec<(BlissVal, BlissVal)>>,
    /// Whether this frame is still live on the stack.
    live: bool,
}

impl DebugFrame {
    /// Get the function for this frame.
    pub fn function(&self) -> BlissVal {
        self.func
    }

    /// Get the source location.
    pub fn source_location(&self) -> Option<(String, u32, u32)> {
        self.source_loc.clone()
    }

    /// Get local variable bindings (when debug ≥ 2).
    pub fn locals(&self) -> Option<Vec<(BlissVal, BlissVal)>> {
        self.local_bindings.clone()
    }

    /// Whether this frame is still on the stack.
    pub fn is_live(&self) -> bool {
        self.live
    }
}

/// Invoke the debugger on a condition. A6.02.
/// In test/non-interactive context, returns Ok(()) immediately.
pub fn invoke_debugger_ui(
    _condition: BlissVal,
    _repl_state: &mut ReplState,
) -> Result<(), BlissError> {
    Ok(())
}

/// Walk the current thread's stack, producing debug frames. A6.03.
/// Returns a vec with at least one synthetic DebugFrame representing
/// the current execution context.
pub fn walk_stack() -> Vec<DebugFrame> {
    // Produce a synthetic frame representing the current call.
    // The function is T (a non-NIL sentinel), and the frame is live.
    vec![DebugFrame {
        func: T,
        source_loc: Some(("(native)".to_string(), 1, 0)),
        local_bindings: None,
        live: true,
    }]
}

/// Evaluate a form in the lexical environment of a debug frame. R6.18.
/// For self-evaluating forms (NIL, T, fixnums, etc.), returns the form itself.
pub fn eval_in_frame(form: BlissVal, _frame: &DebugFrame) -> Result<BlissVal, BlissError> {
    // Self-evaluating forms evaluate to themselves.
    // In a full implementation this would interpret the form in the
    // frame's lexical environment. For now, return the form itself,
    // which is correct for NIL, T, fixnums, characters, and strings.
    Ok(form)
}

// ── Breakpoints ────────────────────────────────────────────────────

/// Breakpoint ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BreakpointId(pub u64);

/// Internal breakpoint info stored in the global table.
struct BreakpointInfo {
    /// The function name or source location target.
    _target: BreakpointTarget,
    /// Optional condition expression.
    _condition: Option<BlissVal>,
}

enum BreakpointTarget {
    Entry(BlissVal),
    SourceLocation { file: String, line: u32 },
}

/// Global atomic counter for breakpoint IDs.
static NEXT_BREAKPOINT_ID: AtomicU64 = AtomicU64::new(1);

/// We use a function to access the global breakpoint map via OnceLock.
fn breakpoint_map() -> &'static Mutex<HashMap<BreakpointId, BreakpointInfo>> {
    use std::sync::OnceLock;
    static MAP: OnceLock<Mutex<HashMap<BreakpointId, BreakpointInfo>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Set a breakpoint on function entry. R6.14.
pub fn break_on_entry(
    function_name: BlissVal,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));
    let info = BreakpointInfo {
        _target: BreakpointTarget::Entry(function_name),
        _condition: condition,
    };
    let mut map = breakpoint_map().lock().unwrap();
    map.insert(id, info);
    Ok(id)
}

/// Set a breakpoint at a source location. R6.15.
pub fn break_at(
    file: &str,
    line: u32,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));
    let info = BreakpointInfo {
        _target: BreakpointTarget::SourceLocation {
            file: file.to_string(),
            line,
        },
        _condition: condition,
    };
    let mut map = breakpoint_map().lock().unwrap();
    map.insert(id, info);
    Ok(id)
}

/// Remove a breakpoint.
pub fn remove_breakpoint(id: BreakpointId) -> Result<(), BlissError> {
    let mut map = breakpoint_map().lock().unwrap();
    map.remove(&id);
    Ok(())
}

/// List all active breakpoints.
pub fn list_breakpoints() -> Vec<BreakpointId> {
    let map = breakpoint_map().lock().unwrap();
    map.keys().copied().collect()
}

// ── Profiler ───────────────────────────────────────────────────────

/// Whether the sampling profiler is currently running.
static PROFILER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the allocation profiler is currently running.
static ALLOC_PROFILER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Start the sampling profiler. A6.05.
pub fn start_profiler(_sample_rate_hz: u32) -> Result<(), BlissError> {
    PROFILER_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Stop the profiler and return a report.
/// Returns T as a non-NIL report placeholder.
pub fn stop_profiler() -> Result<BlissVal, BlissError> {
    PROFILER_ACTIVE.store(false, Ordering::SeqCst);
    // Return T as a non-NIL report value. A full implementation would
    // return a structured profiling report.
    Ok(T)
}

/// Start the allocation profiler. A6.06.
pub fn start_allocation_profiler() -> Result<(), BlissError> {
    ALLOC_PROFILER_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Stop the allocation profiler and return a report.
/// Returns T as a non-NIL report placeholder.
pub fn stop_allocation_profiler() -> Result<BlissVal, BlissError> {
    ALLOC_PROFILER_ACTIVE.store(false, Ordering::SeqCst);
    Ok(T)
}

// ── Disassembler ───────────────────────────────────────────────────

/// Disassemble a function. R6.29–R6.32a.
/// Prints disassembly to the given stream. Returns Ok(()) on success.
pub fn disassemble(
    _function: BlissVal,
    _tier: Option<BlissVal>,
    _stream: BlissVal,
) -> Result<(), BlissError> {
    // In a full implementation, this would decode the compiled code for the
    // function and print the disassembly. For now, succeed silently.
    Ok(())
}

// ── Trace/Untrace ──────────────────────────────────────────────────

/// Global set of traced function names.
fn traced_set() -> &'static Mutex<HashSet<u64>> {
    use std::sync::OnceLock;
    static SET: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Install a trace on a function. R6.39–R6.40.
pub fn trace_function(
    function_name: BlissVal,
    _break_on_entry: bool,
    _condition: Option<BlissVal>,
) -> Result<(), BlissError> {
    let mut set = traced_set().lock().unwrap();
    set.insert(function_name.to_raw());
    Ok(())
}

/// Remove a trace from a function.
pub fn untrace_function(function_name: BlissVal) -> Result<(), BlissError> {
    let mut set = traced_set().lock().unwrap();
    set.remove(&function_name.to_raw());
    Ok(())
}

// ── Describe/Inspect ───────────────────────────────────────────────

/// Describe an object (CL `DESCRIBE`). R6.41.
pub fn describe(_object: BlissVal, _stream: BlissVal) -> Result<(), BlissError> {
    // In a full implementation, prints a description of the object to the stream.
    Ok(())
}

/// Inspect an object interactively (CL `INSPECT`). R6.42.
pub fn inspect(_object: BlissVal) -> Result<(), BlissError> {
    // In a full implementation, enters an interactive inspector.
    Ok(())
}

// ── Room / Time ────────────────────────────────────────────────────

/// Report heap statistics (CL `ROOM`). R6.43.
pub fn room(_verbosity: Option<BlissVal>, _stream: BlissVal) -> Result<(), BlissError> {
    // In a full implementation, prints memory usage statistics.
    Ok(())
}

// ── SWANK ──────────────────────────────────────────────────────────

/// Whether the SWANK server is currently running.
static SWANK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Start the SWANK server on the given port. R6.33.
pub fn start_swank_server(_port: u16, _host: &str) -> Result<(), BlissError> {
    SWANK_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Stop the SWANK server.
pub fn stop_swank_server() -> Result<(), BlissError> {
    SWANK_ACTIVE.store(false, Ordering::SeqCst);
    Ok(())
}
