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
    /// Enable sandbox mode (--sandbox).
    pub sandbox: bool,
    /// Skip loading init file (--no-init).
    pub no_init: bool,
    /// Arguments passed through to CL (after --).
    pub cl_args: Vec<String>,
    /// Positional argument: script file to execute.
    pub script: Option<String>,
}

/// Helper: take the next value from args for a flag that requires one.
fn take_value<'a>(
    flag: &str,
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<String, BlissError> {
    iter.next()
        .map(|s| s.clone())
        .ok_or_else(|| BlissError::Internal(format!("{} requires a value", flag)))
}

impl CliArgs {
    /// Parse CLI arguments from a string slice.
    pub fn parse(args: &[String]) -> Result<Self, BlissError> {
        let mut result = CliArgs {
            image: None,
            eval: None,
            load: None,
            no_image: false,
            bootstrap: false,
            workers: None,
            heap_size: None,
            help: false,
            version: false,
            sandbox: false,
            no_init: false,
            cl_args: Vec::new(),
            script: None,
        };

        let mut iter = args.iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--" => {
                    // Everything after -- is passed through as CL args.
                    result.cl_args = iter.cloned().collect();
                    break;
                }
                "--help" => result.help = true,
                "--version" => result.version = true,
                "--eval" | "-e" => {
                    result.eval = Some(take_value(arg, &mut iter)?);
                }
                "--load" => {
                    result.load = Some(take_value(arg, &mut iter)?);
                }
                "--image" => {
                    result.image = Some(take_value(arg, &mut iter)?);
                }
                "--no-image" => result.no_image = true,
                "--bootstrap" => result.bootstrap = true,
                "--sandbox" => result.sandbox = true,
                "--no-init" => result.no_init = true,
                "--workers" => {
                    let val = take_value(arg, &mut iter)?;
                    let n = val.parse::<usize>().map_err(|_| {
                        BlissError::Internal(format!(
                            "--workers requires a numeric value, got: {}",
                            val
                        ))
                    })?;
                    result.workers = Some(n);
                }
                "--heap-size" => {
                    result.heap_size = Some(take_value(arg, &mut iter)?);
                }
                s if s.starts_with('-') => {
                    return Err(BlissError::Internal(format!("unknown flag: {}", s)));
                }
                _ => {
                    // Positional argument: script file.
                    result.script = Some(arg.clone());
                }
            }
        }

        // ── Conflict checks ──────────────────────────────────────────
        if result.image.is_some() && result.no_image {
            return Err(BlissError::Internal(
                "--image and --no-image are contradictory".into(),
            ));
        }
        if result.sandbox && result.no_image {
            return Err(BlissError::Internal(
                "--sandbox and --no-image are contradictory".into(),
            ));
        }
        if result.no_init && result.bootstrap {
            return Err(BlissError::Internal(
                "--no-init and --bootstrap are contradictory".into(),
            ));
        }
        if result.eval.is_some() && result.load.is_some() {
            return Err(BlissError::Internal(
                "--eval and --load are contradictory".into(),
            ));
        }

        Ok(result)
    }
}

// ── CLI driver ─────────────────────────────────────────────────────

/// Run the CLI: parse args, init runtime, dispatch to REPL/eval/load/script.
pub fn run(args: &[String]) -> Result<i32, BlissError> {
    unimplemented!("not yet implemented: cli::run")
}

/// Print usage/help text to stdout.
pub fn print_help() {
    unimplemented!("not yet implemented: cli::print_help")
}

/// Print version information to stdout.
pub fn print_version() {
    unimplemented!("not yet implemented: cli::print_version")
}

// ── REPL driver ────────────────────────────────────────────────────

/// Initialize and run the interactive REPL.
/// Sets up line editing, history, and completion.
pub fn run_repl() -> Result<i32, BlissError> {
    unimplemented!("not yet implemented: cli::run_repl")
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
        Self {
            history_file: "~/.bliss/repl-history".to_string(),
            history_size: 1000,
            syntax_highlighting: true,
        }
    }
}
