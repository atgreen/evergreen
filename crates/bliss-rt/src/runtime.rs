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

impl RuntimeConfig {
    /// Parse configuration from environment variables.
    pub fn from_env() -> Self {
        unimplemented!("RuntimeConfig::from_env")
    }

    /// Apply CLI argument overrides.
    pub fn apply_cli_args(&mut self, args: &[String]) {
        unimplemented!("RuntimeConfig::apply_cli_args")
    }

    /// Extract GC configuration subset.
    pub fn gc_config(&self) -> GcConfig {
        unimplemented!("RuntimeConfig::gc_config")
    }

    /// Extract scheduler configuration subset.
    pub fn scheduler_config(&self) -> SchedulerConfig {
        unimplemented!("RuntimeConfig::scheduler_config")
    }
}

/// The top-level Bliss runtime instance.
pub struct Runtime {
    _private: (),
}

impl Runtime {
    /// Initialize the runtime: parse config, init GC, init scheduler,
    /// load image, spawn workers. §2.2.
    pub fn init(config: RuntimeConfig) -> Result<Self, BlissError> {
        unimplemented!("Runtime::init")
    }

    /// Run the CL entry point (REPL, --eval, or --load).
    pub fn run(&mut self) -> Result<i32, BlissError> {
        unimplemented!("Runtime::run")
    }

    /// Initiate graceful shutdown. §2.9.
    pub fn shutdown(&mut self) -> Result<(), BlissError> {
        unimplemented!("Runtime::shutdown")
    }

    /// Evaluate a CL form string and return the result.
    pub fn eval(&mut self, form: &str) -> Result<BlissVal, BlissError> {
        unimplemented!("Runtime::eval")
    }

    /// Get a reference to the runtime configuration.
    pub fn config(&self) -> &RuntimeConfig {
        unimplemented!("Runtime::config")
    }
}

/// Parse CLI arguments into arguments for the runtime and arguments
/// to pass through to CL (after `--`).
pub fn parse_cli(args: &[String]) -> (RuntimeConfig, Vec<String>) {
    unimplemented!("parse_cli")
}

/// Install signal handlers (SIGSEGV, SIGINT, SIGTERM, etc.). §2.6.
pub fn install_signal_handlers() -> Result<(), BlissError> {
    unimplemented!("install_signal_handlers")
}
