// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Top-level runtime lifecycle — startup, run, shutdown.
//!
//! See §2.2 and §2.9 of the spec.

use crate::error::EgclError;
use crate::gc::GcConfig;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::value::EgclVal;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::{Condvar, Mutex, OnceLock};

/// Default boot-image path. Unlike an explicitly-requested image, its absence
/// is not fatal: the runtime bootstraps from the prelude instead.
pub const DEFAULT_IMAGE_PATH: &str = "egcl.bimg";

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
    /// Path to boot image (default: "egcl.bimg").
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

type RuntimeInitHook = fn() -> Result<(), EgclError>;

static RUNTIME_INIT_HOOK: OnceLock<RuntimeInitHook> = OnceLock::new();

struct RuntimeLifecycle {
    state: Mutex<RuntimeLifecycleState>,
    available: Condvar,
}

struct RuntimeLifecycleState {
    owner: Option<std::thread::ThreadId>,
    depth: usize,
}

struct RuntimeLifecycleGuard {
    owner: std::thread::ThreadId,
}

fn runtime_lifecycle() -> &'static RuntimeLifecycle {
    static LIFECYCLE: OnceLock<RuntimeLifecycle> = OnceLock::new();
    LIFECYCLE.get_or_init(|| RuntimeLifecycle {
        state: Mutex::new(RuntimeLifecycleState {
            owner: None,
            depth: 0,
        }),
        available: Condvar::new(),
    })
}

fn acquire_runtime_lifecycle() -> RuntimeLifecycleGuard {
    let lifecycle = runtime_lifecycle();
    let current = std::thread::current().id();
    let mut state = lifecycle
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    loop {
        match state.owner {
            None => {
                state.owner = Some(current);
                state.depth = 1;
                return RuntimeLifecycleGuard { owner: current };
            }
            Some(owner) if owner == current => {
                state.depth += 1;
                return RuntimeLifecycleGuard { owner: current };
            }
            Some(_) => {
                state = lifecycle
                    .available
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
        }
    }
}

impl Drop for RuntimeLifecycleGuard {
    fn drop(&mut self) {
        let lifecycle = runtime_lifecycle();
        let mut state = lifecycle
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.owner == Some(self.owner) {
            state.depth = state.depth.saturating_sub(1);
            if state.depth == 0 {
                state.owner = None;
                lifecycle.available.notify_one();
            }
        }
    }
}

/// Register an optional startup hook that runs during `Runtime::init`.
///
/// Later registrations after the first are ignored.
pub fn set_runtime_init_hook(hook: RuntimeInitHook) {
    let _ = RUNTIME_INIT_HOOK.set(hook);
}

/// Available hardware thread count, falling back to 1.
fn available_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn parse_size(value: &str, context: &str) -> Result<usize, EgclError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(EgclError::Internal(format!(
            "{} requires a size value",
            context
        )));
    }

    let split_at = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(split_at);
    if digits.is_empty() {
        return Err(EgclError::Internal(format!(
            "{} requires a numeric size, got: {}",
            context, value
        )));
    }

    let base = digits.parse::<usize>().map_err(|_| {
        EgclError::Internal(format!(
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
            return Err(EgclError::Internal(format!(
                "{} has unsupported size suffix '{}'",
                context, other
            )));
        }
    };

    base.checked_mul(multiplier).ok_or_else(|| {
        EgclError::Internal(format!(
            "{} is too large to fit in usize: {}",
            context, value
        ))
    })
}

fn parse_bool_flag(value: &str, context: &str) -> Result<bool, EgclError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(EgclError::Internal(format!(
            "{} requires a boolean-like value, got: {}",
            context, other
        ))),
    }
}

fn parse_usize(value: &str, context: &str) -> Result<usize, EgclError> {
    value.parse::<usize>().map_err(|_| {
        EgclError::Internal(format!(
            "{} requires a numeric value, got: {}",
            context, value
        ))
    })
}

fn parse_log_level(value: &str, context: &str) -> Result<LogLevel, EgclError> {
    match value.to_ascii_lowercase().as_str() {
        "error" => Ok(LogLevel::Error),
        "warn" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        other => Err(EgclError::Internal(format!(
            "{} has unknown log level: {}",
            context, other
        ))),
    }
}

impl RuntimeConfig {
    /// Parse configuration from environment variables.
    pub fn from_env() -> Result<Self, EgclError> {
        let heap_size = std::env::var("EGCL_HEAP_SIZE")
            .ok()
            .map(|s| parse_size(&s, "EGCL_HEAP_SIZE"))
            .transpose()?
            .unwrap_or(512 * 1024 * 1024);
        let tlab_size = std::env::var("EGCL_TLAB_SIZE")
            .ok()
            .map(|s| parse_size(&s, "EGCL_TLAB_SIZE"))
            .transpose()?
            .unwrap_or(2 * 1024 * 1024);
        let nursery_size = std::env::var("EGCL_NURSERY_SIZE")
            .ok()
            .map(|s| parse_size(&s, "EGCL_NURSERY_SIZE"))
            .transpose()?
            .unwrap_or(64 * 1024 * 1024);
        let stack_size = std::env::var("EGCL_STACK_SIZE")
            .ok()
            .map(|s| parse_size(&s, "EGCL_STACK_SIZE"))
            .transpose()?
            .unwrap_or(512 * 1024);
        let num_workers = std::env::var("EGCL_WORKERS")
            .ok()
            .map(|s| parse_usize(&s, "EGCL_WORKERS"))
            .transpose()?
            .unwrap_or_else(available_parallelism);
        let image_path = std::env::var("EGCL_IMAGE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| Some(DEFAULT_IMAGE_PATH.into()));
        let gc_log = std::env::var("EGCL_GC_LOG").ok().filter(|s| !s.is_empty());
        let jit_dump = std::env::var("EGCL_JIT_DUMP")
            .ok()
            .map(|s| parse_bool_flag(&s, "EGCL_JIT_DUMP"))
            .transpose()?
            .unwrap_or(false);
        let safepoint_spin = std::env::var("EGCL_SAFEPOINT_SPIN")
            .ok()
            .map(|s| parse_usize(&s, "EGCL_SAFEPOINT_SPIN"))
            .transpose()?
            .unwrap_or(1000);
        let ffi_pool_pages = std::env::var("EGCL_FFI_POOL_PAGES")
            .ok()
            .map(|s| parse_usize(&s, "EGCL_FFI_POOL_PAGES"))
            .transpose()?
            .unwrap_or(4);
        let log_level = std::env::var("EGCL_LOG_LEVEL")
            .ok()
            .map(|s| parse_log_level(&s, "EGCL_LOG_LEVEL"))
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
    pub fn apply_cli_args(&mut self, args: &[String]) -> Result<(), EgclError> {
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--eval" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal("--eval requires an argument".into()));
                    }
                    self.eval_form = Some(args[i + 1].clone());
                    i += 2;
                }
                "--load" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal("--load requires an argument".into()));
                    }
                    self.load_file = Some(args[i + 1].clone());
                    i += 2;
                }
                "--heap-size" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal(
                            "--heap-size requires an argument".into(),
                        ));
                    }
                    self.heap_size = parse_size(&args[i + 1], "--heap-size")?;
                    i += 2;
                }
                "--tlab-size" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal(
                            "--tlab-size requires an argument".into(),
                        ));
                    }
                    self.tlab_size = parse_size(&args[i + 1], "--tlab-size")?;
                    i += 2;
                }
                "--nursery-size" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal(
                            "--nursery-size requires an argument".into(),
                        ));
                    }
                    self.nursery_size = parse_size(&args[i + 1], "--nursery-size")?;
                    i += 2;
                }
                "--stack-size" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal(
                            "--stack-size requires an argument".into(),
                        ));
                    }
                    self.stack_size = parse_size(&args[i + 1], "--stack-size")?;
                    i += 2;
                }
                "--workers" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal("--workers requires an argument".into()));
                    }
                    self.num_workers = parse_usize(&args[i + 1], "--workers")?;
                    i += 2;
                }
                "--image" => {
                    if i + 1 >= args.len() {
                        return Err(EgclError::Internal("--image requires an argument".into()));
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
                        return Err(EgclError::Internal("--gc-log requires an argument".into()));
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
                        return Err(EgclError::Internal(
                            "--log-level requires an argument".into(),
                        ));
                    }
                    self.log_level = parse_log_level(&args[i + 1], "--log-level")?;
                    i += 2;
                }
                flag => {
                    return Err(EgclError::Internal(format!("unknown flag: {}", flag)));
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

/// The top-level EGCL runtime instance.
pub struct Runtime {
    config: RuntimeConfig,
    shutdown: bool,
    /// The scheduler instance, initialized during Runtime::init.
    _scheduler: Scheduler,
    /// Owns the process-global heap/scheduler lifecycle while this Runtime lives.
    _lifecycle_guard: RuntimeLifecycleGuard,
}

impl Runtime {
    /// Initialize the runtime: parse config, init GC, init scheduler,
    /// load image, spawn workers. §2.2.
    pub fn init(config: RuntimeConfig) -> Result<Self, EgclError> {
        let lifecycle_guard = acquire_runtime_lifecycle();
        if let Some(hook) = RUNTIME_INIT_HOOK.get() {
            hook()?;
        }
        if config.heap_size == 0 {
            return Err(EgclError::Internal("heap_size must be non-zero".into()));
        }
        if config.nursery_size == 0 {
            return Err(EgclError::Internal("nursery_size must be non-zero".into()));
        }
        if config.stack_size == 0 {
            return Err(EgclError::Internal("stack_size must be non-zero".into()));
        }
        if config.num_workers == 0 {
            return Err(EgclError::Internal("num_workers must be non-zero".into()));
        }
        if !config.no_image {
            // The default image path is optional: if the bundled image is
            // absent we bootstrap from the prelude instead of failing. An
            // explicitly-named image, however, must exist.
            if let Some(image_path) = config.image_path.as_deref() {
                if !std::path::Path::new(image_path).is_file() && image_path != DEFAULT_IMAGE_PATH {
                    return Err(EgclError::InvalidImage(format!(
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
            _lifecycle_guard: lifecycle_guard,
        })
    }

    /// Run the CL entry point (REPL, --eval, or --load).
    /// Issue #10: actually use eval_form and load_file.
    pub fn run(&mut self) -> Result<i32, EgclError> {
        if self.shutdown {
            return Err(EgclError::Shutdown);
        }
        // If there's an eval form, evaluate it
        if let Some(ref form) = self.config.eval_form.clone() {
            let _result = self.eval(form)?;
            return Ok(0);
        }
        // If there's a load file, read and evaluate it
        if let Some(ref path) = self.config.load_file.clone() {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| EgclError::FileError(format!("cannot read {}: {}", path, e)))?;
            let _result = self.eval(&contents)?;
            return Ok(0);
        }
        self.run_repl()
    }

    /// Initiate graceful shutdown. §2.9.
    pub fn shutdown(&mut self) -> Result<(), EgclError> {
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
    /// cons/car/cdr, eq/eql, and list. It exists so `egcl-rt` can be exercised
    /// in isolation by its own unit/integration tests *without* depending on the
    /// higher `egcl` crate.
    ///
    /// It is NOT the language evaluator. The real, full evaluator that every
    /// user-facing behaviour (the `egcl` CLI/REPL, `--eval`, `--load`) runs
    /// through lives in `egcl` (`cli::eval_form`). Because the crate
    /// dependency points `egcl -> egcl-rt` (never the reverse), `egcl-rt`
    /// cannot call into that evaluator; the two are kept separate on purpose
    /// rather than one wrapping the other. Do not grow this bootstrap evaluator
    /// into a second language implementation — extend `cli::eval_form` instead,
    /// and treat any feature added here as test-scaffolding only.
    pub fn eval(&mut self, form: &str) -> Result<EgclVal, EgclError> {
        if self.shutdown {
            return Err(EgclError::Shutdown);
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
                .map_err(|e| EgclError::Internal(format!("read error: {}", e)))?;
            pos = next;
            result = eval_sexpr(&sexpr, &mut env)?;
        }
        Ok(result)
    }

    /// Get a reference to the runtime configuration.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    fn run_repl(&mut self) -> Result<i32, EgclError> {
        let stdin = io::stdin();
        let lock = stdin.lock();
        self.run_repl_with_reader(lock)
    }

    /// Run the REPL loop reading from an arbitrary `BufRead` source.
    ///
    /// `run_repl` calls this with the process stdin lock. It is exposed so tests
    /// (and embedders) can drive the REPL hermetically — e.g. `io::empty()` for
    /// an immediate EOF exit, or a `Cursor` of canned input — instead of
    /// blocking on interactive process stdin, which would hang a test harness
    /// that holds a serial lock and stall every test behind it.
    pub fn run_repl_with_reader<R: std::io::BufRead>(
        &mut self,
        mut reader: R,
    ) -> Result<i32, EgclError> {
        if self.shutdown {
            return Err(EgclError::Shutdown);
        }
        let mut input = String::new();

        loop {
            eprint!("EGCL> ");
            io::stderr().flush().map_err(|e| {
                EgclError::StreamError(format!("failed to flush REPL prompt: {}", e))
            })?;

            input.clear();
            match reader.read_line(&mut input) {
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
                    return Err(EgclError::StreamError(format!(
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
pub fn parse_cli(args: &[String]) -> Result<(RuntimeConfig, Vec<String>), EgclError> {
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
/// Global flag set by the SIGTERM handler to request orderly shutdown.
static SIGTERM_RECEIVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Global flag set by the SIGFPE handler to defer arithmetic-condition delivery.
static SIGFPE_RECEIVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Global flag set by the SIGPIPE handler to defer stream-condition delivery.
static SIGPIPE_RECEIVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Global flag set by the SIGSEGV handler for deferred null-guard TYPE-ERROR delivery.
static SIGSEGV_NULL_GUARD_RECEIVED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Global flag set by the SIGSEGV handler for deferred stack-overflow delivery.
static SIGSEGV_STACK_GUARD_RECEIVED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
const SIGSEGV_RECOVERY_SLOTS: usize = 128;
static SIGSEGV_RECOVERY_TIDS: [std::sync::atomic::AtomicUsize; SIGSEGV_RECOVERY_SLOTS] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; SIGSEGV_RECOVERY_SLOTS];
static SIGSEGV_NULL_GUARD_RECOVERY_IPS: [std::sync::atomic::AtomicUsize; SIGSEGV_RECOVERY_SLOTS] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; SIGSEGV_RECOVERY_SLOTS];
static SIGSEGV_STACK_GUARD_RECOVERY_IPS: [std::sync::atomic::AtomicUsize; SIGSEGV_RECOVERY_SLOTS] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; SIGSEGV_RECOVERY_SLOTS];
/// Per-thread delivery of the two recoverable SIGSEGV kinds, parallel to the
/// recovery-IP slots above.
///
/// These were process-global `AtomicBool`s consumed with `swap(false)`, so a
/// fault raised on one thread could be consumed by ANY other thread that polled
/// first — the faulting thread then saw no pending signal and returned a value
/// instead of signalling. A stack overflow in one thread could be swallowed by
/// another (egcl-2mqf). The globals remain as a fallback for threads that never
/// armed recovery and therefore own no slot.
static SIGSEGV_STACK_GUARD_RECEIVED_SLOTS: [std::sync::atomic::AtomicBool; SIGSEGV_RECOVERY_SLOTS] =
    [const { std::sync::atomic::AtomicBool::new(false) }; SIGSEGV_RECOVERY_SLOTS];
static SIGSEGV_NULL_GUARD_RECEIVED_SLOTS: [std::sync::atomic::AtomicBool; SIGSEGV_RECOVERY_SLOTS] =
    [const { std::sync::atomic::AtomicBool::new(false) }; SIGSEGV_RECOVERY_SLOTS];
const SIGSEGV_STACK_GUARD_SLOTS: usize = 128;
struct StackGuardChunk {
    addresses: [std::sync::atomic::AtomicUsize; SIGSEGV_STACK_GUARD_SLOTS],
    lengths: [std::sync::atomic::AtomicUsize; SIGSEGV_STACK_GUARD_SLOTS],
    next: std::sync::atomic::AtomicPtr<StackGuardChunk>,
}
impl StackGuardChunk {
    const fn new() -> Self {
        Self {
            addresses: [const { std::sync::atomic::AtomicUsize::new(0) };
                SIGSEGV_STACK_GUARD_SLOTS],
            lengths: [const { std::sync::atomic::AtomicUsize::new(0) }; SIGSEGV_STACK_GUARD_SLOTS],
            next: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
        }
    }
    fn next(&self) -> Option<&'static Self> {
        let next = self.next.load(std::sync::atomic::Ordering::Acquire);
        // Chunks are published once and never freed: signal handlers can walk
        // them without a lock, allocator, or reclamation handshake.
        unsafe { next.as_ref() }
    }
}
static STACK_GUARDS: StackGuardChunk = StackGuardChunk::new();
static STACK_GUARD_WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(unix)]
thread_local! {
    static SIGNAL_ALT_STACK: RefCell<Option<SignalStack>> = const { RefCell::new(None) };
}

#[cfg(unix)]
struct SignalStack {
    storage: Option<Box<[u8]>>,
    previous: crate::syscall::StackT,
}

#[cfg(unix)]
impl Drop for SignalStack {
    fn drop(&mut self) {
        let Some(storage) = self.storage.take() else {
            return;
        };
        let still_installed = crate::syscall::current_sigaltstack()
            .map(|current| current.ss_sp == storage.as_ptr().cast_mut())
            .unwrap_or(true);
        // Never free storage still registered with the kernel. A failed restore
        // is exceptional; retaining this thread's buffer is safer than a UAF.
        if still_installed
            && unsafe { crate::syscall::sigaltstack(&self.previous, std::ptr::null_mut()) }.is_err()
        {
            std::mem::forget(storage);
        }
    }
}

/// Ensure this OS thread has its own alternate stack, preserving a host's stack.
#[cfg(unix)]
pub fn ensure_signal_stack() -> Result<(), EgclError> {
    let previous = crate::syscall::current_sigaltstack()
        .map_err(|_| EgclError::SignalError(crate::syscall::SIGSEGV))?;
    if previous.ss_flags & crate::syscall::SS_DISABLE == 0 {
        return Ok(());
    }
    SIGNAL_ALT_STACK.with(|slot| {
        let storage = vec![0u8; 64 * 1024].into_boxed_slice();
        let stack = crate::syscall::StackT {
            ss_sp: storage.as_ptr().cast_mut(),
            ss_flags: 0,
            ss_size: storage.len(),
        };
        // The owning TLS value restores/disables the stack before freeing it.
        unsafe { crate::syscall::sigaltstack(&stack, std::ptr::null_mut()) }
            .map_err(|_| EgclError::SignalError(crate::syscall::SIGSEGV))?;
        *slot.borrow_mut() = Some(SignalStack {
            storage: Some(storage),
            previous,
        });
        Ok(())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigsegvFaultKind {
    SafepointPoll,
    StackGuard,
    NullGuard,
    Ordinary,
}

pub fn classify_sigsegv_address(addr: usize) -> SigsegvFaultKind {
    if crate::safepoint::safepoint_page_contains(addr) {
        SigsegvFaultKind::SafepointPoll
    } else if sigsegv_stack_guard_contains(addr) {
        SigsegvFaultKind::StackGuard
    } else if addr < 4096 {
        SigsegvFaultKind::NullGuard
    } else {
        SigsegvFaultKind::Ordinary
    }
}

/// Summary word for the six process signal flags: "at least one of them may be
/// set". Each `check_sig*` below consumes its flag with a `swap`, a locked
/// read-modify-write; testing all six cost ~15% of every native call
/// (bliss-htff) to answer a question that is "no" essentially always.
///
/// This is the process-global half of the per-thread poll word that both SBCL
/// (`thread-slot-ea thread-pseudo-atomic-bits-slot`, tested with one
/// instruction against a register-addressed thread slot) and HotSpot (the
/// thread-local `_polling_word`, one acquire load plus a bit test) use for the
/// same job. The per-thread half needs a cheap `current_thread()` first — see
/// bliss-htff's follow-up.
/// A monotonic COUNT of arrivals, not a clearable flag.
///
/// It was an `AtomicBool` that `take_process_signal_activity` cleared before
/// draining. That is correct with a single consumer and wrong with several: one
/// thread's clear hides the arrival from every other thread, whose own pending
/// flags then go undrained forever. Concretely, a stack-guard SIGSEGV raised on
/// thread A could be swallowed because thread B consumed the summary first, and
/// A's `run_native` returned a value where it should have signalled
/// STACK-OVERFLOW (egcl-2mqf). Each thread now compares against the last count
/// it drained, so no thread can consume another's notification.
static PROCESS_SIGNAL_ACTIVITY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// Arrival count this thread has already drained.
    static SEEN_SIGNAL_ACTIVITY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Announce that a process signal flag was just set. Called from signal
/// handlers, so it must stay async-signal-safe: one lock-free atomic increment.
pub fn mark_process_signal_activity() {
    PROCESS_SIGNAL_ACTIVITY.fetch_add(1, std::sync::atomic::Ordering::Release);
}

/// True if any process signal flag may be set, clearing the summary so a later
/// arrival re-arms it.
///
/// Clearing *before* the caller drains the individual flags is what makes this
/// safe. A signal landing mid-drain either has its flag swapped by that drain
/// (the handler sets the flag before this word), or re-sets this word after we
/// cleared it and is caught on the next call. Neither loses a signal; a
/// spurious `true` costs one redundant drain.
pub fn take_process_signal_activity() -> bool {
    let current = PROCESS_SIGNAL_ACTIVITY.load(std::sync::atomic::Ordering::Acquire);
    // Still one atomic load on the fast path; the comparison is thread-local, so
    // a drain by one thread cannot hide an arrival from another. During TLS
    // teardown, fall back to draining rather than risk dropping a signal.
    SEEN_SIGNAL_ACTIVITY
        .try_with(|seen| {
            if seen.get() == current {
                false
            } else {
                seen.set(current);
                true
            }
        })
        .unwrap_or(true)
}

/// Check whether a SIGINT has been received since the last check.
pub fn check_sigint() -> bool {
    SIGINT_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Check whether SIGTERM has requested orderly shutdown since the last check.
pub fn check_sigterm() -> bool {
    SIGTERM_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Check whether SIGFPE has been received since the last check.
pub fn check_sigfpe() -> bool {
    SIGFPE_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Check whether SIGPIPE has been received since the last output operation.
pub fn check_sigpipe() -> bool {
    SIGPIPE_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Check whether a null-guard SIGSEGV has been classified since the last check.
pub fn check_sigsegv_null_guard() -> bool {
    // Same per-thread delivery as the stack guard above.
    if let Some(i) = sigsegv_recovery_slot_index(crate::syscall::cached_tid() as usize)
        && SIGSEGV_NULL_GUARD_RECEIVED_SLOTS[i].swap(false, std::sync::atomic::Ordering::Relaxed)
    {
        return true;
    }
    SIGSEGV_NULL_GUARD_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

pub fn post_sigsegv_null_guard() {
    if let Some(i) = sigsegv_recovery_slot_index(crate::syscall::gettid() as usize) {
        SIGSEGV_NULL_GUARD_RECEIVED_SLOTS[i].store(true, std::sync::atomic::Ordering::Relaxed);
        mark_process_signal_activity();
        return;
    }
    SIGSEGV_NULL_GUARD_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    mark_process_signal_activity();
}

/// Publish BOTH recovery IPs through a SINGLE slot resolution.
///
/// The two single-guard setters below each do their own `cached_tid()` plus slot
/// resolution, and the native-entry adapter called them back to back — two full
/// resolutions on every native call, measured at 23.96% + 16.89% of a 200M-call
/// loop (bliss-dsr8). Callers that set both (the c2i adapters) MUST use this.
pub fn set_sigsegv_recovery_ips(null_guard_ip: usize, stack_guard_ip: usize) {
    let tid = crate::syscall::cached_tid() as usize;
    if let Some(slot) = sigsegv_recovery_slot_for_tid(tid) {
        SIGSEGV_NULL_GUARD_RECOVERY_IPS[slot]
            .store(null_guard_ip, std::sync::atomic::Ordering::Release);
        SIGSEGV_STACK_GUARD_RECOVERY_IPS[slot]
            .store(stack_guard_ip, std::sync::atomic::Ordering::Release);
    }
}

pub fn set_sigsegv_null_guard_recovery_ip(ip: usize) {
    let tid = crate::syscall::cached_tid() as usize;
    if let Some(slot) = sigsegv_recovery_slot_for_tid(tid) {
        SIGSEGV_NULL_GUARD_RECOVERY_IPS[slot].store(ip, std::sync::atomic::Ordering::Release);
    }
}

pub fn current_sigsegv_null_guard_recovery_ip() -> usize {
    sigsegv_null_guard_recovery_ip_for_tid(crate::syscall::cached_tid() as usize)
}

/// Check whether a stack-guard SIGSEGV has been classified since the last check.
pub fn check_sigsegv_stack_guard() -> bool {
    // This thread's own delivery first, so it cannot consume a fault raised on
    // another thread.
    if let Some(i) = sigsegv_recovery_slot_index(crate::syscall::cached_tid() as usize)
        && SIGSEGV_STACK_GUARD_RECEIVED_SLOTS[i].swap(false, std::sync::atomic::Ordering::Relaxed)
    {
        return true;
    }
    SIGSEGV_STACK_GUARD_RECEIVED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

pub fn post_sigsegv_stack_guard() {
    // Signal-handler context. `sigsegv_recovery_slot_index` is a linear scan of
    // atomics that takes no lock and touches no thread-local, so it is safe
    // here; the CLAIMING path is not, which is why a thread with no slot falls
    // back to the process-global flag.
    if let Some(i) = sigsegv_recovery_slot_index(crate::syscall::gettid() as usize) {
        SIGSEGV_STACK_GUARD_RECEIVED_SLOTS[i].store(true, std::sync::atomic::Ordering::Relaxed);
    } else {
        SIGSEGV_STACK_GUARD_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    mark_process_signal_activity();
}

pub fn set_sigsegv_stack_guard_recovery_ip(ip: usize) {
    let tid = crate::syscall::cached_tid() as usize;
    if let Some(slot) = sigsegv_recovery_slot_for_tid(tid) {
        SIGSEGV_STACK_GUARD_RECOVERY_IPS[slot].store(ip, std::sync::atomic::Ordering::Release);
    }
}

pub fn current_sigsegv_stack_guard_recovery_ip() -> usize {
    sigsegv_stack_guard_recovery_ip_for_tid(crate::syscall::cached_tid() as usize)
}

/// Fault state belongs to an execution, while signal handlers find the mounted
/// execution through its carrier's existing allocation-free TID slot.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct NativeFaultState {
    null_ip: usize,
    stack_ip: usize,
    null_pending: bool,
    stack_pending: bool,
}

#[derive(Default)]
pub(crate) struct FiberFaultState(std::cell::Cell<NativeFaultState>);

impl FiberFaultState {
    /// Called only by the carrier with exclusive ownership of this mount. The
    /// scope stays on the carrier stack and ends after the fiber switches out.
    pub(crate) fn mount(&self) -> Result<MountedFiberFaultState<'_>, EgclError> {
        let carrier = exchange_native_fault_state(self.0.get())?;
        Ok(MountedFiberFaultState {
            fiber: self,
            carrier,
        })
    }
}

pub(crate) struct MountedFiberFaultState<'a> {
    fiber: &'a FiberFaultState,
    carrier: NativeFaultState,
}

impl Drop for MountedFiberFaultState<'_> {
    fn drop(&mut self) {
        // A slot used by this mount remains owned until the carrier exits, so
        // restoring its original state cannot need a new slot allocation.
        let saved = exchange_native_fault_state(self.carrier)
            .expect("mounted carrier lost its native fault slot");
        self.fiber.0.set(saved);
    }
}

fn exchange_native_fault_state(incoming: NativeFaultState) -> Result<NativeFaultState, EgclError> {
    use std::sync::atomic::Ordering;

    let tid = crate::syscall::gettid() as usize;
    let slot = match sigsegv_recovery_slot_index(tid) {
        Some(slot) => slot,
        // Fibers that never arm native recovery must not consume a scarce TID
        // slot merely by running. The exit exchange detects a slot first claimed
        // inside the fiber, and saves that execution's state as usual.
        None if incoming == NativeFaultState::default() => return Ok(incoming),
        None => sigsegv_recovery_slot_for_tid(tid).ok_or_else(|| {
            EgclError::Internal("no native fault slot available for fiber resume".into())
        })?,
    };
    let outgoing = NativeFaultState {
        null_ip: SIGSEGV_NULL_GUARD_RECOVERY_IPS[slot].swap(incoming.null_ip, Ordering::AcqRel),
        stack_ip: SIGSEGV_STACK_GUARD_RECOVERY_IPS[slot].swap(incoming.stack_ip, Ordering::AcqRel),
        null_pending: SIGSEGV_NULL_GUARD_RECEIVED_SLOTS[slot]
            .swap(incoming.null_pending, Ordering::AcqRel),
        stack_pending: SIGSEGV_STACK_GUARD_RECEIVED_SLOTS[slot]
            .swap(incoming.stack_pending, Ordering::AcqRel),
    };
    if incoming.null_pending || incoming.stack_pending {
        // Another fiber may already have drained this carrier's activity epoch.
        // Re-publish the restored pending flags so their owner will check them.
        mark_process_signal_activity();
    }
    Ok(outgoing)
}

#[cfg(test)]
mod fiber_fault_tests {
    use super::*;

    #[test]
    fn mounting_preserves_the_carrier_and_republishes_pending_fiber_faults() {
        std::thread::spawn(|| {
            set_sigsegv_recovery_ips(0x1000, 0x2000);
            post_sigsegv_null_guard();
            let fiber = FiberFaultState::default();
            {
                let _mounted = fiber.mount().unwrap();
                assert_eq!(current_sigsegv_null_guard_recovery_ip(), 0);
                assert_eq!(current_sigsegv_stack_guard_recovery_ip(), 0);
                set_sigsegv_recovery_ips(0x3000, 0x4000);
                post_sigsegv_stack_guard();
            }
            assert_eq!(current_sigsegv_null_guard_recovery_ip(), 0x1000);
            assert_eq!(current_sigsegv_stack_guard_recovery_ip(), 0x2000);
            assert!(check_sigsegv_null_guard());
            // Simulate a different execution draining the carrier's epoch
            // before the fault's owning fiber is remounted.
            take_process_signal_activity();
            {
                let _mounted = fiber.mount().unwrap();
                assert_eq!(current_sigsegv_null_guard_recovery_ip(), 0x3000);
                assert_eq!(current_sigsegv_stack_guard_recovery_ip(), 0x4000);
                assert!(take_process_signal_activity());
                assert!(check_sigsegv_stack_guard());
            }
            assert_eq!(current_sigsegv_null_guard_recovery_ip(), 0x1000);
            assert_eq!(current_sigsegv_stack_guard_recovery_ip(), 0x2000);
        })
        .join()
        .unwrap();
    }
}

pub fn register_sigsegv_stack_guard_range(addr: usize, len: usize) {
    if addr == 0 || len == 0 {
        return;
    }
    let _writer = STACK_GUARD_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let mut chunk = &STACK_GUARDS;
    let mut vacant = None;
    loop {
        for i in 0..SIGSEGV_STACK_GUARD_SLOTS {
            let old = chunk.addresses[i].load(std::sync::atomic::Ordering::Relaxed);
            if old == addr {
                chunk.lengths[i].store(len, std::sync::atomic::Ordering::Release);
                return;
            }
            if old == 0 && vacant.is_none() {
                vacant = Some((chunk, i));
            }
        }
        if let Some(next) = chunk.next() {
            chunk = next;
        } else {
            break;
        }
    }
    let (target, i) = vacant.unwrap_or_else(|| {
        let next = Box::leak(Box::new(StackGuardChunk::new()));
        chunk.next.store(next, std::sync::atomic::Ordering::Release);
        (&*next, 0)
    });
    target.addresses[i].store(addr, std::sync::atomic::Ordering::Relaxed);
    target.lengths[i].store(len, std::sync::atomic::Ordering::Release);
}

pub fn unregister_sigsegv_stack_guard_range(addr: usize) {
    if addr == 0 {
        return;
    }
    let _writer = STACK_GUARD_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let mut chunk = &STACK_GUARDS;
    loop {
        for i in 0..SIGSEGV_STACK_GUARD_SLOTS {
            if chunk.addresses[i].load(std::sync::atomic::Ordering::Relaxed) == addr {
                chunk.lengths[i].store(0, std::sync::atomic::Ordering::Release);
                chunk.addresses[i].store(0, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        }
        if let Some(next) = chunk.next() {
            chunk = next;
        } else {
            return;
        }
    }
}

/// Capability checked by optional JVM bindings before loading native state.
pub fn supports_jvm_coexistence() -> bool {
    cfg!(all(
        feature = "c-ffi",
        target_arch = "x86_64",
        target_os = "linux",
        target_env = "gnu"
    ))
}

/// Why [`supports_jvm_coexistence`] said no, as a sentence a user can act on.
///
/// Every input is a `cfg!` this crate can see and a caller cannot: `*FEATURES*`
/// carries the architecture and the OS but never `target_env` or a Cargo feature,
/// so from Lisp a static musl build, a build without `egcl-rt/c-ffi`, and a
/// non-x86-64 host are one indistinguishable "unsupported" (bliss-hllzi). Listing
/// only the *unmet* conditions keeps the message short when a single one is wrong,
/// which is the usual case.
pub fn jvm_coexistence_diagnostic() -> String {
    let mut unmet: Vec<String> = Vec::new();
    if !cfg!(feature = "c-ffi") {
        unmet.push("it was built without the egcl-rt/c-ffi feature".into());
    }
    if !cfg!(target_arch = "x86_64") {
        unmet.push(format!(
            "its architecture is {}, and only x86-64 has a JVM bridge so far",
            std::env::consts::ARCH
        ));
    }
    if !cfg!(target_os = "linux") {
        unmet.push(format!("its OS is {}, not linux", std::env::consts::OS));
    }
    if !cfg!(target_env = "gnu") {
        // Naming musl explicitly is worth a branch: it is the default target here,
        // so "you are running the musl binary" is the single most likely answer.
        unmet.push(if cfg!(target_env = "musl") {
            "it is a musl build, which links statically and cannot dlopen libjvm.so; \
             a glibc (gnu) build is required"
                .into()
        } else {
            "it is not a glibc (gnu) build".into()
        });
    }
    match unmet.len() {
        0 => "JVM coexistence is supported by this build".into(),
        1 => unmet.pop().expect("length checked"),
        _ => {
            let last = unmet.pop().expect("length checked");
            format!("{}, and {last}", unmet.join(", "))
        }
    }
}

/// Install signal handlers (SIGSEGV, SIGINT, SIGTERM, etc.). §2.6.
/// Issue #11: actually install at least SIGINT and SIGTERM using libc.
#[cfg(unix)]
pub fn install_signal_handlers() -> Result<(), EgclError> {
    ensure_signal_stack()?;
    // Handler installation is process-wide; stack ownership is per-thread.
    // In particular, a safepoint fallback must not replace a VM installed later.
    static INSTALLED: Mutex<bool> = Mutex::new(false);
    let mut installed = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    if !*installed {
        install_process_signal_handlers()?;
        *installed = true;
    }
    Ok(())
}

#[cfg(unix)]
fn install_process_signal_handlers() -> Result<(), EgclError> {
    use crate::syscall;
    // Dynamic GNU builds can participate in HotSpot's documented libjsig
    // interposition. Refuse JVM-first startup without it before changing any
    // process disposition: silently taking over HotSpot's faults is unsafe.
    #[cfg(all(feature = "c-ffi", target_os = "linux", target_env = "gnu"))]
    unsafe {
        let query = libc::dlsym(libc::RTLD_DEFAULT, c"JNI_GetCreatedJavaVMs".as_ptr());
        if !query.is_null() {
            let query: unsafe extern "system" fn(*mut *mut std::ffi::c_void, i32, *mut i32) -> i32 =
                std::mem::transmute(query);
            let mut count = 0;
            if query(std::ptr::null_mut(), 0, &mut count) != 0 {
                return Err(EgclError::FfiError("cannot query the existing JVM".into()));
            }
            let chaining = libc::dlsym(libc::RTLD_DEFAULT, c"JVM_get_signal_action".as_ptr());
            let mut installed: libc::Dl_info = std::mem::zeroed();
            let mut provider: libc::Dl_info = std::mem::zeroed();
            let interposed = !chaining.is_null()
                && libc::dladdr(libc::sigaction as *const () as *const _, &mut installed) != 0
                && libc::dladdr(chaining, &mut provider) != 0
                && installed.dli_fbase == provider.dli_fbase;
            if count > 0 && !interposed {
                return Err(EgclError::FfiError(
                    "JVM already running: preload the JDK libjsig.so before starting EGCL".into(),
                ));
            }
        }
    }

    unsafe fn install(sig: i32, handler: usize, flags: u64) -> Result<(), i32> {
        #[cfg(all(feature = "c-ffi", target_os = "linux", target_env = "gnu"))]
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler;
            action.sa_flags = flags as i32;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(sig, &action, std::ptr::null_mut()) == 0 {
                Ok(())
            } else {
                Err(*libc::__errno_location())
            }
        }
        #[cfg(not(all(feature = "c-ffi", target_os = "linux", target_env = "gnu")))]
        {
            unsafe { syscall::rt_sigaction(sig, handler, flags) }
        }
    }
    // SIGINT/TERM/SEGV use SA_RESTART; SIGUSR1
    // (the safepoint interrupt) deliberately omits SA_RESTART so a blocking
    // syscall returns EINTR and reaches the next safepoint.
    // SAFETY: each handler is a valid extern "C" fn(i32).
    unsafe {
        install(
            syscall::SIGSEGV,
            sigsegv_handler as *const () as usize,
            syscall::SA_RESTART | syscall::SA_ONSTACK | syscall::SA_SIGINFO,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGSEGV))?;
        #[cfg(target_os = "macos")]
        install(
            syscall::SIGBUS,
            sigsegv_handler as *const () as usize,
            syscall::SA_RESTART | syscall::SA_ONSTACK | syscall::SA_SIGINFO,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGBUS))?;
        install(
            syscall::SIGINT,
            sigint_handler as *const () as usize,
            syscall::SA_RESTART,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGINT))?;
        install(
            syscall::SIGTERM,
            sigterm_handler as *const () as usize,
            syscall::SA_RESTART,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGTERM))?;
        install(
            syscall::SIGALRM,
            sigalrm_handler as *const () as usize,
            syscall::SA_RESTART,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGALRM))?;
        install(
            syscall::SIGFPE,
            sigfpe_handler as *const () as usize,
            syscall::SA_RESTART,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGFPE))?;
        install(
            syscall::SIGPIPE,
            sigpipe_handler as *const () as usize,
            syscall::SA_RESTART,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGPIPE))?;
        install(
            syscall::SIGUSR1,
            crate::safepoint::sigusr1_handler as *const () as usize,
            0,
        )
        .map_err(|_| EgclError::SignalError(syscall::SIGUSR1))?;
    }
    Ok(())
}

extern "C" fn sigint_handler(_sig: i32) {
    // With rt_sigaction the handler stays installed (no SysV one-shot reset), so
    // no re-arming is needed.
    SIGINT_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    mark_process_signal_activity();
}

/// Grace period between SIGTERM and the SIGALRM hard exit (seconds).
#[cfg(unix)]
const SIGTERM_GRACE_SECS: u32 = 5;

#[cfg(unix)]
extern "C" fn sigterm_handler(_sig: i32) {
    SIGTERM_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    mark_process_signal_activity();
    // Arm a hard deadline (bliss-siv7): the cooperative shutdown flag only
    // works where code polls it — a hot T2 native loop has no back-edge poll
    // yet, so a SIGTERM'd process could spin until SIGKILL. If we are still
    // alive when the alarm fires, the SIGALRM handler exits 128+15, matching
    // systemd/timeout escalation semantics. A graceful shutdown that finishes
    // inside the grace period exits first and the alarm dies with the process.
    let _ = crate::syscall::alarm(SIGTERM_GRACE_SECS);
}

#[cfg(unix)]
extern "C" fn sigalrm_handler(_sig: i32) {
    // Only armed by sigterm_handler. Still alive => the cooperative shutdown
    // never ran (or stalled); terminate every thread now.
    crate::syscall::dbg_write(b"egcl: SIGTERM grace period expired; exiting\n");
    crate::syscall::exit_group(128 + crate::syscall::SIGTERM);
}

#[cfg(unix)]
extern "C" fn sigfpe_handler(_sig: i32) {
    SIGFPE_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    mark_process_signal_activity();
}

#[cfg(unix)]
extern "C" fn sigpipe_handler(_sig: i32) {
    SIGPIPE_RECEIVED.store(true, std::sync::atomic::Ordering::Relaxed);
    mark_process_signal_activity();
}

#[cfg(unix)]
extern "C" fn sigsegv_handler(
    sig: i32,
    _info: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) {
    let addr = siginfo_fault_addr(_info);
    match classify_sigsegv_address(addr) {
        SigsegvFaultKind::SafepointPoll => {
            if crate::safepoint::recover_poll_page_sigsegv() {
                return;
            }
            crate::syscall::dbg_write(b"egcl: safepoint poll SIGSEGV\n")
        }
        SigsegvFaultKind::NullGuard => {
            post_sigsegv_null_guard();
            let recovery_ip =
                sigsegv_null_guard_recovery_ip_for_tid(crate::syscall::gettid() as usize);
            if recovery_ip != 0 && rewrite_ucontext_ip(_context, recovery_ip) {
                return;
            }
            crate::syscall::dbg_write(b"egcl: null guard SIGSEGV\n")
        }
        SigsegvFaultKind::StackGuard => {
            post_sigsegv_stack_guard();
            let recovery_ip =
                sigsegv_stack_guard_recovery_ip_for_tid(crate::syscall::gettid() as usize);
            if recovery_ip != 0 && rewrite_ucontext_ip(_context, recovery_ip) {
                return;
            }
            crate::syscall::dbg_write(b"egcl: stack guard SIGSEGV\n")
        }
        SigsegvFaultKind::Ordinary => {
            crate::syscall::dbg_write(b"egcl: unhandled memory fault\n");
            #[cfg(all(target_os = "linux", target_arch = "s390x"))]
            {
                // Allocation-free diagnostics: fault address and the PSW
                // address (the s390x program counter, at the same offset
                // rewrite_ucontext_ip writes) in hex.
                let mut buf = [0u8; 64];
                let mut n = 0;
                let mut put = |bytes: &[u8], n: &mut usize| {
                    for &b in bytes {
                        if *n < buf.len() {
                            buf[*n] = b;
                            *n += 1;
                        }
                    }
                };
                let hex = |mut v: usize, out: &mut [u8; 16]| {
                    for i in (0..16).rev() {
                        let d = (v & 0xF) as u8;
                        out[i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
                        v >>= 4;
                    }
                };
                let mut h = [0u8; 16];
                put(b"addr=0x", &mut n);
                hex(addr, &mut h);
                put(&h, &mut n);
                const UCONTEXT_PSW_ADDR_OFFSET: usize = 48;
                let psw_addr = if _context.is_null() {
                    0
                } else {
                    unsafe {
                        core::ptr::read_unaligned(
                            (_context as *const u8).add(UCONTEXT_PSW_ADDR_OFFSET) as *const usize,
                        )
                    }
                };
                put(b" psw=0x", &mut n);
                hex(psw_addr, &mut h);
                put(&h, &mut n);
                put(b"\n", &mut n);
                crate::syscall::dbg_write(&buf[..n]);
            }
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            {
                // Allocation-free diagnostics: fault address and RIP in hex.
                let mut buf = [0u8; 64];
                let mut n = 0;
                let mut put = |bytes: &[u8], n: &mut usize| {
                    for &b in bytes {
                        if *n < buf.len() {
                            buf[*n] = b;
                            *n += 1;
                        }
                    }
                };
                let hex = |mut v: usize, out: &mut [u8; 16]| {
                    for i in (0..16).rev() {
                        let d = (v & 0xF) as u8;
                        out[i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
                        v >>= 4;
                    }
                };
                let mut h = [0u8; 16];
                put(b"addr=0x", &mut n);
                hex(addr, &mut h);
                put(&h, &mut n);
                const UCONTEXT_RIP_OFFSET: usize = 168;
                let rip = if _context.is_null() {
                    0
                } else {
                    unsafe {
                        core::ptr::read_unaligned(
                            (_context as *const u8).add(UCONTEXT_RIP_OFFSET) as *const usize
                        )
                    }
                };
                put(b" rip=0x", &mut n);
                hex(rip, &mut h);
                put(&h, &mut n);
                put(b"\n", &mut n);
                crate::syscall::dbg_write(&buf[..n]);
                // Registers and the stack top, for wild-jump forensics.
                let greg = |idx: usize| -> usize {
                    if _context.is_null() {
                        0
                    } else {
                        unsafe {
                            core::ptr::read_unaligned(
                                (_context as *const u8).add(40 + idx * 8) as *const usize
                            )
                        }
                    }
                };
                // Linux x86_64 gregs order: R8 R9 R10 R11 R12 R13 R14 R15 RDI RSI
                // RBP RBX RDX RAX RCX RSP RIP.
                let names: [&[u8]; 16] = [
                    b"r8 ", b"r9 ", b"r10", b"r11", b"r12", b"r13", b"r14", b"r15", b"rdi", b"rsi",
                    b"rbp", b"rbx", b"rdx", b"rax", b"rcx", b"rsp",
                ];
                for (i, name) in names.iter().enumerate() {
                    let mut n2 = 0;
                    let mut b2 = [0u8; 32];
                    for &c in name.iter() {
                        b2[n2] = c;
                        n2 += 1;
                    }
                    b2[n2] = b'=';
                    n2 += 1;
                    let mut h2 = [0u8; 16];
                    hex(greg(i), &mut h2);
                    for &c in h2.iter() {
                        b2[n2] = c;
                        n2 += 1;
                    }
                    b2[n2] = b'\n';
                    n2 += 1;
                    crate::syscall::dbg_write(&b2[..n2]);
                }
                let rsp = greg(15);
                for k in 0..8usize {
                    let v = unsafe { core::ptr::read_unaligned((rsp + k * 8) as *const usize) };
                    let mut n2 = 0;
                    let mut b2 = [0u8; 40];
                    for &c in b"stk".iter() {
                        b2[n2] = c;
                        n2 += 1;
                    }
                    b2[n2] = b'0' + k as u8;
                    n2 += 1;
                    b2[n2] = b'=';
                    n2 += 1;
                    let mut h2 = [0u8; 16];
                    hex(v, &mut h2);
                    for &c in h2.iter() {
                        b2[n2] = c;
                        n2 += 1;
                    }
                    b2[n2] = b'\n';
                    n2 += 1;
                    crate::syscall::dbg_write(&b2[..n2]);
                }
            }
        }
    }
    crate::syscall::exit_group(128 + sig);
}

/// Releases this thread's SIGSEGV-recovery slot when the thread exits.
///
/// Without this the slot leaked: `SIGSEGV_RECOVERY_TIDS` was only ever moved
/// `0 -> tid` and never back. Two things then go wrong, because the kernel
/// RECYCLES thread ids:
///
///  * a new thread handed a dead thread's tid matched the dead thread's slot
///    and inherited its recovery IPs — including a `0` meaning "native SIGSEGV
///    recovery disabled", which silently turns a recoverable native stack
///    overflow into a hard crash instead of a Lisp STACK-OVERFLOW; and
///  * the table (128 entries) filled permanently, after which
///    `set_sigsegv_*_recovery_ip` became a silent no-op for every new thread.
///
/// Observed as `run_native_rewrites_stack_guard_sigsegv_to_stack_overflow`
/// failing only in the PARALLEL lib-test run (it passes alone and under
/// `--test-threads=1`): a neighbouring test disables recovery, its thread
/// exits, and the next test's thread is given the same tid.
struct SigsegvSlotGuard {
    slot: std::cell::Cell<Option<usize>>,
}

impl Drop for SigsegvSlotGuard {
    fn drop(&mut self) {
        if let Some(i) = self.slot.get() {
            clear_sigsegv_recovery_slot(i);
            // The fast-path cache must not outlive the slot it names. Writing it
            // here may fail if that thread-local is already destroyed, which is
            // harmless: the thread is going away and the cache dies with it.
            let _ = SIGSEGV_SLOT_CACHE.try_with(|c| c.set(0));
        }
    }
}

thread_local! {
    static SIGSEGV_SLOT: SigsegvSlotGuard = const {
        SigsegvSlotGuard { slot: std::cell::Cell::new(None) }
    };

    /// Fast-path copy of this thread's slot, stored as `index + 1` so that `0`
    /// means "not claimed yet" and the whole thing needs no `Option` and no
    /// initialisation.
    ///
    /// This exists ONLY because `SIGSEGV_SLOT` above has a `Drop` impl. A
    /// `thread_local!` whose type implements `Drop` cannot use the const-init
    /// fast path: every access has to carry lazy-initialisation state and
    /// register a destructor, so even `try_with` on it compiles to a state-byte
    /// load, two branches, and a possible indirect call. That measured as 19.02%
    /// of a 200M-call native loop, with its two callers a further 24% and 17%
    /// (bliss-dsr8). A const-init `Cell<usize>` with no `Drop` compiles instead
    /// to a single `mov %fs:disp`.
    ///
    /// `SIGSEGV_SLOT` keeps the release-at-thread-exit behaviour; this is only a
    /// cache of the index it holds, so the two must be written together.
    static SIGSEGV_SLOT_CACHE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Blank a slot and only then release its tid, so a concurrent reader can never
/// see the tid published with another thread's recovery IPs still attached.
fn clear_sigsegv_recovery_slot(i: usize) {
    SIGSEGV_STACK_GUARD_RECOVERY_IPS[i].store(0, std::sync::atomic::Ordering::Release);
    SIGSEGV_NULL_GUARD_RECOVERY_IPS[i].store(0, std::sync::atomic::Ordering::Release);
    // Undelivered flags must not outlive the slot, or the next owner of this
    // slot would observe the previous thread's fault.
    SIGSEGV_STACK_GUARD_RECEIVED_SLOTS[i].store(false, std::sync::atomic::Ordering::Release);
    SIGSEGV_NULL_GUARD_RECEIVED_SLOTS[i].store(false, std::sync::atomic::Ordering::Release);
    SIGSEGV_RECOVERY_TIDS[i].store(0, std::sync::atomic::Ordering::Release);
}

// Do not let a yielding caller retain this carrier's TLS cache address.
#[inline(never)]
fn sigsegv_recovery_slot_for_tid(tid: usize) -> Option<usize> {
    if tid == 0 {
        return None;
    }

    // Fast path: this thread already owns a slot. Keyed on thread-local state,
    // not on the tid, so a recycled tid cannot masquerade as the same thread.
    // Read the non-Drop CACHE rather than the guard: one `mov %fs:disp` instead
    // of a lazy-init `try_with` (see SIGSEGV_SLOT_CACHE; bliss-dsr8).
    if let Ok(cached) = SIGSEGV_SLOT_CACHE.try_with(|c| c.get())
        && cached != 0
    {
        return Some(cached - 1);
    }

    // First touch from this thread. Any slot still carrying our tid belongs to a
    // dead thread whose tid was recycled (or whose TLS destructor never ran).
    // It must go: the signal handler resolves a tid by taking the FIRST matching
    // slot, so a stale duplicate would shadow the one we are about to claim.
    for (i, owner) in SIGSEGV_RECOVERY_TIDS.iter().enumerate() {
        if owner.load(std::sync::atomic::Ordering::Acquire) == tid {
            clear_sigsegv_recovery_slot(i);
        }
    }

    for i in 0..SIGSEGV_RECOVERY_SLOTS {
        if SIGSEGV_RECOVERY_TIDS[i]
            .compare_exchange(
                0,
                tid,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            // Start from a known state rather than whatever the previous owner
            // left, and arm release at thread exit. If the thread-local is
            // already being destroyed we still hand back the slot; it is then
            // reclaimed by the stale-slot sweep above when the tid is reused.
            SIGSEGV_STACK_GUARD_RECOVERY_IPS[i].store(0, std::sync::atomic::Ordering::Release);
            SIGSEGV_NULL_GUARD_RECOVERY_IPS[i].store(0, std::sync::atomic::Ordering::Release);
            // Guard first (it owns release at thread exit), then the cache, so a
            // visible cache entry always names a slot the guard will release.
            let _ = SIGSEGV_SLOT.try_with(|g| g.slot.set(Some(i)));
            let _ = SIGSEGV_SLOT_CACHE.try_with(|c| c.set(i + 1));
            return Some(i);
        }
    }

    None
}

fn sigsegv_recovery_slot_index(tid: usize) -> Option<usize> {
    if tid == 0 {
        return None;
    }
    SIGSEGV_RECOVERY_TIDS
        .iter()
        .position(|owner| owner.load(std::sync::atomic::Ordering::Acquire) == tid)
}

fn sigsegv_null_guard_recovery_ip_for_tid(tid: usize) -> usize {
    sigsegv_recovery_slot_index(tid).map_or(0, |slot| {
        SIGSEGV_NULL_GUARD_RECOVERY_IPS[slot].load(std::sync::atomic::Ordering::Acquire)
    })
}

fn sigsegv_stack_guard_recovery_ip_for_tid(tid: usize) -> usize {
    sigsegv_recovery_slot_index(tid).map_or(0, |slot| {
        SIGSEGV_STACK_GUARD_RECOVERY_IPS[slot].load(std::sync::atomic::Ordering::Acquire)
    })
}

fn sigsegv_stack_guard_contains(addr: usize) -> bool {
    let mut chunk = &STACK_GUARDS;
    loop {
        for i in 0..SIGSEGV_STACK_GUARD_SLOTS {
            let len = chunk.lengths[i].load(std::sync::atomic::Ordering::Acquire);
            if len == 0 {
                continue;
            }
            let base = chunk.addresses[i].load(std::sync::atomic::Ordering::Relaxed);
            if let Some(end) = base.checked_add(len) {
                if addr >= base && addr < end {
                    return true;
                }
            }
        }
        if let Some(next) = chunk.next() {
            chunk = next;
        } else {
            return false;
        }
    }
}

#[cfg(target_os = "macos")]
fn siginfo_fault_addr(info: *mut core::ffi::c_void) -> usize {
    if info.is_null() {
        return usize::MAX;
    }
    unsafe { (*(info as *const libc::siginfo_t)).si_addr as usize }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn siginfo_fault_addr(info: *mut core::ffi::c_void) -> usize {
    if info.is_null() {
        return usize::MAX;
    }
    // Linux siginfo_t stores si_addr for SIGSEGV at offset 16 on the supported
    // 64-bit ABIs. This is read-only, allocation-free handler work.
    unsafe { core::ptr::read_unaligned((info as *const u8).add(16) as *const usize) }
}

#[cfg(all(unix, target_arch = "x86_64"))]
fn rewrite_ucontext_ip(context: *mut core::ffi::c_void, ip: usize) -> bool {
    if context.is_null() {
        return false;
    }
    // Linux x86_64 ucontext_t begins with:
    // uc_flags:8, uc_link:8, stack_t:24, then mcontext_t.gregs.
    // REG_RIP is gregs[16], so RIP lives at byte offset 40 + 16 * 8.
    const UCONTEXT_RIP_OFFSET: usize = 168;
    unsafe {
        core::ptr::write_unaligned(
            (context as *mut u8).add(UCONTEXT_RIP_OFFSET) as *mut usize,
            ip,
        );
    }
    true
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn rewrite_ucontext_ip(context: *mut core::ffi::c_void, ip: usize) -> bool {
    unsafe extern "C" {
        fn egcl_macos_rewrite_ucontext_pc(context: *mut core::ffi::c_void, pc: usize) -> bool;
    }
    unsafe { egcl_macos_rewrite_ucontext_pc(context, ip) }
}

#[cfg(all(target_os = "linux", target_arch = "s390x"))]
fn rewrite_ucontext_ip(context: *mut core::ffi::c_void, ip: usize) -> bool {
    if context.is_null() {
        return false;
    }
    // Linux s390x ucontext_t begins with uc_flags:8, uc_link:8, stack_t:24,
    // then the _sigregs mcontext whose first member is the PSW: mask:8,
    // addr:8. So the resume address lives at byte offset 40 + 8 = 48 --
    // measured with offsetof(ucontext_t, uc_mcontext.psw.addr) on real
    // hardware (cfarm191, Debian 13, glibc 2.41), not read off a header.
    // Rewriting it is how a null-guard or stack-guard fault resumes at the
    // thread's published recovery address instead of dying (bliss-mfjwt).
    const UCONTEXT_PSW_ADDR_OFFSET: usize = 48;
    unsafe {
        core::ptr::write_unaligned(
            (context as *mut u8).add(UCONTEXT_PSW_ADDR_OFFSET) as *mut usize,
            ip,
        );
    }
    true
}

#[cfg(all(
    unix,
    not(target_arch = "x86_64"),
    not(target_os = "macos"),
    not(all(target_os = "linux", target_arch = "s390x"))
))]
fn rewrite_ucontext_ip(_context: *mut core::ffi::c_void, _ip: usize) -> bool {
    false
}

// ══════════════════════════════════════════════════════════════════
// Bootstrap evaluator — a minimal Lisp interpreter for runtime-level
// integration before the compiler crate is wired up.
// ══════════════════════════════════════════════════════════════════

// ── Bootstrap side-tables ────────────────────────────────────────
// These thread-local stores let the bootstrap evaluator represent cons
// cells, symbols, strings, and lambdas without GC heap allocation.

struct BootstrapStore {
    cons_cells: HashMap<u64, (EgclVal, EgclVal)>,
    cons_counter: u64,
    strings: HashMap<u64, String>,
    lambdas: HashMap<u64, BootLambda>,
    lambda_counter: u64,
}

#[derive(Clone)]
struct BootLambda {
    params: Vec<String>,
    body: SExpr,
    /// Captured lexical environment (flattened vars and fns at definition time).
    captured_vars: HashMap<String, EgclVal>,
    captured_fns: HashMap<String, (Vec<String>, SExpr)>,
}

impl BootstrapStore {
    fn new() -> Self {
        BootstrapStore {
            cons_cells: HashMap::new(),
            cons_counter: 1, // start at 1 to avoid zero-tagged values
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
    fn enter() -> Result<Self, EgclError> {
        BOOT_EVAL_DEPTH.with(|depth| {
            let next = depth.get() + 1;
            if next > MAX_BOOT_EVAL_DEPTH {
                let execution = crate::thread::current_fiber_id().unwrap_or_else(|| {
                    crate::thread::FiberId(crate::thread::current_thread_id().0)
                });
                Err(EgclError::StackOverflow(execution))
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

fn boot_cons(car: EgclVal, cdr: EgclVal) -> EgclVal {
    BOOT_STORE.with(|store| {
        let mut s = store.borrow_mut();
        let id = s.cons_counter;
        s.cons_counter += 1;
        s.cons_cells.insert(id, (car, cdr));
        EgclVal((id << 3) | crate::value::TAG_CONS)
    })
}

fn boot_car(val: EgclVal) -> EgclVal {
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

fn boot_cdr(val: EgclVal) -> EgclVal {
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

fn boot_intern(name: &str) -> EgclVal {
    // Symbols share the one global heap-resident registry (bliss-jtc.6 Stage B);
    // the bootstrap evaluator no longer keeps its own symbol index space.
    EgclVal::from_symbol_index(crate::symbols::intern(name))
}

fn boot_symbol_name(val: EgclVal) -> Option<String> {
    if val.tag() != crate::value::TAG_SYMBOL {
        return None;
    }
    crate::symbols::symbol_name(val.as_symbol_index())
}

fn boot_make_string(s: &str) -> EgclVal {
    BOOT_STORE.with(|store| {
        let mut st = store.borrow_mut();
        // Reader string literals are immutable, so (bliss-em3p) allocate the
        // narrowest fixed-width representation: an 8-bit SIMPLE_BASE_STRING when
        // every character is a BASE-CHAR, else a 32-bit SIMPLE_CHARACTER_STRING.
        // The choke point sizes the block to match the header word count.
        let (_tid, padded) = crate::object::narrowest_string_alloc(s);
        let layout = std::alloc::Layout::from_size_align(padded, 8).unwrap();
        let ptr = unsafe {
            let ptr = std::alloc::alloc_zeroed(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            crate::object::write_narrowest_string(ptr, s);
            ptr
        };
        let id = (ptr as u64) >> 3;
        st.strings.insert(id, s.to_string());
        unsafe { EgclVal::from_heap_ptr(ptr) }
    })
}

fn boot_make_lambda(
    params: Vec<String>,
    body: SExpr,
    captured_vars: HashMap<String, EgclVal>,
    captured_fns: HashMap<String, (Vec<String>, SExpr)>,
) -> EgclVal {
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
        EgclVal((id << 3) | crate::value::TAG_FUNCTION)
    })
}

fn boot_get_lambda(val: EgclVal) -> Option<BootLambda> {
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
    vars: HashMap<String, EgclVal>,
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
        captured_vars: &HashMap<String, EgclVal>,
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
    fn flatten_vars(&self) -> HashMap<String, EgclVal> {
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

    fn lookup(&self, name: &str) -> Option<EgclVal> {
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

fn sexpr_to_egclval(s: &SExpr) -> EgclVal {
    match s {
        SExpr::Fixnum(n) => EgclVal::from_fixnum(*n),
        SExpr::Float(f) => EgclVal::from_single_float(*f),
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
                let val = sexpr_to_egclval(e);
                result = boot_cons(val, result);
            }
            result
        }
    }
}

/// The bootstrap evaluator core. See the note on `Runtime::eval`: this is a
/// scoped, test-only subset — the full language evaluator is `cli::eval_form`
/// in the `egcl` crate. Keep these in sync only as far as the small subset
/// listed on `Runtime::eval`; do not expand this into a rival implementation.
fn eval_sexpr(expr: &SExpr, env: &mut BootEnv) -> Result<EgclVal, EgclError> {
    let _depth_guard = BootEvalDepthGuard::enter()?;
    match expr {
        SExpr::Fixnum(n) => Ok(EgclVal::from_fixnum(*n)),
        SExpr::Float(f) => Ok(EgclVal::from_single_float(*f)),
        SExpr::Nil => Ok(crate::value::NIL),
        SExpr::Bool(true) => Ok(crate::value::T),
        SExpr::Bool(false) => Ok(crate::value::NIL),
        SExpr::Str(s) => Ok(boot_make_string(s)),
        SExpr::Symbol(name) => match name.as_str() {
            "T" => Ok(crate::value::T),
            "NIL" => Ok(crate::value::NIL),
            _ => env
                .lookup(name)
                .ok_or_else(|| EgclError::Internal(format!("unbound variable: {}", name))),
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
                            return Err(EgclError::Internal(
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
                            return Err(EgclError::Internal(
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
                                return Err(EgclError::Internal(
                                    "FUNCTION: invalid argument".into(),
                                ));
                            }
                        }
                    }
                    "FUNCALL" => {
                        // (funcall fn arg1 arg2 ...)
                        if elems.len() < 2 {
                            return Err(EgclError::Internal(
                                "FUNCALL requires at least a function argument".into(),
                            ));
                        }
                        let func_val = eval_sexpr(&elems[1], env)?;
                        return eval_lambda_call(func_val, &elems[2..], env);
                    }
                    "APPLY" => {
                        // (apply fn arg1 ... argN list)
                        if elems.len() < 3 {
                            return Err(EgclError::Internal(
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
                        return Err(EgclError::Internal(format!("undefined function: {}", op)));
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

fn eval_quote(elems: &[SExpr]) -> Result<EgclVal, EgclError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    Ok(sexpr_to_egclval(&elems[1]))
}

fn eval_if(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_progn(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
    let mut result = crate::value::NIL;
    for e in &elems[1..] {
        result = eval_sexpr(e, env)?;
    }
    Ok(result)
}

fn eval_let(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_defun(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_setq(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_arith(elems: &[SExpr], env: &mut BootEnv, op: ArithOp) -> Result<EgclVal, EgclError> {
    let args: Vec<EgclVal> = elems[1..]
        .iter()
        .map(|e| eval_sexpr(e, env))
        .collect::<Result<_, _>>()?;
    if args.is_empty() {
        return match op {
            ArithOp::Add => Ok(EgclVal::from_fixnum(0)),
            ArithOp::Mul => Ok(EgclVal::from_fixnum(1)),
            ArithOp::Sub | ArithOp::Div => Err(EgclError::Internal(format!(
                "wrong number of arguments for {}",
                match op {
                    ArithOp::Sub => "-",
                    _ => "/",
                }
            ))),
        };
    }
    if !args[0].is_fixnum() {
        return Err(EgclError::TypeError {
            datum: args[0],
            expected: "number".into(),
        });
    }
    let mut acc = args[0].as_fixnum();
    if args.len() == 1 {
        return Ok(match op {
            ArithOp::Sub => EgclVal::from_fixnum(-acc),
            _ => args[0],
        });
    }
    for a in &args[1..] {
        if !a.is_fixnum() {
            return Err(EgclError::TypeError {
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
                    return Err(EgclError::ArithmeticError("division by zero".into()));
                }
                acc / n
            }
        };
    }
    Ok(EgclVal::from_fixnum(acc))
}

fn eval_numeq(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_numcmp(elems: &[SExpr], env: &mut BootEnv, cmp: NumCmp) -> Result<EgclVal, EgclError> {
    if elems.len() < 3 {
        return Ok(crate::value::T);
    }
    let a = eval_sexpr(&elems[1], env)?;
    let b = eval_sexpr(&elems[2], env)?;
    if !a.is_fixnum() {
        return Err(EgclError::TypeError {
            datum: a,
            expected: "number".into(),
        });
    }
    if !b.is_fixnum() {
        return Err(EgclError::TypeError {
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

fn eval_eq(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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
/// keyed by a monotonic counter encoded as a cons-tagged EgclVal.
fn eval_cons(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
    if elems.len() < 3 {
        return Ok(crate::value::NIL);
    }
    let car = eval_sexpr(&elems[1], env)?;
    let cdr = eval_sexpr(&elems[2], env)?;
    Ok(boot_cons(car, cdr))
}

fn eval_car(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    let val = eval_sexpr(&elems[1], env)?;
    Ok(boot_car(val))
}

fn eval_cdr(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
    if elems.len() < 2 {
        return Ok(crate::value::NIL);
    }
    let val = eval_sexpr(&elems[1], env)?;
    Ok(boot_cdr(val))
}

fn eval_list(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_null(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_atom(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_numberp(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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

fn eval_and(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
    let mut result = crate::value::T;
    for e in &elems[1..] {
        result = eval_sexpr(e, env)?;
        if result.is_nil() {
            return Ok(crate::value::NIL);
        }
    }
    Ok(result)
}

fn eval_or(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
    for e in &elems[1..] {
        let result = eval_sexpr(e, env)?;
        if !result.is_nil() {
            return Ok(result);
        }
    }
    Ok(crate::value::NIL)
}

fn eval_cond(elems: &[SExpr], env: &mut BootEnv) -> Result<EgclVal, EgclError> {
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
) -> Result<EgclVal, EgclError> {
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
    func_val: EgclVal,
    arg_exprs: &[SExpr],
    env: &mut BootEnv,
) -> Result<EgclVal, EgclError> {
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
        Err(EgclError::Internal(
            "invalid function call: not a function".into(),
        ))
    }
}

/// Call a lambda/closure value with already-evaluated argument values.
fn eval_lambda_call_with_vals(
    func_val: EgclVal,
    args: &[EgclVal],
    env: &mut BootEnv,
) -> Result<EgclVal, EgclError> {
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
        Err(EgclError::Internal(
            "invalid function call: not a function".into(),
        ))
    }
}

/// Bootstrap print: format a EgclVal as a string for output.
/// If `escape` is true, strings are printed with quotes (like PRINT/WRITE).
fn boot_print_val(val: EgclVal, escape: bool) -> String {
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
fn boot_intern_builtin(name: &str) -> EgclVal {
    // Reuse the symbol interning — when funcall'd, we check for known builtins
    boot_intern(&format!("__BUILTIN_{}", name))
}

/// Check if a value is a built-in function reference and return its name.
fn boot_builtin_name(val: EgclVal) -> Option<String> {
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
) -> Result<EgclVal, EgclError> {
    let mut evaled = Vec::new();
    for a in arg_exprs {
        evaled.push(eval_sexpr(a, env)?);
    }
    eval_builtin_call_with_vals(name, &evaled, env)
}

/// Call a built-in function by name with already-evaluated args.
fn eval_builtin_call_with_vals(
    name: &str,
    args: &[EgclVal],
    _env: &mut BootEnv,
) -> Result<EgclVal, EgclError> {
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
        _ => Err(EgclError::Internal(format!("undefined function: {}", name))),
    }
}

/// Arithmetic on pre-evaluated EgclVal args (for built-in funcall/apply).
fn arith_builtin(args: &[EgclVal], op: ArithOp) -> Result<EgclVal, EgclError> {
    if args.is_empty() {
        return match op {
            ArithOp::Add => Ok(EgclVal::from_fixnum(0)),
            ArithOp::Mul => Ok(EgclVal::from_fixnum(1)),
            ArithOp::Sub | ArithOp::Div => Err(EgclError::Internal(format!(
                "wrong number of arguments for {}",
                match op {
                    ArithOp::Sub => "-",
                    _ => "/",
                }
            ))),
        };
    }
    if !args[0].is_fixnum() {
        return Err(EgclError::TypeError {
            datum: args[0],
            expected: "number".into(),
        });
    }
    let mut acc = args[0].as_fixnum();
    if args.len() == 1 {
        return Ok(match op {
            ArithOp::Sub => EgclVal::from_fixnum(-acc),
            _ => args[0],
        });
    }
    for a in &args[1..] {
        if !a.is_fixnum() {
            return Err(EgclError::TypeError {
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
                    return Err(EgclError::ArithmeticError("division by zero".into()));
                }
                acc / n
            }
        };
    }
    Ok(EgclVal::from_fixnum(acc))
}

/// Register cooperative console interruption without Unix signal layouts.
#[cfg(windows)]
pub fn install_signal_handlers() -> Result<(), EgclError> {
    use windows_sys::Win32::System::Console::*;
    unsafe extern "system" fn handler(event: u32) -> i32 {
        match event {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                sigint_handler(0);
                1
            }
            _ => 0,
        }
    }
    if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
        return Err(EgclError::Internal(format!(
            "console handler: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}
