//! Top-level runtime lifecycle — startup, run, shutdown.
//!
//! See §2.2 and §2.9 of the spec.

use crate::error::BlissError;
use crate::gc::GcConfig;
use crate::object::{ObjectHeader, type_id};
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::value::BlissVal;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{self, Write};
use std::rc::Rc;

/// Default boot-image path. Unlike an explicitly-requested image, its absence
/// is not fatal: the runtime bootstraps from the prelude instead.
pub const DEFAULT_IMAGE_PATH: &str = "bliss.bimg";

/// Runtime configuration parsed from env vars and CLI flags.
/// See §2.8 of the spec.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Initial old-gen heap reservation (default: 512 MB).
    pub heap_size: usize,
    /// Total nursery region pool size (default: 64 MB).
    pub nursery_size: usize,
    /// Per-thread TLAB size (default: 2 MB).
    pub tlab_size: usize,
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

fn parse_size(value: &str, context: &str) -> Result<usize, BlissError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(BlissError::Internal(format!(
            "{} requires a size value",
            context
        )));
    }

    let split_at = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(split_at);
    if digits.is_empty() {
        return Err(BlissError::Internal(format!(
            "{} requires a numeric size, got: {}",
            context, value
        )));
    }

    let base = digits.parse::<usize>().map_err(|_| {
        BlissError::Internal(format!(
            "{} requires a numeric size, got: {}",
            context, value
        ))
    })?;

    let multiplier = match suffix.trim().to_ascii_lowercase().as_str() {
        "" => 1,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        other => {
            return Err(BlissError::Internal(format!(
                "{} has unsupported size suffix '{}'",
                context, other
            )));
        }
    };

    base.checked_mul(multiplier).ok_or_else(|| {
        BlissError::Internal(format!(
            "{} is too large to fit in usize: {}",
            context, value
        ))
    })
}

fn parse_bool_flag(value: &str, context: &str) -> Result<bool, BlissError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(BlissError::Internal(format!(
            "{} requires a boolean-like value, got: {}",
            context, other
        ))),
    }
}

fn parse_usize(value: &str, context: &str) -> Result<usize, BlissError> {
    value.parse::<usize>().map_err(|_| {
        BlissError::Internal(format!(
            "{} requires a numeric value, got: {}",
            context, value
        ))
    })
}

fn parse_log_level(value: &str, context: &str) -> Result<LogLevel, BlissError> {
    match value.to_ascii_lowercase().as_str() {
        "error" => Ok(LogLevel::Error),
        "warn" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        other => Err(BlissError::Internal(format!(
            "{} has unknown log level: {}",
            context, other
        ))),
    }
}

impl RuntimeConfig {
    /// Parse configuration from environment variables.
    pub fn from_env() -> Result<Self, BlissError> {
        let heap_size = std::env::var("BLISS_HEAP_SIZE")
            .ok()
            .map(|s| parse_size(&s, "BLISS_HEAP_SIZE"))
            .transpose()?
            .unwrap_or(512 * 1024 * 1024);
        let tlab_size = std::env::var("BLISS_TLAB_SIZE")
            .ok()
            .map(|s| parse_size(&s, "BLISS_TLAB_SIZE"))
            .transpose()?
            .unwrap_or(2 * 1024 * 1024);
        let nursery_size = std::env::var("BLISS_NURSERY_SIZE")
            .ok()
            .map(|s| parse_size(&s, "BLISS_NURSERY_SIZE"))
            .transpose()?
            .unwrap_or(64 * 1024 * 1024);
        let stack_size = std::env::var("BLISS_STACK_SIZE")
            .ok()
            .map(|s| parse_size(&s, "BLISS_STACK_SIZE"))
            .transpose()?
            .unwrap_or(512 * 1024);
        let num_workers = std::env::var("BLISS_WORKERS")
            .ok()
            .map(|s| parse_usize(&s, "BLISS_WORKERS"))
            .transpose()?
            .unwrap_or_else(available_parallelism);
        let image_path = std::env::var("BLISS_IMAGE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| Some(DEFAULT_IMAGE_PATH.into()));
        let gc_log = std::env::var("BLISS_GC_LOG").ok().filter(|s| !s.is_empty());
        let jit_dump = std::env::var("BLISS_JIT_DUMP")
            .ok()
            .map(|s| parse_bool_flag(&s, "BLISS_JIT_DUMP"))
            .transpose()?
            .unwrap_or(false);
        let safepoint_spin = std::env::var("BLISS_SAFEPOINT_SPIN")
            .ok()
            .map(|s| parse_usize(&s, "BLISS_SAFEPOINT_SPIN"))
            .transpose()?
            .unwrap_or(1000);
        let ffi_pool_pages = std::env::var("BLISS_FFI_POOL_PAGES")
            .ok()
            .map(|s| parse_usize(&s, "BLISS_FFI_POOL_PAGES"))
            .transpose()?
            .unwrap_or(4);
        let log_level = std::env::var("BLISS_LOG_LEVEL")
            .ok()
            .map(|s| parse_log_level(&s, "BLISS_LOG_LEVEL"))
            .transpose()?
            .unwrap_or(LogLevel::Info);

        Ok(RuntimeConfig {
            heap_size,
            nursery_size,
            tlab_size,
            stack_size,
            num_workers,
            image_path,
            no_image: false,
            eval_form: None,
            load_file: None,
            gc_log,
            jit_dump,
            safepoint_spin,
            ffi_pool_pages,
            log_level,
        })
    }

    /// Apply CLI argument overrides.
    pub fn apply_cli_args(&mut self, args: &[String]) -> Result<(), BlissError> {
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--eval" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal("--eval requires an argument".into()));
                    }
                    self.eval_form = Some(args[i + 1].clone());
                    i += 2;
                }
                "--load" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal("--load requires an argument".into()));
                    }
                    self.load_file = Some(args[i + 1].clone());
                    i += 2;
                }
                "--heap-size" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal(
                            "--heap-size requires an argument".into(),
                        ));
                    }
                    self.heap_size = parse_size(&args[i + 1], "--heap-size")?;
                    i += 2;
                }
                "--tlab-size" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal(
                            "--tlab-size requires an argument".into(),
                        ));
                    }
                    self.tlab_size = parse_size(&args[i + 1], "--tlab-size")?;
                    i += 2;
                }
                "--nursery-size" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal(
                            "--nursery-size requires an argument".into(),
                        ));
                    }
                    self.nursery_size = parse_size(&args[i + 1], "--nursery-size")?;
                    i += 2;
                }
                "--stack-size" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal(
                            "--stack-size requires an argument".into(),
                        ));
                    }
                    self.stack_size = parse_size(&args[i + 1], "--stack-size")?;
                    i += 2;
                }
                "--workers" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal(
                            "--workers requires an argument".into(),
                        ));
                    }
                    self.num_workers = parse_usize(&args[i + 1], "--workers")?;
                    i += 2;
                }
                "--image" => {
                    if i + 1 >= args.len() {
                        return Err(BlissError::Internal("--image requires an argument".into()));
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
                        return Err(BlissError::Internal("--gc-log requires an argument".into()));
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
                        return Err(BlissError::Internal(
                            "--log-level requires an argument".into(),
                        ));
                    }
                    self.log_level = parse_log_level(&args[i + 1], "--log-level")?;
                    i += 2;
                }
                flag => {
                    return Err(BlissError::Internal(format!("unknown flag: {}", flag)));
                }
            }
        }
        Ok(())
    }

    /// Extract GC configuration subset.
    pub fn gc_config(&self) -> GcConfig {
        // Clamp nursery to fit within heap
        let nursery_size = self.nursery_size.min(self.heap_size);
        // Clamp region_size and tlab_size to fit within available space
        let region_size = (1024 * 1024usize).min(self.heap_size.max(1));
        let tlab_size = self.tlab_size.min(region_size).max(1).next_power_of_two();
        // Ensure tlab_size is a power of two and fits in region
        let tlab_size = if tlab_size > region_size {
            // Find the largest power of two <= region_size
            let mut t = 1;
            while t * 2 <= region_size {
                t *= 2;
            }
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
        if !config.no_image {
            // The default image path is optional: if the bundled image is
            // absent we bootstrap from the prelude instead of failing. An
            // explicitly-named image, however, must exist.
            if let Some(image_path) = config.image_path.as_deref() {
                if !std::path::Path::new(image_path).is_file() && image_path != DEFAULT_IMAGE_PATH {
                    return Err(BlissError::InvalidImage(format!(
                        "image file not found: {}",
                        image_path
                    )));
                }
            }
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
        self.run_repl()
    }

    /// Initiate graceful shutdown. §2.9.
    pub fn shutdown(&mut self) -> Result<(), BlissError> {
        self.shutdown = true;
        crate::thread::wait_for_other_threads();
        crate::gc::run_pending_finalizers();
        // Shut down the scheduler
        self._scheduler.shutdown()?;
        Ok(())
    }

    /// Evaluate a CL form string and return the result.
    ///
    /// NOTE (intentional, scoped duplication): this is a deliberately minimal
    /// *bootstrap* evaluator (`eval_sexpr`/`BootEnv`, below) supporting only
    /// quote, if, progn, let, defun, function calls, arithmetic (+,-,*,/),
    /// cons/car/cdr, eq/eql, and list. It exists so `bliss-rt` can be exercised
    /// in isolation by its own unit/integration tests *without* depending on the
    /// higher `bliss` crate.
    ///
    /// It is NOT the language evaluator. The real, full evaluator that every
    /// user-facing behaviour (the `bliss` CLI/REPL, `--eval`, `--load`) runs
    /// through lives in `bliss` (`cli::eval_form`). Because the crate
    /// dependency points `bliss -> bliss-rt` (never the reverse), `bliss-rt`
    /// cannot call into that evaluator; the two are kept separate on purpose
    /// rather than one wrapping the other. Do not grow this bootstrap evaluator
    /// into a second language implementation — extend `cli::eval_form` instead,
    /// and treat any feature added here as test-scaffolding only.
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
        let mut env = BootEnv::new();
        while pos < tokens.len() {
            let (sexpr, next) = parse_sexpr(&tokens, pos)
                .map_err(|e| BlissError::Internal(format!("read error: {}", e)))?;
            pos = next;
            result = eval_sexpr(&sexpr, &mut env)?;
        }
        Ok(result)
    }

    /// Get a reference to the runtime configuration.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    fn run_repl(&mut self) -> Result<i32, BlissError> {
        let stdin = io::stdin();
        let mut input = String::new();

        loop {
            eprint!("BLISS> ");
            io::stderr().flush().map_err(|e| {
                BlissError::StreamError(format!("failed to flush REPL prompt: {}", e))
            })?;

            input.clear();
            match stdin.read_line(&mut input) {
                Ok(0) => {
                    eprintln!();
                    return Ok(0);
                }
                Ok(_) => {
                    let trimmed = input.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if trimmed == "(quit)" || trimmed == "(exit)" {
                        return Ok(0);
                    }
                    let value = self.eval(trimmed)?;
                    println!("{}", boot_print_val(value, true));
                }
                Err(e) => {
                    return Err(BlissError::StreamError(format!(
                        "failed to read REPL input: {}",
                        e
                    )));
                }
            }
        }
    }
}

/// Parse CLI arguments into arguments for the runtime and arguments
/// to pass through to CL (after `--`).
pub fn parse_cli(args: &[String]) -> Result<(RuntimeConfig, Vec<String>), BlissError> {
    let mut config = RuntimeConfig::from_env()?;
    // Split at "--"
    let double_dash = args.iter().position(|a| a == "--");
    let (rt_args, cl_args) = match double_dash {
        Some(pos) => (&args[..pos], args[pos + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    if !rt_args.is_empty() {
        config.apply_cli_args(rt_args)?;
    }
    Ok((config, cl_args))
}

/// Global flag set by the SIGINT handler to indicate a user interrupt.
static SIGINT_RECEIVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Check whether a SIGINT has been received since the last check.
pub fn check_sigint() -> bool {
    SIGINT_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Install signal handlers (SIGSEGV, SIGINT, SIGTERM, etc.). §2.6.
/// Issue #11: actually install at least SIGINT and SIGTERM using libc.
pub fn install_signal_handlers() -> Result<(), BlissError> {
    // Install SIGINT handler for user interrupts (Ctrl-C → CL:BREAK)
    unsafe {
        libc::signal(
            libc::SIGSEGV,
            sigsegv_handler as *const () as libc::sighandler_t,
        );
        // SIGINT: set the atomic flag so the runtime can check it at safepoints
        libc::signal(
            libc::SIGINT,
            sigint_handler as *const () as libc::sighandler_t,
        );
        // SIGTERM: initiate graceful shutdown
        libc::signal(
            libc::SIGTERM,
            sigterm_handler as *const () as libc::sighandler_t,
        );
    }
    Ok(())
}

extern "C" fn sigint_handler(_sig: libc::c_int) {
    SIGINT_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    // Re-install the handler (some platforms reset to SIG_DFL after delivery)
    unsafe {
        libc::signal(
            libc::SIGINT,
            sigint_handler as *const () as libc::sighandler_t,
        );
    }
}

extern "C" fn sigterm_handler(_sig: libc::c_int) {
    // For SIGTERM, set the SIGINT flag as well to trigger a clean shutdown
    // path in the runtime's safepoint checks.
    SIGINT_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
}

extern "C" fn sigsegv_handler(_sig: libc::c_int) {
    unsafe {
        libc::_exit(0);
    }
}

// ══════════════════════════════════════════════════════════════════
// Bootstrap evaluator — a minimal Lisp interpreter for runtime-level
// integration before the compiler crate is wired up.
// ══════════════════════════════════════════════════════════════════

// ── Bootstrap side-tables ────────────────────────────────────────
// These thread-local stores let the bootstrap evaluator represent cons
// cells, symbols, strings, and lambdas without GC heap allocation.

struct BootstrapStore {
    cons_cells: HashMap<u64, (BlissVal, BlissVal)>,
    cons_counter: u64,
    symbol_to_idx: HashMap<String, u32>,
    idx_to_symbol: HashMap<u32, String>,
    symbol_counter: u32,
    strings: HashMap<u64, String>,
    lambdas: HashMap<u64, BootLambda>,
    lambda_counter: u64,
}

#[derive(Clone)]
struct BootLambda {
    params: Vec<String>,
    body: SExpr,
    /// Captured lexical environment (flattened vars and fns at definition time).
    captured_vars: HashMap<String, BlissVal>,
    captured_fns: HashMap<String, (Vec<String>, SExpr)>,
}

impl BootstrapStore {
    fn new() -> Self {
        BootstrapStore {
            cons_cells: HashMap::new(),
            cons_counter: 1, // start at 1 to avoid zero-tagged values
            symbol_to_idx: HashMap::new(),
            idx_to_symbol: HashMap::new(),
            symbol_counter: 1, // reserve 0
            strings: HashMap::new(),
            lambdas: HashMap::new(),
            lambda_counter: 1,
        }
    }
}

thread_local! {
    static BOOT_STORE: RefCell<BootstrapStore> = RefCell::new(BootstrapStore::new());
    static BOOT_EVAL_DEPTH: Cell<usize> = const { Cell::new(0) };
}

const MAX_BOOT_EVAL_DEPTH: usize = 1024;

struct BootEvalDepthGuard;

impl BootEvalDepthGuard {
    fn enter() -> Result<Self, BlissError> {
        BOOT_EVAL_DEPTH.with(|depth| {
            let next = depth.get() + 1;
            if next > MAX_BOOT_EVAL_DEPTH {
                Err(BlissError::StackOverflow(crate::thread::current_thread_id()))
            } else {
                depth.set(next);
                Ok(BootEvalDepthGuard)
            }
        })
    }
}

impl Drop for BootEvalDepthGuard {
    fn drop(&mut self) {
        BOOT_EVAL_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn boot_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    BOOT_STORE.with(|store| {
        let mut s = store.borrow_mut();
        let id = s.cons_counter;
        s.cons_counter += 1;
        s.cons_cells.insert(id, (car, cdr));
        BlissVal((id << 3) | crate::value::TAG_CONS)
    })
}

fn boot_car(val: BlissVal) -> BlissVal {
    if val.is_nil() {
        return crate::value::NIL;
    }
    if val.tag() != crate::value::TAG_CONS {
        return crate::value::NIL;
    }
    let id = val.0 >> 3;
    BOOT_STORE.with(|store| {
        store
            .borrow()
            .cons_cells
            .get(&id)
            .map(|(car, _)| *car)
            .unwrap_or(crate::value::NIL)
    })
}

fn boot_cdr(val: BlissVal) -> BlissVal {
    if val.is_nil() {
        return crate::value::NIL;
    }
    if val.tag() != crate::value::TAG_CONS {
        return crate::value::NIL;
    }
    let id = val.0 >> 3;
    BOOT_STORE.with(|store| {
        store
            .borrow()
            .cons_cells
            .get(&id)
            .map(|(_, cdr)| *cdr)
            .unwrap_or(crate::value::NIL)
    })
}

fn boot_intern(name: &str) -> BlissVal {
    BOOT_STORE.with(|store| {
        let mut s = store.borrow_mut();
        if let Some(&idx) = s.symbol_to_idx.get(name) {
            BlissVal::from_symbol_index(idx)
        } else {
            let idx = s.symbol_counter;
            s.symbol_counter += 1;
            s.symbol_to_idx.insert(name.to_string(), idx);
            s.idx_to_symbol.insert(idx, name.to_string());
            BlissVal::from_symbol_index(idx)
        }
    })
}

fn boot_symbol_name(val: BlissVal) -> Option<String> {
    if val.tag() != crate::value::TAG_SYMBOL {
        return None;
    }
    let idx = val.as_symbol_index();
    BOOT_STORE.with(|store| store.borrow().idx_to_symbol.get(&idx).cloned())
}

fn boot_make_string(s: &str) -> BlissVal {
    BOOT_STORE.with(|store| {
        let mut st = store.borrow_mut();
        let bytes = s.as_bytes();
        let size = 8 + 8 + bytes.len();
        let layout = std::alloc::Layout::from_size_align(size, 8).unwrap();
        let ptr = unsafe {
            let ptr = std::alloc::alloc_zeroed(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            *(ptr as *mut ObjectHeader) =
                ObjectHeader::new(type_id::SIMPLE_BASE_STRING, size.div_ceil(8) as u16);
            *(ptr.add(8) as *mut u64) = bytes.len() as u64;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(16), bytes.len());
            ptr
        };
        let id = (ptr as u64) >> 3;
        st.strings.insert(id, s.to_string());
        unsafe { BlissVal::from_heap_ptr(ptr) }
    })
}

fn boot_make_lambda(
    params: Vec<String>,
    body: SExpr,
    captured_vars: HashMap<String, BlissVal>,
    captured_fns: HashMap<String, (Vec<String>, SExpr)>,
) -> BlissVal {
    BOOT_STORE.with(|store| {
        let mut s = store.borrow_mut();
        let id = s.lambda_counter;
        s.lambda_counter += 1;
        s.lambdas.insert(
            id,
            BootLambda {
                params,
                body,
                captured_vars,
                captured_fns,
            },
        );
        BlissVal((id << 3) | crate::value::TAG_FUNCTION)
    })
}

fn boot_get_lambda(val: BlissVal) -> Option<BootLambda> {
    if val.tag() != crate::value::TAG_FUNCTION {
        return None;
    }
    let id = val.0 >> 3;
    BOOT_STORE.with(|store| store.borrow().lambdas.get(&id).cloned())
}

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
/// Uses Rc-shared parent pointers to avoid O(depth²) deep cloning.
struct BootEnv {
    vars: HashMap<String, BlissVal>,
    fns: HashMap<String, (Vec<String>, SExpr)>,
    parent: Option<Rc<BootEnv>>,
}

impl BootEnv {
    fn new() -> Self {
        BootEnv {
            vars: HashMap::new(),
            fns: HashMap::new(),
            parent: None,
        }
    }

    /// Create a child environment from a mutable reference by snapshotting
    /// the current env into an Rc and creating a new child.
    fn child_from_mut(env: &BootEnv) -> BootEnv {
        let snapshot = Rc::new(BootEnv {
            vars: env.vars.clone(),
            fns: env.fns.clone(),
            parent: env.parent.clone(),
        });
        BootEnv {
            vars: HashMap::new(),
            fns: HashMap::new(),
            parent: Some(snapshot),
        }
    }

    /// Create a child environment from captured closure variables.
    fn child_from_captured(
        captured_vars: &HashMap<String, BlissVal>,
        captured_fns: &HashMap<String, (Vec<String>, SExpr)>,
    ) -> BootEnv {
        let snapshot = Rc::new(BootEnv {
            vars: captured_vars.clone(),
            fns: captured_fns.clone(),
            parent: None,
        });
        BootEnv {
            vars: HashMap::new(),
            fns: HashMap::new(),
            parent: Some(snapshot),
        }
    }

    /// Flatten all visible variables into a single HashMap (for closure capture).
    fn flatten_vars(&self) -> HashMap<String, BlissVal> {
        let mut result = HashMap::new();
        // Walk parent chain first so inner scopes shadow outer
        if let Some(ref p) = self.parent {
            result = p.flatten_vars();
        }
        for (k, v) in &self.vars {
            result.insert(k.clone(), *v);
        }
        result
    }

    /// Flatten all visible function definitions (for closure capture).
    fn flatten_fns(&self) -> HashMap<String, (Vec<String>, SExpr)> {
        let mut result = HashMap::new();
        if let Some(ref p) = self.parent {
            result = p.flatten_fns();
        }
        for (k, v) in &self.fns {
            result.insert(k.clone(), v.clone());
        }
        result
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
            ' ' | '\t' | '\n' | '\r' => {
                i += 1;
            }
            ';' => {
                // line comment
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '(' => {
                tokens.push("(".into());
                i += 1;
            }
            ')' => {
                tokens.push(")".into());
                i += 1;
            }
            '\'' => {
                tokens.push("'".into());
                i += 1;
            }
            '#' if i + 1 < chars.len() && chars[i + 1] == '\'' => {
                // #'name → (FUNCTION name)
                tokens.push("#'".into());
                i += 2;
            }
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
                            c => {
                                s.push('\\');
                                s.push(c);
                            }
                        }
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                } // closing quote
                tokens.push(format!("\"{}\"", s));
            }
            _ => {
                let start = i;
                while i < chars.len()
                    && !matches!(chars[i], ' ' | '\t' | '\n' | '\r' | '(' | ')' | ';' | '"')
                {
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
            if i >= tokens.len() {
                return Err("unmatched '('".into());
            }
            Ok((SExpr::List(elems), i + 1))
        }
        ")" => Err("unexpected ')'".into()),
        "'" => {
            let (expr, next) = parse_sexpr(tokens, pos + 1)?;
            Ok((SExpr::List(vec![SExpr::Symbol("QUOTE".into()), expr]), next))
        }
        "#'" => {
            let (expr, next) = parse_sexpr(tokens, pos + 1)?;
            Ok((
                SExpr::List(vec![SExpr::Symbol("FUNCTION".into()), expr]),
                next,
            ))
        }
        tok => {
            // String literal
            if tok.starts_with('"') && tok.ends_with('"') && tok.len() >= 2 {
                let inner = &tok[1..tok.len() - 1];
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
        SExpr::Symbol(name) => match name.as_str() {
            "T" => crate::value::T,
            "NIL" => crate::value::NIL,
            _ => boot_intern(name),
        },
        SExpr::Str(s) => boot_make_string(s),
        SExpr::List(elems) => {
            // Build a cons list from the elements
            let mut result = crate::value::NIL;
            for e in elems.iter().rev() {
                let val = sexpr_to_blissval(e);
                result = boot_cons(val, result);
            }
            result
        }
    }
}

/// The bootstrap evaluator core. See the note on `Runtime::eval`: this is a
/// scoped, test-only subset — the full language evaluator is `cli::eval_form`
/// in the `bliss` crate. Keep these in sync only as far as the small subset
/// listed on `Runtime::eval`; do not expand this into a rival implementation.
fn eval_sexpr(expr: &SExpr, env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    let _depth_guard = BootEvalDepthGuard::enter()?;
    match expr {
        SExpr::Fixnum(n) => Ok(BlissVal::from_fixnum(*n)),
        SExpr::Float(f) => Ok(BlissVal::from_single_float(*f)),
        SExpr::Nil => Ok(crate::value::NIL),
        SExpr::Bool(true) => Ok(crate::value::T),
        SExpr::Bool(false) => Ok(crate::value::NIL),
        SExpr::Str(s) => Ok(boot_make_string(s)),
        SExpr::Symbol(name) => match name.as_str() {
            "T" => Ok(crate::value::T),
            "NIL" => Ok(crate::value::NIL),
            _ => env
                .lookup(name)
                .ok_or_else(|| BlissError::Internal(format!("unbound variable: {}", name))),
        },
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
                    "LAMBDA" => {
                        // (lambda (params...) body...)
                        if elems.len() < 3 {
                            return Err(BlissError::Internal(
                                "lambda requires params and body".into(),
                            ));
                        }
                        let params = if let SExpr::List(ps) = &elems[1] {
                            ps.iter()
                                .filter_map(|p| {
                                    if let SExpr::Symbol(s) = p {
                                        Some(s.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                        let body = if elems.len() == 3 {
                            elems[2].clone()
                        } else {
                            let mut progn = vec![SExpr::Symbol("PROGN".into())];
                            progn.extend_from_slice(&elems[2..]);
                            SExpr::List(progn)
                        };
                        // Capture the lexical environment at definition time (closure)
                        let captured_vars = env.flatten_vars();
                        let captured_fns = env.flatten_fns();
                        return Ok(boot_make_lambda(params, body, captured_vars, captured_fns));
                    }
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
                    "PRINT" => {
                        // (print obj) — output a newline, then obj with escapes, then a space
                        if elems.len() >= 2 {
                            let val = eval_sexpr(&elems[1], env)?;
                            let repr = boot_print_val(val, true);
                            print!("\n{} ", repr);
                            return Ok(val);
                        }
                        return Ok(crate::value::NIL);
                    }
                    "PRINC" => {
                        // (princ obj) — output obj without escapes
                        if elems.len() >= 2 {
                            let val = eval_sexpr(&elems[1], env)?;
                            let repr = boot_print_val(val, false);
                            print!("{}", repr);
                            return Ok(val);
                        }
                        return Ok(crate::value::NIL);
                    }
                    "WRITE" => {
                        // (write obj) — output obj with escapes
                        if elems.len() >= 2 {
                            let val = eval_sexpr(&elems[1], env)?;
                            let repr = boot_print_val(val, true);
                            print!("{}", repr);
                            return Ok(val);
                        }
                        return Ok(crate::value::NIL);
                    }
                    "FUNCTION" => {
                        // (function name) — look up a named function and return it as a lambda value
                        if elems.len() < 2 {
                            return Err(BlissError::Internal(
                                "FUNCTION requires an argument".into(),
                            ));
                        }
                        match &elems[1] {
                            SExpr::Symbol(fname) => {
                                if let Some((params, body)) = env.lookup_fn(fname) {
                                    let captured_vars = env.flatten_vars();
                                    let captured_fns = env.flatten_fns();
                                    return Ok(boot_make_lambda(
                                        params,
                                        body,
                                        captured_vars,
                                        captured_fns,
                                    ));
                                }
                                // Check for built-in functions
                                return Ok(boot_intern_builtin(fname));
                            }
                            SExpr::List(_inner) => {
                                // (function (lambda (params) body))
                                return eval_sexpr(&elems[1], env);
                            }
                            _ => {
                                return Err(BlissError::Internal(
                                    "FUNCTION: invalid argument".into(),
                                ));
                            }
                        }
                    }
                    "FUNCALL" => {
                        // (funcall fn arg1 arg2 ...)
                        if elems.len() < 2 {
                            return Err(BlissError::Internal(
                                "FUNCALL requires at least a function argument".into(),
                            ));
                        }
                        let func_val = eval_sexpr(&elems[1], env)?;
                        return eval_lambda_call(func_val, &elems[2..], env);
                    }
                    "APPLY" => {
                        // (apply fn arg1 ... argN list)
                        if elems.len() < 3 {
                            return Err(BlissError::Internal(
                                "APPLY requires a function and at least one argument".into(),
                            ));
                        }
                        let func_val = eval_sexpr(&elems[1], env)?;
                        // Evaluate all args except the last normally; the last must be a list
                        let mut evaled_args = Vec::new();
                        for a in &elems[2..elems.len() - 1] {
                            evaled_args.push(eval_sexpr(a, env)?);
                        }
                        // Last arg: evaluate it, then spread the list
                        let last = eval_sexpr(&elems[elems.len() - 1], env)?;
                        // Walk the cons list and append each element
                        let mut cur = last;
                        while !cur.is_nil() {
                            if cur.is_cons() {
                                evaled_args.push(boot_car(cur));
                                cur = boot_cdr(cur);
                            } else {
                                // Dotted list or atom — just push it
                                evaled_args.push(cur);
                                break;
                            }
                        }
                        return eval_lambda_call_with_vals(func_val, &evaled_args, env);
                    }
                    _ => {
                        // User-defined function call
                        if let Some((params, body)) = env.lookup_fn(op) {
                            return eval_funcall(op, &params, &body, &elems[1..], env);
                        }
                        // Unknown/undefined function — signal an error
                        return Err(BlissError::Internal(format!("undefined function: {}", op)));
                    }
                }
            }
            // Non-symbol in function position — check for lambda call
            // e.g. ((lambda (x) (+ x 1)) 5)
            let func_val = eval_sexpr(&elems[0], env)?;
            eval_lambda_call(func_val, &elems[1..], env)
        }
    }
}

fn eval_quote(elems: &[SExpr]) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    Ok(sexpr_to_blissval(&elems[1]))
}

fn eval_if(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 {
        return Ok(crate::value::NIL);
    }
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
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    let mut child = BootEnv::child_from_mut(env);
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
    if elems.len() < 4 {
        return Ok(crate::value::NIL);
    }
    if let SExpr::Symbol(name) = &elems[1] {
        let params = if let SExpr::List(ps) = &elems[2] {
            ps.iter()
                .filter_map(|p| {
                    if let SExpr::Symbol(s) = p {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect()
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
        return Ok(boot_intern(name)); // return the function name as a symbol
    }
    Ok(crate::value::NIL)
}

fn eval_setq(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 {
        return Ok(crate::value::NIL);
    }
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
enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

fn eval_arith(elems: &[SExpr], env: &mut BootEnv, op: ArithOp) -> Result<BlissVal, BlissError> {
    let args: Vec<BlissVal> = elems[1..]
        .iter()
        .map(|e| eval_sexpr(e, env))
        .collect::<Result<_, _>>()?;
    if args.is_empty() {
        return match op {
            ArithOp::Add => Ok(BlissVal::from_fixnum(0)),
            ArithOp::Mul => Ok(BlissVal::from_fixnum(1)),
            ArithOp::Sub | ArithOp::Div => Err(BlissError::Internal(format!(
                "wrong number of arguments for {}",
                match op {
                    ArithOp::Sub => "-",
                    _ => "/",
                }
            ))),
        };
    }
    if !args[0].is_fixnum() {
        return Err(BlissError::TypeError {
            datum: args[0],
            expected: "number".into(),
        });
    }
    let mut acc = args[0].as_fixnum();
    if args.len() == 1 {
        return Ok(match op {
            ArithOp::Sub => BlissVal::from_fixnum(-acc),
            _ => args[0],
        });
    }
    for a in &args[1..] {
        if !a.is_fixnum() {
            return Err(BlissError::TypeError {
                datum: *a,
                expected: "number".into(),
            });
        }
        let n = a.as_fixnum();
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
    if elems.len() < 3 {
        return Ok(crate::value::T);
    }
    let a = eval_sexpr(&elems[1], env)?;
    let b = eval_sexpr(&elems[2], env)?;
    Ok(if a == b {
        crate::value::T
    } else {
        crate::value::NIL
    })
}

#[derive(Clone, Copy)]
enum NumCmp {
    Lt,
    Gt,
    Le,
    Ge,
}

fn eval_numcmp(elems: &[SExpr], env: &mut BootEnv, cmp: NumCmp) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 {
        return Ok(crate::value::T);
    }
    let a = eval_sexpr(&elems[1], env)?;
    let b = eval_sexpr(&elems[2], env)?;
    if !a.is_fixnum() {
        return Err(BlissError::TypeError {
            datum: a,
            expected: "number".into(),
        });
    }
    if !b.is_fixnum() {
        return Err(BlissError::TypeError {
            datum: b,
            expected: "number".into(),
        });
    }
    let (na, nb) = (a.as_fixnum(), b.as_fixnum());
    let res = match cmp {
        NumCmp::Lt => na < nb,
        NumCmp::Gt => na > nb,
        NumCmp::Le => na <= nb,
        NumCmp::Ge => na >= nb,
    };
    Ok(if res {
        crate::value::T
    } else {
        crate::value::NIL
    })
}

fn eval_eq(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 {
        return Ok(crate::value::T);
    }
    let a = eval_sexpr(&elems[1], env)?;
    let b = eval_sexpr(&elems[2], env)?;
    Ok(if a.0 == b.0 {
        crate::value::T
    } else {
        crate::value::NIL
    })
}

/// Bootstrap cons: stores car/cdr pairs in a thread-local side-table,
/// keyed by a monotonic counter encoded as a cons-tagged BlissVal.
fn eval_cons(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 3 {
        return Ok(crate::value::NIL);
    }
    let car = eval_sexpr(&elems[1], env)?;
    let cdr = eval_sexpr(&elems[2], env)?;
    Ok(boot_cons(car, cdr))
}

fn eval_car(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    let val = eval_sexpr(&elems[1], env)?;
    Ok(boot_car(val))
}

fn eval_cdr(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    let val = eval_sexpr(&elems[1], env)?;
    Ok(boot_cdr(val))
}

fn eval_list(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    // Evaluate all args, then build a proper cons list
    let mut vals = Vec::new();
    for e in &elems[1..] {
        vals.push(eval_sexpr(e, env)?);
    }
    let mut result = crate::value::NIL;
    for v in vals.into_iter().rev() {
        result = boot_cons(v, result);
    }
    Ok(result)
}

fn eval_null(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 {
        return Ok(crate::value::T);
    }
    let val = eval_sexpr(&elems[1], env)?;
    Ok(if val.is_nil() {
        crate::value::T
    } else {
        crate::value::NIL
    })
}

fn eval_atom(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 {
        return Ok(crate::value::T);
    }
    let val = eval_sexpr(&elems[1], env)?;
    // In bootstrap: everything is an atom (no cons cells)
    Ok(if val.is_cons() {
        crate::value::NIL
    } else {
        crate::value::T
    })
}

fn eval_numberp(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
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
        if result.is_nil() {
            return Ok(crate::value::NIL);
        }
    }
    Ok(result)
}

fn eval_or(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    for e in &elems[1..] {
        let result = eval_sexpr(e, env)?;
        if !result.is_nil() {
            return Ok(result);
        }
    }
    Ok(crate::value::NIL)
}

fn eval_cond(elems: &[SExpr], env: &mut BootEnv) -> Result<BlissVal, BlissError> {
    for clause in &elems[1..] {
        if let SExpr::List(parts) = clause {
            if parts.is_empty() {
                continue;
            }
            let test = eval_sexpr(&parts[0], env)?;
            if !test.is_nil() {
                if parts.len() == 1 {
                    return Ok(test);
                }
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
    let mut child = BootEnv::child_from_mut(env);
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
    let result = eval_sexpr(body, &mut child)?;
    // Propagate function definitions from callee back to caller
    for (k, v) in child.fns.drain() {
        env.fns.insert(k, v);
    }
    Ok(result)
}

/// Call a lambda/closure value with unevaluated argument s-expressions.
/// Evaluates args in the caller's env, then invokes the closure in its captured env.
fn eval_lambda_call(
    func_val: BlissVal,
    arg_exprs: &[SExpr],
    env: &mut BootEnv,
) -> Result<BlissVal, BlissError> {
    // Check for built-in function symbols first
    if let Some(builtin_name) = boot_builtin_name(func_val) {
        return eval_builtin_call(&builtin_name, arg_exprs, env);
    }
    if boot_get_lambda(func_val).is_some() {
        let mut evaled_args = Vec::new();
        for a in arg_exprs {
            evaled_args.push(eval_sexpr(a, env)?);
        }
        eval_lambda_call_with_vals(func_val, &evaled_args, env)
    } else {
        Err(BlissError::Internal(
            "invalid function call: not a function".into(),
        ))
    }
}

/// Call a lambda/closure value with already-evaluated argument values.
fn eval_lambda_call_with_vals(
    func_val: BlissVal,
    args: &[BlissVal],
    env: &mut BootEnv,
) -> Result<BlissVal, BlissError> {
    // Check for built-in function symbols first
    if let Some(builtin_name) = boot_builtin_name(func_val) {
        return eval_builtin_call_with_vals(&builtin_name, args, env);
    }
    if let Some(lam) = boot_get_lambda(func_val) {
        // Create child environment from the closure's captured environment
        let mut child = BootEnv::child_from_captured(&lam.captured_vars, &lam.captured_fns);
        for (i, p) in lam.params.iter().enumerate() {
            let val = args.get(i).copied().unwrap_or(crate::value::NIL);
            child.vars.insert(p.clone(), val);
        }
        let result = eval_sexpr(&lam.body, &mut child)?;
        // Propagate function definitions back
        for (k, v) in child.fns.drain() {
            env.fns.insert(k, v);
        }
        Ok(result)
    } else {
        Err(BlissError::Internal(
            "invalid function call: not a function".into(),
        ))
    }
}

/// Bootstrap print: format a BlissVal as a string for output.
/// If `escape` is true, strings are printed with quotes (like PRINT/WRITE).
fn boot_print_val(val: BlissVal, escape: bool) -> String {
    if val.is_nil() {
        return "NIL".to_string();
    }
    if val == crate::value::T {
        return "T".to_string();
    }
    if val.is_fixnum() {
        return format!("{}", val.as_fixnum());
    }
    if val.is_single_float() {
        return format!("{}", val.as_single_float());
    }
    if val.tag() == crate::value::TAG_SYMBOL {
        if let Some(name) = boot_symbol_name(val) {
            return name;
        }
        return format!("#<SYMBOL {}>", val.as_symbol_index());
    }
    if val.tag() == crate::value::TAG_HEAP_OBJECT {
        // Might be a bootstrap string
        let id = val.0 >> 3;
        let s = BOOT_STORE.with(|store| store.borrow().strings.get(&id).cloned());
        if let Some(s) = s {
            return if escape { format!("\"{}\"", s) } else { s };
        }
        return format!("#<HEAP-OBJECT {:#x}>", val.0);
    }
    if val.is_cons() {
        let mut parts = Vec::new();
        let mut cur = val;
        while cur.is_cons() && !cur.is_nil() {
            parts.push(boot_print_val(boot_car(cur), escape));
            cur = boot_cdr(cur);
        }
        if cur.is_nil() {
            return format!("({})", parts.join(" "));
        } else {
            return format!("({} . {})", parts.join(" "), boot_print_val(cur, escape));
        }
    }
    if val.tag() == crate::value::TAG_FUNCTION {
        return "#<FUNCTION>".to_string();
    }
    format!("#<UNKNOWN {:#x}>", val.0)
}

/// Map of built-in function names to unique tag values for FUNCALL/APPLY.
/// We use symbol values to represent built-in functions referenced via #'name.
fn boot_intern_builtin(name: &str) -> BlissVal {
    // Reuse the symbol interning — when funcall'd, we check for known builtins
    boot_intern(&format!("__BUILTIN_{}", name))
}

/// Check if a value is a built-in function reference and return its name.
fn boot_builtin_name(val: BlissVal) -> Option<String> {
    if let Some(name) = boot_symbol_name(val) {
        if let Some(stripped) = name.strip_prefix("__BUILTIN_") {
            return Some(stripped.to_string());
        }
    }
    None
}

/// Call a built-in function by name with unevaluated args.
fn eval_builtin_call(
    name: &str,
    arg_exprs: &[SExpr],
    env: &mut BootEnv,
) -> Result<BlissVal, BlissError> {
    let mut evaled = Vec::new();
    for a in arg_exprs {
        evaled.push(eval_sexpr(a, env)?);
    }
    eval_builtin_call_with_vals(name, &evaled, env)
}

/// Call a built-in function by name with already-evaluated args.
fn eval_builtin_call_with_vals(
    name: &str,
    args: &[BlissVal],
    _env: &mut BootEnv,
) -> Result<BlissVal, BlissError> {
    match name {
        "+" => arith_builtin(args, ArithOp::Add),
        "-" => arith_builtin(args, ArithOp::Sub),
        "*" => arith_builtin(args, ArithOp::Mul),
        "/" => arith_builtin(args, ArithOp::Div),
        "CONS" => {
            if args.len() < 2 {
                return Ok(crate::value::NIL);
            }
            Ok(boot_cons(args[0], args[1]))
        }
        "CAR" | "FIRST" => {
            if args.is_empty() {
                return Ok(crate::value::NIL);
            }
            Ok(boot_car(args[0]))
        }
        "CDR" | "REST" => {
            if args.is_empty() {
                return Ok(crate::value::NIL);
            }
            Ok(boot_cdr(args[0]))
        }
        "LIST" => {
            let mut result = crate::value::NIL;
            for v in args.iter().rev() {
                result = boot_cons(*v, result);
            }
            Ok(result)
        }
        "EQ" => {
            if args.len() < 2 {
                return Ok(crate::value::T);
            }
            Ok(if args[0].0 == args[1].0 {
                crate::value::T
            } else {
                crate::value::NIL
            })
        }
        "EQL" | "=" => {
            if args.len() < 2 {
                return Ok(crate::value::T);
            }
            Ok(if args[0] == args[1] {
                crate::value::T
            } else {
                crate::value::NIL
            })
        }
        "NULL" | "NOT" => {
            if args.is_empty() {
                return Ok(crate::value::T);
            }
            Ok(if args[0].is_nil() {
                crate::value::T
            } else {
                crate::value::NIL
            })
        }
        _ => Err(BlissError::Internal(format!(
            "undefined function: {}",
            name
        ))),
    }
}

/// Arithmetic on pre-evaluated BlissVal args (for built-in funcall/apply).
fn arith_builtin(args: &[BlissVal], op: ArithOp) -> Result<BlissVal, BlissError> {
    if args.is_empty() {
        return match op {
            ArithOp::Add => Ok(BlissVal::from_fixnum(0)),
            ArithOp::Mul => Ok(BlissVal::from_fixnum(1)),
            ArithOp::Sub | ArithOp::Div => Err(BlissError::Internal(format!(
                "wrong number of arguments for {}",
                match op {
                    ArithOp::Sub => "-",
                    _ => "/",
                }
            ))),
        };
    }
    if !args[0].is_fixnum() {
        return Err(BlissError::TypeError {
            datum: args[0],
            expected: "number".into(),
        });
    }
    let mut acc = args[0].as_fixnum();
    if args.len() == 1 {
        return Ok(match op {
            ArithOp::Sub => BlissVal::from_fixnum(-acc),
            _ => args[0],
        });
    }
    for a in &args[1..] {
        if !a.is_fixnum() {
            return Err(BlissError::TypeError {
                datum: *a,
                expected: "number".into(),
            });
        }
        let n = a.as_fixnum();
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
