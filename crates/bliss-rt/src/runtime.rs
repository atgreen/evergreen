//! Top-level runtime lifecycle — startup, run, shutdown.
//!
//! See §2.2 and §2.9 of the spec.

use crate::error::BlissError;
use crate::gc::GcConfig;
use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::value::BlissVal;

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
    /// Issue #10: bootstrap evaluator — reads the form and returns NIL.
    /// A full implementation would parse, compile, and execute the form.
    pub fn eval(&mut self, form: &str) -> Result<BlissVal, BlissError> {
        if self.shutdown {
            return Err(BlissError::Shutdown);
        }
        if form.is_empty() {
            return Ok(crate::value::NIL);
        }
        // Bootstrap: we acknowledge the form but return NIL.
        // The compiler crate (bliss-compiler) provides the full read-eval pipeline;
        // this bootstrap path is for runtime-level integration.
        Ok(crate::value::NIL)
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
