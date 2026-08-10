//! CLI entry point — argument parsing, REPL driver, and image-load entry.
//!
//! See spec §6.1 (REPL), §7.4 (deployment modes), §2.8 (CLI args).

use bliss_rt::error::BlissError;

// ── CLI arguments ──────────────────────────────────────────────────

/// Parsed CLI arguments.
#[derive(Clone, Debug)]
pub struct CliArgs {
    /// Path to the boot image (--image).
    pub image: Option<String>,
    /// Expression to evaluate and exit (--eval / -e).
    pub eval: Option<String>,
    /// File to load and exit (--load).
    pub load: Option<String>,
    /// Start without an image (--no-image).
    pub no_image: bool,
    /// Bootstrap from lib/boot.lisp (--bootstrap).
    pub bootstrap: bool,
    /// Number of worker threads (--workers).
    pub workers: Option<usize>,
    /// Heap size (--heap-size).
    pub heap_size: Option<String>,
    /// Print help and exit (--help).
    pub help: bool,
    /// Print version and exit (--version).
    pub version: bool,
    /// Arguments passed through to CL (after --).
    pub cl_args: Vec<String>,
    /// Positional argument: script file to execute.
    pub script: Option<String>,
}

impl CliArgs {
    /// Parse CLI arguments from a string slice.
    pub fn parse(args: &[String]) -> Result<Self, BlissError> {
        unimplemented!("CliArgs::parse")
    }
}

// ── CLI driver ─────────────────────────────────────────────────────

/// Run the CLI: parse args, init runtime, dispatch to REPL/eval/load/script.
pub fn run(args: &[String]) -> Result<i32, BlissError> {
    unimplemented!("cli::run")
}

/// Print usage/help text to stdout.
pub fn print_help() {
    unimplemented!("cli::print_help")
}

/// Print version information to stdout.
pub fn print_version() {
    unimplemented!("cli::print_version")
}

// ── REPL driver ────────────────────────────────────────────────────

/// Initialize and run the interactive REPL.
/// Sets up line editing, history, and completion.
pub fn run_repl() -> Result<i32, BlissError> {
    unimplemented!("cli::run_repl")
}

/// REPL configuration.
#[derive(Clone, Debug)]
pub struct ReplConfig {
    /// Path to history file (default: ~/.bliss/repl-history).
    pub history_file: String,
    /// Maximum history entries.
    pub history_size: usize,
    /// Enable syntax highlighting.
    pub syntax_highlighting: bool,
}

impl Default for ReplConfig {
    fn default() -> Self {
        unimplemented!("ReplConfig::default")
    }
}
