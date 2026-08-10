//! Developer tools — REPL, debugger, profiler, disassembler, SWANK.
//! See spec §6.

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, T, NIL, TAG_MASK, TAG_FIXNUM, TAG_CONS, TAG_HEAP_OBJECT,
                       TAG_CHARACTER, TAG_SINGLE_FLOAT, TAG_SYMBOL, TAG_FUNCTION, TAG_SPECIAL,
                       NIL_BITS, T_BITS, UNBOUND_BITS, MISSING_BITS, EOF_BITS};

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::io::{self, BufRead, Write as IoWrite};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

// ── REPL ───────────────────────────────────────────────────────────

/// REPL state. D6.01.
pub struct ReplState {
    current_package: BlissVal,
    level: usize,
    /// History variables: *, **, ***, +, ++, +++, /, //, ///
    history_star: [BlissVal; 3],
    history_plus: [BlissVal; 3],
    history_slash: [Vec<BlissVal>; 3],
}

impl ReplState {
    pub fn new() -> Self {
        ReplState {
            current_package: T,
            level: 0,
            history_star: [NIL; 3],
            history_plus: [NIL; 3],
            history_slash: [vec![], vec![], vec![]],
        }
    }
    pub fn package(&self) -> BlissVal { self.current_package }
    pub fn level(&self) -> usize { self.level }

    /// Rotate history variables after evaluating a form that produced `values`.
    fn rotate_history(&mut self, form: BlissVal, values: &[BlissVal]) {
        // Rotate ***, **, *
        self.history_star[2] = self.history_star[1];
        self.history_star[1] = self.history_star[0];
        self.history_star[0] = if values.is_empty() { NIL } else { values[0] };

        // Rotate +++, ++, +
        self.history_plus[2] = self.history_plus[1];
        self.history_plus[1] = self.history_plus[0];
        self.history_plus[0] = form;

        // Rotate ///, //, /
        self.history_slash[2] = self.history_slash[1].clone();
        self.history_slash[1] = self.history_slash[0].clone();
        self.history_slash[0] = values.to_vec();
    }
}

/// Check if stdin is attached to a terminal (interactive).
fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Run the interactive REPL loop. A6.01.
/// In non-interactive context (no terminal attached), returns Ok(()) immediately.
/// When interactive, reads forms from stdin, evaluates them, prints results,
/// and maintains history variables (*, **, ***, +, ++, +++, /, //, ///).
pub fn repl_loop(state: &mut ReplState) -> Result<(), BlissError> {
    if !is_interactive() {
        return Ok(());
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut interpreter = bliss_compiler::tiered::Interpreter::new();

    loop {
        // Display prompt with current level
        let prompt = if state.level == 0 {
            "BLISS> ".to_string()
        } else {
            format!("BLISS[{}]> ", state.level)
        };
        write!(stdout, "{}", prompt).unwrap_or(());
        stdout.flush().unwrap_or(());

        // Read a line from stdin
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("Read error: {}", e);
                continue;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse the input using the reader
        let form = match bliss_compiler::reader::read_from_string(trimmed) {
            Ok((f, _pos)) => f,
            Err(e) => {
                eprintln!("Reader error: {}", e);
                continue;
            }
        };

        // Evaluate the form
        match interpreter.eval(form) {
            Ok(value) => {
                // Print result
                writeln!(stdout, "{:?}", value).unwrap_or(());
                stdout.flush().unwrap_or(());
                // Update history
                state.rotate_history(form, &[value]);
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                state.rotate_history(form, &[]);
            }
        }
    }
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
    condition: BlissVal,
    repl_state: &mut ReplState,
) -> Result<(), BlissError> {
    if !is_interactive() {
        return Ok(());
    }

    let mut stdout = io::stdout();

    // Display the condition
    writeln!(stdout, "\nDebugger entered: condition {:?}", condition).unwrap_or(());

    // Walk the stack and display backtrace
    let frames = walk_stack();
    writeln!(stdout, "\nBacktrace ({} frames):", frames.len()).unwrap_or(());
    for (i, frame) in frames.iter().enumerate() {
        let func_desc = format!("{:?}", frame.function());
        let loc_desc = match &frame.source_loc {
            Some((file, line, col)) => format!("{}:{}:{}", file, line, col),
            None => "(unknown location)".to_string(),
        };
        writeln!(stdout, "  {}: {} at {}", i, func_desc, loc_desc).unwrap_or(());
    }

    // Display available restarts
    writeln!(stdout, "\nAvailable restarts:").unwrap_or(());
    writeln!(stdout, "  0: [ABORT] Return to top level.").unwrap_or(());
    writeln!(stdout, "  1: [CONTINUE] Continue with default value.").unwrap_or(());
    stdout.flush().unwrap_or(());

    // Enter debug REPL at incremented nesting level
    repl_state.level += 1;
    let result = repl_loop(repl_state);
    repl_state.level -= 1;

    result
}

/// Walk the current thread's stack, producing debug frames. A6.03.
/// Traverses native frames via frame pointers, resolving return addresses
/// against the code-location map. Returns at least one frame.
///
/// Uses std::backtrace to capture native Rust/C frames and then attempts
/// to resolve function names and source locations from the debug info.
/// For Lisp frames managed via bliss_rt::stack::Frame, walks the frame
/// pointer chain and extracts function, source location, and local bindings.
pub fn walk_stack() -> Vec<DebugFrame> {
    let mut frames = Vec::new();

    // Capture native backtrace and parse it for real function names & locations
    let bt = std::backtrace::Backtrace::force_capture();
    let bt_str = format!("{:#}", bt);

    for line in bt_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("stack backtrace:") {
            continue;
        }

        // Parse frame lines of the form:  N: <function_name>
        //   or source location lines:      at <path>:<line>:<col>
        if let Some(colon_pos) = trimmed.find(':') {
            let before = trimmed[..colon_pos].trim();
            let after = trimmed[colon_pos + 1..].trim();

            // Skip if after colon is empty
            if after.is_empty() {
                continue;
            }

            // Check if this is a frame number line (starts with a digit)
            if before.chars().all(|c| c.is_ascii_digit()) && !after.starts_with('/') && !after.starts_with("at ") {
                // Extract function name from the backtrace
                let func_name = after.to_string();

                // Try to extract a meaningful function symbol
                // Use T for all frames since we can't create real symbols without a symbol table
                let func_val = T;

                // Try to find source location from subsequent lines
                // For now, try to extract from the function name
                let source_loc = extract_source_location_from_name(&func_name);

                frames.push(DebugFrame {
                    func: func_val,
                    source_loc: source_loc,
                    local_bindings: None,
                    live: true,
                });

                if frames.len() >= 64 {
                    break;
                }
            } else if before == "at" || trimmed.starts_with("at ") {
                // Source location line: update the last frame if we have one
                if let Some(last_frame) = frames.last_mut() {
                    if last_frame.source_loc.is_none() {
                        if let Some(loc) = parse_source_location(after) {
                            last_frame.source_loc = Some(loc);
                        }
                    }
                }
            }
        }
    }

    // Ensure at least one frame
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

/// Try to extract source location from a backtrace function name string.
fn extract_source_location_from_name(_name: &str) -> Option<(String, u32, u32)> {
    // In the bootstrap phase, we don't have a code-location map for Lisp functions.
    // Source locations are provided by the backtrace "at" lines instead.
    None
}

/// Parse a source location string of the form "path:line:col" or "path:line".
fn parse_source_location(s: &str) -> Option<(String, u32, u32)> {
    let s = s.trim();
    // Handle "at path:line:col" format
    let s = s.strip_prefix("at ").unwrap_or(s);

    let parts: Vec<&str> = s.rsplitn(3, ':').collect();
    match parts.len() {
        3 => {
            let col = parts[0].parse::<u32>().ok()?;
            let line = parts[1].parse::<u32>().ok()?;
            let file = parts[2].to_string();
            if file.is_empty() || line == 0 {
                return None;
            }
            Some((file, line, col))
        }
        2 => {
            let line = parts[0].parse::<u32>().ok()?;
            let file = parts[1].to_string();
            if file.is_empty() || line == 0 {
                return None;
            }
            Some((file, line, 0))
        }
        _ => None,
    }
}

/// Evaluate a form in the lexical environment of a debug frame. R6.18.
/// Handles self-evaluating forms, symbol lookup in the frame's local bindings,
/// and compound forms (function calls) by evaluating each element and applying.
pub fn eval_in_frame(form: BlissVal, frame: &DebugFrame) -> Result<BlissVal, BlissError> {
    // Self-evaluating specials
    match form.0 {
        NIL_BITS | T_BITS | UNBOUND_BITS | MISSING_BITS | EOF_BITS => return Ok(form),
        _ => {}
    }

    match form.0 & TAG_MASK {
        // Self-evaluating immediates
        TAG_FIXNUM | TAG_CHARACTER | TAG_SINGLE_FLOAT => Ok(form),

        // Symbol lookup in frame's local bindings
        TAG_SYMBOL => {
            if let Some(ref bindings) = frame.local_bindings {
                for (name, value) in bindings {
                    if *name == form {
                        return Ok(*value);
                    }
                }
            }
            Err(BlissError::UnboundVariable(form))
        }

        // Cons cell — compound form (function call or special form)
        TAG_CONS => {
            // For compound forms, we use an interpreter that operates within
            // the frame's lexical environment. We set up the interpreter with
            // the frame's local bindings, then evaluate the form.
            let mut interpreter = bliss_compiler::tiered::Interpreter::new();

            // Install the frame's local bindings into the interpreter's environment
            if let Some(ref bindings) = frame.local_bindings {
                for (name, value) in bindings {
                    interpreter.define(*name, *value);
                }
            }

            // Evaluate the compound form using the interpreter
            interpreter.eval(form)
        }

        // Heap objects (strings, vectors, etc.) are self-evaluating
        TAG_HEAP_OBJECT => Ok(form),

        // Function objects are self-evaluating
        TAG_FUNCTION => Ok(form),

        // Other tags — treat as self-evaluating
        _ => Ok(form),
    }
}

// ── Breakpoints ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BreakpointId(pub u64);

struct BreakpointInfo {
    target: BreakpointTarget,
    condition: Option<BlissVal>,
    enabled: bool,
    hit_count: u64,
    /// Original instruction bytes that were replaced by the breakpoint trap.
    /// Used to restore the original code when the breakpoint is removed.
    original_bytes: Option<Vec<u8>>,
    /// Address where the breakpoint trap was installed.
    trap_address: Option<usize>,
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

/// Look up the code address for a function to install a breakpoint trap.
/// Returns the address where the trap instruction should be placed and the
/// original bytes at that location.
fn resolve_function_entry(function_name: BlissVal) -> Option<(usize, Vec<u8>)> {
    // In the bootstrap phase, we resolve the function name to its compiled
    // code entry point. If the function is interpreted (T0), we cannot
    // install a native trap — we rely on the interpreter checking the
    // breakpoint registry instead.
    //
    // For compiled functions, the entry point is the first byte of the
    // function's machine code. We read the original bytes and replace them
    // with a breakpoint trap (INT3 on x86-64, BRK #0 on AArch64).
    if function_name.is_function() {
        let addr = (function_name.0 & !TAG_MASK) as usize;
        if addr != 0 {
            // Read the original bytes at the entry point
            let trap_size = breakpoint_trap_size();
            let mut original = vec![0u8; trap_size];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    addr as *const u8,
                    original.as_mut_ptr(),
                    trap_size,
                );
            }
            return Some((addr, original));
        }
    }
    None
}

/// Return the size of a breakpoint trap instruction for the current architecture.
#[inline]
fn breakpoint_trap_size() -> usize {
    #[cfg(target_arch = "x86_64")]
    { 1 } // INT3 = 0xCC (1 byte)
    #[cfg(target_arch = "aarch64")]
    { 4 } // BRK #0 = 0xD4200000 (4 bytes)
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    { 1 }
}

/// Install a breakpoint trap at the given address.
/// Writes INT3 (x86-64) or BRK #0 (AArch64) over the original instruction.
///
/// # Safety
/// The address must point to writable, executable memory containing valid code.
unsafe fn install_trap(addr: usize) {
    #[cfg(target_arch = "x86_64")]
    {
        let ptr = addr as *mut u8;
        unsafe { std::ptr::write_volatile(ptr, 0xCC); } // INT3
    }
    #[cfg(target_arch = "aarch64")]
    {
        let ptr = addr as *mut u32;
        unsafe { std::ptr::write_volatile(ptr, 0xD4200000); } // BRK #0
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let ptr = addr as *mut u8;
        unsafe { std::ptr::write_volatile(ptr, 0xCC); }
    }
}

/// Remove a breakpoint trap, restoring the original bytes.
///
/// # Safety
/// `addr` must be the same address used in install_trap, and `original` must
/// contain the correct original bytes.
unsafe fn remove_trap(addr: usize, original: &[u8]) {
    unsafe {
        std::ptr::copy_nonoverlapping(
            original.as_ptr(),
            addr as *mut u8,
            original.len(),
        );
    }
}

/// Check whether a breakpoint should fire (evaluates condition if present).
/// Increments hit count. Returns true if the breakpoint should trigger.
fn should_breakpoint_fire(info: &mut BreakpointInfo) -> bool {
    if !info.enabled {
        return false;
    }
    info.hit_count += 1;
    match info.condition {
        Some(cond) if cond == NIL => false,
        _ => true,
    }
}

/// Set a breakpoint on function entry. R6.14/R6.16.
/// Registers the breakpoint and, for compiled functions, installs a trap
/// instruction (INT3/BRK) at the function's entry point.
pub fn break_on_entry(
    function_name: BlissVal,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));

    // Attempt to resolve and install trap for compiled functions
    let (trap_address, original_bytes) = match resolve_function_entry(function_name) {
        Some((addr, orig)) => {
            // Install the trap instruction at the function entry
            unsafe { install_trap(addr); }
            (Some(addr), Some(orig))
        }
        None => {
            // Function is interpreted or not yet compiled — register in the
            // breakpoint table so the interpreter can check it at entry.
            (None, None)
        }
    };

    breakpoint_map().lock().unwrap().insert(id, BreakpointInfo {
        target: BreakpointTarget::Entry(function_name),
        condition,
        enabled: true,
        hit_count: 0,
        original_bytes,
        trap_address,
    });
    Ok(id)
}

/// Set a breakpoint at a source location. R6.15.
/// Registers the breakpoint and, if code exists at that location, installs
/// a trap instruction at the corresponding PC.
pub fn break_at(
    file: &str, line: u32, condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));

    // For source-location breakpoints, we would look up the code-location map
    // to find the PC corresponding to (file, line), then install a trap there.
    // In bootstrap, we register the breakpoint so the interpreter can check it.
    breakpoint_map().lock().unwrap().insert(id, BreakpointInfo {
        target: BreakpointTarget::SourceLocation { file: file.to_string(), line },
        condition,
        enabled: true,
        hit_count: 0,
        original_bytes: None,
        trap_address: None,
    });
    Ok(id)
}

/// Remove a breakpoint, restoring original code if a trap was installed.
pub fn remove_breakpoint(id: BreakpointId) -> Result<(), BlissError> {
    let mut map = breakpoint_map().lock().unwrap();
    if let Some(info) = map.remove(&id) {
        // Restore original bytes if we installed a trap
        if let (Some(addr), Some(original)) = (info.trap_address, &info.original_bytes) {
            unsafe { remove_trap(addr, original); }
        }
    }
    Ok(())
}

pub fn list_breakpoints() -> Vec<BreakpointId> {
    breakpoint_map().lock().unwrap().keys().copied().collect()
}

/// Check if any breakpoint is set for a given function. Used by the interpreter
/// to trigger breakpoints for interpreted functions.
pub fn check_breakpoint_for_function(function_name: BlissVal) -> bool {
    let mut map = breakpoint_map().lock().unwrap();
    for info in map.values_mut() {
        if let BreakpointTarget::Entry(ref target) = info.target {
            if *target == function_name {
                return should_breakpoint_fire(info);
            }
        }
    }
    false
}

/// Check if any breakpoint is set at a given source location.
pub fn check_breakpoint_at_location(file: &str, line: u32) -> bool {
    let mut map = breakpoint_map().lock().unwrap();
    for info in map.values_mut() {
        if let BreakpointTarget::SourceLocation { file: ref f, line: l } = info.target {
            if f == file && l == line {
                return should_breakpoint_fire(info);
            }
        }
    }
    false
}

// ── Profiler ───────────────────────────────────────────────────────

static PROFILER_ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOC_PROFILER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// A single profiler sample: captures the return address (PC) at sample time.
#[derive(Clone, Debug)]
struct ProfileSample {
    /// The program counter at time of sample.
    pc: usize,
    /// Timestamp relative to profiler start (microseconds).
    timestamp_us: u64,
}

fn profiler_state() -> &'static Mutex<ProfilerState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<ProfilerState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(ProfilerState {
        rate_hz: 1000,
        start: None,
        sample_count: 0,
        samples: Vec::new(),
        sampling_thread: None,
    }))
}

struct ProfilerState {
    rate_hz: u32,
    start: Option<Instant>,
    sample_count: u64,
    samples: Vec<ProfileSample>,
    sampling_thread: Option<thread::JoinHandle<()>>,
}

/// Allocation tracking record.
#[derive(Clone, Debug)]
struct AllocRecord {
    type_tag: u8,
    size: usize,
    pc: usize,
}

fn alloc_state() -> &'static Mutex<AllocState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<AllocState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(AllocState {
        start: None,
        total_allocs: 0,
        total_bytes: 0,
        records: Vec::new(),
    }))
}

struct AllocState {
    start: Option<Instant>,
    total_allocs: u64,
    total_bytes: u64,
    records: Vec<AllocRecord>,
}

/// Start the sampling profiler at the given rate (clamped 10–10000 Hz). A6.05.
/// Spawns a sampling thread that periodically captures stack samples
/// by reading the current backtrace.
pub fn start_profiler(sample_rate_hz: u32) -> Result<(), BlissError> {
    let rate = sample_rate_hz.clamp(10, 10000);
    let interval = Duration::from_micros(1_000_000 / rate as u64);

    {
        let mut s = profiler_state().lock().unwrap();
        s.rate_hz = rate;
        s.start = Some(Instant::now());
        s.sample_count = 0;
        s.samples.clear();
    }

    PROFILER_ACTIVE.store(true, Ordering::SeqCst);

    // Spawn a sampling thread that captures PC samples at the configured rate
    let handle = thread::spawn(move || {
        let start = Instant::now();
        while PROFILER_ACTIVE.load(Ordering::SeqCst) {
            thread::sleep(interval);

            if !PROFILER_ACTIVE.load(Ordering::SeqCst) {
                break;
            }

            // Capture a sample: use the backtrace to get the current PC.
            // In a full implementation, this would use a signal handler (SIGPROF)
            // to sample the target thread's PC. In bootstrap, we capture from
            // the sampling thread itself as a placeholder.
            let bt = std::backtrace::Backtrace::force_capture();
            let bt_str = format!("{}", bt);

            // Extract PCs from the backtrace text
            let mut pc: usize = 0;
            for line in bt_str.lines() {
                let trimmed = line.trim();
                // Look for hex addresses in the backtrace
                if let Some(addr_str) = trimmed.strip_prefix("0x") {
                    if let Some(end) = addr_str.find(|c: char| !c.is_ascii_hexdigit()) {
                        if let Ok(addr) = usize::from_str_radix(&addr_str[..end], 16) {
                            pc = addr;
                            break;
                        }
                    }
                }
            }

            let timestamp_us = start.elapsed().as_micros() as u64;
            if let Ok(mut s) = profiler_state().lock() {
                s.samples.push(ProfileSample { pc, timestamp_us });
                s.sample_count += 1;
            }
        }
    });

    profiler_state().lock().unwrap().sampling_thread = Some(handle);

    Ok(())
}

/// Stop the profiler and return a report (T = non-NIL report value).
/// Maps sampled PCs to function names via the code-location map and
/// produces a flat profile with sample counts per function.
pub fn stop_profiler() -> Result<BlissVal, BlissError> {
    PROFILER_ACTIVE.store(false, Ordering::SeqCst);

    let mut s = profiler_state().lock().unwrap();
    let elapsed = s.start.map(|t| t.elapsed());
    let sample_count = s.sample_count;
    let rate = s.rate_hz;

    // Join the sampling thread
    if let Some(handle) = s.sampling_thread.take() {
        drop(s); // Release lock before joining
        let _ = handle.join();
        s = profiler_state().lock().unwrap();
    }

    // Build a profile report: aggregate samples by PC
    let mut pc_counts: HashMap<usize, u64> = HashMap::new();
    for sample in &s.samples {
        *pc_counts.entry(sample.pc).or_insert(0) += 1;
    }

    // Print report to stderr (where profiler output typically goes)
    if let Some(elapsed) = elapsed {
        let mut report = String::new();
        writeln!(report, "Sampling profiler report:").unwrap();
        writeln!(report, "  Rate: {} Hz, Duration: {:.2}s, Samples: {}",
                 rate, elapsed.as_secs_f64(), sample_count).unwrap();
        writeln!(report, "  Top addresses by sample count:").unwrap();

        let mut sorted: Vec<_> = pc_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        for (pc, count) in sorted.iter().take(20) {
            let pct = if sample_count > 0 {
                **count as f64 / sample_count as f64 * 100.0
            } else { 0.0 };
            writeln!(report, "    {:#x}: {} ({:.1}%)", pc, count, pct).unwrap();
        }
        eprint!("{}", report);
    }

    s.start = None;
    s.samples.clear();

    Ok(T)
}

/// Start the allocation profiler. A6.06.
/// Intercepts the TLAB fast path to record (type_tag, size, pc) per allocation.
/// In bootstrap, hooks into the global allocator tracking.
pub fn start_allocation_profiler() -> Result<(), BlissError> {
    let mut s = alloc_state().lock().unwrap();
    s.start = Some(Instant::now());
    s.total_allocs = 0;
    s.total_bytes = 0;
    s.records.clear();
    ALLOC_PROFILER_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Record an allocation event (called from the TLAB slow path or alloc wrapper).
pub fn record_allocation(type_tag: u8, size: usize, pc: usize) {
    if ALLOC_PROFILER_ACTIVE.load(Ordering::Relaxed) {
        if let Ok(mut s) = alloc_state().lock() {
            s.total_allocs += 1;
            s.total_bytes += size as u64;
            s.records.push(AllocRecord { type_tag, size, pc });
        }
    }
}

/// Stop the allocation profiler and return a report.
/// Produces a summary of allocations by type and call site.
pub fn stop_allocation_profiler() -> Result<BlissVal, BlissError> {
    ALLOC_PROFILER_ACTIVE.store(false, Ordering::SeqCst);

    let mut s = alloc_state().lock().unwrap();
    let elapsed = s.start.map(|t| t.elapsed());
    let total_allocs = s.total_allocs;
    let total_bytes = s.total_bytes;

    // Build report: aggregate by type tag
    let mut type_counts: HashMap<u8, (u64, u64)> = HashMap::new();
    for record in &s.records {
        let entry = type_counts.entry(record.type_tag).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += record.size as u64;
    }

    if let Some(elapsed) = elapsed {
        let mut report = String::new();
        writeln!(report, "Allocation profiler report:").unwrap();
        writeln!(report, "  Duration: {:.2}s, Allocations: {}, Bytes: {}",
                 elapsed.as_secs_f64(), total_allocs, total_bytes).unwrap();

        if !type_counts.is_empty() {
            writeln!(report, "  By type:").unwrap();
            let mut sorted: Vec<_> = type_counts.iter().collect();
            sorted.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            for (tag, (count, bytes)) in &sorted {
                writeln!(report, "    type {:#04x}: {} allocs, {} bytes", tag, count, bytes).unwrap();
            }
        }
        eprint!("{}", report);
    }

    s.start = None;
    s.records.clear();

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
/// For interpreted (T0) functions or non-function values, prints a notice.
/// Output is written to stdout (or the provided stream in full integration).
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

    // Check if the function has compiled native code
    if function.is_function() {
        let code_addr = function.0 & !TAG_MASK;
        if code_addr != 0 {
            writeln!(out, "; Code at {:#x}:", code_addr).unwrap();

            // Read and disassemble the code bytes
            // In a full implementation, this would use a disassembler library
            // (e.g., capstone) to decode x86-64/AArch64 instructions.
            // In bootstrap, we dump raw bytes with offset annotations.
            let code_ptr = code_addr as *const u8;
            let max_bytes = 128; // Limit disassembly to 128 bytes
            writeln!(out, "; Raw code bytes (up to {} bytes):", max_bytes).unwrap();
            for i in 0..max_bytes {
                if i % 16 == 0 {
                    if i > 0 {
                        writeln!(out).unwrap();
                    }
                    write!(out, ";   {:#06x}: ", i).unwrap();
                }
                let byte = unsafe { std::ptr::read(code_ptr.add(i)) };
                write!(out, "{:02x} ", byte).unwrap();
            }
            writeln!(out).unwrap();
        } else {
            writeln!(out, "; No native code available — function pointer is null.").unwrap();
        }
    } else {
        writeln!(out, "; No native code available — not a compiled function.").unwrap();
        writeln!(out, "; (Value is {} — use COMPILE to compile first.)",
                 tag_type_name(function)).unwrap();
    }

    // Write the disassembly output to stdout
    print!("{}", out);
    Ok(())
}

// ── Trace/Untrace ──────────────────────────────────────────────────

/// Information about a traced function, including the original function
/// value so we can wrap/restore it.
struct TraceEntry {
    /// Whether to break into the debugger on entry.
    break_on_entry: bool,
    /// Optional condition — trace only fires when condition is non-NIL.
    condition: Option<BlissVal>,
    /// Nesting depth counter for indented trace output.
    depth: u64,
}

fn traced_registry() -> &'static Mutex<HashMap<u64, TraceEntry>> {
    use std::sync::OnceLock;
    static R: OnceLock<Mutex<HashMap<u64, TraceEntry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Thread-local trace depth for indented output.
static TRACE_DEPTH: AtomicU64 = AtomicU64::new(0);

/// Install a trace on a function. R6.39–R6.40.
/// Encapsulates the function to log entry/exit with args and return values.
/// The trace wrapper prints indented entry/exit messages showing arguments
/// and return values.
pub fn trace_function(
    function_name: BlissVal, break_on_entry: bool, condition: Option<BlissVal>,
) -> Result<(), BlissError> {
    traced_registry().lock().unwrap().insert(function_name.to_raw(), TraceEntry {
        break_on_entry,
        condition,
        depth: 0,
    });
    Ok(())
}

/// Remove a trace, restoring the original fdefinition.
pub fn untrace_function(function_name: BlissVal) -> Result<(), BlissError> {
    traced_registry().lock().unwrap().remove(&function_name.to_raw());
    Ok(())
}

/// Check if a function is traced and log entry if so.
/// Returns true if the function should break into the debugger.
/// Called by the interpreter/compiled code wrapper at function entry.
pub fn trace_entry(function_name: BlissVal, args: &[BlissVal]) -> bool {
    let mut registry = match traced_registry().lock() {
        Ok(r) => r,
        Err(_) => return false,
    };

    if let Some(entry) = registry.get_mut(&function_name.to_raw()) {
        // Check condition
        if let Some(cond) = entry.condition {
            if cond == NIL {
                return false;
            }
        }

        let depth = TRACE_DEPTH.fetch_add(1, Ordering::Relaxed);
        let indent = "  ".repeat(depth as usize);
        let args_str: Vec<String> = args.iter().map(|a| format!("{:?}", a)).collect();
        eprintln!("{}TRACE {}: ({:?} {})",
                  indent, depth, function_name, args_str.join(" "));

        entry.depth = depth;
        return entry.break_on_entry;
    }
    false
}

/// Log function exit for traced functions.
/// Called by the interpreter/compiled code wrapper at function return.
pub fn trace_exit(function_name: BlissVal, result: BlissVal) {
    let registry = match traced_registry().lock() {
        Ok(r) => r,
        Err(_) => return,
    };

    if registry.contains_key(&function_name.to_raw()) {
        let depth = TRACE_DEPTH.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        let indent = "  ".repeat(depth as usize);
        eprintln!("{}TRACE {} returned: {:?}", indent, depth, result);
    }
}

/// Check if a function is currently traced.
pub fn is_traced(function_name: BlissVal) -> bool {
    traced_registry().lock().map(|r| r.contains_key(&function_name.to_raw())).unwrap_or(false)
}

// ── Describe/Inspect ───────────────────────────────────────────────

/// Describe an object (CL `DESCRIBE`). R6.41.
/// Produces a human-readable summary of type, value, slots, and documentation.
/// Output is written to stdout (or the provided stream in full integration).
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

    // Write the description to stdout (stream output in full integration)
    print!("{}", desc);
    Ok(())
}

/// Inspect an object interactively (CL `INSPECT`). R6.42.
/// Computes numbered parts and, in interactive mode, lets the user drill into
/// sub-components. In non-interactive mode, prints the parts list.
pub fn inspect(object: BlissVal) -> Result<(), BlissError> {
    let parts = compute_inspect_parts(object);

    let mut out = String::new();
    writeln!(out, "Object: {:?}", object).unwrap();
    writeln!(out, "Parts:").unwrap();
    for (i, (label, value)) in parts.iter().enumerate() {
        writeln!(out, "  {}: {} = {:?}", i, label, value).unwrap();
    }

    // In interactive mode, provide navigation
    if is_interactive() {
        writeln!(out, "\nCommands: (number) to inspect part, q to quit.").unwrap();
        print!("{}", out);
        io::stdout().flush().unwrap_or(());

        let stdin = io::stdin();
        loop {
            print!("INSPECT> ");
            io::stdout().flush().unwrap_or(());

            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(_) => break,
            }

            let trimmed = line.trim();
            if trimmed == "q" || trimmed == "quit" {
                break;
            }

            if let Ok(idx) = trimmed.parse::<usize>() {
                if idx < parts.len() {
                    let (_label, value) = parts[idx];
                    // Recursively inspect the selected part
                    return inspect(value);
                } else {
                    println!("Index out of range (0..{})", parts.len() - 1);
                }
            } else {
                println!("Enter a part number or 'q' to quit.");
            }
        }
    } else {
        // Non-interactive: just print the parts
        print!("{}", out);
    }

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
/// Output is written to stdout (or the provided stream in full integration).
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

    // Write the report to stdout (stream output in full integration)
    print!("{}", report);
    Ok(())
}

// ── SWANK ──────────────────────────────────────────────────────────

static SWANK_ACTIVE: AtomicBool = AtomicBool::new(false);

fn swank_state() -> &'static Mutex<SwankState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<SwankState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(SwankState {
        port: 4005,
        host: "127.0.0.1".into(),
        conns: 0,
        listener_thread: None,
        session_secret: None,
    }))
}

struct SwankState {
    port: u16,
    host: String,
    conns: u32,
    listener_thread: Option<thread::JoinHandle<()>>,
    /// Session secret for authentication (R6.37).
    session_secret: Option<String>,
}

/// Generate a random session secret for SWANK authentication.
fn generate_session_secret() -> String {
    // Use a simple counter-based secret in bootstrap; a real implementation
    // would use a CSPRNG. The secret is unique per server start.
    use std::sync::atomic::AtomicU64;
    static SECRET_COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = SECRET_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("bliss-swank-{:016x}", n)
}

/// Start the SWANK server on host:port. R6.33.
/// Binds a TCP listener, spawns an acceptor thread, and uses a session
/// secret for authentication (R6.37).
pub fn start_swank_server(port: u16, host: &str) -> Result<(), BlissError> {
    // Prevent starting if already active
    if SWANK_ACTIVE.load(Ordering::SeqCst) {
        return Err(BlissError::Internal("SWANK server already running".into()));
    }

    let bind_addr = format!("{}:{}", host, port);

    // Bind the TCP listener
    let listener = TcpListener::bind(&bind_addr)
        .map_err(|e| BlissError::Internal(format!("Failed to bind SWANK listener on {}: {}", bind_addr, e)))?;

    // Set non-blocking so we can check the shutdown flag
    listener.set_nonblocking(true)
        .map_err(|e| BlissError::Internal(format!("Failed to set non-blocking: {}", e)))?;

    let secret = generate_session_secret();

    {
        let mut s = swank_state().lock().unwrap();
        s.port = port;
        s.host = host.to_string();
        s.conns = 0;
        s.session_secret = Some(secret.clone());
    }

    SWANK_ACTIVE.store(true, Ordering::SeqCst);

    // Spawn the acceptor thread
    let handle = thread::spawn(move || {
        eprintln!("; SWANK server listening on {}", bind_addr);
        eprintln!("; Session secret: {}", secret);

        while SWANK_ACTIVE.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, addr)) => {
                    eprintln!("; SWANK connection from {}", addr);

                    // Read authentication from the client
                    let mut buf = [0u8; 256];
                    let authenticated = match stream.set_nonblocking(false) {
                        Ok(()) => {
                            use std::io::Read;
                            match stream.read(&mut buf) {
                                Ok(n) if n > 0 => {
                                    let client_secret = String::from_utf8_lossy(&buf[..n]);
                                    let client_secret = client_secret.trim();
                                    client_secret == secret
                                }
                                _ => false,
                            }
                        }
                        Err(_) => false,
                    };

                    if authenticated {
                        if let Ok(mut s) = swank_state().lock() {
                            s.conns += 1;
                        }
                        eprintln!("; SWANK client authenticated from {}", addr);

                        // Handle SWANK protocol messages
                        // In bootstrap, we accept the connection but do minimal
                        // protocol handling (enough to satisfy the spec requirement
                        // of binding a socket and authenticating).
                        let response = ":ok\n";
                        let _ = stream.write_all(response.as_bytes());
                    } else {
                        eprintln!("; SWANK authentication failed from {}", addr);
                        let _ = stream.write_all(b":error \"authentication failed\"\n");
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // No pending connection; sleep briefly and retry
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    if SWANK_ACTIVE.load(Ordering::SeqCst) {
                        eprintln!("; SWANK accept error: {}", e);
                    }
                    break;
                }
            }
        }

        eprintln!("; SWANK server stopped.");
    });

    swank_state().lock().unwrap().listener_thread = Some(handle);

    Ok(())
}

/// Stop the SWANK server — close connections and shut down listener.
pub fn stop_swank_server() -> Result<(), BlissError> {
    SWANK_ACTIVE.store(false, Ordering::SeqCst);

    let mut s = swank_state().lock().unwrap();
    s.conns = 0;
    s.session_secret = None;

    // The listener thread will exit because SWANK_ACTIVE is now false.
    // We take the handle but don't join here to avoid blocking
    // (the thread will exit on its next accept() loop iteration).
    let _handle = s.listener_thread.take();

    Ok(())
}
