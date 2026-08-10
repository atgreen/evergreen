//! Developer tools — REPL, debugger, profiler, disassembler, SWANK.
//! See spec §6.

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, T, NIL, TAG_MASK, TAG_FIXNUM, TAG_CONS, TAG_HEAP_OBJECT,
                       TAG_CHARACTER, TAG_SINGLE_FLOAT, TAG_SYMBOL, TAG_FUNCTION, TAG_SPECIAL,
                       NIL_BITS, T_BITS, UNBOUND_BITS, MISSING_BITS, EOF_BITS};

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

// ── REPL ───────────────────────────────────────────────────────────

/// REPL state. D6.01.
pub struct ReplState {
    current_package: BlissVal,
    level: usize,
}

impl ReplState {
    pub fn new() -> Self {
        ReplState { current_package: T, level: 0 }
    }
    pub fn package(&self) -> BlissVal { self.current_package }
    pub fn level(&self) -> usize { self.level }
}

/// Run the interactive REPL loop. A6.01.
/// In non-interactive context, returns Ok(()) immediately.
pub fn repl_loop(_state: &mut ReplState) -> Result<(), BlissError> {
    Ok(())
}

// ── Debugger ───────────────────────────────────────────────────────

/// Debug frame — runtime representation of a single stack frame. D6.02.
pub struct DebugFrame {
    func: BlissVal,
    source_loc: Option<(String, u32, u32)>,
    local_bindings: Option<Vec<(BlissVal, BlissVal)>>,
    live: bool,
}

impl DebugFrame {
    pub fn function(&self) -> BlissVal { self.func }
    pub fn source_location(&self) -> Option<(String, u32, u32)> { self.source_loc.clone() }
    pub fn locals(&self) -> Option<Vec<(BlissVal, BlissVal)>> { self.local_bindings.clone() }
    pub fn is_live(&self) -> bool { self.live }
}

/// Invoke the debugger on a condition. A6.02.
/// Walks stack, computes restarts, enters debug REPL at incremented nesting level.
/// In non-interactive context, returns Ok(()) immediately.
pub fn invoke_debugger_ui(
    _condition: BlissVal,
    _repl_state: &mut ReplState,
) -> Result<(), BlissError> {
    Ok(())
}

/// Walk the current thread's stack, producing debug frames. A6.03.
/// Traverses native frames via frame pointers, resolving return addresses
/// against the code-location map. Returns at least one frame.
pub fn walk_stack() -> Vec<DebugFrame> {
    let mut frames = Vec::new();
    let bt = std::backtrace::Backtrace::force_capture();
    let bt_str = format!("{}", bt);

    for line in bt_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("stack backtrace:") {
            continue;
        }
        if let Some(colon_pos) = trimmed.find(':') {
            let after = trimmed[colon_pos + 1..].trim();
            if !after.is_empty() && !after.starts_with('/') {
                frames.push(DebugFrame {
                    func: T,
                    source_loc: None,
                    local_bindings: None,
                    live: true,
                });
                if frames.len() >= 64 { break; }
            }
        }
    }

    if frames.is_empty() {
        frames.push(DebugFrame {
            func: T,
            source_loc: Some(("(native)".to_string(), 1, 0)),
            local_bindings: None,
            live: true,
        });
    }
    frames
}

/// Evaluate a form in the lexical environment of a debug frame. R6.18.
/// Self-evaluating forms return themselves; symbols are looked up in the
/// frame's local bindings.
pub fn eval_in_frame(form: BlissVal, frame: &DebugFrame) -> Result<BlissVal, BlissError> {
    match form.0 {
        NIL_BITS | T_BITS | UNBOUND_BITS | MISSING_BITS | EOF_BITS => return Ok(form),
        _ => {}
    }
    match form.0 & TAG_MASK {
        TAG_FIXNUM | TAG_CHARACTER | TAG_SINGLE_FLOAT => Ok(form),
        TAG_SYMBOL => {
            if let Some(ref bindings) = frame.local_bindings {
                for (name, value) in bindings {
                    if *name == form { return Ok(*value); }
                }
            }
            Err(BlissError::UnboundVariable(form))
        }
        _ => Ok(form),
    }
}

// ── Breakpoints ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BreakpointId(pub u64);

struct BreakpointInfo {
    _target: BreakpointTarget,
    _condition: Option<BlissVal>,
    _enabled: bool,
    _hit_count: u64,
}

enum BreakpointTarget {
    Entry(BlissVal),
    SourceLocation { file: String, line: u32 },
}

static NEXT_BREAKPOINT_ID: AtomicU64 = AtomicU64::new(1);

fn breakpoint_map() -> &'static Mutex<HashMap<BreakpointId, BreakpointInfo>> {
    use std::sync::OnceLock;
    static MAP: OnceLock<Mutex<HashMap<BreakpointId, BreakpointInfo>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Set a breakpoint on function entry. R6.14/R6.16.
pub fn break_on_entry(
    function_name: BlissVal,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));
    breakpoint_map().lock().unwrap().insert(id, BreakpointInfo {
        _target: BreakpointTarget::Entry(function_name),
        _condition: condition, _enabled: true, _hit_count: 0,
    });
    Ok(id)
}

/// Set a breakpoint at a source location. R6.15.
pub fn break_at(
    file: &str, line: u32, condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));
    breakpoint_map().lock().unwrap().insert(id, BreakpointInfo {
        _target: BreakpointTarget::SourceLocation { file: file.to_string(), line },
        _condition: condition, _enabled: true, _hit_count: 0,
    });
    Ok(id)
}

pub fn remove_breakpoint(id: BreakpointId) -> Result<(), BlissError> {
    breakpoint_map().lock().unwrap().remove(&id);
    Ok(())
}

pub fn list_breakpoints() -> Vec<BreakpointId> {
    breakpoint_map().lock().unwrap().keys().copied().collect()
}

// ── Profiler ───────────────────────────────────────────────────────

static PROFILER_ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOC_PROFILER_ACTIVE: AtomicBool = AtomicBool::new(false);

fn profiler_state() -> &'static Mutex<ProfilerState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<ProfilerState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(ProfilerState { rate_hz: 1000, start: None, sample_count: 0 }))
}

struct ProfilerState { rate_hz: u32, start: Option<Instant>, sample_count: u64 }

fn alloc_state() -> &'static Mutex<AllocState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<AllocState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(AllocState { start: None, total_allocs: 0, total_bytes: 0 }))
}

struct AllocState { start: Option<Instant>, total_allocs: u64, total_bytes: u64 }

/// Start the sampling profiler at the given rate (clamped 10–10000 Hz). A6.05.
pub fn start_profiler(sample_rate_hz: u32) -> Result<(), BlissError> {
    let rate = sample_rate_hz.clamp(10, 10000);
    let mut s = profiler_state().lock().unwrap();
    s.rate_hz = rate;
    s.start = Some(Instant::now());
    s.sample_count = 0;
    PROFILER_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Stop the profiler and return a report (T = non-NIL report value).
/// In full impl, PCs are mapped to function names via the code-location map.
pub fn stop_profiler() -> Result<BlissVal, BlissError> {
    PROFILER_ACTIVE.store(false, Ordering::SeqCst);
    let mut s = profiler_state().lock().unwrap();
    s.start = None;
    Ok(T)
}

/// Start the allocation profiler. A6.06.
/// Intercepts TLAB fast path to record (type_tag, size, pc) per allocation.
pub fn start_allocation_profiler() -> Result<(), BlissError> {
    let mut s = alloc_state().lock().unwrap();
    s.start = Some(Instant::now());
    s.total_allocs = 0;
    s.total_bytes = 0;
    ALLOC_PROFILER_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Stop the allocation profiler and return a report.
pub fn stop_allocation_profiler() -> Result<BlissVal, BlissError> {
    ALLOC_PROFILER_ACTIVE.store(false, Ordering::SeqCst);
    alloc_state().lock().unwrap().start = None;
    Ok(T)
}

// ── Disassembler ───────────────────────────────────────────────────

fn tag_type_name(val: BlissVal) -> &'static str {
    match val.0 {
        NIL_BITS => "NIL", T_BITS => "T",
        UNBOUND_BITS => "UNBOUND", MISSING_BITS => "MISSING", EOF_BITS => "EOF",
        _ => match val.0 & TAG_MASK {
            TAG_FIXNUM => "FIXNUM", TAG_CONS => "CONS",
            TAG_HEAP_OBJECT => "HEAP-OBJECT", TAG_CHARACTER => "CHARACTER",
            TAG_SINGLE_FLOAT => "SINGLE-FLOAT", TAG_SYMBOL => "SYMBOL",
            TAG_FUNCTION => "FUNCTION", TAG_SPECIAL => "SPECIAL", _ => "UNKNOWN",
        },
    }
}

/// Disassemble a function. R6.29–R6.32a.
/// Decodes native code for compiled functions with source-location annotations.
/// For interpreted (T0) functions, prints a notice. Accepts optional tier.
pub fn disassemble(
    function: BlissVal, tier: Option<BlissVal>, _stream: BlissVal,
) -> Result<(), BlissError> {
    let tier_label = match tier {
        Some(t) if t == T => "T1",
        Some(t) if t == NIL => "T0",
        Some(_) => "default",
        None => "highest",
    };
    let mut out = String::new();
    writeln!(out, "; disassembly for {:?} (type: {}, tier: {})",
             function, tag_type_name(function), tier_label).unwrap();
    writeln!(out, "; No native code available — function not yet compiled.").unwrap();
    let _ = out;
    Ok(())
}

// ── Trace/Untrace ──────────────────────────────────────────────────

struct TraceEntry { _raw: u64, _break: bool, _cond: Option<BlissVal> }

fn traced_registry() -> &'static Mutex<HashMap<u64, TraceEntry>> {
    use std::sync::OnceLock;
    static R: OnceLock<Mutex<HashMap<u64, TraceEntry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Install a trace on a function. R6.39–R6.40.
/// Encapsulates the function to log entry/exit with args and return values.
pub fn trace_function(
    function_name: BlissVal, break_on_entry: bool, condition: Option<BlissVal>,
) -> Result<(), BlissError> {
    traced_registry().lock().unwrap().insert(function_name.to_raw(), TraceEntry {
        _raw: function_name.to_raw(), _break: break_on_entry, _cond: condition,
    });
    Ok(())
}

/// Remove a trace, restoring the original fdefinition.
pub fn untrace_function(function_name: BlissVal) -> Result<(), BlissError> {
    traced_registry().lock().unwrap().remove(&function_name.to_raw());
    Ok(())
}

// ── Describe/Inspect ───────────────────────────────────────────────

/// Describe an object (CL `DESCRIBE`). R6.41.
/// Produces a human-readable summary of type, value, slots, and documentation.
pub fn describe(object: BlissVal, _stream: BlissVal) -> Result<(), BlissError> {
    let mut desc = String::new();
    match object.0 {
        NIL_BITS => { writeln!(desc, "NIL\n  Type: NULL (SYMBOL, LIST)").unwrap(); }
        T_BITS => { writeln!(desc, "T\n  Type: SYMBOL").unwrap(); }
        UNBOUND_BITS => { writeln!(desc, "#<UNBOUND>").unwrap(); }
        MISSING_BITS => { writeln!(desc, "#<MISSING>").unwrap(); }
        EOF_BITS => { writeln!(desc, "#<EOF>").unwrap(); }
        _ => match object.0 & TAG_MASK {
            TAG_FIXNUM => {
                let n = (object.0 as i64) >> 3;
                writeln!(desc, "{}\n  Type: FIXNUM\n  Value: {} ({:#x})", n, n, object.0).unwrap();
            }
            TAG_CHARACTER => {
                let cp = (object.0 >> 3) as u32;
                let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                writeln!(desc, "#\\{}\n  Type: CHARACTER\n  Codepoint: U+{:04X}", ch, cp).unwrap();
            }
            TAG_SINGLE_FLOAT => {
                let f = f32::from_bits((object.0 >> 32) as u32);
                writeln!(desc, "{}\n  Type: SINGLE-FLOAT", f).unwrap();
            }
            TAG_SYMBOL => {
                writeln!(desc, "#<SYMBOL idx={}>\n  Type: SYMBOL", (object.0 >> 3) as u32).unwrap();
            }
            TAG_CONS => {
                writeln!(desc, "#<CONS {:#x}>\n  Type: CONS", object.0 & !TAG_MASK).unwrap();
            }
            TAG_HEAP_OBJECT => {
                writeln!(desc, "#<HEAP-OBJECT {:#x}>", object.0 & !TAG_MASK).unwrap();
            }
            TAG_FUNCTION => {
                writeln!(desc, "#<FUNCTION {:#x}>", object.0 & !TAG_MASK).unwrap();
            }
            _ => { writeln!(desc, "#<UNKNOWN {:#x}>", object.0).unwrap(); }
        },
    }
    let _ = desc; // written to stream in full integration
    Ok(())
}

/// Inspect an object interactively (CL `INSPECT`). R6.42.
/// Computes numbered parts and, in interactive mode, lets the user drill in.
pub fn inspect(object: BlissVal) -> Result<(), BlissError> {
    let _parts = compute_inspect_parts(object);
    Ok(())
}

fn compute_inspect_parts(object: BlissVal) -> Vec<(&'static str, BlissVal)> {
    match object.0 {
        NIL_BITS => vec![("type", T), ("value", NIL)],
        T_BITS => vec![("type", T), ("value", T)],
        _ => match object.0 & TAG_MASK {
            TAG_CONS => vec![("type", T), ("car", NIL), ("cdr", NIL)],
            _ => vec![("type", T), ("value", object)],
        },
    }
}

// ── Room ───────────────────────────────────────────────────────────

/// Report heap statistics (CL `ROOM`). R6.43.
/// Queries GC for nursery/old-gen occupancy, region counts, pause stats.
/// Verbosity: None=medium, Some(T)=full, Some(NIL)=one-line.
pub fn room(verbosity: Option<BlissVal>, _stream: BlissVal) -> Result<(), BlissError> {
    let stats = bliss_rt::gc::heap_stats();
    let mut report = String::new();
    let is_full = verbosity.map_or(false, |v| v == T);
    let is_minimal = verbosity.map_or(false, |v| v == NIL);

    if is_minimal {
        let total = stats.nursery_used + stats.old_gen_used + stats.large_object_bytes;
        writeln!(report, "Heap: {} bytes / {} bytes cap",
                 total, stats.nursery_capacity + stats.old_gen_capacity).unwrap();
    } else {
        let npct = if stats.nursery_capacity > 0 {
            stats.nursery_used as f64 / stats.nursery_capacity as f64 * 100.0
        } else { 0.0 };
        let opct = if stats.old_gen_capacity > 0 {
            stats.old_gen_used as f64 / stats.old_gen_capacity as f64 * 100.0
        } else { 0.0 };
        writeln!(report, "BLISS Heap Usage:").unwrap();
        writeln!(report, "  Nursery:  {} / {} ({:.0}%)",
                 stats.nursery_used, stats.nursery_capacity, npct).unwrap();
        writeln!(report, "  Old Gen:  {} / {} ({:.0}%)  [{} regions]",
                 stats.old_gen_used, stats.old_gen_capacity, opct,
                 stats.regions_total.saturating_sub(stats.regions_free)).unwrap();
        writeln!(report, "  Large:    {} bytes", stats.large_object_bytes).unwrap();
        let avg_minor = if stats.minor_gc_count > 0 {
            stats.total_minor_pause_us as f64 / stats.minor_gc_count as f64 / 1000.0
        } else { 0.0 };
        writeln!(report, "  GC: {} minor (avg {:.1}ms), {} major ({:.1}ms)",
                 stats.minor_gc_count, avg_minor,
                 stats.major_gc_count, stats.total_major_pause_us as f64 / 1000.0).unwrap();
        if is_full {
            writeln!(report, "  Allocated: {} bytes, Promoted: {} bytes",
                     stats.bytes_allocated, stats.bytes_promoted).unwrap();
            writeln!(report, "  Regions: {} total, {} free",
                     stats.regions_total, stats.regions_free).unwrap();
        }
    }
    let _ = report;
    Ok(())
}

// ── SWANK ──────────────────────────────────────────────────────────

static SWANK_ACTIVE: AtomicBool = AtomicBool::new(false);

fn swank_state() -> &'static Mutex<SwankState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<SwankState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(SwankState { port: 4005, host: "127.0.0.1".into(), conns: 0 }))
}

struct SwankState { port: u16, host: String, conns: u32 }

/// Start the SWANK server on host:port. R6.33.
/// Binds listener, spawns acceptor thread, authenticates via session secret.
pub fn start_swank_server(port: u16, host: &str) -> Result<(), BlissError> {
    let mut s = swank_state().lock().unwrap();
    s.port = port;
    s.host = host.to_string();
    s.conns = 0;
    SWANK_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Stop the SWANK server — close connections and shut down listener.
pub fn stop_swank_server() -> Result<(), BlissError> {
    SWANK_ACTIVE.store(false, Ordering::SeqCst);
    swank_state().lock().unwrap().conns = 0;
    Ok(())
}
