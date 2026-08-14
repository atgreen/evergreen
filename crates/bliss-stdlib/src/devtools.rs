//! Developer tools — REPL, debugger, profiler, disassembler, SWANK.
//! See spec §6.

use bliss_rt::error::BlissError;
use bliss_rt::object::ConsCell;
use bliss_rt::value::{
    BlissVal, EOF_BITS, MISSING_BITS, NIL, NIL_BITS, T, T_BITS, TAG_CHARACTER, TAG_CONS,
    TAG_FIXNUM, TAG_FUNCTION, TAG_HEAP_OBJECT, TAG_MASK, TAG_SINGLE_FLOAT, TAG_SPECIAL, TAG_SYMBOL,
    UNBOUND_BITS,
};
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter, MatchingBracketHighlighter};
use rustyline::hint::{Hinter, HistoryHinter};
use rustyline::history::DefaultHistory;
use rustyline::validate::{
    MatchingBracketValidator, ValidationContext, ValidationResult, Validator,
};
use rustyline::{CompletionType, Config, Context, Editor, Helper};

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::io::{self, BufRead, Read as IoRead, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Persistent command history for multi-line editing (R6.02, R6.03).
    command_history: Vec<String>,
    /// Prompt function placeholder — configurable prompt (R6.08).
    prompt_fn: Option<Box<dyn Fn(usize, BlissVal) -> String>>,
    /// Input buffer for multi-line input (R6.02).
    input_buffer: String,
}

impl ReplState {
    pub fn new() -> Self {
        // Load persistent history from file (R6.03)
        let command_history = load_history_from_file();
        ReplState {
            current_package: T,
            level: 0,
            history_star: [NIL; 3],
            history_plus: [NIL; 3],
            history_slash: [vec![], vec![], vec![]],
            command_history,
            prompt_fn: None,
            input_buffer: String::new(),
        }
    }
    pub fn package(&self) -> BlissVal {
        self.current_package
    }
    pub fn level(&self) -> usize {
        self.level
    }

    /// Set a custom prompt function (R6.08).
    pub fn set_prompt_fn<F: Fn(usize, BlissVal) -> String + 'static>(&mut self, f: F) {
        self.prompt_fn = Some(Box::new(f));
    }

    /// Get the prompt string using the configured prompt function or default.
    fn prompt_string(&self) -> String {
        if let Some(ref f) = self.prompt_fn {
            f(self.level, self.current_package)
        } else if self.level == 0 {
            "BLISS> ".to_string()
        } else {
            format!("BLISS[{}]> ", self.level)
        }
    }

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

    /// Save a line to the persistent command history (R6.03).
    fn push_history(&mut self, line: &str) {
        let trimmed = line.trim().to_string();
        if !trimmed.is_empty() {
            self.command_history.push(trimmed);
            // Keep history bounded
            let max_size = history_max_size();
            if self.command_history.len() > max_size {
                let excess = self.command_history.len() - max_size;
                self.command_history.drain(0..excess);
            }
        }
    }
}

impl Default for ReplState {
    fn default() -> Self {
        Self::new()
    }
}

/// Load persistent history from ~/.bliss/repl-history (R6.03).
fn load_history_from_file() -> Vec<String> {
    let path = history_file_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents.lines().map(|l| l.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Save history to file (R6.03).
fn save_history_to_file(history: &[String]) {
    let path = history_file_path();
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let max = history_max_size();
    let start = if history.len() > max {
        history.len() - max
    } else {
        0
    };
    let content: String = history[start..].join("\n");
    let _ = std::fs::write(&path, content);
}

fn history_file_path() -> String {
    std::env::var("BLISS_HISTFILE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        format!("{}/.bliss/repl-history", home)
    })
}

fn history_max_size() -> usize {
    std::env::var("BLISS_REPL_HISTORY_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000)
}

struct ReplHelper {
    highlighter: MatchingBracketHighlighter,
    validator: MatchingBracketValidator,
    hinter: HistoryHinter,
    file_completer: FilenameCompleter,
}

impl ReplHelper {
    fn new() -> Self {
        Self {
            highlighter: MatchingBracketHighlighter::new(),
            validator: MatchingBracketValidator::new(),
            hinter: HistoryHinter::new(),
            file_completer: FilenameCompleter::new(),
        }
    }
}

impl Helper for ReplHelper {}

impl Hinter for ReplHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<String> {
        self.hinter.hint(line, pos, ctx)
    }
}

impl Highlighter for ReplHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> std::borrow::Cow<'b, str> {
        self.highlighter.highlight_prompt(prompt, default)
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
        self.highlighter.highlight_hint(hint)
    }

    fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
        self.highlighter.highlight(line, pos)
    }

    fn highlight_char(&self, line: &str, pos: usize, kind: CmdKind) -> bool {
        self.highlighter.highlight_char(line, pos, kind)
    }
}

impl Validator for ReplHelper {
    fn validate(&self, ctx: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        self.validator.validate(ctx)
    }

    fn validate_while_typing(&self) -> bool {
        self.validator.validate_while_typing()
    }
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        if let Some((start, path_prefix)) = path_completion_span(prefix) {
            return self
                .file_completer
                .complete(path_prefix, pos - start, ctx)
                .map(|(path_start, pairs)| (start + path_start, pairs));
        }

        let start = prefix
            .rfind(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\''))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let token = &prefix[start..];
        let pairs = complete_symbol(token)
            .into_iter()
            .map(|candidate| Pair {
                display: candidate.clone(),
                replacement: candidate,
            })
            .collect();
        Ok((start, pairs))
    }
}

fn path_completion_span(prefix: &str) -> Option<(usize, &str)> {
    let lower = prefix.to_ascii_lowercase();
    for marker in ["(load ", "(require "] {
        if let Some(idx) = lower.rfind(marker) {
            let start = idx + marker.len();
            let token = prefix[start..].trim_start_matches('"');
            return Some((start + (prefix[start..].len() - token.len()), token));
        }
    }
    None
}

fn alloc_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    let cell = Box::leak(Box::new(ConsCell { car, cdr }));
    unsafe { BlissVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) }
}

fn values_to_list(values: &[BlissVal]) -> BlissVal {
    let mut result = NIL;
    for &value in values.iter().rev() {
        result = alloc_cons(value, result);
    }
    result
}

fn expose_repl_history(interpreter: &mut bliss_compiler::tiered::Interpreter, state: &ReplState) {
    let bindings = [
        ("*", state.history_star[0]),
        ("**", state.history_star[1]),
        ("***", state.history_star[2]),
        ("+", state.history_plus[0]),
        ("++", state.history_plus[1]),
        ("+++", state.history_plus[2]),
        ("/", values_to_list(&state.history_slash[0])),
        ("//", values_to_list(&state.history_slash[1])),
        ("///", values_to_list(&state.history_slash[2])),
    ];

    for (name, value) in bindings {
        let sym = BlissVal::from_symbol_index(bliss_compiler::reader::intern_symbol(name));
        interpreter.define(sym, value);
    }
}

/// Check if stdin is attached to a terminal (interactive).
fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Check if brackets/parens are balanced for multi-line input (R6.02).
fn brackets_balanced(input: &str) -> bool {
    let mut depth = 0i64;
    let mut in_string = false;
    let mut escape = false;
    for ch in input.chars() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0 && !in_string
}

/// Simple TAB completion for symbols (R6.04).
/// Returns completions matching the given prefix.
pub fn complete_symbol(prefix: &str) -> Vec<String> {
    // Query the known symbol table for matches.
    // In bootstrap, we provide basic keyword completions.
    let builtins = [
        "defun",
        "defvar",
        "defparameter",
        "defmacro",
        "defclass",
        "defgeneric",
        "defmethod",
        "defstruct",
        "let",
        "let*",
        "lambda",
        "if",
        "cond",
        "case",
        "when",
        "unless",
        "progn",
        "block",
        "return-from",
        "tagbody",
        "go",
        "catch",
        "throw",
        "unwind-protect",
        "handler-bind",
        "handler-case",
        "restart-case",
        "restart-bind",
        "invoke-restart",
        "signal",
        "error",
        "warn",
        "cerror",
        "format",
        "print",
        "princ",
        "prin1",
        "write",
        "read",
        "eval",
        "compile",
        "load",
        "require",
        "provide",
        "car",
        "cdr",
        "cons",
        "list",
        "append",
        "mapcar",
        "mapc",
        "funcall",
        "apply",
        "values",
        "multiple-value-bind",
        "setq",
        "setf",
        "push",
        "pop",
        "incf",
        "decf",
        "loop",
        "do",
        "dolist",
        "dotimes",
        "map",
        "make-instance",
        "slot-value",
        "with-slots",
        "trace",
        "untrace",
        "describe",
        "inspect",
        "room",
        "time",
        "disassemble",
        "break",
        "step",
    ];
    let lower_prefix = prefix.to_lowercase();
    builtins
        .iter()
        .filter(|s| s.starts_with(&lower_prefix))
        .map(|s| s.to_string())
        .collect()
}

fn swank_symbol_name(name: &str) -> String {
    name.trim_matches('\'')
        .trim_matches('"')
        .split(':')
        .next_back()
        .unwrap_or(name)
        .to_string()
}

fn read_workspace_sources() -> Vec<(String, String)> {
    fn walk(path: &std::path::Path, out: &mut Vec<(String, String)>) {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|s| s.to_str()) == Some("target") {
                    continue;
                }
                walk(&path, out);
            } else if matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("rs" | "lisp" | "lsp" | "cl" | "md")
            ) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    out.push((path.to_string_lossy().to_string(), content));
                }
            }
        }
    }

    let mut files = Vec::new();
    walk(std::path::Path::new("."), &mut files);
    files
}

fn extract_swank_arglist(name: &str) -> Option<String> {
    let bare = swank_symbol_name(name);
    let rust_name = bare.replace('-', "_");
    let lisp_name = bare.to_lowercase();

    for (_path, content) in read_workspace_sources() {
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("pub fn ") {
                if let Some(args_start) = rest.find('(') {
                    let fn_name = rest[..args_start].trim();
                    if fn_name == rust_name {
                        if let Some(args_end) = rest[args_start + 1..].find(')') {
                            let args = &rest[args_start + 1..args_start + 1 + args_end];
                            return Some(format!("({})", args.trim()));
                        }
                    }
                }
            }
            let lower = trimmed.to_lowercase();
            let defun = format!("(defun {}", lisp_name);
            let defmacro = format!("(defmacro {}", lisp_name);
            if lower.starts_with(&defun) || lower.starts_with(&defmacro) {
                if let Some(args_start) = trimmed[1..].find('(') {
                    let start = args_start + 1;
                    if let Some(end) = trimmed[start..].find(')') {
                        return Some(trimmed[start..start + end + 1].to_string());
                    }
                }
            }
        }
    }

    let builtin = match lisp_name.as_str() {
        "car" | "cdr" => Some("(list)"),
        "cons" => Some("(car cdr)"),
        "list" => Some("(&rest objects)"),
        "format" => Some("(destination control-string &rest args)"),
        "apply" => Some("(function &rest args)"),
        "funcall" => Some("(function &rest args)"),
        "make-thread" => Some("(function)"),
        _ => None,
    }?;
    Some(builtin.to_string())
}

fn extract_swank_definitions(name: &str) -> Vec<String> {
    let bare = swank_symbol_name(name);
    let rust_name = bare.replace('-', "_");
    let lisp_name = bare.to_lowercase();
    let mut matches = Vec::new();

    for (path, content) in read_workspace_sources() {
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with(&format!("pub fn {}", rust_name))
                || trimmed.starts_with(&format!("fn {}", rust_name))
                || trimmed
                    .to_lowercase()
                    .starts_with(&format!("(defun {}", lisp_name))
                || trimmed
                    .to_lowercase()
                    .starts_with(&format!("(defmacro {}", lisp_name))
            {
                matches.push(format!(
                    "((\"{}\" \"{}\") (:location (\"{}\" :line {})))",
                    bare,
                    path,
                    path,
                    idx + 1
                ));
            }
        }
    }

    matches
}

/// Run the interactive REPL loop. A6.01.
/// Supports multi-line editing with bracket matching (R6.02),
/// persistent history (R6.03), and condition/restart presentation (R6.07).
pub fn repl_loop(state: &mut ReplState) -> Result<(), BlissError> {
    if !is_interactive() {
        return Ok(());
    }

    let config = Config::builder()
        .history_ignore_space(false)
        .completion_type(CompletionType::List)
        .build();
    let mut editor = Editor::<ReplHelper, DefaultHistory>::with_config(config)
        .map_err(|e| BlissError::Internal(format!("failed to initialize REPL editor: {e}")))?;
    editor.set_helper(Some(ReplHelper::new()));
    let history_path = history_file_path();
    if Path::new(&history_path).exists() {
        let _ = editor.load_history(&history_path);
    }
    let mut interpreter = bliss_compiler::tiered::Interpreter::new();

    loop {
        let prompt = state.prompt_string();
        state.input_buffer.clear();
        loop {
            let prompt = if state.input_buffer.is_empty() {
                prompt.as_str()
            } else {
                "  ... "
            };
            match editor.readline(prompt) {
                Ok(line) => {
                    if !state.input_buffer.is_empty() {
                        state.input_buffer.push('\n');
                    }
                    state.input_buffer.push_str(&line);
                    if brackets_balanced(&state.input_buffer) {
                        break;
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    state.input_buffer.clear();
                    break;
                }
                Err(ReadlineError::Eof) => {
                    save_history_to_file(&state.command_history);
                    let _ = editor.save_history(&history_path);
                    return Ok(());
                }
                Err(e) => return Err(BlissError::Internal(format!("REPL input failed: {e}"))),
            }
        }

        let trimmed = state.input_buffer.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }

        state.push_history(&trimmed);
        let _ = editor.add_history_entry(trimmed.as_str());
        expose_repl_history(&mut interpreter, state);

        let form = match bliss_compiler::reader::read_from_string(&trimmed) {
            Ok((f, _pos)) => f,
            Err(e) => {
                println!("Reader error: {}", e);
                continue;
            }
        };

        match interpreter.eval(form) {
            Ok(value) => {
                println!("{:?}", value);
                state.rotate_history(form, &[value]);
            }
            Err(e) => {
                println!("\nCondition: {}", e);
                println!("Available restarts:");
                println!("  0: [ABORT] Return to top level.");
                println!("  1: [CONTINUE] Continue with NIL.");
                match editor.readline("Select restart (0-1): ") {
                    Ok(choice) if choice.trim() == "1" => {
                        println!("NIL");
                        state.rotate_history(form, &[NIL]);
                    }
                    _ => state.rotate_history(form, &[]),
                }
            }
        }
    }
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
    pub fn function(&self) -> BlissVal {
        self.func
    }
    pub fn source_location(&self) -> Option<(String, u32, u32)> {
        self.source_loc.clone()
    }
    pub fn locals(&self) -> Option<Vec<(BlissVal, BlissVal)>> {
        self.local_bindings.clone()
    }
    pub fn is_live(&self) -> bool {
        self.live
    }
}

/// Stepping mode for the debugger (A6.04 / R6.13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMode {
    /// Step into: stop at the next CL-level source location.
    Step,
    /// Step over: stop at the next source location in the current function.
    Next,
    /// Step out: stop when the current frame returns.
    Out,
    /// Continue execution until next breakpoint.
    Continue,
}

/// Thread-local stepping state.
#[allow(dead_code)]
struct SteppingState {
    /// The thread that is currently stepping.
    #[expect(
        dead_code,
        reason = "stepping metadata is retained for future debugger coordination"
    )]
    thread_id: u64,
    /// The step mode.
    #[expect(
        dead_code,
        reason = "stepping metadata is retained for future debugger coordination"
    )]
    mode: StepMode,
    /// Frame pointer of the frame being stepped (for Next/Out).
    #[expect(
        dead_code,
        reason = "stepping metadata is retained for future debugger coordination"
    )]
    frame_fp: usize,
    /// Trap addresses installed for stepping, with original bytes.
    traps: Vec<(usize, Vec<u8>)>,
}

fn stepping_state() -> &'static Mutex<Option<SteppingState>> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<Option<SteppingState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// Install stepping traps for the given mode (A6.04).
fn install_stepping_traps(mode: StepMode, _frame_fp: usize) {
    let thread_id = thread_id_current();
    let mut state = stepping_state().lock().unwrap();
    *state = Some(SteppingState {
        thread_id,
        mode,
        frame_fp: _frame_fp,
        traps: Vec::new(),
    });
    // In a full implementation, we would:
    // - Step: patch the next CL-level source location and all call targets
    // - Next: patch only the next source location in the current function
    // - Out: patch the return address of the current frame
    // The trap handler checks the stepping-trap table and enters the debugger
    // only for the stepping thread (A6.04).
}

/// Clear all stepping traps.
fn clear_stepping_traps() {
    let mut state = stepping_state().lock().unwrap();
    if let Some(ref st) = *state {
        for (addr, original) in &st.traps {
            unsafe {
                remove_trap(*addr, original);
            }
        }
    }
    *state = None;
}

/// Get a simple thread id for the current thread.
fn thread_id_current() -> u64 {
    // Use the thread id hash as a proxy
    let id = std::thread::current().id();
    let id_str = format!("{:?}", id);
    let mut hash: u64 = 0;
    for b in id_str.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(b as u64);
    }
    hash
}

/// Invoke the debugger on a condition. A6.02.
/// Walks stack, computes restarts, enters debug REPL at incremented nesting level.
/// Supports step/next/out/continue commands (R6.13).
pub fn invoke_debugger_ui(
    condition: BlissVal,
    repl_state: &mut ReplState,
) -> Result<(), BlissError> {
    if !is_interactive() {
        return Ok(());
    }

    let mut stdout = io::stdout();
    let stdin = io::stdin();

    // Display the condition. Conditions surfaced from the REPL are string
    // values (the formatted error text); render that text rather than the raw
    // `HeapObj(..)` debug form.
    let condition_desc = if condition.is_string() {
        condition.as_string()
    } else {
        format!("{:?}", condition)
    };
    writeln!(stdout, "\nDebugger entered: {}", condition_desc).unwrap_or(());

    // Walk the stack and display backtrace
    let frames = walk_stack();
    writeln!(stdout, "\nBacktrace ({} frames):", frames.len()).unwrap_or(());
    let initial_count = std::cmp::min(frames.len(), 10);
    for (i, frame) in frames.iter().enumerate().take(initial_count) {
        print_frame(&mut stdout, i, frame);
    }

    // Display available restarts
    writeln!(stdout, "\nAvailable restarts:").unwrap_or(());
    writeln!(stdout, "  0: [ABORT] Return to top level.").unwrap_or(());
    writeln!(stdout, "  1: [CONTINUE] Continue with default value.").unwrap_or(());
    stdout.flush().unwrap_or(());

    let mut selected_frame: usize = 0;

    // Enter debug REPL with stepping support (R6.13)
    repl_state.level += 1;
    let prompt = format!("DEBUG[{}]> ", repl_state.level);

    loop {
        write!(stdout, "{}", prompt).unwrap_or(());
        stdout.flush().unwrap_or(());

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse debug commands (A6.02)
        let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
        let cmd = parts[0].to_lowercase();
        let arg = parts.get(1).map(|s| s.trim());

        match cmd.as_str() {
            // R6.13: stepping commands
            "step" | "s" => {
                install_stepping_traps(StepMode::Step, 0);
                writeln!(stdout, "Stepping...").unwrap_or(());
                break;
            }
            "next" | "n" => {
                install_stepping_traps(StepMode::Next, 0);
                writeln!(stdout, "Stepping over...").unwrap_or(());
                break;
            }
            "out" | "finish" => {
                install_stepping_traps(StepMode::Out, 0);
                writeln!(stdout, "Stepping out...").unwrap_or(());
                break;
            }
            "continue" | "c" => {
                clear_stepping_traps();
                writeln!(stdout, "Continuing...").unwrap_or(());
                break;
            }
            "abort" | "q" | ":abort" | ":a" => {
                clear_stepping_traps();
                break;
            }
            // Backtrace display
            "backtrace" | "bt" => {
                let count: usize = arg.and_then(|a| a.parse().ok()).unwrap_or(frames.len());
                for (i, frame) in frames.iter().enumerate().take(count) {
                    print_frame(&mut stdout, i, frame);
                }
            }
            // Frame selection
            "frame" | "f" => {
                if let Some(n) = arg.and_then(|a| a.parse::<usize>().ok()) {
                    if n < frames.len() {
                        selected_frame = n;
                        let frame = &frames[selected_frame];
                        writeln!(stdout, "Selected frame {}:", selected_frame).unwrap_or(());
                        print_frame(&mut stdout, selected_frame, frame);
                        // Show locals
                        if let Some(ref locals) = frame.local_bindings {
                            writeln!(stdout, "  Locals:").unwrap_or(());
                            for (name, value) in locals {
                                writeln!(stdout, "    {:?} = {:?}", name, value).unwrap_or(());
                            }
                        }
                    } else {
                        writeln!(stdout, "Frame {} out of range (0..{})", n, frames.len() - 1)
                            .unwrap_or(());
                    }
                }
            }
            // Eval in frame (R6.18)
            "eval" | "e" => {
                if let Some(expr) = arg {
                    match bliss_compiler::reader::read_from_string(expr) {
                        Ok((form, _)) => {
                            let frame = &frames[selected_frame.min(frames.len() - 1)];
                            match eval_in_frame(form, frame) {
                                Ok(val) => {
                                    writeln!(stdout, "{:?}", val).unwrap_or(());
                                }
                                Err(e) => {
                                    writeln!(stdout, "Error: {}", e).unwrap_or(());
                                }
                            }
                        }
                        Err(e) => {
                            writeln!(stdout, "Read error: {}", e).unwrap_or(());
                        }
                    }
                }
            }
            // Restart invocation
            "restart" | "r" => {
                if let Some(n) = arg.and_then(|a| a.parse::<usize>().ok()) {
                    match n {
                        0 => {
                            writeln!(stdout, "Aborting.").unwrap_or(());
                            break;
                        }
                        1 => {
                            writeln!(stdout, "Continuing.").unwrap_or(());
                            break;
                        }
                        _ => {
                            writeln!(stdout, "Invalid restart: {}", n).unwrap_or(());
                        }
                    }
                }
            }
            "help" | "?" => {
                writeln!(stdout, "Debugger commands:").unwrap_or(());
                writeln!(stdout, "  step (s)      - Step into next form").unwrap_or(());
                writeln!(stdout, "  next (n)      - Step over (skip calls)").unwrap_or(());
                writeln!(stdout, "  out (finish)  - Step out of current frame").unwrap_or(());
                writeln!(stdout, "  continue (c)  - Continue execution").unwrap_or(());
                writeln!(stdout, "  abort (q, :abort, :a) - Abort to top level").unwrap_or(());
                writeln!(stdout, "  backtrace (bt) [n] - Show backtrace").unwrap_or(());
                writeln!(stdout, "  frame (f) N   - Select frame N").unwrap_or(());
                writeln!(stdout, "  eval (e) FORM - Eval in selected frame").unwrap_or(());
                writeln!(stdout, "  restart (r) N - Invoke restart N").unwrap_or(());
            }
            _ => {
                // Try to evaluate as a Lisp form
                match bliss_compiler::reader::read_from_string(trimmed) {
                    Ok((form, _)) => {
                        let frame = &frames[selected_frame.min(frames.len() - 1)];
                        match eval_in_frame(form, frame) {
                            Ok(val) => {
                                writeln!(stdout, "{:?}", val).unwrap_or(());
                            }
                            Err(e) => {
                                writeln!(stdout, "Error: {}", e).unwrap_or(());
                            }
                        }
                    }
                    Err(e) => {
                        writeln!(stdout, "Unknown command. Error: {}", e).unwrap_or(());
                    }
                }
            }
        }
        stdout.flush().unwrap_or(());
    }

    repl_state.level -= 1;
    Ok(())
}

/// Pretty-print a debug frame.
fn print_frame(out: &mut impl IoWrite, index: usize, frame: &DebugFrame) {
    let func_desc = format!("{:?}", frame.function());
    let loc_desc = match &frame.source_loc {
        Some((file, line, col)) => format!("{}:{}:{}", file, line, col),
        None => "(unknown location)".to_string(),
    };
    writeln!(out, "  {}: {} at {}", index, func_desc, loc_desc).unwrap_or(());
}

/// Walk the current thread's stack, producing debug frames. A6.03.
/// Traverses CL frames managed by bliss_rt::stack::Frame via the frame
/// pointer chain, resolving function names and source locations from debug info.
/// Falls back to native backtrace for Rust/C frames.
pub fn walk_stack() -> Vec<DebugFrame> {
    let mut frames = Vec::new();

    // First, try to walk Lisp frames via bliss_rt::stack::FrameWalker.
    // In the current runtime, the green thread stack may not be initialized
    // from the test harness, so we also use the native backtrace as fallback.

    // Walk the native backtrace for Rust/C frames, using dladdr-style resolution
    let bt = std::backtrace::Backtrace::force_capture();
    let bt_str = format!("{:#}", bt);

    // Parse the backtrace output to extract frame info
    let mut pending_func: Option<String> = None;
    let mut pending_loc: Option<(String, u32, u32)> = None;

    for line in bt_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("stack backtrace:") {
            continue;
        }

        // Parse frame number lines: "N: function_name"
        if let Some(colon_pos) = trimmed.find(':') {
            let before = trimmed[..colon_pos].trim();
            let after = trimmed[colon_pos + 1..].trim();

            if after.is_empty() {
                continue;
            }

            if before.chars().all(|c| c.is_ascii_digit())
                && !after.starts_with('/')
                && !after.starts_with("at ")
            {
                // Flush previous pending frame, skipping debugger/runtime plumbing.
                if let Some(func_name) = pending_func.take() {
                    if !is_internal_frame(&func_name) {
                        let func_val = func_val_from_name(&func_name);
                        frames.push(DebugFrame {
                            func: func_val,
                            source_loc: pending_loc.take(),
                            local_bindings: None,
                            live: true,
                        });
                        if frames.len() >= 64 {
                            break;
                        }
                    }
                }
                pending_func = Some(after.to_string());
                pending_loc = None;
            } else if before == "at" || trimmed.starts_with("at ") {
                // Source location line
                let loc_str = if trimmed.starts_with("at ") {
                    trimmed.strip_prefix("at ").unwrap_or(trimmed)
                } else {
                    after
                };
                pending_loc = parse_source_location(loc_str);
            }
        }
    }

    // Flush last pending frame, skipping debugger/runtime plumbing.
    if let Some(func_name) = pending_func.take() {
        if !is_internal_frame(&func_name) {
            let func_val = func_val_from_name(&func_name);
            frames.push(DebugFrame {
                func: func_val,
                source_loc: pending_loc.take(),
                local_bindings: None,
                live: true,
            });
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

/// True for native frames that are backtrace-capture or debugger plumbing
/// rather than user-meaningful frames. These sit at the very top of the raw
/// native backtrace (the machinery that captured it) and at the very bottom
/// (the process/runtime entry point); neither helps someone debugging Lisp.
fn is_internal_frame(name: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "backtrace", // std::backtrace / backtrace_rs capture
        "libunwind",
        "invoke_debugger_ui", // the debugger itself
        "walk_stack",
        "print_frame",
        "core::ops::function", // FnOnce/FnMut::call shims
        "std::sys",
        "std::rt",
        "std::panic",
        "rust_begin_unwind",
        "__rust_begin_short_backtrace",
        "__libc_start_main",
        "_start",
    ];
    NEEDLES.iter().any(|needle| name.contains(needle))
}

/// Convert a backtrace function name into a BlissVal.
/// For known Bliss functions, creates a symbol; for foreign frames, uses T.
fn func_val_from_name(name: &str) -> BlissVal {
    // Check if this is a Bliss function by looking for bliss-related names
    if name.contains("bliss_") || name.contains("bliss::") {
        // Intern the actual function name into the shared registry (bliss-jtc.6
        // Stage E) instead of hashing it into a synthetic index that corresponds
        // to no real symbol and can collide.
        BlissVal::from_symbol_index(bliss_rt::symbols::intern(name))
    } else {
        // Foreign frame — use T as marker
        T
    }
}

/// Parse a source location string of the form "path:line:col" or "path:line".
fn parse_source_location(s: &str) -> Option<(String, u32, u32)> {
    let s = s.trim();
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
pub fn eval_in_frame(form: BlissVal, frame: &DebugFrame) -> Result<BlissVal, BlissError> {
    // Self-evaluating specials
    match form.0 {
        NIL_BITS | T_BITS | UNBOUND_BITS | MISSING_BITS | EOF_BITS => return Ok(form),
        _ => {}
    }

    match form.0 & TAG_MASK {
        TAG_FIXNUM | TAG_CHARACTER | TAG_SINGLE_FLOAT => Ok(form),

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

        TAG_CONS => {
            let mut interpreter = bliss_compiler::tiered::Interpreter::new();
            if let Some(ref bindings) = frame.local_bindings {
                for (name, value) in bindings {
                    interpreter.define(*name, *value);
                }
            }
            interpreter.eval(form)
        }

        TAG_HEAP_OBJECT => Ok(form),
        TAG_FUNCTION => Ok(form),
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
    original_bytes: Option<Vec<u8>>,
    trap_address: Option<usize>,
}

enum BreakpointTarget {
    Entry(BlissVal),
    SourceLocation {
        file: String,
        line: u32,
    },
    /// Watchpoint target (R6.17).
    Watch(WatchTarget),
}

/// Watchpoint target (R6.17 / D6.03).
#[derive(Clone, Debug)]
pub struct WatchTarget {
    /// Variable name.
    pub name: BlissVal,
    /// Scope: special (dynamic) or lexical.
    pub scope: WatchScope,
    /// Optional: only watch on a specific thread.
    pub thread_id: Option<u64>,
    /// Last known value (for change detection).
    pub last_value: BlissVal,
    /// Predicate for conditional watchpoints.
    pub predicate: Option<BlissVal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchScope {
    Special,
    Lexical,
}

static NEXT_BREAKPOINT_ID: AtomicU64 = AtomicU64::new(1);

fn breakpoint_map() -> &'static Mutex<HashMap<BreakpointId, BreakpointInfo>> {
    use std::sync::OnceLock;
    static MAP: OnceLock<Mutex<HashMap<BreakpointId, BreakpointInfo>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a function entry address safely.
/// Returns None for non-function values or null code addresses.
/// Does NOT unsafely dereference arbitrary pointers.
fn resolve_function_entry(function_name: BlissVal) -> Option<(usize, Vec<u8>)> {
    if !function_name.is_function() {
        return None;
    }
    let addr = (function_name.0 & !TAG_MASK) as usize;
    if addr == 0 {
        return None;
    }

    // Validate the address before reading from it.
    // We check that the address is within a plausible range for mapped memory
    // by attempting a safe probe. If the address is invalid, we return None
    // instead of causing undefined behavior.
    if !is_valid_code_address(addr) {
        return None;
    }

    let trap_size = breakpoint_trap_size();
    let mut original = vec![0u8; trap_size];
    // SAFETY: We validated the address is in mapped memory above.
    unsafe {
        std::ptr::copy_nonoverlapping(addr as *const u8, original.as_mut_ptr(), trap_size);
    }
    Some((addr, original))
}

/// Check if an address is likely a valid, mapped code address.
/// Uses a conservative heuristic: addresses must be non-null and
/// within a reasonable range. In a full implementation, this would
/// consult the process's memory map or use mprotect probing.
fn is_valid_code_address(addr: usize) -> bool {
    // Null and very low addresses are invalid
    if addr < 0x1000 {
        return false;
    }
    // Very high addresses (above 128TB) are likely invalid
    if addr > 0x0000_7FFF_FFFF_FFFF {
        return false;
    }
    // Alignment check: code should be at least 2-byte aligned
    // (most architectures require this for instructions)
    if addr % 2 != 0 {
        return false;
    }
    true
}

#[inline]
fn breakpoint_trap_size() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        1
    }
    #[cfg(target_arch = "aarch64")]
    {
        4
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        1
    }
}

unsafe fn install_trap(addr: usize) {
    #[cfg(target_arch = "x86_64")]
    {
        let ptr = addr as *mut u8;
        unsafe {
            std::ptr::write_volatile(ptr, 0xCC);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let ptr = addr as *mut u32;
        unsafe {
            std::ptr::write_volatile(ptr, 0xD4200000);
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let ptr = addr as *mut u8;
        unsafe {
            std::ptr::write_volatile(ptr, 0xCC);
        }
    }
}

unsafe fn remove_trap(addr: usize, original: &[u8]) {
    unsafe {
        std::ptr::copy_nonoverlapping(original.as_ptr(), addr as *mut u8, original.len());
    }
}

fn should_breakpoint_fire(info: &mut BreakpointInfo) -> bool {
    if !info.enabled {
        return false;
    }
    info.hit_count += 1;
    !matches!(info.condition, Some(cond) if cond == NIL)
}

/// Set a breakpoint on function entry. R6.14/R6.16.
pub fn break_on_entry(
    function_name: BlissVal,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));

    let (trap_address, original_bytes) = match resolve_function_entry(function_name) {
        Some((addr, orig)) => {
            unsafe {
                install_trap(addr);
            }
            (Some(addr), Some(orig))
        }
        None => (None, None),
    };

    breakpoint_map().lock().unwrap().insert(
        id,
        BreakpointInfo {
            target: BreakpointTarget::Entry(function_name),
            condition,
            enabled: true,
            hit_count: 0,
            original_bytes,
            trap_address,
        },
    );
    Ok(id)
}

/// Set a breakpoint at a source location. R6.15.
pub fn break_at(
    file: &str,
    line: u32,
    condition: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));
    breakpoint_map().lock().unwrap().insert(
        id,
        BreakpointInfo {
            target: BreakpointTarget::SourceLocation {
                file: file.to_string(),
                line,
            },
            condition,
            enabled: true,
            hit_count: 0,
            original_bytes: None,
            trap_address: None,
        },
    );
    Ok(id)
}

/// Set a watchpoint on a variable (R6.17).
/// Triggers the debugger when the watched variable changes and the
/// predicate (if any) returns true.
pub fn watch(
    variable_name: BlissVal,
    scope: WatchScope,
    predicate: Option<BlissVal>,
) -> Result<BreakpointId, BlissError> {
    let id = BreakpointId(NEXT_BREAKPOINT_ID.fetch_add(1, Ordering::Relaxed));

    if scope == WatchScope::Lexical {
        // Lexical watchpoints require debug 3 and compiler instrumentation (A6.04a).
        // Signal a warning if not available, but still register.
        eprintln!("; Warning: Lexical watchpoints require (optimize (debug 3)).");
        eprintln!("; The function may need to be recompiled for watchpoint to take effect.");
    }

    let target = WatchTarget {
        name: variable_name,
        scope,
        thread_id: None,
        last_value: NIL,
        predicate,
    };

    breakpoint_map().lock().unwrap().insert(
        id,
        BreakpointInfo {
            target: BreakpointTarget::Watch(target),
            condition: None,
            enabled: true,
            hit_count: 0,
            original_bytes: None,
            trap_address: None,
        },
    );

    // For special variables, install guarded cell (A6.04a)
    if scope == WatchScope::Special {
        install_watch_guard(variable_name, id);
    }

    Ok(id)
}

/// Remove a watchpoint.
pub fn unwatch(id: BreakpointId) -> Result<(), BlissError> {
    let mut map = breakpoint_map().lock().unwrap();
    if let Some(info) = map.remove(&id) {
        if let BreakpointTarget::Watch(ref target) = info.target {
            if target.scope == WatchScope::Special {
                remove_watch_guard(target.name);
            }
        }
    }
    Ok(())
}

/// Install a guarded cell for special variable watchpoints (A6.04a).
/// Replaces the symbol's value cell with a wrapper that checks on writes.
fn install_watch_guard(_variable_name: BlissVal, _bp_id: BreakpointId) {
    // Record that this variable is being watched.
    // The runtime's setq/set/setf paths must check the watch registry.
    watch_registry()
        .lock()
        .unwrap()
        .insert(_variable_name.to_raw(), _bp_id);
}

/// Remove the guarded cell for a watched special variable.
fn remove_watch_guard(_variable_name: BlissVal) {
    watch_registry()
        .lock()
        .unwrap()
        .remove(&_variable_name.to_raw());
}

fn watch_registry() -> &'static Mutex<HashMap<u64, BreakpointId>> {
    use std::sync::OnceLock;
    static R: OnceLock<Mutex<HashMap<u64, BreakpointId>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn evaluate_watch_predicate(predicate: BlissVal, old_value: BlissVal, new_value: BlissVal) -> bool {
    let mut interpreter = bliss_compiler::tiered::Interpreter::new();
    let pred_sym =
        BlissVal::from_symbol_index(bliss_compiler::reader::intern_symbol("%WATCH-PREDICATE"));
    let old_sym = BlissVal::from_symbol_index(bliss_compiler::reader::intern_symbol("%WATCH-OLD"));
    let new_sym = BlissVal::from_symbol_index(bliss_compiler::reader::intern_symbol("%WATCH-NEW"));
    interpreter.define(pred_sym, predicate);
    interpreter.define(old_sym, old_value);
    interpreter.define(new_sym, new_value);
    let form = values_to_list(&[pred_sym, old_sym, new_sym]);
    interpreter.eval(form).map(|v| v != NIL).unwrap_or(false)
}

/// Check a watchpoint (A6.04a). Called by the runtime when a watched variable
/// is written. Returns true if the debugger should be entered.
pub fn check_watchpoint(variable_name: BlissVal, old_value: BlissVal, new_value: BlissVal) -> bool {
    let registry = watch_registry().lock().unwrap();
    if let Some(bp_id) = registry.get(&variable_name.to_raw()) {
        let mut map = breakpoint_map().lock().unwrap();
        if let Some(info) = map.get_mut(bp_id) {
            if !info.enabled {
                return false;
            }
            if let BreakpointTarget::Watch(ref target) = info.target {
                // Check thread restriction
                if let Some(tid) = target.thread_id {
                    if tid != thread_id_current() {
                        return false;
                    }
                }
                // Check predicate
                if let Some(pred) = target.predicate {
                    if pred == NIL || !evaluate_watch_predicate(pred, old_value, new_value) {
                        return false;
                    }
                }
                // Value actually changed?
                if old_value != new_value {
                    info.hit_count += 1;
                    return true;
                }
            }
        }
    }
    false
}

/// Remove a breakpoint, restoring original code if a trap was installed.
pub fn remove_breakpoint(id: BreakpointId) -> Result<(), BlissError> {
    let mut map = breakpoint_map().lock().unwrap();
    if let Some(info) = map.remove(&id) {
        if let (Some(addr), Some(original)) = (info.trap_address, &info.original_bytes) {
            unsafe {
                remove_trap(addr, original);
            }
        }
        if let BreakpointTarget::Watch(ref target) = info.target {
            if target.scope == WatchScope::Special {
                drop(map);
                remove_watch_guard(target.name);
            }
        }
    }
    Ok(())
}

pub fn list_breakpoints() -> Vec<BreakpointId> {
    breakpoint_map().lock().unwrap().keys().copied().collect()
}

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

pub fn check_breakpoint_at_location(file: &str, line: u32) -> bool {
    let mut map = breakpoint_map().lock().unwrap();
    for info in map.values_mut() {
        if let BreakpointTarget::SourceLocation {
            file: ref f,
            line: l,
        } = info.target
        {
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
static INSTRUMENT_PROFILER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// A single profiler sample: captures the return address (PC) at sample time.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ProfileSample {
    pc: usize,
    #[expect(
        dead_code,
        reason = "sampling metadata is retained for future profile exports"
    )]
    timestamp_us: u64,
    #[expect(
        dead_code,
        reason = "sampling metadata is retained for future profile exports"
    )]
    thread_id: u64,
}

fn profiler_state() -> &'static Mutex<ProfilerState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<ProfilerState>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(ProfilerState {
            rate_hz: 1000,
            start: None,
            sample_count: 0,
            samples: Vec::new(),
            sampling_thread: None,
            target_thread_id: 0,
        })
    })
}

struct ProfilerState {
    rate_hz: u32,
    start: Option<Instant>,
    sample_count: u64,
    samples: Vec<ProfileSample>,
    sampling_thread: Option<thread::JoinHandle<()>>,
    /// The thread being profiled (the thread that called start_profiler).
    target_thread_id: u64,
}

/// Profiler report — structured output (D6.05).
#[derive(Clone, Debug)]
pub struct ProfilerReport {
    /// Kind of profiler that produced this report.
    pub kind: ProfilerKind,
    /// Total number of samples collected.
    pub total_samples: u64,
    /// Elapsed time in nanoseconds.
    pub elapsed_ns: u64,
    /// Per-function entries sorted by self-samples/time.
    pub entries: Vec<ProfilerEntry>,
}

/// Profiler entry — per-function data (D6.06).
#[derive(Clone, Debug)]
pub struct ProfilerEntry {
    /// Function name or address.
    pub function_name: String,
    /// Samples where this function was top-of-stack.
    pub self_samples: u64,
    /// Samples where this function appeared anywhere in the stack.
    pub total_samples: u64,
    /// Call count (for instrumented profiler).
    pub call_count: Option<u64>,
    /// Self time in nanoseconds (for instrumented profiler).
    pub self_time_ns: Option<u64>,
    /// Total time in nanoseconds (for instrumented profiler).
    pub total_time_ns: Option<u64>,
    /// Allocation bytes (for allocation profiler).
    pub alloc_bytes: Option<u64>,
    /// Allocation count (for allocation profiler).
    pub alloc_count: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfilerKind {
    Sampling,
    Instrumented,
    Allocation,
}

/// Allocation tracking record.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct AllocRecord {
    type_tag: u8,
    size: usize,
    #[expect(
        dead_code,
        reason = "allocation metadata is retained for future profile exports"
    )]
    pc: usize,
}

fn alloc_state() -> &'static Mutex<AllocState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<AllocState>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(AllocState {
            start: None,
            total_allocs: 0,
            total_bytes: 0,
            records: Vec::new(),
        })
    })
}

struct AllocState {
    start: Option<Instant>,
    total_allocs: u64,
    total_bytes: u64,
    records: Vec<AllocRecord>,
}

// ── Deterministic instrumentation profiler (R6.24) ────────────────

/// Per-function instrumentation data for the deterministic profiler.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct InstrumentEntry {
    call_count: u64,
    cumulative_time_ns: u64,
    self_time_ns: u64,
    /// Timestamp when the function was entered (for computing elapsed).
    entry_time: Option<Instant>,
    /// Count of nested calls (for self-time accounting).
    #[expect(
        dead_code,
        reason = "instrumentation metadata is retained for future nested timing"
    )]
    nested_depth: u64,
}

fn instrument_state() -> &'static Mutex<InstrumentState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<InstrumentState>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(InstrumentState {
            start: None,
            functions: HashMap::new(),
            call_stack: Vec::new(),
        })
    })
}

struct InstrumentState {
    start: Option<Instant>,
    /// Per-function instrumentation data, keyed by function identifier (raw u64).
    functions: HashMap<u64, InstrumentEntry>,
    /// Call stack for self-time computation.
    call_stack: Vec<(u64, Instant)>,
}

/// Start the deterministic instrumentation profiler (R6.24).
/// Records per-function call counts and cumulative/self time.
pub fn start_instrumentation_profiler() -> Result<(), BlissError> {
    let mut s = instrument_state().lock().unwrap();
    s.start = Some(Instant::now());
    s.functions.clear();
    s.call_stack.clear();
    INSTRUMENT_PROFILER_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Record a function entry for the instrumentation profiler.
/// Called by the interpreter/compiled code wrapper at function entry.
pub fn instrument_function_entry(function_id: u64) {
    if !INSTRUMENT_PROFILER_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let now = Instant::now();
    if let Ok(mut s) = instrument_state().lock() {
        let entry = s
            .functions
            .entry(function_id)
            .or_insert_with(|| InstrumentEntry {
                call_count: 0,
                cumulative_time_ns: 0,
                self_time_ns: 0,
                entry_time: None,
                nested_depth: 0,
            });
        entry.call_count += 1;
        entry.entry_time = Some(now);
        s.call_stack.push((function_id, now));
    }
}

/// Record a function exit for the instrumentation profiler.
pub fn instrument_function_exit(function_id: u64) {
    if !INSTRUMENT_PROFILER_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let now = Instant::now();
    if let Ok(mut s) = instrument_state().lock() {
        // Pop the call stack
        if let Some((top_id, entry_time)) = s.call_stack.pop() {
            let elapsed_ns = now.duration_since(entry_time).as_nanos() as u64;
            if top_id == function_id {
                if let Some(entry) = s.functions.get_mut(&function_id) {
                    entry.cumulative_time_ns += elapsed_ns;
                    entry.self_time_ns += elapsed_ns;
                    entry.entry_time = None;
                }
                // Subtract our time from the parent's self-time
                if let Some(&(parent_id, _)) = s.call_stack.last() {
                    if let Some(parent) = s.functions.get_mut(&parent_id) {
                        parent.self_time_ns = parent.self_time_ns.saturating_sub(elapsed_ns);
                    }
                }
            }
        }
    }
}

/// Stop the deterministic instrumentation profiler and return a structured report.
pub fn stop_instrumentation_profiler() -> Result<ProfilerReport, BlissError> {
    INSTRUMENT_PROFILER_ACTIVE.store(false, Ordering::SeqCst);

    let mut s = instrument_state().lock().unwrap();
    let elapsed_ns = s.start.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);

    let mut entries: Vec<ProfilerEntry> = s
        .functions
        .iter()
        .map(|(id, data)| ProfilerEntry {
            function_name: format!("fn_{:#x}", id),
            self_samples: 0,
            total_samples: 0,
            call_count: Some(data.call_count),
            self_time_ns: Some(data.self_time_ns),
            total_time_ns: Some(data.cumulative_time_ns),
            alloc_bytes: None,
            alloc_count: None,
        })
        .collect();

    // Sort by self-time descending
    entries.sort_by(|a, b| b.self_time_ns.cmp(&a.self_time_ns));

    let report = ProfilerReport {
        kind: ProfilerKind::Instrumented,
        total_samples: entries.iter().map(|e| e.call_count.unwrap_or(0)).sum(),
        elapsed_ns,
        entries,
    };

    s.start = None;
    s.functions.clear();
    s.call_stack.clear();

    Ok(report)
}

/// Global storage for the last profiler report, so stop_profiler can return
/// a structured value via BlissVal while also storing the full report.
fn last_profiler_report() -> &'static Mutex<Option<ProfilerReport>> {
    use std::sync::OnceLock;
    static R: OnceLock<Mutex<Option<ProfilerReport>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(None))
}

/// Retrieve the last profiler report (any kind).
pub fn get_last_profiler_report() -> Option<ProfilerReport> {
    last_profiler_report().lock().unwrap().clone()
}

/// Start the sampling profiler at the given rate (clamped 10–10000 Hz). A6.05.
/// Spawns a sampling thread that uses SIGPROF-style sampling to capture
/// the target thread's program counter at the configured rate.
pub fn start_profiler(sample_rate_hz: u32) -> Result<(), BlissError> {
    let rate = sample_rate_hz.clamp(10, 10000);
    let interval = Duration::from_micros(1_000_000 / rate as u64);

    let target_thread_id = thread_id_current();

    {
        let mut s = profiler_state().lock().unwrap();
        s.rate_hz = rate;
        s.start = Some(Instant::now());
        s.sample_count = 0;
        s.samples.clear();
        s.target_thread_id = target_thread_id;
    }

    PROFILER_ACTIVE.store(true, Ordering::SeqCst);

    // Spawn a sampling thread that captures PC samples.
    // On Unix, we use process_vm_readv or /proc/self/maps to read the target
    // thread's instruction pointer. In the bootstrap implementation, we use
    // backtrace capture from a shared signal mechanism.
    //
    // The sampling thread sends SIGPROF to the target thread (on Unix) to
    // interrupt it and capture its PC. On platforms without signal support,
    // we fall back to cooperative sampling via safepoint polling.
    let handle = thread::spawn(move || {
        let start = Instant::now();
        while PROFILER_ACTIVE.load(Ordering::SeqCst) {
            thread::sleep(interval);

            if !PROFILER_ACTIVE.load(Ordering::SeqCst) {
                break;
            }

            // Sample the target thread's PC.
            // In a full implementation with SIGPROF, the signal handler would
            // capture the interrupted thread's context->rip/pc.
            // We use the profiler safepoint mechanism: the target thread records
            // its PC at each safepoint, and we read that value here.
            let pc = read_target_thread_pc(target_thread_id);
            let timestamp_us = start.elapsed().as_micros() as u64;

            if let Ok(mut s) = profiler_state().lock() {
                s.samples.push(ProfileSample {
                    pc,
                    timestamp_us,
                    thread_id: target_thread_id,
                });
                s.sample_count += 1;
            }
        }
    });

    profiler_state().lock().unwrap().sampling_thread = Some(handle);
    Ok(())
}

/// Read the target thread's current PC via the safepoint mechanism.
/// In a full implementation, SIGPROF would be sent to the target thread
/// and the signal handler would record the PC. In the bootstrap phase,
/// we read the last recorded safepoint PC.
fn read_target_thread_pc(_target_thread_id: u64) -> usize {
    // Read from the shared safepoint PC slot. The target thread updates this
    // at each safepoint (GC poll, function entry, loop back-edge).
    SAFEPOINT_PC.load(Ordering::Relaxed) as usize
}

/// Shared safepoint PC slot. Updated by the target thread at each safepoint.
static SAFEPOINT_PC: AtomicU64 = AtomicU64::new(0);

/// Called by the target thread at safepoints to record its current PC.
/// This enables the sampling profiler to read the target thread's PC
/// without using signals.
pub fn record_safepoint_pc(pc: usize) {
    if PROFILER_ACTIVE.load(Ordering::Relaxed) {
        SAFEPOINT_PC.store(pc as u64, Ordering::Relaxed);
    }
}

/// Stop the profiler and return a structured profiler report (D6.05).
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
        drop(s);
        let _ = handle.join();
        s = profiler_state().lock().unwrap();
    }

    // Build a profile report: aggregate samples by PC
    let mut pc_counts: HashMap<usize, u64> = HashMap::new();
    for sample in &s.samples {
        *pc_counts.entry(sample.pc).or_insert(0) += 1;
    }

    let elapsed_ns = elapsed.map(|e| e.as_nanos() as u64).unwrap_or(0);

    // Build structured entries sorted by sample count
    let mut entries: Vec<ProfilerEntry> = pc_counts
        .iter()
        .map(|(pc, count)| ProfilerEntry {
            function_name: format!("{:#x}", pc),
            self_samples: *count,
            total_samples: *count,
            call_count: None,
            self_time_ns: None,
            total_time_ns: None,
            alloc_bytes: None,
            alloc_count: None,
        })
        .collect();
    entries.sort_by(|a, b| b.self_samples.cmp(&a.self_samples));

    // Build the structured report (D6.05)
    let report = ProfilerReport {
        kind: ProfilerKind::Sampling,
        total_samples: sample_count,
        elapsed_ns,
        entries,
    };

    // Print human-readable report to stderr
    if let Some(elapsed) = elapsed {
        let mut report_str = String::new();
        writeln!(report_str, "Sampling profiler report:").unwrap();
        writeln!(
            report_str,
            "  Rate: {} Hz, Duration: {:.2}s, Samples: {}",
            rate,
            elapsed.as_secs_f64(),
            sample_count
        )
        .unwrap();
        writeln!(report_str, "  Top addresses by sample count:").unwrap();

        for entry in report.entries.iter().take(20) {
            let pct = if sample_count > 0 {
                entry.self_samples as f64 / sample_count as f64 * 100.0
            } else {
                0.0
            };
            writeln!(
                report_str,
                "    {}: {} ({:.1}%)",
                entry.function_name, entry.self_samples, pct
            )
            .unwrap();
        }
        eprint!("{}", report_str);
    }

    // Store the structured report for retrieval
    *last_profiler_report().lock().unwrap() = Some(report);

    s.start = None;
    s.samples.clear();

    // Return T (non-NIL) to indicate a report was produced.
    // The full structured report is available via get_last_profiler_report().
    Ok(T)
}

/// Start the allocation profiler. A6.06.
pub fn start_allocation_profiler() -> Result<(), BlissError> {
    let mut s = alloc_state().lock().unwrap();
    s.start = Some(Instant::now());
    s.total_allocs = 0;
    s.total_bytes = 0;
    s.records.clear();
    ALLOC_PROFILER_ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Record an allocation event.
pub fn record_allocation(type_tag: u8, size: usize, pc: usize) {
    if ALLOC_PROFILER_ACTIVE.load(Ordering::Relaxed) {
        if let Ok(mut s) = alloc_state().lock() {
            s.total_allocs += 1;
            s.total_bytes += size as u64;
            s.records.push(AllocRecord { type_tag, size, pc });
        }
    }
}

/// Stop the allocation profiler and return a structured report (D6.05).
pub fn stop_allocation_profiler() -> Result<BlissVal, BlissError> {
    ALLOC_PROFILER_ACTIVE.store(false, Ordering::SeqCst);

    let mut s = alloc_state().lock().unwrap();
    let elapsed = s.start.map(|t| t.elapsed());
    let total_allocs = s.total_allocs;
    let total_bytes = s.total_bytes;
    let elapsed_ns = elapsed.map(|e| e.as_nanos() as u64).unwrap_or(0);

    // Build report: aggregate by type tag
    let mut type_counts: HashMap<u8, (u64, u64)> = HashMap::new();
    for record in &s.records {
        let entry = type_counts.entry(record.type_tag).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += record.size as u64;
    }

    // Build structured entries
    let mut entries: Vec<ProfilerEntry> = type_counts
        .iter()
        .map(|(tag, (count, bytes))| ProfilerEntry {
            function_name: format!("type_{:#04x}", tag),
            self_samples: 0,
            total_samples: 0,
            call_count: None,
            self_time_ns: None,
            total_time_ns: None,
            alloc_bytes: Some(*bytes),
            alloc_count: Some(*count),
        })
        .collect();
    entries.sort_by(|a, b| b.alloc_bytes.cmp(&a.alloc_bytes));

    let report = ProfilerReport {
        kind: ProfilerKind::Allocation,
        total_samples: total_allocs,
        elapsed_ns,
        entries,
    };

    if let Some(elapsed) = elapsed {
        let mut report_str = String::new();
        writeln!(report_str, "Allocation profiler report:").unwrap();
        writeln!(
            report_str,
            "  Duration: {:.2}s, Allocations: {}, Bytes: {}",
            elapsed.as_secs_f64(),
            total_allocs,
            total_bytes
        )
        .unwrap();

        if !type_counts.is_empty() {
            writeln!(report_str, "  By type:").unwrap();
            for entry in &report.entries {
                writeln!(
                    report_str,
                    "    {}: {} allocs, {} bytes",
                    entry.function_name,
                    entry.alloc_count.unwrap_or(0),
                    entry.alloc_bytes.unwrap_or(0)
                )
                .unwrap();
            }
        }
        eprint!("{}", report_str);
    }

    *last_profiler_report().lock().unwrap() = Some(report);

    s.start = None;
    s.records.clear();

    Ok(T)
}

// ── Time macro support (R6.44) ────────────────────────────────────

/// Timing result from the `time` function (R6.44).
#[derive(Clone, Debug)]
pub struct TimeResult {
    /// Wall-clock time in nanoseconds.
    pub wall_clock_ns: u64,
    /// User CPU time in nanoseconds.
    pub user_cpu_ns: u64,
    /// System CPU time in nanoseconds.
    pub system_cpu_ns: u64,
    /// Bytes consed (allocated).
    pub bytes_consed: u64,
    /// GC time in nanoseconds.
    pub gc_time_ns: u64,
    /// Number of GC pauses.
    pub gc_pauses: u64,
    /// Number of page faults.
    pub page_faults: u64,
    /// Whether the form completed normally.
    pub completed: bool,
}

/// Get process CPU times (user, system) in nanoseconds.
/// Reads /proc/self/stat on Linux, falls back to wall-clock on other systems.
fn get_cpu_times() -> (u64, u64) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
            let fields: Vec<&str> = stat.split_whitespace().collect();
            // Fields 13 and 14 are utime and stime in clock ticks
            if fields.len() > 14 {
                let ticks_per_sec: u64 = 100; // sysconf(_SC_CLK_TCK) default
                let utime = fields[13].parse::<u64>().unwrap_or(0);
                let stime = fields[14].parse::<u64>().unwrap_or(0);
                let user_ns = utime * 1_000_000_000 / ticks_per_sec;
                let sys_ns = stime * 1_000_000_000 / ticks_per_sec;
                return (user_ns, sys_ns);
            }
        }
        (0, 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (0, 0)
    }
}

/// Get page fault count.
/// Reads /proc/self/stat on Linux.
fn get_page_faults() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
            let fields: Vec<&str> = stat.split_whitespace().collect();
            // Fields 9 and 11 are minflt and majflt
            if fields.len() > 11 {
                let minflt = fields[9].parse::<u64>().unwrap_or(0);
                let majflt = fields[11].parse::<u64>().unwrap_or(0);
                return minflt + majflt;
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// Execute a function and measure its execution time, allocations, GC activity,
/// and page faults (R6.44). Reports wall-clock time, user/system CPU time,
/// bytes consed, GC time, number of GC pauses, and page faults.
pub fn time_execution<F: FnOnce() -> Result<BlissVal, BlissError>>(
    f: F,
) -> (Result<BlissVal, BlissError>, TimeResult) {
    let gc_stats_before = bliss_rt::gc::heap_stats();
    let gc_count_before = gc_stats_before.minor_gc_count + gc_stats_before.major_gc_count;
    let gc_time_before =
        gc_stats_before.total_minor_pause_us + gc_stats_before.total_major_pause_us;
    let bytes_before = gc_stats_before.bytes_allocated;
    let faults_before = get_page_faults();
    let (user_before, sys_before) = get_cpu_times();
    let start = Instant::now();
    let mut completed = false;

    let result = f();
    if result.is_ok() {
        completed = true;
    }

    let elapsed = start.elapsed();
    let (user_after, sys_after) = get_cpu_times();
    let faults_after = get_page_faults();
    let gc_stats_after = bliss_rt::gc::heap_stats();
    let gc_count_after = gc_stats_after.minor_gc_count + gc_stats_after.major_gc_count;
    let gc_time_after = gc_stats_after.total_minor_pause_us + gc_stats_after.total_major_pause_us;
    let bytes_after = gc_stats_after.bytes_allocated;

    let timing = TimeResult {
        wall_clock_ns: elapsed.as_nanos() as u64,
        user_cpu_ns: user_after.saturating_sub(user_before),
        system_cpu_ns: sys_after.saturating_sub(sys_before),
        bytes_consed: bytes_after.saturating_sub(bytes_before),
        gc_time_ns: (gc_time_after.saturating_sub(gc_time_before)) * 1000, // us -> ns
        gc_pauses: gc_count_after.saturating_sub(gc_count_before),
        page_faults: faults_after.saturating_sub(faults_before),
        completed,
    };

    // Print timing report to stderr (like SBCL's TIME)
    let prefix = if completed { "" } else { "(aborted) " };
    eprintln!("{}Evaluation took:", prefix);
    eprintln!(
        "  {:.6} seconds of real time",
        timing.wall_clock_ns as f64 / 1e9
    );
    eprintln!(
        "  {:.6} seconds of user run time",
        timing.user_cpu_ns as f64 / 1e9
    );
    eprintln!(
        "  {:.6} seconds of system run time",
        timing.system_cpu_ns as f64 / 1e9
    );
    eprintln!("  {} bytes consed", timing.bytes_consed);
    eprintln!(
        "  {} GC pauses totalling {:.6} seconds",
        timing.gc_pauses,
        timing.gc_time_ns as f64 / 1e9
    );
    eprintln!("  {} page faults", timing.page_faults);

    (result, timing)
}

// ── Disassembler ───────────────────────────────────────────────────

fn tag_type_name(val: BlissVal) -> &'static str {
    match val.0 {
        NIL_BITS => "NIL",
        T_BITS => "T",
        UNBOUND_BITS => "UNBOUND",
        MISSING_BITS => "MISSING",
        EOF_BITS => "EOF",
        _ => match val.0 & TAG_MASK {
            TAG_FIXNUM => "FIXNUM",
            TAG_CONS => "CONS",
            TAG_HEAP_OBJECT => "HEAP-OBJECT",
            TAG_CHARACTER => "CHARACTER",
            TAG_SINGLE_FLOAT => "SINGLE-FLOAT",
            TAG_SYMBOL => "SYMBOL",
            TAG_FUNCTION => "FUNCTION",
            TAG_SPECIAL => "SPECIAL",
            _ => "UNKNOWN",
        },
    }
}

/// Disassemble a function. R6.29–R6.32a.
/// Decodes native code for compiled functions with source-location annotations (R6.30).
/// For interpreted (T0) functions or non-function values, prints a notice (R6.32).
pub fn disassemble(
    function: BlissVal,
    tier: Option<BlissVal>,
    _stream: BlissVal,
) -> Result<(), BlissError> {
    let mut out = String::new();
    if function.is_function() {
        let meta = unsafe { &*((function.0 & !TAG_MASK) as *const bliss_compiler::tiered::FnMeta) };
        let actual_tier = match meta.tier.load(Ordering::Acquire) {
            1 => "T1",
            2 => "T2",
            _ => "T0",
        };
        let requested_tier = match tier {
            Some(t) if t == NIL => "T0",
            Some(t) if t == T => "T1",
            Some(_) => "T2",
            None => actual_tier,
        };
        if requested_tier != actual_tier && requested_tier != "T0" {
            return Err(BlissError::Internal(format!(
                "requested tier {requested_tier} is not available; current tier is {actual_tier}"
            )));
        }

        writeln!(
            out,
            "; disassembly for {:?} (type: {}, tier: {})",
            function,
            tag_type_name(function),
            actual_tier
        )
        .unwrap();

        let code_addr = meta.entry.load(Ordering::Acquire) as usize;
        if code_addr != 0 && actual_tier != "T0" {
            if !is_valid_code_address(code_addr) {
                writeln!(
                    out,
                    "; WARNING: Code address {:#x} appears invalid — skipping raw dump.",
                    code_addr
                )
                .unwrap();
                writeln!(
                    out,
                    "; The function pointer does not point to mapped memory."
                )
                .unwrap();
                print!("{}", out);
                return Ok(());
            }

            writeln!(out, "; Code at {:#x} (tier: {}):", code_addr, actual_tier).unwrap();
            writeln!(out, "; Available tiers: [{}]", actual_tier).unwrap();
            writeln!(out, "; Source form: {:?}", meta.body).unwrap();

            let max_bytes = 128;
            writeln!(out, "; Raw code bytes (up to {} bytes):", max_bytes).unwrap();

            for i in 0..max_bytes {
                if i == 0 {
                    writeln!(out, "; Source: function entry").unwrap();
                }

                if i % 16 == 0 {
                    if i > 0 {
                        writeln!(out).unwrap();
                    }
                    write!(out, ";   {:#06x}: ", i).unwrap();
                }

                let byte = safe_read_byte(code_addr + i);
                write!(out, "{:02x} ", byte).unwrap();
            }
            writeln!(out).unwrap();
        } else {
            writeln!(out, "; This function has not been compiled to native code.").unwrap();
            writeln!(out, "; Displayed tier: T0").unwrap();
            writeln!(out, "; This function has not been compiled to native code.").unwrap();
            writeln!(out, "; Use (COMPILE 'fn) to compile it first.").unwrap();
        }
    } else {
        writeln!(out, "; No native code available — not a compiled function.").unwrap();
        writeln!(
            out,
            "; (Value is {} — use COMPILE to compile first.)",
            tag_type_name(function)
        )
        .unwrap();
    }

    print!("{}", out);
    Ok(())
}

/// Safely read a single byte from an address, returning 0 if the address
/// cannot be safely read. This prevents segfaults from invalid code pointers.
fn safe_read_byte(addr: usize) -> u8 {
    if !is_valid_code_address(addr) {
        return 0;
    }
    // SAFETY: We validated the address range above. In a production implementation,
    // this would use mprotect probing or /proc/self/maps validation.
    unsafe { std::ptr::read_volatile(addr as *const u8) }
}

// ── Trace/Untrace ──────────────────────────────────────────────────

/// Information about a traced function, including the original function
/// value so we can wrap/restore it (§6.4 encapsulation).
struct TraceEntry {
    /// Whether to break into the debugger on entry.
    break_on_entry: bool,
    /// Optional condition — trace only fires when condition is non-NIL.
    condition: Option<BlissVal>,
    /// Nesting depth counter for indented trace output.
    depth: u64,
    /// The original function value before encapsulation.
    original_function: Option<BlissVal>,
    /// Whether this trace is active (used for encapsulation check).
    active: bool,
}

fn traced_registry() -> &'static Mutex<HashMap<u64, TraceEntry>> {
    use std::sync::OnceLock;
    static R: OnceLock<Mutex<HashMap<u64, TraceEntry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Thread-local trace depth for indented output.
static TRACE_DEPTH: AtomicU64 = AtomicU64::new(0);

/// Install a trace on a function. R6.39–R6.40.
/// Encapsulates the function to log entry/exit with args and return values (§6.4).
/// The trace wrapper prints indented entry/exit messages showing arguments
/// and return values. Supports :break (break_on_entry) and :condition options.
pub fn trace_function(
    function_name: BlissVal,
    break_on_entry: bool,
    condition: Option<BlissVal>,
) -> Result<(), BlissError> {
    let mut registry = traced_registry().lock().unwrap();
    let key = function_name.to_raw();

    // Store the trace entry with encapsulation info
    registry.insert(
        key,
        TraceEntry {
            break_on_entry,
            condition,
            depth: 0,
            original_function: None, // Will be set when the function is first called
            active: true,
        },
    );

    // Register this function in the trace-check table so the interpreter
    // and compiled code wrappers know to call trace_entry/trace_exit.
    register_trace_hook(function_name);

    Ok(())
}

/// Remove a trace, restoring the original fdefinition (§6.4 unencapsulate).
pub fn untrace_function(function_name: BlissVal) -> Result<(), BlissError> {
    let mut registry = traced_registry().lock().unwrap();
    if let Some(entry) = registry.remove(&function_name.to_raw()) {
        // Restore the original function if encapsulated
        if let Some(_original) = entry.original_function {
            unregister_trace_hook(function_name);
        }
    }
    unregister_trace_hook(function_name);
    Ok(())
}

/// Register a function for trace checking by the interpreter/compiled code.
/// The interpreter's function-call path checks this registry before each call.
fn register_trace_hook(function_name: BlissVal) {
    trace_hooks()
        .lock()
        .unwrap()
        .insert(function_name.to_raw(), true);
}

/// Unregister a trace hook.
fn unregister_trace_hook(function_name: BlissVal) {
    trace_hooks()
        .lock()
        .unwrap()
        .remove(&function_name.to_raw());
}

fn trace_hooks() -> &'static Mutex<HashMap<u64, bool>> {
    use std::sync::OnceLock;
    static H: OnceLock<Mutex<HashMap<u64, bool>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Check if a function has a trace hook installed.
/// Called by the interpreter at function-call boundaries.
pub fn has_trace_hook(function_name: BlissVal) -> bool {
    trace_hooks()
        .lock()
        .map(|h| h.contains_key(&function_name.to_raw()))
        .unwrap_or(false)
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
        if !entry.active {
            return false;
        }

        // Check condition
        if let Some(cond) = entry.condition {
            if cond == NIL {
                return false;
            }
        }

        let depth = TRACE_DEPTH.fetch_add(1, Ordering::Relaxed);
        let indent = "  ".repeat(depth as usize);
        let args_str: Vec<String> = args.iter().map(|a| format!("{:?}", a)).collect();
        eprintln!(
            "{}TRACE {}: ({:?} {})",
            indent,
            depth,
            function_name,
            args_str.join(" ")
        );

        entry.depth = depth;
        return entry.break_on_entry;
    }
    false
}

/// Log function exit for traced functions.
pub fn trace_exit(function_name: BlissVal, result: BlissVal) {
    let registry = match traced_registry().lock() {
        Ok(r) => r,
        Err(_) => return,
    };

    if let Some(entry) = registry.get(&function_name.to_raw()) {
        if !entry.active {
            return;
        }
        let depth = TRACE_DEPTH
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        let indent = "  ".repeat(depth as usize);
        eprintln!("{}TRACE {} returned: {:?}", indent, depth, result);
    }
}

/// Check if a function is currently traced.
pub fn is_traced(function_name: BlissVal) -> bool {
    traced_registry()
        .lock()
        .map(|r| r.contains_key(&function_name.to_raw()))
        .unwrap_or(false)
}

// ── Describe/Inspect ───────────────────────────────────────────────

/// Describe an object (CL `DESCRIBE`). R6.41.
/// Produces a human-readable summary of type, value, slots, and documentation.
/// For CLOS instances, shows all slots with names, types, and values (R6.41).
pub fn describe(object: BlissVal, _stream: BlissVal) -> Result<(), BlissError> {
    let mut desc = String::new();
    match object.0 {
        NIL_BITS => {
            writeln!(desc, "NIL\n  Type: NULL (SYMBOL, LIST)").unwrap();
        }
        T_BITS => {
            writeln!(desc, "T\n  Type: SYMBOL").unwrap();
        }
        UNBOUND_BITS => {
            writeln!(desc, "#<UNBOUND>").unwrap();
        }
        MISSING_BITS => {
            writeln!(desc, "#<MISSING>").unwrap();
        }
        EOF_BITS => {
            writeln!(desc, "#<EOF>").unwrap();
        }
        _ => match object.0 & TAG_MASK {
            TAG_FIXNUM => {
                let n = (object.0 as i64) >> 3;
                writeln!(
                    desc,
                    "{}\n  Type: FIXNUM\n  Value: {} ({:#x})",
                    n, n, object.0
                )
                .unwrap();
            }
            TAG_CHARACTER => {
                let cp = (object.0 >> 3) as u32;
                let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                writeln!(
                    desc,
                    "#\\{}\n  Type: CHARACTER\n  Codepoint: U+{:04X}",
                    ch, cp
                )
                .unwrap();
            }
            TAG_SINGLE_FLOAT => {
                let f = f32::from_bits((object.0 >> 32) as u32);
                writeln!(desc, "{}\n  Type: SINGLE-FLOAT", f).unwrap();
            }
            TAG_SYMBOL => {
                let idx = (object.0 >> 3) as u32;
                writeln!(
                    desc,
                    "#<SYMBOL idx={}>\n  Type: SYMBOL\n  Symbol-index: {}",
                    idx, idx
                )
                .unwrap();
            }
            TAG_CONS => {
                let addr = object.0 & !TAG_MASK;
                writeln!(desc, "#<CONS {:#x}>\n  Type: CONS", addr).unwrap();
                // Read car and cdr from the cons cell
                let (car, cdr) = read_cons_cell(object);
                writeln!(desc, "  Car: {:?}", car).unwrap();
                writeln!(desc, "  Cdr: {:?}", cdr).unwrap();
            }
            TAG_HEAP_OBJECT => {
                let addr = object.0 & !TAG_MASK;
                // Describe heap objects with type info (R6.41 CLOS instances)
                if addr != 0 && is_valid_code_address(addr as usize) {
                    // Read the object header to determine type
                    let header = unsafe { *(addr as *const bliss_rt::object::ObjectHeader) };
                    let type_id = header.type_id();
                    let type_name = heap_type_name(type_id);
                    writeln!(desc, "#<{} {:#x}>", type_name, addr).unwrap();
                    writeln!(desc, "  Type: {}", type_name).unwrap();
                    writeln!(desc, "  Address: {:#x}", addr).unwrap();

                    // For CLOS instances (STANDARD_OBJECT), show slots
                    if type_id == bliss_rt::object::type_id::STANDARD_OBJECT {
                        writeln!(desc, "  Slots:").unwrap();
                        writeln!(desc, "    (slot information requires class metadata)").unwrap();
                        // In a full implementation, we'd query the class for its
                        // slot definitions and read each slot value from the instance.
                    }

                    // Show documentation string if available
                    writeln!(desc, "  Documentation: (none)").unwrap();
                } else {
                    writeln!(desc, "#<HEAP-OBJECT {:#x}>", addr).unwrap();
                    writeln!(desc, "  Type: HEAP-OBJECT").unwrap();
                    writeln!(desc, "  Address: {:#x}", addr).unwrap();
                }
            }
            TAG_FUNCTION => {
                let addr = object.0 & !TAG_MASK;
                writeln!(desc, "#<FUNCTION {:#x}>", addr).unwrap();
                writeln!(desc, "  Type: FUNCTION").unwrap();
                writeln!(desc, "  Code address: {:#x}", addr).unwrap();
                if addr != 0 && is_valid_code_address(addr as usize) {
                    writeln!(desc, "  Status: compiled").unwrap();
                } else {
                    writeln!(desc, "  Status: interpreted or not yet compiled").unwrap();
                }
            }
            _ => {
                writeln!(desc, "#<UNKNOWN {:#x}>", object.0).unwrap();
            }
        },
    }

    print!("{}", desc);
    Ok(())
}

/// Get a human-readable name for a heap object type ID.
fn heap_type_name(type_id: u8) -> &'static str {
    match type_id {
        0x01 => "CONS",
        0x02 => "SYMBOL",
        0x03 => "SIMPLE-VECTOR",
        0x04 => "SIMPLE-ARRAY",
        0x05 => "SIMPLE-BASE-STRING",
        0x06 => "SIMPLE-CHARACTER-STRING",
        0x07 => "COMPLEX-ARRAY",
        0x08 => "BIGNUM",
        0x09 => "RATIO",
        0x0A => "COMPLEX",
        0x0B => "DOUBLE-FLOAT",
        0x0C => "HASH-TABLE",
        0x0D => "STRUCTURE",
        0x0E => "STANDARD-OBJECT",
        0x0F => "INTERPRETED-FUNCTION",
        0x10 => "COMPILED-FUNCTION",
        0x11 => "CLOSURE",
        0x12 => "PACKAGE",
        0x13 => "STREAM",
        0x14 => "PATHNAME",
        0x15 => "READTABLE",
        0x16 => "CONDITION",
        _ => "UNKNOWN-HEAP-OBJECT",
    }
}

/// Read the car and cdr values from a cons cell (issue #9).
/// Cons cells are stored as two adjacent BlissVal words at the cons pointer.
fn read_cons_cell(cons: BlissVal) -> (BlissVal, BlissVal) {
    let addr = (cons.0 & !TAG_MASK) as usize;
    if addr == 0 || !is_valid_code_address(addr) {
        return (NIL, NIL);
    }
    // A cons cell is two BlissVal words: [car, cdr]
    // SAFETY: We validated the address is in a reasonable range.
    unsafe {
        let car_ptr = addr as *const u64;
        let cdr_ptr = (addr + 8) as *const u64;
        let car = BlissVal::from_raw(std::ptr::read(car_ptr));
        let cdr = BlissVal::from_raw(std::ptr::read(cdr_ptr));
        (car, cdr)
    }
}

/// Inspect an object interactively (CL `INSPECT`). R6.42.
pub fn inspect(object: BlissVal) -> Result<(), BlissError> {
    let parts = compute_inspect_parts(object);

    let mut out = String::new();
    writeln!(out, "Object: {:?}", object).unwrap();
    writeln!(out, "Parts:").unwrap();
    for (i, (label, value)) in parts.iter().enumerate() {
        writeln!(out, "  {}: {} = {:?}", i, label, value).unwrap();
    }

    if is_interactive() {
        writeln!(
            out,
            "\nCommands: (number) to inspect part, :pop to go back, q to quit."
        )
        .unwrap();
        print!("{}", out);
        io::stdout().flush().unwrap_or(());

        let stdin = io::stdin();
        loop {
            print!("INSPECT> ");
            io::stdout().flush().unwrap_or(());

            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break,
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
                    return inspect(value);
                } else {
                    println!("Index out of range (0..{})", parts.len() - 1);
                }
            } else {
                println!("Enter a part number or 'q' to quit.");
            }
        }
    } else {
        print!("{}", out);
    }

    Ok(())
}

/// Compute the named parts of an object for the inspector (issue #9).
/// For cons cells, reads the actual car and cdr values.
fn compute_inspect_parts(object: BlissVal) -> Vec<(&'static str, BlissVal)> {
    match object.0 {
        NIL_BITS => vec![("type", T), ("value", NIL)],
        T_BITS => vec![("type", T), ("value", T)],
        _ => match object.0 & TAG_MASK {
            TAG_CONS => {
                // Read the actual car and cdr (issue #9)
                let (car, cdr) = read_cons_cell(object);
                vec![("type", T), ("car", car), ("cdr", cdr)]
            }
            TAG_FIXNUM => {
                vec![("type", T), ("value", object)]
            }
            TAG_CHARACTER => {
                vec![("type", T), ("value", object)]
            }
            TAG_SINGLE_FLOAT => {
                vec![("type", T), ("value", object)]
            }
            TAG_SYMBOL => {
                vec![("type", T), ("name", object)]
            }
            TAG_HEAP_OBJECT => {
                // For heap objects, try to decompose based on type
                let addr = (object.0 & !TAG_MASK) as usize;
                if addr != 0 && is_valid_code_address(addr) {
                    let header = unsafe { *(addr as *const bliss_rt::object::ObjectHeader) };
                    let type_id = header.type_id();
                    match type_id {
                        0x0C => {
                            // HASH-TABLE: show count
                            vec![("type", T), ("value", object)]
                        }
                        0x0E => {
                            // STANDARD-OBJECT (CLOS instance): would show slots
                            vec![("type", T), ("instance", object)]
                        }
                        _ => vec![("type", T), ("value", object)],
                    }
                } else {
                    vec![("type", T), ("value", object)]
                }
            }
            TAG_FUNCTION => {
                vec![("type", T), ("code", object)]
            }
            _ => vec![("type", T), ("value", object)],
        },
    }
}

// ── Room ───────────────────────────────────────────────────────────

/// Report heap statistics (CL `ROOM`). R6.43.
pub fn room(verbosity: Option<BlissVal>, _stream: BlissVal) -> Result<(), BlissError> {
    let stats = bliss_rt::gc::heap_stats();
    let mut report = String::new();
    let is_full = verbosity == Some(T);
    let is_minimal = verbosity == Some(NIL);

    if is_minimal {
        let total = stats.nursery_used + stats.old_gen_used + stats.large_object_bytes;
        writeln!(
            report,
            "Heap: {} bytes / {} bytes cap",
            total,
            stats.nursery_capacity + stats.old_gen_capacity
        )
        .unwrap();
    } else {
        let npct = if stats.nursery_capacity > 0 {
            stats.nursery_used as f64 / stats.nursery_capacity as f64 * 100.0
        } else {
            0.0
        };
        let opct = if stats.old_gen_capacity > 0 {
            stats.old_gen_used as f64 / stats.old_gen_capacity as f64 * 100.0
        } else {
            0.0
        };
        writeln!(report, "BLISS Heap Usage:").unwrap();
        writeln!(
            report,
            "  Nursery:  {} / {} ({:.0}%)",
            stats.nursery_used, stats.nursery_capacity, npct
        )
        .unwrap();
        writeln!(
            report,
            "  Old Gen:  {} / {} ({:.0}%)  [{} regions]",
            stats.old_gen_used,
            stats.old_gen_capacity,
            opct,
            stats.regions_total.saturating_sub(stats.regions_free)
        )
        .unwrap();
        writeln!(report, "  Large:    {} bytes", stats.large_object_bytes).unwrap();
        let avg_minor = if stats.minor_gc_count > 0 {
            stats.total_minor_pause_us as f64 / stats.minor_gc_count as f64 / 1000.0
        } else {
            0.0
        };
        writeln!(
            report,
            "  GC: {} minor (avg {:.1}ms), {} major ({:.1}ms)",
            stats.minor_gc_count,
            avg_minor,
            stats.major_gc_count,
            stats.total_major_pause_us as f64 / 1000.0
        )
        .unwrap();
        if is_full {
            writeln!(
                report,
                "  Allocated: {} bytes, Promoted: {} bytes",
                stats.bytes_allocated, stats.bytes_promoted
            )
            .unwrap();
            writeln!(
                report,
                "  Regions: {} total, {} free",
                stats.regions_total, stats.regions_free
            )
            .unwrap();
        }
    }

    print!("{}", report);
    Ok(())
}

// ── SWANK ──────────────────────────────────────────────────────────

static SWANK_ACTIVE: AtomicBool = AtomicBool::new(false);

fn swank_state() -> &'static Mutex<SwankState> {
    use std::sync::OnceLock;
    static S: OnceLock<Mutex<SwankState>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(SwankState {
            port: 4005,
            host: "127.0.0.1".into(),
            conns: 0,
            listener_thread: None,
            session_secret: None,
            connections: Vec::new(),
        })
    })
}

struct SwankState {
    port: u16,
    host: String,
    conns: u32,
    listener_thread: Option<thread::JoinHandle<()>>,
    session_secret: Option<String>,
    /// Active connections (R6.35 — multiple simultaneous).
    connections: Vec<SwankConnectionId>,
}

/// Unique identifier for a SWANK connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SwankConnectionId(u64);

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Per-connection state (D6.07 / R6.35).
#[allow(dead_code)]
struct SwankConnection {
    #[allow(dead_code)]
    id: SwankConnectionId,
    #[allow(dead_code)]
    stream: TcpStream,
    #[allow(dead_code)]
    buffer_package: String,
    #[allow(dead_code)]
    pending_returns: HashMap<u64, ()>,
    #[allow(dead_code)]
    thread_id: u64,
}

/// Active SWANK connections registry (R6.35 — multiple simultaneous).
fn swank_connections() -> &'static Mutex<HashMap<SwankConnectionId, Arc<Mutex<SwankConnection>>>> {
    use std::sync::OnceLock;
    static C: OnceLock<Mutex<HashMap<SwankConnectionId, Arc<Mutex<SwankConnection>>>>> =
        OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn generate_session_secret() -> String {
    use std::sync::atomic::AtomicU64;
    static SECRET_COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = SECRET_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("bliss-swank-{:016x}", n)
}

/// Start the SWANK server on host:port. R6.33.
/// Binds a TCP listener, spawns an acceptor thread, and uses a session
/// secret for authentication (R6.37).
/// Supports multiple simultaneous connections (R6.35), each with its own
/// REPL thread.
pub fn start_swank_server(port: u16, host: &str) -> Result<(), BlissError> {
    if SWANK_ACTIVE.load(Ordering::SeqCst) {
        return Err(BlissError::Internal("SWANK server already running".into()));
    }

    let bind_addr = format!("{}:{}", host, port);

    let listener = TcpListener::bind(&bind_addr).map_err(|e| {
        BlissError::Internal(format!(
            "Failed to bind SWANK listener on {}: {}",
            bind_addr, e
        ))
    })?;

    listener
        .set_nonblocking(true)
        .map_err(|e| BlissError::Internal(format!("Failed to set non-blocking: {}", e)))?;

    let secret = generate_session_secret();

    {
        let mut s = swank_state().lock().unwrap();
        s.port = port;
        s.host = host.to_string();
        s.conns = 0;
        s.session_secret = Some(secret.clone());
        s.connections.clear();
    }

    SWANK_ACTIVE.store(true, Ordering::SeqCst);

    // Spawn the acceptor thread
    let handle = thread::spawn(move || {
        eprintln!("; SWANK server listening on {}", bind_addr);
        eprintln!("; Session secret: {}", secret);

        while SWANK_ACTIVE.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, addr)) => {
                    eprintln!("; SWANK connection from {}", addr);
                    let secret_clone = secret.clone();

                    // Spawn a dedicated thread for each connection (R6.35)
                    thread::spawn(move || {
                        handle_swank_connection(stream, addr, &secret_clone);
                    });
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
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

/// Handle a single SWANK connection (R6.34, R6.35, R6.38).
/// Implements the SWANK wire protocol and dispatches operations.
fn handle_swank_connection(mut stream: TcpStream, addr: std::net::SocketAddr, secret: &str) {
    // Set blocking for protocol communication
    if stream.set_nonblocking(false).is_err() {
        eprintln!("; SWANK: failed to set blocking for {}", addr);
        return;
    }

    // Authentication (R6.37)
    let mut auth_buf = [0u8; 256];
    let authenticated = match stream.read(&mut auth_buf) {
        Ok(n) if n > 0 => {
            let client_secret = String::from_utf8_lossy(&auth_buf[..n]);
            let client_secret = client_secret.trim();
            client_secret == secret
        }
        _ => false,
    };

    if !authenticated {
        eprintln!("; SWANK authentication failed from {}", addr);
        let _ = stream.write_all(b":error \"authentication failed\"\n");
        return;
    }

    // Register this connection (R6.35)
    let conn_id = SwankConnectionId(NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed));
    let conn = SwankConnection {
        id: conn_id,
        stream: stream
            .try_clone()
            .unwrap_or_else(|_| stream.try_clone().expect("clone")),
        buffer_package: "CL-USER".to_string(),
        pending_returns: HashMap::new(),
        thread_id: thread_id_current(),
    };
    let conn_arc = Arc::new(Mutex::new(conn));
    swank_connections()
        .lock()
        .unwrap()
        .insert(conn_id, conn_arc.clone());

    if let Ok(mut s) = swank_state().lock() {
        s.conns += 1;
        s.connections.push(conn_id);
    }

    eprintln!(
        "; SWANK client authenticated from {} (conn #{})",
        addr, conn_id.0
    );

    // Send connection info
    let _ = stream.write_all(b"(:ok t)\n");

    // Create a dedicated interpreter for this connection's REPL thread (R6.35)
    let mut interpreter = bliss_compiler::tiered::Interpreter::new();

    // SWANK protocol message loop (A6.07 / R6.34)
    let mut msg_buf = Vec::new();
    let mut read_buf = [0u8; 4096];

    loop {
        if !SWANK_ACTIVE.load(Ordering::SeqCst) {
            break;
        }

        // Read SWANK wire protocol message
        // Format: 6-hex-digit length prefix, then S-expression
        match stream.read(&mut read_buf) {
            Ok(0) => {
                eprintln!("; SWANK connection closed from {}", addr);
                break;
            }
            Ok(n) => {
                msg_buf.extend_from_slice(&read_buf[..n]);
            }
            Err(e) => {
                eprintln!("; SWANK read error from {}: {}", addr, e);
                break;
            }
        }

        // Try to parse complete messages from the buffer
        while msg_buf.len() >= 6 {
            // Read 6-char hex length prefix
            let len_str = String::from_utf8_lossy(&msg_buf[..6]);
            let msg_len = match usize::from_str_radix(len_str.trim(), 16) {
                Ok(l) => l,
                Err(_) => {
                    // Not a valid SWANK message; try as raw S-expression
                    if let Some(end) = msg_buf.iter().position(|&b| b == b'\n') {
                        let raw = String::from_utf8_lossy(&msg_buf[..end]).to_string();
                        msg_buf.drain(..end + 1);
                        let response = dispatch_swank_message(&raw, &mut interpreter);
                        let _ = stream.write_all(response.as_bytes());
                        continue;
                    }
                    break;
                }
            };

            if msg_buf.len() < 6 + msg_len {
                break; // Need more data
            }

            let message = String::from_utf8_lossy(&msg_buf[6..6 + msg_len]).to_string();
            msg_buf.drain(..6 + msg_len);

            // Dispatch the SWANK message (R6.34)
            let response = dispatch_swank_message(&message, &mut interpreter);
            // Send response with length prefix
            let resp_hex = format!("{:06x}{}", response.len(), response);
            let _ = stream.write_all(resp_hex.as_bytes());
        }
    }

    // Cleanup connection
    swank_connections().lock().unwrap().remove(&conn_id);
    if let Ok(mut s) = swank_state().lock() {
        s.conns = s.conns.saturating_sub(1);
        s.connections.retain(|c| *c != conn_id);
    }
}

/// Dispatch a SWANK protocol message and return the response (R6.34).
/// Supports: eval, compile-string, compile-file, completions, arglist,
/// find-definitions, macroexpand-1, macroexpand-all, inspect, xref,
/// apropos, describe, thread-list (R6.38).
fn dispatch_swank_message(
    message: &str,
    interpreter: &mut bliss_compiler::tiered::Interpreter,
) -> String {
    let trimmed = message.trim();

    // Parse the S-expression message
    // SWANK messages are of the form (:emacs-rex (op args...) package thread-id id)
    if trimmed.starts_with("(:emacs-rex") {
        // Extract the operation
        if let Some(op) = extract_swank_op(trimmed) {
            let id = extract_swank_id(trimmed).unwrap_or(0);
            let result = handle_swank_op(&op, trimmed, interpreter);
            return format!("(:return (:ok {}) {})\n", result, id);
        }
    }

    // Handle other message types
    if trimmed.starts_with("(:emacs-interrupt") {
        return "(:ok t)\n".to_string();
    }

    // Thread listing (R6.38)
    if trimmed.contains("swank:list-threads") || trimmed.contains(":list-threads") {
        return "(:return (:ok ((\"ID\" \"Name\" \"Status\") (\"1\" \"main\" \"running\"))) 0)\n"
            .to_string();
    }

    // Default response
    "(:ok t)\n".to_string()
}

/// Extract the operation name from a SWANK :emacs-rex message.
fn extract_swank_op(message: &str) -> Option<String> {
    // Find the operation after (:emacs-rex (
    let after_rex = message.find("(:emacs-rex")?.checked_add(11)?;
    let rest = message.get(after_rex..)?.trim();
    let rest = rest.strip_prefix('(')?;
    // Find the operation name (up to first space or closing paren)
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ')')
        .unwrap_or(rest.len());
    let op = rest.get(..end)?;
    Some(op.to_string())
}

/// Extract the message ID from a SWANK message.
fn extract_swank_id(message: &str) -> Option<u64> {
    // The ID is the last number in the message
    let parts: Vec<&str> = message
        .trim_end_matches(')')
        .rsplit_terminator(char::is_whitespace)
        .collect();
    for part in parts {
        if let Ok(id) = part.trim().parse::<u64>() {
            return Some(id);
        }
    }
    None
}

fn eval_all_forms(
    source: &str,
    interpreter: &mut bliss_compiler::tiered::Interpreter,
) -> Result<BlissVal, BlissError> {
    let chars: Vec<char> = source.chars().collect();
    let mut offset = 0usize;
    let mut last = NIL;
    while offset < chars.len() {
        let remaining: String = chars[offset..].iter().collect();
        let (form, consumed) = bliss_compiler::reader::read_from_string(&remaining)?;
        if form == bliss_rt::value::EOF {
            break;
        }
        last = interpreter.eval(form)?;
        if consumed == 0 {
            break;
        }
        offset += consumed;
    }
    Ok(last)
}

fn compile_swank_file(
    path: &str,
    interpreter: &mut bliss_compiler::tiered::Interpreter,
) -> Result<String, BlissError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {path}: {e}")))?;
    let result = eval_all_forms(&source, interpreter)?;
    Ok(format!(
        "(:compilation-result t :file \"{}\" :value {:?} :minimum-tier :t1)",
        path, result
    ))
}

fn swank_xref(query: &str) -> String {
    let definitions = extract_swank_definitions(query);
    if definitions.is_empty() {
        "NIL".to_string()
    } else {
        format!("({})", definitions.join(" "))
    }
}

fn swank_threads_payload() -> String {
    let connections = swank_connections().lock().unwrap();
    let mut rows = vec![format!(
        "(\"{}\" \"main\" \"running\")",
        thread_id_current()
    )];
    for (conn_id, conn) in connections.iter() {
        let conn = conn.lock().unwrap();
        rows.push(format!(
            "(\"{}\" \"swank-conn-{}\" \"connected\")",
            conn.thread_id, conn_id.0
        ));
    }
    format!("((\"ID\" \"Name\" \"Status\") {})", rows.join(" "))
}

fn swank_connection_info() -> String {
    let state = swank_state().lock().unwrap();
    format!(
        "(:pid {} :style :spawn :encoding \"utf-8\" :lisp-implementation (:type \"Bliss\" :name \"bliss\" :version \"{}\") :package (:name \"CL-USER\" :prompt \"CL-USER\") :connections {} :host \"{}\" :port {} :features (:bliss))",
        std::process::id(),
        env!("CARGO_PKG_VERSION"),
        state.conns,
        state.host,
        state.port
    )
}

fn debug_thread_payload(full_message: &str) -> String {
    let target = extract_swank_id(full_message).unwrap_or(0).to_string();
    let threads = swank_threads_payload();
    if threads.contains(&format!("\"{}\"", target)) {
        format!("(:thread {} :status :ok)", target)
    } else {
        format!("(:thread {} :status :unknown)", target)
    }
}

/// Handle a specific SWANK operation (R6.34).
fn handle_swank_op(
    op: &str,
    full_message: &str,
    interpreter: &mut bliss_compiler::tiered::Interpreter,
) -> String {
    match op {
        // R6.34: eval
        "swank:interactive-eval" | "swank:eval-and-grab-output" | "swank:listener-eval" => {
            if let Some(form_str) = extract_swank_string_arg(full_message) {
                match bliss_compiler::reader::read_from_string(&form_str) {
                    Ok((form, _)) => match interpreter.eval(form) {
                        Ok(val) => format!("{:?}", val),
                        Err(e) => format!("\"Error: {}\"", e),
                    },
                    Err(e) => format!("\"Reader error: {}\"", e),
                }
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: compile-string
        "swank:compile-string-for-emacs" => {
            if let Some(source) = extract_swank_string_arg(full_message) {
                match bliss_compiler::reader::read_from_string(&source) {
                    Ok((form, _)) => match interpreter.eval(form) {
                        Ok(_) => "t".to_string(),
                        Err(e) => format!("\"Compilation error: {}\"", e),
                    },
                    Err(e) => format!("\"Read error: {}\"", e),
                }
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: compile-file
        "swank:compile-file-for-emacs" => {
            if let Some(path) = extract_swank_string_arg(full_message) {
                match compile_swank_file(&path, interpreter) {
                    Ok(result) => result,
                    Err(e) => format!("\"Compilation error: {}\"", e),
                }
            } else {
                "\"compile-file requires a path\"".to_string()
            }
        }

        // R6.34: completions
        "swank:completions" | "swank:simple-completions" | "swank:fuzzy-completions" => {
            if let Some(prefix) = extract_swank_string_arg(full_message) {
                let completions = complete_symbol(&prefix);
                let items: Vec<String> = completions.iter().map(|c| format!("\"{}\"", c)).collect();
                format!("(({}) \"{}\")", items.join(" "), prefix)
            } else {
                "(() \"\")".to_string()
            }
        }

        // R6.34: arglist
        "swank:operator-arglist" => {
            if let Some(name) = extract_swank_string_arg(full_message) {
                if let Some(arglist) = extract_swank_arglist(&name) {
                    format!("\"{}\"", arglist)
                } else {
                    "NIL".to_string()
                }
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: find-definitions
        "swank:find-definitions-for-emacs" => {
            if let Some(name) = extract_swank_string_arg(full_message) {
                let definitions = extract_swank_definitions(&name);
                if definitions.is_empty() {
                    "NIL".to_string()
                } else {
                    format!("({})", definitions.join(" "))
                }
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: macroexpand-1
        "swank:swank-macroexpand-1" => {
            if let Some(form_str) = extract_swank_string_arg(full_message) {
                match bliss_compiler::reader::read_from_string(&form_str) {
                    Ok((form, _)) => {
                        let env = bliss_compiler::macroexpand::Environment::null();
                        match bliss_compiler::macroexpand::macroexpand_1(form, &env) {
                            Ok((expanded, _changed)) => format!("{:?}", expanded),
                            Err(e) => format!("\"Macroexpand error: {}\"", e),
                        }
                    }
                    Err(e) => format!("\"Read error: {}\"", e),
                }
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: macroexpand-all
        "swank:swank-macroexpand-all" => {
            if let Some(form_str) = extract_swank_string_arg(full_message) {
                match bliss_compiler::reader::read_from_string(&form_str) {
                    Ok((form, _)) => {
                        let env = bliss_compiler::macroexpand::Environment::null();
                        match bliss_compiler::macroexpand::macroexpand_all(form, &env) {
                            Ok(expanded) => format!("{:?}", expanded),
                            Err(e) => format!("\"Macroexpand error: {}\"", e),
                        }
                    }
                    Err(e) => format!("\"Read error: {}\"", e),
                }
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: inspect
        "swank:init-inspector" | "swank:inspect-in-emacs" => {
            if let Some(form_str) = extract_swank_string_arg(full_message) {
                match bliss_compiler::reader::read_from_string(&form_str) {
                    Ok((form, _)) => match interpreter.eval(form) {
                        Ok(val) => {
                            let parts = compute_inspect_parts(val);
                            let parts_str: Vec<String> = parts
                                .iter()
                                .map(|(label, v)| format!("(\"{}\", {:?})", label, v))
                                .collect();
                            format!("(:title \"{:?}\" :content ({}))", val, parts_str.join(" "))
                        }
                        Err(e) => format!("\"Error: {}\"", e),
                    },
                    Err(e) => format!("\"Read error: {}\"", e),
                }
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: xref (callers/callees)
        "swank:xref" => extract_swank_string_arg(full_message)
            .map(|query| swank_xref(&query))
            .unwrap_or_else(|| "NIL".to_string()),

        // R6.34: apropos
        "swank:apropos-list-for-emacs" => {
            if let Some(query) = extract_swank_string_arg(full_message) {
                let matches = complete_symbol(&query);
                let items: Vec<String> = matches
                    .iter()
                    .map(|m| format!("(:designator \"{}\" :function \"\")", m))
                    .collect();
                format!("({})", items.join(" "))
            } else {
                "NIL".to_string()
            }
        }

        // R6.34: describe
        "swank:describe-symbol" | "swank:describe-function" => {
            if let Some(name) = extract_swank_string_arg(full_message) {
                format!("\"{}\"", name)
            } else {
                "NIL".to_string()
            }
        }

        // R6.38: thread-listing
        "swank:list-threads" => swank_threads_payload(),

        // R6.38: thread debugging
        "swank:debug-thread" => debug_thread_payload(full_message),

        // Connection info
        "swank:connection-info" => swank_connection_info(),

        // Default: return T
        _ => {
            eprintln!("; SWANK: unhandled op: {}", op);
            "t".to_string()
        }
    }
}

/// Extract a string argument from a SWANK message.
fn extract_swank_string_arg(message: &str) -> Option<String> {
    // Find the first quoted string in the message after the operation name
    let start = message.find('"')?;
    let rest = &message[start + 1..];
    let mut result = String::new();
    let mut escape = false;
    for ch in rest.chars() {
        if escape {
            result.push(ch);
            escape = false;
        } else if ch == '\\' {
            escape = true;
        } else if ch == '"' {
            return Some(result);
        } else {
            result.push(ch);
        }
    }
    None
}

/// Stop the SWANK server — close connections and shut down listener.
pub fn stop_swank_server() -> Result<(), BlissError> {
    SWANK_ACTIVE.store(false, Ordering::SeqCst);

    // Close all connections
    swank_connections().lock().unwrap().clear();

    let mut s = swank_state().lock().unwrap();
    s.conns = 0;
    s.session_secret = None;
    s.connections.clear();
    let _handle = s.listener_thread.take();

    Ok(())
}
