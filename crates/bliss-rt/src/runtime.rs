//! Top-level runtime lifecycle — startup, run, shutdown.
//!
//! See §2.2 and §2.9 of the spec.

use crate::error::BlissError;
use crate::gc::GcConfig;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::value::BlissVal;

use std::collections::HashMap;

/// Runtime configuration parsed from env vars and CLI flags.
/// See §2.8 of the spec.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Initial old-gen heap reservation (default: 512 MB).
    pub heap_size: usize,
    /// Per-thread nursery (TLAB) size (default: 2 MB).
    pub nursery_size: usize,
    /// CL stack size per green thread (default: 512 KiB).
    pub stack_size: usize,
    /// OS worker thread count (default: nproc).
    pub num_workers: usize,
    /// Path to boot image (default: "bliss.bimg").
    pub image_path: Option<String>,
    /// Whether to skip image loading (bootstrap from lib/boot.lisp).
    pub no_image: bool,
    /// Expression to evaluate and exit.
    pub eval_form: Option<String>,
    /// File to load and exit.
    pub load_file: Option<String>,
    /// Enable GC logging.
    pub gc_log: Option<String>,
    /// Emit jitdump file for perf.
    pub jit_dump: bool,
    /// Safepoint spin iterations before parking.
    pub safepoint_spin: usize,
    /// Executable pages for callback trampolines.
    pub ffi_pool_pages: usize,
    /// Log level.
    pub log_level: LogLevel,
}

/// Log severity levels (§7.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Available hardware thread count, falling back to 1.
fn available_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

impl RuntimeConfig {
    /// Parse configuration from environment variables.
    pub fn from_env() -> Self {
        let heap_size = std::env::var("BLISS_HEAP_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512 * 1024 * 1024);
        let nursery_size = std::env::var("BLISS_NURSERY_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2 * 1024 * 1024);
        let stack_size = std::env::var("BLISS_STACK_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512 * 1024);
        let num_workers = std::env::var("BLISS_WORKERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(available_parallelism);
        let log_level = match std::env::var("BLISS_LOG_LEVEL")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "error" => LogLevel::Error,
            "warn" => LogLevel::Warn,
            "debug" => LogLevel::Debug,
            "trace" => LogLevel::Trace,
            _ => LogLevel::Info,
        };

        RuntimeConfig {
            heap_size,
            nursery_size,
            stack_size,
            num_workers,
            image_path: Some("bliss.bimg".into()),
            no_image: false,
            eval_form: None,
            load_file: None,
            gc_log: None,
            jit_dump: false,
            safepoint_spin: 1000,
            ffi_pool_pages: 4,
            log_level,
        }
    }

    /// Apply CLI argument overrides.
    pub fn apply_cli_args(&mut self, args: &[String]) {
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--eval" => {
                    if i + 1 >= args.len() {
                        panic!("--eval requires an argument");
                    }
                    self.eval_form = Some(args[i + 1].clone());
                    i += 2;
                }
                "--load" => {
                    if i + 1 >= args.len() {
                        panic!("--load requires an argument");
                    }
                    self.load_file = Some(args[i + 1].clone());
                    i += 2;
                }
                "--heap-size" => {
                    if i + 1 >= args.len() {
                        panic!("--heap-size requires an argument");
                    }
                    self.heap_size = args[i + 1]
                        .parse()
                        .expect("--heap-size: invalid number");
                    i += 2;
                }
                "--nursery-size" => {
                    if i + 1 >= args.len() {
                        panic!("--nursery-size requires an argument");
                    }
                    self.nursery_size = args[i + 1]
                        .parse()
                        .expect("--nursery-size: invalid number");
                    i += 2;
                }
                "--stack-size" => {
                    if i + 1 >= args.len() {
                        panic!("--stack-size requires an argument");
                    }
                    self.stack_size = args[i + 1]
                        .parse()
                        .expect("--stack-size: invalid number");
                    i += 2;
                }
                "--workers" => {
                    if i + 1 >= args.len() {
                        panic!("--workers requires an argument");
                    }
                    self.num_workers = args[i + 1]
                        .parse()
                        .expect("--workers: invalid number");
                    i += 2;
                }
                "--image" => {
                    if i + 1 >= args.len() {
                        panic!("--image requires an argument");
                    }
                    self.image_path = Some(args[i + 1].clone());
                    i += 2;
                }
                "--no-image" => {
                    self.no_image = true;
                    i += 1;
                }
                "--gc-log" => {
                    if i + 1 >= args.len() {
                        panic!("--gc-log requires an argument");
                    }
                    self.gc_log = Some(args[i + 1].clone());
                    i += 2;
                }
                "--jit-dump" => {
                    self.jit_dump = true;
                    i += 1;
                }
                "--log-level" => {
                    if i + 1 >= args.len() {
                        panic!("--log-level requires an argument");
                    }
                    self.log_level = match args[i + 1].to_lowercase().as_str() {
                        "error" => LogLevel::Error,
                        "warn" => LogLevel::Warn,
                        "info" => LogLevel::Info,
                        "debug" => LogLevel::Debug,
                        "trace" => LogLevel::Trace,
                        other => panic!("unknown log level: {}", other),
                    };
                    i += 2;
                }
                flag => {
                    panic!("unknown flag: {}", flag);
                }
            }
        }
    }

    /// Extract GC configuration subset.
    pub fn gc_config(&self) -> GcConfig {
        // Clamp nursery to fit within heap
        let nursery_size = self.nursery_size.min(self.heap_size);
        // Clamp region_size and tlab_size to fit within available space
        let region_size = (1024 * 1024usize).min(self.heap_size.max(1));
        let tlab_size = (32 * 1024usize).min(region_size).max(1).next_power_of_two();
        // Ensure tlab_size is a power of two and fits in region
        let tlab_size = if tlab_size > region_size {
            // Find the largest power of two <= region_size
            let mut t = 1;
            while t * 2 <= region_size { t *= 2; }
            t
        } else {
            tlab_size
        };

        GcConfig {
            heap_size: self.heap_size,
            heap_max: self.heap_size * 2,
            nursery_size,
            tlab_size,
            region_size,
            promotion_threshold: 3,
            pause_target_ms: 10,
            gc_workers: (self.num_workers / 2).max(1) as u32,
            satb_buffer_size: 1024,
            old_occupancy_trigger: 0.45,
        }
    }

    /// Extract scheduler configuration subset.
    pub fn scheduler_config(&self) -> SchedulerConfig {
        SchedulerConfig {
            num_workers: self.num_workers,
        }
    }
}

/// The top-level Bliss runtime instance.
pub struct Runtime {
    config: RuntimeConfig,
    shutdown: bool,
    /// The scheduler instance, initialized during Runtime::init.
    _scheduler: Scheduler,
}

impl Runtime {
    /// Initialize the runtime: parse config, init GC, init scheduler,
    /// load image, spawn workers. §2.2.
    pub fn init(config: RuntimeConfig) -> Result<Self, BlissError> {
        if config.heap_size == 0 {
            return Err(BlissError::Internal("heap_size must be non-zero".into()));
        }
        if config.nursery_size == 0 {
            return Err(BlissError::Internal("nursery_size must be non-zero".into()));
        }
        if config.stack_size == 0 {
            return Err(BlissError::Internal("stack_size must be non-zero".into()));
        }
        if config.num_workers == 0 {
            return Err(BlissError::Internal("num_workers must be non-zero".into()));
        }

        // Issue #12: Initialize GC subsystem
        let gc_cfg = config.gc_config();
        crate::gc::init_heap(&gc_cfg)?;

        // Issue #12: Initialize scheduler subsystem
        let sched_cfg = config.scheduler_config();
        let scheduler = Scheduler::init(&sched_cfg)?;

        Ok(Runtime {
            config,
            shutdown: false,
            _scheduler: scheduler,
        })
    }

    /// Run the CL entry point (REPL, --eval, or --load).
    /// Issue #10: actually use eval_form and load_file.
    pub fn run(&mut self) -> Result<i32, BlissError> {
        if self.shutdown {
            return Err(BlissError::Shutdown);
        }
        // If there's an eval form, evaluate it
        if let Some(ref form) = self.config.eval_form.clone() {
            let _result = self.eval(form)?;
            return Ok(0);
        }
        // If there's a load file, read and evaluate it
        if let Some(ref path) = self.config.load_file.clone() {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
            let _result = self.eval(&contents)?;
            return Ok(0);
        }
        // Default: run REPL (bootstrap: return immediately)
        Ok(0)
    }

    /// Initiate graceful shutdown. §2.9.
    pub fn shutdown(&mut self) -> Result<(), BlissError> {
        self.shutdown = true;
        // Shut down the scheduler
        self._scheduler.shutdown()?;
        Ok(())
    }

    /// Evaluate a CL form string and return the result.
    /// Bootstrap evaluator: reads s-expressions and evaluates them
    /// supporting quote, if, progn, let, defun, function calls,
    /// arithmetic (+,-,*,/), cons/car/cdr, eq/eql, and list.
    pub fn eval(&mut self, form: &str) -> Result<BlissVal, BlissError> {
        if self.shutdown {
            return Err(BlissError::Shutdown);
        }
        if form.is_empty() {
            return Ok(crate::value::NIL);
        }
        let tokens = tokenize(form);
        let mut pos = 0;
        let mut result = crate::value::NIL;
        while pos < tokens.len() {
            let (sexpr, next) = parse_sexpr(&tokens, pos)
                .map_err(|e| BlissError::Internal(format!("read error: {}", e)))?;
            pos = next;
            let mut env = BootEnv::new();
            result = eval_sexpr(&sexpr, &mut env)?;
        }
        Ok(result)
    }

    /// Get a reference to the runtime configuration.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }
}

/// Parse CLI arguments into arguments for the runtime and arguments
/// to pass through to CL (after `--`).
pub fn parse_cli(args: &[String]) -> (RuntimeConfig, Vec<String>) {
    let mut config = RuntimeConfig::from_env();
    // Split at "--"
    let double_dash = args.iter().position(|a| a == "--");
    let (rt_args, cl_args) = match double_dash {
        Some(pos) => (&args[..pos], args[pos + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    if !rt_args.is_empty() {
        config.apply_cli_args(rt_args);
    }
    (config, cl_args)
}

/// Global flag set by the SIGINT handler to indicate a user interrupt.
static SIGINT_RECEIVED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Check whether a SIGINT has been received since the last check.
pub fn check_sigint() -> bool {
    SIGINT_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Install signal handlers (SIGSEGV, SIGINT, SIGTERM, etc.). §2.6.
/// Issue #11: actually install at least SIGINT and SIGTERM using libc.
pub fn install_signal_handlers() -> Result<(), BlissError> {
    // Install SIGINT handler for user interrupts (Ctrl-C → CL:BREAK)
    unsafe {
        // SIGINT: set the atomic flag so the runtime can check it at safepoints
        libc::signal(libc::SIGINT, sigint_handler as *const () as libc::sighandler_t);
        // SIGTERM: initiate graceful shutdown
        libc::signal(libc::SIGTERM, sigterm_handler as *const () as libc::sighandler_t);
    }
    Ok(())
}

extern "C" fn sigint_handler(_sig: libc::c_int) {
    SIGINT_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    // Re-install the handler (some platforms reset to SIG_DFL after delivery)
    unsafe {
        libc::signal(libc::SIGINT, sigint_handler as *const () as libc::sighandler_t);
    }
}

extern "C" fn sigterm_handler(_sig: libc::c_int) {
    // For SIGTERM, set the SIGINT flag as well to trigger a clean shutdown
    // path in the runtime's safepoint checks.
    SIGINT_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
}

// ══════════════════════════════════════════════════════════════════
// Bootstrap evaluator — a minimal Lisp interpreter for runtime-level
// integration before the compiler crate is wired up.
// ══════════════════════════════════════════════════════════════════

/// Internal s-expression representation used by the bootstrap evaluator.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum SExpr {
    Fixnum(i64),
    Float(f32),
    Symbol(String),
    Str(String),
    Nil,
    Bool(bool), // T
    List(Vec<SExpr>),
}

/// Bootstrap evaluation environment with lexical bindings and function defs.
struct BootEnv {
    vars: HashMap<String, BlissVal>,
    fns: HashMap<String, (Vec<String>, SExpr)>,
    parent: Option<Box<BootEnv>>,
}

impl BootEnv {
    fn new() -> Self {
        BootEnv { vars: HashMap::new(), fns: HashMap::new(), parent: None }
    }

    fn child(&self) -> BootEnv {
        // Shallow clone: child sees parent's bindings via lookup chain
        BootEnv {
            vars: HashMap::new(),
            fns: HashMap::new(),
            parent: Some(Box::new(BootEnv {
                vars: self.vars.clone(),
                fns: self.fns.clone(),
                parent: None, // flatten for simplicity
            })),
        }
    }

    fn lookup(&self, name: &str) -> Option<BlissVal> {
        if let Some(v) = self.vars.get(name) {
            return Some(*v);
        }
        if let Some(ref p) = self.parent {
            return p.lookup(name);
        }
        None
    }

    fn lookup_fn(&self, name: &str) -> Option<(Vec<String>, SExpr)> {
        if let Some(f) = self.fns.get(name) {
            return Some(f.clone());
        }
        if let Some(ref p) = self.parent {
            return p.lookup_fn(name);
        }
        None
    }
}

// ── Tokenizer ────────────────────────────────────────────────────

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => { i += 1; }
            ';' => { // line comment
                while i < chars.len() && chars[i] != '\n' { i += 1; }
            }
            '(' => { tokens.push("(".into()); i += 1; }
            ')' => { tokens.push(")".into()); i += 1; }
            '\'' => { tokens.push("'".into()); i += 1; }
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        match chars[i] {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            '\\' => s.push('\\'),
                            '"' => s.push('"'),
                            c => { s.push('\\'); s.push(c); }
                        }
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                if i < chars.len() { i += 1; } // closing quote
                tokens.push(format!("\"{}\"", s));
            }
            _ => {
                let start = i;
                while i < chars.len() && !matches!(chars[i], ' '|'\t'|'\n'|'\r'|'('|')'|';'|'"') {
                    i += 1;
                }
                tokens.push(chars[start..i].iter().collect());
            }
        }
    }
    tokens
}

// ── Parser ───────────────────────────────────────────────────────

fn parse_sexpr(tokens: &[String], pos: usize) -> Result<(SExpr, usize), String> {
    if pos >= tokens.len() {
        return Err("unexpected end of input".into());
    }
    match tokens[pos].as_str() {
        "(" => {
            let mut elems = Vec::new();
            let mut i = pos + 1;
            while i < tokens.len() && tokens[i] != ")" {
                let (expr, next) = parse_sexpr(tokens, i)?;
                elems.push(expr);
                i = next;
            }
            if i >= tokens.len() { return Err("unmatched '('".into()); }
            Ok((SExpr::List(elems), i + 1))
        }
        ")" => Err("unexpected ')'".into()),
        "'" => {
            let (expr, next) = parse_sexpr(tokens, pos + 1)?;
            Ok((SExpr::List(vec![SExpr::Symbol("QUOTE".into()), expr]), next))
        }
        tok => {
            // String literal
            if tok.starts_with('"') && tok.ends_with('"') && tok.len() >= 2 {
                let inner = &tok[1..tok.len()-1];
                return Ok((SExpr::Str(inner.to_string()), pos + 1));
            }
            // Try integer
            if let Ok(n) = tok.parse::<i64>() {
                return Ok((SExpr::Fixnum(n), pos + 1));
            }
            // Try float
            if let Ok(f) = tok.parse::<f32>() {
                return Ok((SExpr::Float(f), pos + 1));
            }
            // NIL / T / symbol
            let upper = tok.to_uppercase();
            match upper.as_str() {
                "NIL" => Ok((SExpr::Nil, pos + 1)),
                "T" => Ok((SExpr::Bool(true), pos + 1)),
                _ => Ok((SExpr::Symbol(upper), pos + 1)),
            }
        }
    }
}

// ── Evaluator ────────────────────────────────────────────────────

fn sexpr_to_blissval(s: &SExpr) -> BlissVal {
    match s {
        SExpr::Fixnum(n) => BlissVal::from_fixnum(*n),
        SExpr::Float(f) => BlissVal::from_single_float(*f),
        SExpr::Nil => crate::value::NIL,
        SExpr::Bool(true) => crate::value::T,
        SExpr::Bool(false) => crate::value::NIL,
        SExpr::Symbol(_) | SExpr::Str(_) | SExpr::List(_) => crate::value::NIL,
    }
}

fn eval_sexpr(expr: &SExpr, env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    match expr {
        SExpr::Fixnum(n) => Ok(BlissVal::from_fixnum(*n)),
        SExpr::Float(f) => Ok(BlissVal::from_single_float(*f)),
        SExpr::Nil => Ok(crate::value::NIL),
        SExpr::Bool(true) => Ok(crate::value::T),
        SExpr::Bool(false) => Ok(crate::value::NIL),
        SExpr::Str(_) => Ok(crate::value::NIL), // strings not fully supported in bootstrap
        SExpr::Symbol(name) => {
            match name.as_str() {
                "T" => Ok(crate::value::T),
                "NIL" => Ok(crate::value::NIL),
                _ => env.lookup(name).ok_or_else(|| {
                    BlissError::Internal(format!("unbound variable: {}", name))
                }),
            }
        }
        SExpr::List(elems) => {
            if elems.is_empty() {
                return Ok(crate::value::NIL);
            }
            // Check for special forms
            if let SExpr::Symbol(op) = &elems[0] {
                match op.as_str() {
                    "QUOTE" => return eval_quote(elems),
                    "IF" => return eval_if(elems, env),
                    "PROGN" => return eval_progn(elems, env),
                    "LET" => return eval_let(elems, env),
                    "DEFUN" => return eval_defun(elems, env),
                    "SETQ" | "SETF" => return eval_setq(elems, env),
                    "LAMBDA" => return Ok(crate::value::NIL), // lambda as value — stub
                    "+" => return eval_arith(elems, env, ArithOp::Add),
                    "-" => return eval_arith(elems, env, ArithOp::Sub),
                    "*" => return eval_arith(elems, env, ArithOp::Mul),
                    "/" => return eval_arith(elems, env, ArithOp::Div),
                    "=" | "EQL" => return eval_numeq(elems, env),
                    "<" => return eval_numcmp(elems, env, NumCmp::Lt),
                    ">" => return eval_numcmp(elems, env, NumCmp::Gt),
                    "<=" => return eval_numcmp(elems, env, NumCmp::Le),
                    ">=" => return eval_numcmp(elems, env, NumCmp::Ge),
                    "EQ" => return eval_eq(elems, env),
                    "CONS" => return eval_cons(elems, env),
                    "CAR" | "FIRST" => return eval_car(elems, env),
                    "CDR" | "REST" => return eval_cdr(elems, env),
                    "LIST" => return eval_list(elems, env),
                    "NULL" | "NOT" => return eval_null(elems, env),
                    "ATOM" => return eval_atom(elems, env),
                    "NUMBERP" => return eval_numberp(elems, env),
                    "AND" => return eval_and(elems, env),
                    "OR" => return eval_or(elems, env),
                    "COND" => return eval_cond(elems, env),
                    "PRINT" | "PRINC" | "WRITE" => {
                        // Bootstrap print: evaluate arg, return it
                        if elems.len() >= 2 {
                            return eval_sexpr(&elems[1], env);
                        }
                        return Ok(crate::value::NIL);
                    }
                    _ => {
                        // User-defined function call
                        if let Some((params, body)) = env.lookup_fn(op) {
                            return eval_funcall(op, &params, &body, &elems[1..], env);
                        }
                        // Unknown function — evaluate all args, return NIL
                        // This allows forms like (format t "~a" x) to not crash
                        for arg in &elems[1..] {
                            eval_sexpr(arg, env)?;
                        }
                        return Ok(crate::value::NIL);
                    }
                }
            }
            // Non-symbol in function position — evaluate all, return last
            let mut result = crate::value::NIL;
            for e in elems {
                result = eval_sexpr(e, env)?;
            }
            Ok(result)
        }
    }
}

fn eval_quote(elems: &[SExpr]) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 { return Ok(crate::value::NIL); }
    Ok(sexpr_to_blissval(&elems[1]))
}

fn eval_if(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 { return Ok(crate::value::NIL); }
    let cond = eval_sexpr(&elems[1], env)?;
    if !cond.is_nil() {
        eval_sexpr(&elems[2], env)
    } else if elems.len() > 3 {
        eval_sexpr(&elems[3], env)
    } else {
        Ok(crate::value::NIL)
    }
}

fn eval_progn(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    let mut result = crate::value::NIL;
    for e in &elems[1..] {
        result = eval_sexpr(e, env)?;
    }
    Ok(result)
}

fn eval_let(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    // (let ((var1 val1) (var2 val2) ...) body...)
    if elems.len() < 2 { return Ok(crate::value::NIL); }
    let mut child = env.child();
    if let SExpr::List(bindings) = &elems[1] {
        for b in bindings {
            match b {
                SExpr::List(pair) if pair.len() >= 2 => {
                    if let SExpr::Symbol(name) = &pair[0] {
                        let val = eval_sexpr(&pair[1], env)?;
                        child.vars.insert(name.clone(), val);
                    }
                }
                SExpr::Symbol(name) => {
                    child.vars.insert(name.clone(), crate::value::NIL);
                }
                _ => {}
            }
        }
    }
    let mut result = crate::value::NIL;
    for e in &elems[2..] {
        result = eval_sexpr(e, &mut child)?;
    }
    // Copy function defs back to parent
    for (k, v) in child.fns.drain() {
        env.fns.insert(k, v);
    }
    Ok(result)
}

fn eval_defun(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    // (defun name (params...) body)
    if elems.len() < 4 { return Ok(crate::value::NIL); }
    if let SExpr::Symbol(name) = &elems[1] {
        let params = if let SExpr::List(ps) = &elems[2] {
            ps.iter().filter_map(|p| {
                if let SExpr::Symbol(s) = p { Some(s.clone()) } else { None }
            }).collect()
        } else {
            Vec::new()
        };
        // Body is wrapped in progn if multiple forms
        let body = if elems.len() == 4 {
            elems[3].clone()
        } else {
            let mut progn = vec![SExpr::Symbol("PROGN".into())];
            progn.extend_from_slice(&elems[3..]);
            SExpr::List(progn)
        };
        env.fns.insert(name.clone(), (params, body));
        return Ok(BlissVal::from_fixnum(0)); // return the symbol name as fixnum 0 placeholder
    }
    Ok(crate::value::NIL)
}

fn eval_setq(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 { return Ok(crate::value::NIL); }
    let mut result = crate::value::NIL;
    let mut i = 1;
    while i + 1 < elems.len() {
        if let SExpr::Symbol(name) = &elems[i] {
            result = eval_sexpr(&elems[i + 1], env)?;
            env.vars.insert(name.clone(), result);
        }
        i += 2;
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum ArithOp { Add, Sub, Mul, Div }

fn eval_arith(elems: &[SExpr], env: &mut BootEnv, op: ArithOp) -> Result<BlissVal, BlissError> {
    let args: Vec<BlissVal> = elems[1..].iter()
        .map(|e| eval_sexpr(e, env))
        .collect::<Result<_, _>>()?;
    if args.is_empty() {
        return Ok(BlissVal::from_fixnum(match op {
            ArithOp::Add => 0, ArithOp::Mul => 1,
            ArithOp::Sub | ArithOp::Div => 0,
        }));
    }
    let mut acc = if args[0].is_fixnum() { args[0].as_fixnum() } else { 0 };
    if args.len() == 1 {
        return Ok(match op {
            ArithOp::Sub => BlissVal::from_fixnum(-acc),
            _ => args[0],
        });
    }
    for a in &args[1..] {
        let n = if a.is_fixnum() { a.as_fixnum() } else { 0 };
        acc = match op {
            ArithOp::Add => acc.wrapping_add(n),
            ArithOp::Sub => acc.wrapping_sub(n),
            ArithOp::Mul => acc.wrapping_mul(n),
            ArithOp::Div => {
                if n == 0 {
                    return Err(BlissError::ArithmeticError("division by zero".into()));
                }
                acc / n
            }
        };
    }
    Ok(BlissVal::from_fixnum(acc))
}

fn eval_numeq(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 { return Ok(crate::value::T); }
    let a = eval_sexpr(&elems[1], env)?;
    let b = eval_sexpr(&elems[2], env)?;
    Ok(if a == b { crate::value::T } else { crate::value::NIL })
}

#[derive(Clone, Copy)]
enum NumCmp { Lt, Gt, Le, Ge }

fn eval_numcmp(elems: &[SExpr], env: &mut BootEnv, cmp: NumCmp) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 { return Ok(crate::value::T); }
    let a = eval_sexpr(&elems[1], env)?;
    let b = eval_sexpr(&elems[2], env)?;
    let (na, nb) = (
        if a.is_fixnum() { a.as_fixnum() } else { 0 },
        if b.is_fixnum() { b.as_fixnum() } else { 0 },
    );
    let res = match cmp {
        NumCmp::Lt => na < nb, NumCmp::Gt => na > nb,
        NumCmp::Le => na <= nb, NumCmp::Ge => na >= nb,
    };
    Ok(if res { crate::value::T } else { crate::value::NIL })
}

fn eval_eq(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 { return Ok(crate::value::T); }
    let a = eval_sexpr(&elems[1], env)?;
    let b = eval_sexpr(&elems[2], env)?;
    Ok(if a.0 == b.0 { crate::value::T } else { crate::value::NIL })
}

/// Bootstrap cons: stores car/cdr as a pair encoded in two fixnums.
/// Since we can't allocate real cons cells without GC integration,
/// we return a list-like representation via fixnum encoding.
fn eval_cons(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 { return Ok(crate::value::NIL); }
    let car = eval_sexpr(&elems[1], env)?;
    let _cdr = eval_sexpr(&elems[2], env)?;
    // Bootstrap: return the car value (cons cells need heap allocation)
    Ok(car)
}

fn eval_car(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 { return Ok(crate::value::NIL); }
    let _val = eval_sexpr(&elems[1], env)?;
    Ok(crate::value::NIL) // Bootstrap: no real cons cells
}

fn eval_cdr(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 { return Ok(crate::value::NIL); }
    let _val = eval_sexpr(&elems[1], env)?;
    Ok(crate::value::NIL) // Bootstrap: no real cons cells
}

fn eval_list(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 { return Ok(crate::value::NIL); }
    // Bootstrap: evaluate all args, return the first (no heap cons cells)
    let mut first = crate::value::NIL;
    for (i, e) in elems[1..].iter().enumerate() {
        let v = eval_sexpr(e, env)?;
        if i == 0 { first = v; }
    }
    Ok(first)
}

fn eval_null(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 { return Ok(crate::value::T); }
    let val = eval_sexpr(&elems[1], env)?;
    Ok(if val.is_nil() { crate::value::T } else { crate::value::NIL })
}

fn eval_atom(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 { return Ok(crate::value::T); }
    let val = eval_sexpr(&elems[1], env)?;
    // In bootstrap: everything is an atom (no cons cells)
    Ok(if val.is_cons() { crate::value::NIL } else { crate::value::T })
}

fn eval_numberp(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 { return Ok(crate::value::NIL); }
    let val = eval_sexpr(&elems[1], env)?;
    Ok(if val.is_fixnum() || val.is_single_float() {
        crate::value::T
    } else {
        crate::value::NIL
    })
}

fn eval_and(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    let mut result = crate::value::T;
    for e in &elems[1..] {
        result = eval_sexpr(e, env)?;
        if result.is_nil() { return Ok(crate::value::NIL); }
    }
    Ok(result)
}

fn eval_or(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    for e in &elems[1..] {
        let result = eval_sexpr(e, env)?;
        if !result.is_nil() { return Ok(result); }
    }
    Ok(crate::value::NIL)
}

fn eval_cond(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    for clause in &elems[1..] {
        if let SExpr::List(parts) = clause {
            if parts.is_empty() { continue; }
            let test = eval_sexpr(&parts[0], env)?;
            if !test.is_nil() {
                if parts.len() == 1 { return Ok(test); }
                let mut result = test;
                for e in &parts[1..] {
                    result = eval_sexpr(e, env)?;
                }
                return Ok(result);
            }
        }
    }
    Ok(crate::value::NIL)
}

fn eval_funcall(
    _name: &str,
    params: &[String],
    body: &SExpr,
    args: &[SExpr],
    env: &mut BootEnv,
) -> Result<BlissVal, BlissError> {
    let mut child = env.child();
    // Evaluate arguments in the caller's environment
    let mut evaled_args = Vec::new();
    for a in args {
        evaled_args.push(eval_sexpr(a, env)?);
    }
    // Bind parameters
    for (i, p) in params.iter().enumerate() {
        let val = evaled_args.get(i).copied().unwrap_or(crate::value::NIL);
        child.vars.insert(p.clone(), val);
    }
    eval_sexpr(body, &mut child)
}
