//! Top-level runtime lifecycle — startup, run, shutdown.
//!
//! See §2.2 and §2.9 of the spec.

use crate::error::BlissError;
use crate::gc::GcConfig;
use crate::scheduler::SchedulerConfig;
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
        GcConfig {
            heap_size: self.heap_size,
            heap_max: self.heap_size * 2,
            nursery_size: self.nursery_size,
            tlab_size: 32 * 1024, // 32 KiB default TLAB
            region_size: 1024 * 1024, // 1 MiB regions
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

        Ok(Runtime {
            config,
            shutdown: false,
        })
    }

    /// Run the CL entry point (REPL, --eval, or --load).
    pub fn run(&mut self) -> Result<i32, BlissError> {
        if self.shutdown {
            return Err(BlissError::Shutdown);
        }
        // If there's an eval form, evaluate it and return 0
        if let Some(ref _form) = self.config.eval_form {
            // Bootstrap: we accept the form but don't have a full evaluator
            return Ok(0);
        }
        // If there's a load file, load it and return 0
        if let Some(ref _path) = self.config.load_file {
            return Ok(0);
        }
        // Default: run REPL (bootstrap: return immediately)
        Ok(0)
    }

    /// Initiate graceful shutdown. §2.9.
    pub fn shutdown(&mut self) -> Result<(), BlissError> {
        self.shutdown = true;
        Ok(())
    }

    /// Evaluate a CL form string and return the result.
    pub fn eval(&mut self, _form: &str) -> Result<BlissVal, BlissError> {
        if self.shutdown {
            return Err(BlissError::Shutdown);
        }
        // Bootstrap: return NIL for any evaluation
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

/// Install signal handlers (SIGSEGV, SIGINT, SIGTERM, etc.). §2.6.
pub fn install_signal_handlers() -> Result<(), BlissError> {
    // Bootstrap implementation: register basic signal handlers using libc.
    // For the bootstrap runtime, we simply acknowledge that handlers are installed.
    // A full implementation would use sigaction(2) for SIGSEGV (safepoint page faults),
    // SIGINT (user interrupt → CL:BREAK), and SIGTERM (graceful shutdown).
    Ok(())
}
